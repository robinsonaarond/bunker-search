use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use regex::Regex;
use reqwest::{Client, Url};
use scraper::{Html, Selector};

use crate::config::KiwixConfig;
use crate::search::SearchHit;

static HEADER_TOTAL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bof\s+([0-9,]+)\b").expect("valid total regex"));

static CONTENT_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/content/([^/?#]+)").expect("valid content id regex"));

#[derive(Debug, Clone)]
pub struct KiwixCollection {
    pub id: String,
    pub title: String,
    pub category: String,
}

#[derive(Debug, Clone)]
pub struct KiwixSearchResult {
    pub total_hits: usize,
    pub hits: Vec<SearchHit>,
}

#[derive(Clone)]
pub struct KiwixClient {
    client: Client,
    base_url: Url,
    collections: Vec<KiwixCollection>,
    max_hits_per_collection: usize,
}

impl KiwixClient {
    pub async fn from_config(config: KiwixConfig) -> Result<Self> {
        let base_url = normalize_base_url(&config.base_url)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .context("failed to build Kiwix HTTP client")?;

        let categories: HashSet<String> = config
            .categories
            .iter()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty())
            .collect();

        let discovered = if config.auto_discover_collections
            || !categories.is_empty()
            || config.collections.is_empty()
        {
            discover_collections(&client, &base_url).await?
        } else {
            Vec::new()
        };

        let discovered_by_id: BTreeMap<String, KiwixCollection> = discovered
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect();

        let mut collections = if config.collections.is_empty() {
            discovered_by_id.values().cloned().collect::<Vec<_>>()
        } else {
            config
                .collections
                .iter()
                .map(|id| {
                    discovered_by_id
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| KiwixCollection {
                            id: id.to_string(),
                            title: id.to_string(),
                            category: String::new(),
                        })
                })
                .collect::<Vec<_>>()
        };

        if !categories.is_empty() {
            collections.retain(|entry| categories.contains(&entry.category.to_lowercase()));
        }

        collections.sort_by(|a, b| a.id.cmp(&b.id));
        collections.dedup_by(|a, b| a.id == b.id);

        Ok(Self {
            client,
            base_url,
            collections,
            max_hits_per_collection: config.max_hits_per_collection.max(1),
        })
    }

    pub fn source_names(&self) -> Vec<String> {
        self.collections
            .iter()
            .map(|entry| format!("kiwix:{}", entry.id))
            .collect()
    }

    pub fn collection_count(&self) -> usize {
        self.collections.len()
    }

    pub async fn search(
        &self,
        query: &str,
        source_filter: Option<&str>,
        limit: usize,
    ) -> Result<KiwixSearchResult> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(KiwixSearchResult {
                total_hits: 0,
                hits: Vec::new(),
            });
        }

        let selected = self.filtered_collections(source_filter);
        if selected.is_empty() {
            return Ok(KiwixSearchResult {
                total_hits: 0,
                hits: Vec::new(),
            });
        }

        let mut total_hits = 0usize;
        let mut hits = Vec::new();
        let page_len = self.max_hits_per_collection.max(limit.max(1)).min(75);

        for collection in selected {
            match self.search_collection(collection, query, page_len).await {
                Ok(result) => {
                    total_hits += result.total_hits;
                    hits.extend(result.hits);
                }
                Err(err) => {
                    tracing::warn!(
                        collection = %collection.id,
                        error = %err,
                        "Kiwix collection query failed"
                    );
                }
            }
        }

        hits.sort_by(|left, right| right.score.total_cmp(&left.score));

        Ok(KiwixSearchResult { total_hits, hits })
    }

    fn filtered_collections(&self, source_filter: Option<&str>) -> Vec<&KiwixCollection> {
        let Some(filter) = source_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return self.collections.iter().collect();
        };

        if filter.eq_ignore_ascii_case("kiwix") {
            return self.collections.iter().collect();
        }

        if let Some(collection_id) = filter.strip_prefix("kiwix:") {
            return self
                .collections
                .iter()
                .filter(|entry| entry.id == collection_id)
                .collect();
        }

        Vec::new()
    }

    async fn search_collection(
        &self,
        collection: &KiwixCollection,
        query: &str,
        page_len: usize,
    ) -> Result<KiwixSearchResult> {
        let search_url = self
            .base_url
            .join("search")
            .context("failed to construct Kiwix search URL")?;

        let page_len_str = page_len.to_string();

        let response = self
            .client
            .get(search_url)
            .query(&[
                ("content", collection.id.as_str()),
                ("pattern", query),
                ("start", "0"),
                ("pageLength", page_len_str.as_str()),
            ])
            .send()
            .await
            .context("failed to call Kiwix search endpoint")?
            .error_for_status()
            .context("Kiwix search returned non-success status")?;

        let body = response
            .text()
            .await
            .context("failed reading Kiwix search response body")?;

        parse_search_html(&self.base_url, collection, &body)
    }
}

