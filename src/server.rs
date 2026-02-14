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
        .saturating_mul(3)
        .min(state.max_limit.saturating_mul(20).max(limit));

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

    rerank_hits(&query, &mut hits);

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

fn rerank_hits(query: &str, hits: &mut [SearchHit]) {
    let normalized_query = normalize_for_matching(query);
    if normalized_query.is_empty() || hits.is_empty() {
        return;
    }

    let query_tokens = tokenize(&normalized_query);
    if query_tokens.is_empty() {
        return;
    }

    for hit in hits.iter_mut() {
        hit.score = rerank_score(hit, &normalized_query, &query_tokens);
    }

    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.title.len().cmp(&right.title.len()))
            .then_with(|| left.title.cmp(&right.title))
    });
}

fn rerank_score(hit: &SearchHit, normalized_query: &str, query_tokens: &[String]) -> f32 {
    let base_score = hit.score.max(0.0);

    let normalized_title = normalize_for_matching(&hit.title);
    let normalized_preview = normalize_for_matching(&hit.preview);
    let normalized_location = normalize_for_matching(&hit.location);
    let location_lc = hit.location.to_lowercase();
    let title_lc = hit.title.to_lowercase();
    let source_lc = hit.source.to_lowercase();

    let title_coverage = token_coverage(query_tokens, &normalized_title);
    let preview_coverage = token_coverage(query_tokens, &normalized_preview);

    let mut boost = 0.0;

    if normalized_title == normalized_query {
        boost += 320.0;
    }
    if normalized_title.contains(normalized_query) && normalized_query.len() >= 5 {
        boost += 210.0;
    }

    // Title coverage gets stronger weight than snippet coverage.
    boost += title_coverage * 340.0;
    boost += preview_coverage * 90.0;

    let is_gutenberg = source_lc.contains("gutenberg");
    if is_gutenberg {
        boost += title_coverage * 240.0;
        if title_coverage >= 0.6 {
            boost += 80.0;
        }
        if title_coverage >= 0.75 {
            boost += 220.0;
        }
        if title_coverage >= 0.9 {
            boost += 160.0;
        }

        if !normalized_query.contains("chapter")
            && (title_lc.contains(", chapters") || location_lc.contains("chapters%20"))
        {
            boost -= 130.0;
        }

        if !normalized_query.contains("cover")
            && (title_lc.contains('(') || title_lc.contains("edition"))
        {
            boost -= 35.0;
        }

        if location_lc.ends_with(".html")
            && !location_lc.contains("chapters%20")
            && !location_lc.contains("_cover")
        {
            boost += 90.0;
        }
    }

    // Prefer full book page over cover page for normal title searches.
    let is_cover = normalized_title.contains(" cover")
        || normalized_location.contains(" cover")
        || location_lc.contains("_cover");
    if is_cover && !normalized_query.contains("cover") {
        boost -= 90.0;
    }

    base_score + boost
}

fn token_coverage(query_tokens: &[String], target_text: &str) -> f32 {
    if query_tokens.is_empty() || target_text.is_empty() {
        return 0.0;
    }

    let target_tokens: Vec<&str> = target_text.split_whitespace().collect();
    if target_tokens.is_empty() {
        return 0.0;
    }

    let mut exact_hits = 0usize;
    let mut prefix_hits = 0usize;

    for query_token in query_tokens {
        if target_tokens
            .iter()
            .any(|target| *target == query_token.as_str())
        {
            exact_hits += 1;
            continue;
        }

        if query_token.len() >= 3
            && target_tokens.iter().any(|target| {
                target.starts_with(query_token.as_str()) || query_token.starts_with(*target)
            })
        {
            prefix_hits += 1;
        }
    }

    (exact_hits as f32 + prefix_hits as f32 * 0.7) / query_tokens.len() as f32
}

fn tokenize(normalized_text: &str) -> Vec<String> {
    normalized_text
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
        .collect()
}

fn normalize_for_matching(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = false;

    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }

    out.trim().to_string()
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
