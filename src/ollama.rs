use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::OllamaConfig;
use crate::search::SearchHit;

#[derive(Clone)]
pub struct OllamaClient {
    client: Client,
    base_url: String,
    model: String,
    max_context_hits: usize,
    max_context_chars: usize,
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: String,
    stream: bool,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

impl OllamaClient {
    pub fn from_config(config: OllamaConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .context("failed to build Ollama HTTP client")?;

        Ok(Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            model: config.model,
            max_context_hits: config.max_context_hits.max(1),
            max_context_chars: config.max_context_chars.max(500),
        })
    }

    pub async fn synthesize_answer(&self, query: &str, hits: &[SearchHit]) -> Result<String> {
        let context = self.build_context(hits);
        if context.is_empty() {
            return Ok(String::new());
        }

        let prompt = format!(
            "You are answering questions using only the provided offline search snippets. \
If the snippets are insufficient, say what is missing.\n\nQuestion:\n{query}\n\nSearch snippets:\n{context}\n\nInstructions:\n- Give a concise answer in plain English.\n- Include 2-5 inline citations in [source | location] format.\n- Do not invent details not present in snippets."
        );

        let url = format!("{}/api/generate", self.base_url);
        let payload = GenerateRequest {
            model: &self.model,
            prompt,
            stream: false,
        };

        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .context("failed to call Ollama generate endpoint")?
            .error_for_status()
            .context("Ollama generate returned non-success status")?;

        let generated: GenerateResponse = response
            .json()
            .await
            .context("failed to parse Ollama JSON response")?;

        Ok(generated.response.trim().to_string())
    }

    fn build_context(&self, hits: &[SearchHit]) -> String {
        let mut out = String::new();
        let mut chars = 0usize;

        for hit in hits.iter().take(self.max_context_hits) {
            let chunk = format!(
                "- [{} | {}]\n  title: {}\n  preview: {}\n",
                hit.source, hit.location, hit.title, hit.preview
            );

            if chars + chunk.len() > self.max_context_chars {
                break;
            }

            chars += chunk.len();
            out.push_str(&chunk);
        }

        out
    }
}
