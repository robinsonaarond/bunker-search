use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use blake3::Hasher;
use content_inspector::{inspect, ContentType};
use once_cell::sync::Lazy;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use regex::Regex;
use serde_json::Value;
use walkdir::WalkDir;

use crate::config::{AppConfig, SourceConfig};

static HTML_TITLE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("valid html title regex"));

static DEFAULT_TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "rst", "org", "tex", "html", "htm", "xhtml", "xml", "json", "jsonl",
    "csv", "tsv", "log",
];

#[derive(Debug, Clone)]
pub struct RawDocument {
    pub doc_id: String,
    pub source: String,
    pub title: String,
    pub body: String,
    pub preview: String,
    pub location: String,
    pub url: Option<String>,
    pub fingerprint: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct IngestStats {
    pub scanned: u64,
    pub emitted: u64,
    pub skipped: u64,
}

pub fn ingest_sources<F>(config: &AppConfig, mut on_doc: F) -> Result<IngestStats>
where
    F: FnMut(RawDocument) -> Result<()>,
{
    let mut total = IngestStats::default();

    for source in &config.sources {
        let source_stats = match source {
            SourceConfig::Filesystem {
                name,
                path,
                extensions,
                follow_symlinks,
            } => ingest_filesystem(
                config,
                name,
                path,
                extensions,
                *follow_symlinks,
                &mut on_doc,
            )?,
            SourceConfig::Jsonl {
                name,
                path,
                id_field,
                title_field,
                body_field,
                url_field,
            } => ingest_jsonl(
                config,
                name,
                path,
                id_field.as_deref(),
                title_field.as_deref(),
                body_field.as_deref(),
                url_field.as_deref(),
                &mut on_doc,
            )?,
            SourceConfig::StackExchangeXml { name, path } => {
                ingest_stackexchange_xml(config, name, path, &mut on_doc)?
            }
        };

        total.scanned += source_stats.scanned;
        total.emitted += source_stats.emitted;
        total.skipped += source_stats.skipped;
    }

    Ok(total)
}

fn ingest_filesystem<F>(
    config: &AppConfig,
    source_name: &str,
    root: &Path,
    extensions: &[String],
    follow_symlinks: bool,
    on_doc: &mut F,
) -> Result<IngestStats>
where
    F: FnMut(RawDocument) -> Result<()>,
{
    let mut stats = IngestStats::default();

    let whitelist: Vec<String> = if extensions.is_empty() {
        DEFAULT_TEXT_EXTENSIONS
            .iter()
            .map(|ext| (*ext).to_string())
            .collect()
    } else {
        extensions.iter().map(|ext| ext.to_lowercase()).collect()
    };

    for entry in WalkDir::new(root)
        .follow_links(follow_symlinks)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(%err, "walkdir entry error");
                None
            }
        })
    {
        if !entry.file_type().is_file() {
            continue;
        }

        stats.scanned += 1;

        let path = entry.path();
        if !is_extension_allowed(path, &whitelist) {
            stats.skipped += 1;
            continue;
        }

        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "unable to read file");
                stats.skipped += 1;
                continue;
            }
        };

        if matches!(inspect(&bytes), ContentType::BINARY) {
            stats.skipped += 1;
            continue;
        }

        let raw_text = String::from_utf8_lossy(&bytes).into_owned();
        let ext = file_extension(path).unwrap_or_default();
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let (mut title, body_source) = if is_html_ext(&ext) {
            let extracted_title = extract_html_title(&raw_text)
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| path_to_title(rel));
            let body = html2text::from_read(raw_text.as_bytes(), 120);
            (extracted_title, body)
        } else {
            let title = path_to_title(rel);
            (title, raw_text)
        };

        title = normalize_whitespace(&title);
        if title.is_empty() {
            title = rel_str.clone();
        }

        let body = truncate_chars(
            &normalize_whitespace(&body_source),
            config.max_indexed_chars,
        );
        if body.is_empty() {
            stats.skipped += 1;
            continue;
        }

        let fingerprint = fingerprint_for_file(path).unwrap_or_else(|_| "0:0".to_string());

        let doc = RawDocument {
            doc_id: format!("fs:{source_name}:{rel_str}"),
            source: source_name.to_string(),
            title,
            preview: preview_from_text(&body, 280),
            body,
            location: rel_str,
            url: None,
            fingerprint,
        };

        on_doc(doc)?;
        stats.emitted += 1;
    }

    Ok(stats)
}

