use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tantivy::{TantivyDocument, Term};

use crate::config::AppConfig;
use crate::ingest;
use crate::search;

const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, Copy)]
pub struct IndexStats {
    pub scanned: u64,
    pub indexed: u64,
    pub skipped: u64,
    pub removed: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    version: u8,
    docs: BTreeMap<String, String>,
}

pub fn index_sources(config: &AppConfig, rebuild: bool) -> Result<IndexStats> {
    if config.sources.is_empty() {
        tracing::warn!("config has no sources; nothing to index");
    }

    let manifest_path = manifest_path(&config.index_dir);
    let old_manifest = if rebuild {
        Manifest::default()
    } else {
        load_manifest(&manifest_path)?
    };

    let index_handle = search::open_or_create_index(&config.index_dir)?;
    let fields = index_handle.fields;

    let mut writer = index_handle
        .index
        .writer(config.writer_memory_bytes)
        .context("failed to create tantivy index writer")?;

    if rebuild {
        writer
            .delete_all_documents()
            .context("failed to clear index for rebuild")?;
    }

    let mut new_docs = BTreeMap::new();
    let mut seen_doc_ids = HashSet::new();

    let mut indexed_count = 0u64;
    let mut unchanged_count = 0u64;

    let ingest_stats = ingest::ingest_sources(config, |doc| {
        if let Some(old_fp) = old_manifest.docs.get(&doc.doc_id) {
            if !rebuild && old_fp == &doc.fingerprint {
                unchanged_count += 1;
                seen_doc_ids.insert(doc.doc_id.clone());
                new_docs.insert(doc.doc_id, old_fp.clone());
                return Ok(());
            }
        }

        let doc_id = doc.doc_id.clone();
        seen_doc_ids.insert(doc_id.clone());

        writer.delete_term(Term::from_field_text(fields.doc_id, &doc_id));

        let mut indexed_doc = TantivyDocument::default();
        indexed_doc.add_text(fields.doc_id, doc_id.clone());
        indexed_doc.add_text(fields.source, doc.source);
        indexed_doc.add_text(fields.title, doc.title);
        indexed_doc.add_text(fields.body, doc.body);
        indexed_doc.add_text(fields.preview, doc.preview);
        indexed_doc.add_text(fields.location, doc.location);
        if let Some(url) = doc.url {
            indexed_doc.add_text(fields.url, url);
        }

        writer
            .add_document(indexed_doc)
            .context("failed to add document to index")?;

        new_docs.insert(doc_id, doc.fingerprint);
        indexed_count += 1;

        Ok(())
    })?;

    let mut removed_count = 0u64;
    if !rebuild {
        for old_doc_id in old_manifest.docs.keys() {
            if !seen_doc_ids.contains(old_doc_id) {
                writer.delete_term(Term::from_field_text(fields.doc_id, old_doc_id));
                removed_count += 1;
            }
        }
    }

    if rebuild || indexed_count > 0 || removed_count > 0 {
        writer.commit().context("failed to commit index changes")?;
    }

    let new_manifest = Manifest {
        version: 1,
        docs: new_docs,
    };
    save_manifest(&manifest_path, &new_manifest)?;

    Ok(IndexStats {
        scanned: ingest_stats.scanned,
        indexed: indexed_count,
        skipped: ingest_stats.skipped + unchanged_count,
        removed: removed_count,
    })
}

fn manifest_path(index_dir: &Path) -> PathBuf {
    index_dir.join(MANIFEST_FILE)
}

fn load_manifest(path: &Path) -> Result<Manifest> {
    if !path.exists() {
        return Ok(Manifest::default());
    }

    let data = fs::read_to_string(path)
        .with_context(|| format!("failed to read manifest at {}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse manifest at {}", path.display()))?;
    Ok(manifest)
}

fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create manifest dir {}", parent.display()))?;
    }

    let data = serde_json::to_vec(manifest).context("failed to serialize manifest")?;
    fs::write(path, data)
        .with_context(|| format!("failed to write manifest at {}", path.display()))?;
    Ok(())
}