fn normalize_base_url(raw: &str) -> Result<Url> {
    let mut base = raw.trim().to_string();
    if !base.ends_with('/') {
        base.push('/');
    }

    Url::parse(&base).with_context(|| format!("invalid Kiwix base_url '{raw}'"))
}

async fn discover_collections(client: &Client, base_url: &Url) -> Result<Vec<KiwixCollection>> {
    let catalog_url = base_url
        .join("catalog/v2/entries")
        .context("failed to build Kiwix OPDS URL")?;

    let xml = client
        .get(catalog_url)
        .send()
        .await
        .context("failed to fetch Kiwix OPDS feed")?
        .error_for_status()
        .context("Kiwix OPDS feed returned non-success status")?
        .text()
        .await
        .context("failed to read Kiwix OPDS body")?;

    parse_catalog_xml(&xml)
}

fn parse_catalog_xml(xml: &str) -> Result<Vec<KiwixCollection>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut in_entry = false;
    let mut current_tag: Option<String> = None;
    let mut entry = EntryTmp::default();
    let mut out = BTreeMap::<String, KiwixCollection>::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(tag)) => {
                let tag_name = String::from_utf8_lossy(tag.name().as_ref()).to_string();
                if tag_name == "entry" {
                    in_entry = true;
                    current_tag = None;
                    entry = EntryTmp::default();
                } else if in_entry {
                    if tag_name == "title" || tag_name == "category" {
                        current_tag = Some(tag_name.clone());
                    }
                    if tag_name == "link" {
                        maybe_capture_content_link(&tag, &mut entry);
                    }
                }
            }
            Ok(Event::Empty(tag)) => {
                if in_entry && tag.name().as_ref() == b"link" {
                    maybe_capture_content_link(&tag, &mut entry);
                }
            }
            Ok(Event::Text(text)) => {
                if !in_entry {
                    buf.clear();
                    continue;
                }

                if let Some(tag) = current_tag.as_deref() {
                    let value = text
                        .unescape()
                        .map(|decoded| decoded.into_owned())
                        .unwrap_or_default();
                    match tag {
                        "title" => entry.title = normalize_ws(&value),
                        "category" => entry.category = normalize_ws(&value),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(tag)) => {
                let tag_name = String::from_utf8_lossy(tag.name().as_ref()).to_string();

                if tag_name == "entry" {
                    if let Some(content_id) = entry.content_id.take() {
                        out.insert(
                            content_id.clone(),
                            KiwixCollection {
                                id: content_id,
                                title: if entry.title.is_empty() {
                                    "Kiwix".to_string()
                                } else {
                                    entry.title.clone()
                                },
                                category: entry.category.clone(),
                            },
                        );
                    }

                    in_entry = false;
                    current_tag = None;
                    entry = EntryTmp::default();
                } else if current_tag.as_deref() == Some(tag_name.as_str()) {
                    current_tag = None;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(anyhow::anyhow!("failed parsing Kiwix OPDS XML: {err}"));
            }
        }

        buf.clear();
    }

    Ok(out.into_values().collect())
}