fn ingest_jsonl<F>(
    config: &AppConfig,
    source_name: &str,
    path: &Path,
    id_field: Option<&str>,
    title_field: Option<&str>,
    body_field: Option<&str>,
    url_field: Option<&str>,
    on_doc: &mut F,
) -> Result<IngestStats>
where
    F: FnMut(RawDocument) -> Result<()>,
{
    let mut stats = IngestStats::default();

    let file = File::open(path)
        .with_context(|| format!("failed to open JSONL source {}", path.display()))?;
    let reader = BufReader::new(file);

    let id_field = id_field.unwrap_or("id");
    let title_field = title_field.unwrap_or("title");
    let body_field = body_field.unwrap_or("body");
    let url_field = url_field.unwrap_or("url");

    for (line_idx, line) in reader.lines().enumerate() {
        stats.scanned += 1;

        let line = match line {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!(path = %path.display(), line = line_idx + 1, %err, "failed to read JSONL line");
                stats.skipped += 1;
                continue;
            }
        };

        if line.trim().is_empty() {
            stats.skipped += 1;
            continue;
        }

        let parsed: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(path = %path.display(), line = line_idx + 1, %err, "invalid JSONL object");
                stats.skipped += 1;
                continue;
            }
        };

        let id =
            value_to_string(parsed.get(id_field)).unwrap_or_else(|| (line_idx + 1).to_string());
        let mut title =
            value_to_string(parsed.get(title_field)).unwrap_or_else(|| format!("Document {id}"));
        let body = value_to_string(parsed.get(body_field)).unwrap_or_default();
        let url = value_to_string(parsed.get(url_field)).filter(|value| !value.trim().is_empty());

        let body = truncate_chars(&normalize_whitespace(&body), config.max_indexed_chars);
        if body.is_empty() {
            stats.skipped += 1;
            continue;
        }

        title = normalize_whitespace(&title);
        if title.is_empty() {
            title = format!("Document {id}");
        }

        let mut hasher = Hasher::new();
        hasher.update(line.as_bytes());

        let location = format!("{}:{}", path.display(), line_idx + 1);
        let doc = RawDocument {
            doc_id: format!("jsonl:{source_name}:{id}"),
            source: source_name.to_string(),
            title,
            preview: preview_from_text(&body, 280),
            body,
            location,
            url,
            fingerprint: hasher.finalize().to_hex().to_string(),
        };

        on_doc(doc)?;
        stats.emitted += 1;
    }

    Ok(stats)
}

fn ingest_stackexchange_xml<F>(
    config: &AppConfig,
    source_name: &str,
    path: &Path,
    on_doc: &mut F,
) -> Result<IngestStats>
where
    F: FnMut(RawDocument) -> Result<()>,
{
    let mut stats = IngestStats::default();

    let file = File::open(path).with_context(|| {
        format!(
            "failed to open Stack Exchange XML source {}",
            path.display()
        )
    })?;
    let mut reader = Reader::from_reader(BufReader::new(file));
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(tag)) if tag.name().as_ref() == b"row" => {
                process_stackexchange_row(config, source_name, path, &tag, on_doc, &mut stats)?;
            }
            Ok(Event::Start(tag)) if tag.name().as_ref() == b"row" => {
                process_stackexchange_row(config, source_name, path, &tag, on_doc, &mut stats)?;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(anyhow::anyhow!(
                    "error while parsing {} at byte {}: {err}",
                    path.display(),
                    reader.buffer_position()
                ));
            }
        }

        buf.clear();
    }

    Ok(stats)
}

