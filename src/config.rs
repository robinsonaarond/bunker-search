use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_index_dir")]
    pub index_dir: PathBuf,

    #[serde(default = "default_bind")]
    pub bind: String,

    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,

    #[serde(default = "default_result_limit")]
    pub default_result_limit: usize,

    #[serde(default = "default_max_result_limit")]
    pub max_result_limit: usize,

    #[serde(default = "default_max_indexed_chars")]
    pub max_indexed_chars: usize,

    #[serde(default = "default_writer_memory_bytes")]
    pub writer_memory_bytes: usize,

    #[serde(default)]
    pub sources: Vec<SourceConfig>,

    #[serde(default)]
    pub kiwix: Option<KiwixConfig>,

    #[serde(default)]
    pub ollama: Option<OllamaConfig>,
}

impl AppConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let mut cfg: AppConfig = toml::from_str(&raw)
            .with_context(|| format!("failed to parse TOML config at {}", path.display()))?;

        if cfg.default_result_limit == 0 {
            cfg.default_result_limit = default_result_limit();
        }
        if cfg.max_result_limit == 0 {
            cfg.max_result_limit = default_max_result_limit();
        }
        if cfg.max_indexed_chars == 0 {
            cfg.max_indexed_chars = default_max_indexed_chars();
        }
        if cfg.writer_memory_bytes < 50_000_000 {
            cfg.writer_memory_bytes = default_writer_memory_bytes();
        }
        if let Some(kiwix) = cfg.kiwix.as_mut() {
            if kiwix.max_hits_per_collection == 0 {
                kiwix.max_hits_per_collection = default_kiwix_max_hits_per_collection();
            }
            if kiwix.timeout_secs == 0 {
                kiwix.timeout_secs = default_kiwix_timeout_secs();
            }
        }
        if let Some(ollama) = cfg.ollama.as_mut() {
            if ollama.timeout_secs == 0 {
                ollama.timeout_secs = default_ollama_timeout_secs();
            }
            if ollama.max_context_hits == 0 {
                ollama.max_context_hits = default_ollama_max_context_hits();
            }
            if ollama.max_context_chars == 0 {
                ollama.max_context_chars = default_ollama_max_context_chars();
            }
        }

        Ok(cfg)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    Filesystem {
        name: String,
        path: PathBuf,
        #[serde(default)]
        extensions: Vec<String>,
        #[serde(default)]
        follow_symlinks: bool,
    },
    Jsonl {
        name: String,
        path: PathBuf,
        #[serde(default)]
        id_field: Option<String>,
        #[serde(default)]
        title_field: Option<String>,
        #[serde(default)]
        body_field: Option<String>,
        #[serde(default)]
        url_field: Option<String>,
    },
    StackExchangeXml {
        name: String,
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct KiwixConfig {
    pub base_url: String,

    #[serde(default)]
    pub collections: Vec<String>,

    #[serde(default)]
    pub categories: Vec<String>,

    #[serde(default = "default_kiwix_auto_discover")]
    pub auto_discover_collections: bool,

    #[serde(default = "default_kiwix_max_hits_per_collection")]
    pub max_hits_per_collection: usize,

    #[serde(default = "default_kiwix_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,

    #[serde(default = "default_ollama_timeout_secs")]
    pub timeout_secs: u64,

    #[serde(default = "default_ollama_max_context_hits")]
    pub max_context_hits: usize,

    #[serde(default = "default_ollama_max_context_chars")]
    pub max_context_chars: usize,
}

fn default_index_dir() -> PathBuf {
    PathBuf::from("data/index")
}

fn default_bind() -> String {
    "127.0.0.1:8787".to_string()
}

fn default_result_limit() -> usize {
    20
}

fn default_max_result_limit() -> usize {
    100
}

fn default_max_indexed_chars() -> usize {
    200_000
}

fn default_writer_memory_bytes() -> usize {
    200_000_000
}

fn default_kiwix_auto_discover() -> bool {
    true
}

fn default_kiwix_max_hits_per_collection() -> usize {
    20
}

fn default_kiwix_timeout_secs() -> u64 {
    10
}

fn default_ollama_timeout_secs() -> u64 {
    20
}

fn default_ollama_max_context_hits() -> usize {
    8
}

fn default_ollama_max_context_chars() -> usize {
    4_000
}
