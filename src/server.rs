use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::config::{AppConfig, SourceConfig};
use crate::kiwix::KiwixClient;
use crate::ollama::OllamaClient;
use crate::search::{SearchEngine, SearchHit};

const EMBED_JS: &str = include_str!("static/bunker-search.js");

#[derive(Clone)]
struct AppState {
    engine: SearchEngine,
    kiwix: Option<KiwixClient>,
    ollama: Option<OllamaClient>,
    default_limit: usize,
    max_limit: usize,
    sources: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    source: Option<String>,
    answer: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ApiInfo {
    service: &'static str,
    docs: &'static str,
}

#[derive(Debug, Serialize)]
struct SourcesResponse {
    sources: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    total_hits: usize,
    hits: Vec<SearchHit>,
    answer: Option<String>,
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
}

struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(value: E) -> Self {
        Self(value.into())
    }
}

pub async fn serve(config: AppConfig) -> Result<()> {
    let engine = SearchEngine::open(&config.index_dir).with_context(|| {
        format!(
            "failed to open search index at {}",
            config.index_dir.display()
        )
    })?;

    let kiwix = if let Some(kiwix_config) = config.kiwix.clone() {
        let client = KiwixClient::from_config(kiwix_config)
            .await
            .context("failed to initialize Kiwix integration")?;
        tracing::info!(
            collections = client.collection_count(),
            "Kiwix integration enabled"
        );
        Some(client)
    } else {
        None
    };

    let ollama = if let Some(ollama_config) = config.ollama.clone() {
        Some(
            OllamaClient::from_config(ollama_config)
                .context("failed to initialize Ollama integration")?,
        )
    } else {
        None
    };

    let mut sources = collect_local_sources(&config.sources);
    if let Some(kiwix_client) = &kiwix {
        sources.extend(kiwix_client.source_names());
    }
    sources.sort();
    sources.dedup();

    let app_state = AppState {
        engine,
        kiwix,
        ollama,
        default_limit: config.default_result_limit,
        max_limit: config.max_result_limit,
        sources,
    };

    let app = Router::new()
        .route("/", get(api_info))
        .route("/healthz", get(healthz))
        .route("/api/search", get(search_handler))
        .route("/api/sources", get(sources_handler))
        .route("/embed/bunker-search.js", get(embed_js))
        .with_state(app_state)
        .layer(build_cors(&config.cors_allowed_origins));

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.bind))?;

    tracing::info!(bind = %config.bind, "search API listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")?;

    Ok(())
}

async fn api_info() -> Json<ApiInfo> {
    Json(ApiInfo {
        service: "bunker-search",
        docs: "GET /api/search?q=...&limit=20&source=kiwix OR source=<local>; GET /api/sources",
    })
}

async fn healthz() -> &'static str {
    "ok"
}

async fn sources_handler(State(state): State<AppState>) -> Json<SourcesResponse> {
    Json(SourcesResponse {
        sources: state.sources,
    })
}

async fn search_handler(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, ApiError> {
    let limit = params
        .limit
        .unwrap_or(state.default_limit)
        .clamp(1, state.max_limit);
    let offset = params.offset.unwrap_or(0);
    let query = params.q.unwrap_or_default();
    let source_filter = params
        .source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let want_answer = params.answer.unwrap_or(false);

    let fetch_count = offset
        .saturating_add(limit)
        .min(state.max_limit.saturating_mul(10).max(limit));

    let mut total_hits = 0usize;
    let mut hits = Vec::new();

    let local_filter = match source_filter {
        Some(filter) if is_kiwix_filter(filter) => None,
        _ => source_filter,
    };

    if source_filter.is_none() || local_filter.is_some() {
        let local_result = state
            .engine
            .search(&query, fetch_count.max(1), 0, local_filter)
            .context("local search query failed")?;

        total_hits += local_result.total_hits;
        hits.extend(local_result.hits);
    }

    if let Some(kiwix_client) = &state.kiwix {
        if source_filter.is_none() || source_filter.is_some_and(is_kiwix_filter) {
            let kiwix_result = kiwix_client
                .search(&query, source_filter, fetch_count.max(1))
                .await
                .context("Kiwix search failed")?;

            total_hits += kiwix_result.total_hits;
            hits.extend(kiwix_result.hits);
        }
    }

    let paged_hits: Vec<SearchHit> = hits.into_iter().skip(offset).take(limit).collect();

    let answer = if want_answer {
        if let Some(ollama_client) = &state.ollama {
            let generated = ollama_client
                .synthesize_answer(&query, &paged_hits)
                .await
                .context("failed generating answer from Ollama")?;
            if generated.is_empty() {
                None
            } else {
                Some(generated)
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(Json(SearchResponse {
        total_hits,
        hits: paged_hits,
        answer,
    }))
}

async fn embed_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        EMBED_JS,
    )
}

fn build_cors(origins: &[String]) -> CorsLayer {
    let base = CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_headers(Any);

    if origins.is_empty() {
        return base.allow_origin(Any);
    }

    let parsed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();

    if parsed.is_empty() {
        base.allow_origin(Any)
    } else {
        base.allow_origin(AllowOrigin::list(parsed))
    }
}

fn collect_local_sources(sources: &[SourceConfig]) -> Vec<String> {
    sources
        .iter()
        .map(|source| match source {
            SourceConfig::Filesystem { name, .. }
            | SourceConfig::Jsonl { name, .. }
            | SourceConfig::StackExchangeXml { name, .. } => name.clone(),
        })
        .collect()
}

fn is_kiwix_filter(value: &str) -> bool {
    value.eq_ignore_ascii_case("kiwix") || value.starts_with("kiwix:")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};

        if let Ok(mut signal_stream) = signal(SignalKind::terminate()) {
            let _ = signal_stream.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received");
}