fn maybe_capture_content_link(tag: &BytesStart<'_>, entry: &mut EntryTmp) {
    let mut href_value = None::<String>;
    let mut is_html_link = false;

    for attr in tag.attributes().with_checks(false) {
        let Ok(attr) = attr else {
            continue;
        };

        let key = attr.key.as_ref();
        let value = attr
            .unescape_value()
            .map(|value| value.into_owned())
            .unwrap_or_default();

        if key == b"type" && value == "text/html" {
            is_html_link = true;
        }

        if key == b"href" {
            href_value = Some(value);
        }
    }

    if !is_html_link {
        return;
    }

    let Some(href) = href_value else {
        return;
    };

    if let Some(content_id) = extract_content_id(&href) {
        entry.content_id = Some(content_id);
    }
}

fn parse_search_html(
    base_url: &Url,
    collection: &KiwixCollection,
    html: &str,
) -> Result<KiwixSearchResult> {
    static HEADER_SELECTOR: Lazy<Selector> =
        Lazy::new(|| Selector::parse(".header").expect("valid selector"));
    static RESULT_SELECTOR: Lazy<Selector> =
        Lazy::new(|| Selector::parse(".results li").expect("valid selector"));
    static LINK_SELECTOR: Lazy<Selector> =
        Lazy::new(|| Selector::parse("a").expect("valid selector"));
    static CITE_SELECTOR: Lazy<Selector> =
        Lazy::new(|| Selector::parse("cite").expect("valid selector"));

    let document = Html::parse_document(html);

    let header_text = document
        .select(&HEADER_SELECTOR)
        .next()
        .map(|node| normalize_ws(&node.text().collect::<Vec<_>>().join(" ")))
        .unwrap_or_default();

    let mut hits = Vec::new();
    for (idx, row) in document.select(&RESULT_SELECTOR).enumerate() {
        let Some(link) = row.select(&LINK_SELECTOR).next() else {
            continue;
        };

        let href = link.value().attr("href").unwrap_or_default().to_string();
        if href.trim().is_empty() {
            continue;
        }

        let title = normalize_ws(&link.text().collect::<Vec<_>>().join(" "));
        let preview_html = row
            .select(&CITE_SELECTOR)
            .next()
            .map(|snippet| snippet.inner_html())
            .unwrap_or_default();

        let preview = preview_from_html(&preview_html);
        let preview = if preview.is_empty() {
            format!("From {}", collection.title)
        } else {
            preview
        };

        let absolute_url = if href.starts_with('/') {
            base_url.join(href.trim_start_matches('/')).ok()
        } else {
            base_url.join(&href).ok()
        }
        .map(|url| url.to_string());

        hits.push(SearchHit {
            score: 500.0 - idx as f32,
            doc_id: format!("kiwix:{}:{}", collection.id, href),
            source: format!("kiwix:{}", collection.id),
            title: if title.is_empty() {
                "Untitled".to_string()
            } else {
                title
            },
            preview,
            location: href,
            url: absolute_url,
        });
    }

    let total_hits = parse_total_from_header(&header_text).unwrap_or(hits.len());

    Ok(KiwixSearchResult { total_hits, hits })
}

fn preview_from_html(html: &str) -> String {
    let text = html2text::from_read(html.as_bytes(), 120);
    normalize_ws(&text)
}

fn parse_total_from_header(header_text: &str) -> Option<usize> {
    let captures = HEADER_TOTAL_RE.captures(header_text)?;
    let value = captures.get(1)?.as_str().replace(',', "");
    value.parse::<usize>().ok()
}

fn extract_content_id(link: &str) -> Option<String> {
    CONTENT_ID_RE
        .captures(link)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim_end_matches('/').to_string())
}

fn normalize_ws(input: &str) -> String {
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

#[derive(Default)]
struct EntryTmp {
    title: String,
    category: String,
    content_id: Option<String>,
}
