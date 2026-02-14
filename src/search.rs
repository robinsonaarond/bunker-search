use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tantivy::collector::{Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, STORED, STRING, TEXT};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term};

pub const DOC_ID_FIELD: &str = "doc_id";
pub const SOURCE_FIELD: &str = "source";
pub const TITLE_FIELD: &str = "title";
pub const BODY_FIELD: &str = "body";
pub const PREVIEW_FIELD: &str = "preview";
pub const LOCATION_FIELD: &str = "location";
pub const URL_FIELD: &str = "url";

#[derive(Debug, Clone, Copy)]
pub struct IndexFields {
    pub doc_id: Field,
    pub source: Field,
    pub title: Field,
    pub body: Field,
    pub preview: Field,
    pub location: Field,
    pub url: Field,
}

#[derive(Clone)]
pub struct IndexHandle {
    pub index: Index,
    pub fields: IndexFields,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub score: f32,
    pub doc_id: String,
    pub source: String,
    pub title: String,
    pub preview: String,
    pub location: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub total_hits: usize,
    pub hits: Vec<SearchHit>,
}

#[derive(Clone)]
pub struct SearchEngine {
    index: Index,
    reader: IndexReader,
    fields: IndexFields,
}

impl SearchEngine {
    pub fn open(index_dir: &Path) -> Result<Self> {
        let handle = open_or_create_index(index_dir)?;
        let reader = handle
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .context("failed to create tantivy reader")?;

        Ok(Self {
            index: handle.index,
            reader,
            fields: handle.fields,
        })
    }

    pub fn search(
        &self,
        query_text: &str,
        limit: usize,
        offset: usize,
        source_filter: Option<&str>,
    ) -> Result<SearchResult> {
        let query_text = query_text.trim();
        if query_text.is_empty() {
            return Ok(SearchResult {
                total_hits: 0,
                hits: Vec::new(),
            });
        }

        self.reader
            .reload()
            .context("failed to refresh index reader")?;

        let searcher = self.reader.searcher();

        let parser = QueryParser::for_index(&self.index, vec![self.fields.title, self.fields.body]);
        let parsed_query = parser
            .parse_query(query_text)
            .with_context(|| format!("invalid query: {query_text}"))?;

        let combined_query: Box<dyn Query> = match source_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(source) => {
                let source_term = Term::from_field_text(self.fields.source, source);
                let source_query = TermQuery::new(source_term, IndexRecordOption::Basic);
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, parsed_query),
                    (Occur::Must, Box::new(source_query)),
                ]))
            }
            None => parsed_query,
        };

        let total_hits = searcher.search(combined_query.as_ref(), &Count)?;
        let top_docs = searcher.search(
            combined_query.as_ref(),
            &TopDocs::with_limit(limit).and_offset(offset),
        )?;

        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, doc_addr) in top_docs {
            let doc = searcher
                .doc::<TantivyDocument>(doc_addr)
                .context("failed to read indexed document")?;

            let doc_id = get_field_str(&doc, self.fields.doc_id);
            let source = get_field_str(&doc, self.fields.source);
            let title = get_field_str(&doc, self.fields.title);
            let preview = get_field_str(&doc, self.fields.preview);
            let location = get_field_str(&doc, self.fields.location);
            let url = get_field_str(&doc, self.fields.url);

            hits.push(SearchHit {
                score,
                doc_id,
                source,
                title,
                preview,
                location,
                url: if url.is_empty() { None } else { Some(url) },
            });
        }

        Ok(SearchResult { total_hits, hits })
    }
}

pub fn open_or_create_index(index_dir: &Path) -> Result<IndexHandle> {
    fs::create_dir_all(index_dir)
        .with_context(|| format!("failed to create index dir {}", index_dir.display()))?;

    let schema = build_schema();
    let mmap_dir = MmapDirectory::open(index_dir)
        .with_context(|| format!("bad index dir {}", index_dir.display()))?;
    let index = Index::open_or_create(mmap_dir, schema)
        .with_context(|| format!("failed to open/create index at {}", index_dir.display()))?;

    let fields = fields_from_schema(index.schema())?;

    Ok(IndexHandle { index, fields })
}

fn build_schema() -> Schema {
    let mut builder = Schema::builder();

    builder.add_text_field(DOC_ID_FIELD, STRING | STORED);
    builder.add_text_field(SOURCE_FIELD, STRING | STORED);
    builder.add_text_field(TITLE_FIELD, TEXT | STORED);
    builder.add_text_field(BODY_FIELD, TEXT);
    builder.add_text_field(PREVIEW_FIELD, STORED);
    builder.add_text_field(LOCATION_FIELD, STORED);
    builder.add_text_field(URL_FIELD, STORED);

    builder.build()
}

fn fields_from_schema(schema: Schema) -> Result<IndexFields> {
    Ok(IndexFields {
        doc_id: field_or_err(&schema, DOC_ID_FIELD)?,
        source: field_or_err(&schema, SOURCE_FIELD)?,
        title: field_or_err(&schema, TITLE_FIELD)?,
        body: field_or_err(&schema, BODY_FIELD)?,
        preview: field_or_err(&schema, PREVIEW_FIELD)?,
        location: field_or_err(&schema, LOCATION_FIELD)?,
        url: field_or_err(&schema, URL_FIELD)?,
    })
}

fn field_or_err(schema: &Schema, field_name: &str) -> Result<Field> {
    schema
        .get_field(field_name)
        .map_err(|_| anyhow!("missing field '{field_name}' in tantivy schema"))
}

fn get_field_str(doc: &TantivyDocument, field: Field) -> String {
    doc.get_first(field)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}