fn process_stackexchange_row<F>(
    config: &AppConfig,
    source_name: &str,
    path: &Path,
    tag: &BytesStart<'_>,
    on_doc: &mut F,
    stats: &mut IngestStats,
) -> Result<()>
where
    F: FnMut(RawDocument) -> Result<()>,
{
    stats.scanned += 1;

    let mut id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut body: Option<String> = None;
    let mut last_activity: Option<String> = None;

    for attr in tag.attributes().with_checks(false) {
        let attr = match attr {
            Ok(attr) => attr,
            Err(err) => {
                tracing::warn!(%err, "invalid XML attribute");
                continue;
            }
        };

        let value = attr
            .unescape_value()
            .map(|v| v.into_owned())
            .unwrap_or_default();

        match attr.key.as_ref() {
            b"Id" => id = Some(value),
            b"Title" => title = Some(value),
            b"Body" => body = Some(value),
            b"LastActivityDate" => last_activity = Some(value),
            _ => {}
        }
    }

    let id = match id {
        Some(id) => id,
        None => {
            stats.skipped += 1;
            return Ok(());
        }
    };

    let body_raw = body.unwrap_or_default();
    let body_plain = if body_raw.is_empty() {
        String::new()
    } else {
        html2text::from_read(body_raw.as_bytes(), 120)
    };
    let body = truncate_chars(&normalize_whitespace(&body_plain), config.max_indexed_chars);

    if body.is_empty() && title.as_deref().unwrap_or_default().trim().is_empty() {
        stats.skipped += 1;
        return Ok(());
    }

    let title = normalize_whitespace(&title.unwrap_or_else(|| infer_title_from_body(&body, &id)));
    let title = if title.is_empty() {
        format!("Post {id}")
    } else {
        title
    };

    let body = if body.is_empty() { title.clone() } else { body };

    let doc = RawDocument {
        doc_id: format!("stackexchange:{source_name}:{id}"),
        source: source_name.to_string(),
        title,
        preview: preview_from_text(&body, 280),
        body,
        location: format!("{}#{}", path.display(), id),
        url: None,
        fingerprint: format!("{}:{}", last_activity.unwrap_or_default(), body_raw.len()),
    };

    on_doc(doc)?;
    stats.emitted += 1;
    Ok(())
}

fn path_to_title(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
        return stem.replace(['_', '-'], " ");
    }

    path.to_string_lossy().to_string()
}

fn extract_html_title(raw_html: &str) -> Option<String> {
    HTML_TITLE_RE
        .captures(raw_html)
        .and_then(|capture| capture.get(1))
        .map(|match_| normalize_whitespace(match_.as_str()))
}

fn normalize_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(4096));
    let mut last_was_space = false;

    for ch in input.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }

    out.trim().to_string()
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut char_count = 0usize;
    for (byte_idx, _) in input.char_indices() {
        if char_count == max_chars {
            return input[..byte_idx].to_string();
        }
        char_count += 1;
    }

    input.to_string()
}

fn preview_from_text(input: &str, max_chars: usize) -> String {
    let truncated = truncate_chars(input, max_chars);
    if truncated.len() < input.len() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn value_to_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) => Some(value.to_string()),
        Some(Value::Number(value)) => Some(value.to_string()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn is_extension_allowed(path: &Path, whitelist: &[String]) -> bool {
    let ext = file_extension(path);
    match ext {
        Some(ext) => whitelist.iter().any(|allowed| allowed == &ext),
        None => false,
    }
}

fn file_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
}

fn is_html_ext(ext: &str) -> bool {
    matches!(ext, "html" | "htm" | "xhtml")
}

fn infer_title_from_body(body: &str, id: &str) -> String {
    if body.is_empty() {
        return format!("Post {id}");
    }

    preview_from_text(body, 80)
}

fn fingerprint_for_file(path: &Path) -> Result<String> {
    let meta =
        fs::metadata(path).with_context(|| format!("metadata failed for {}", path.display()))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    Ok(format!("{}:{}", meta.len(), modified))
}

#[allow(dead_code)]
fn _normalize_path(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().replace('\\', "/"))
}
