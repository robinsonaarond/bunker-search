# bunker-search

Local search backend for offline datasets, with two modes:

- Local indexing (Tantivy) for files/JSONL/XML you control.
- Kiwix federation for existing ZIM libraries (no JSONL export required).

## What it does

- Exposes unified search API at `/api/search`.
- Merges local Tantivy hits and Kiwix native hits.
- Optionally generates an AI answer via Ollama (`answer=true`).
- Ships embeddable widget at `/embed/bunker-search.js`.

## Why this avoids huge storage growth

- Kiwix datasets are queried directly using Kiwix's own index.
- Local indexing stores terms/postings plus metadata/preview, not full documents.
- Incremental manifest skips unchanged local docs.

## Source types

Local indexable sources:

- `filesystem`: recursive text/HTML/JSON/XML files.
- `jsonl`: one object per line (`id/title/body/url` configurable).
- `stack_exchange_xml`: Stack Exchange `Posts.xml` streaming parser.

Federated source:

- `[kiwix]`: query Kiwix `/search` and auto-discover collections from `/catalog/v2/entries`.

## Quick start

1. Copy and edit config:

```bash
cp config.example.toml config.toml
```

2. Optional: build local index (skip if you only use Kiwix):

```bash
cargo run -- index --config config.toml
```

3. Start API:

```bash
cargo run -- serve --config config.toml
```

4. Test search:

```bash
curl "http://127.0.0.1:8787/api/search?q=borrow+checker&limit=8"
```

5. See source names you can filter by:

```bash
curl "http://127.0.0.1:8787/api/sources"
```

## Local browser test page (PHP Apache container)

Use `run_search_test.sh` to launch a temporary `php:apache` container with:

- the same `bunker-search.js` widget
- a proxy endpoint (`proxy.php`) so browser calls do not require CORS changes
- a small API smoke-test panel

Start:

```bash
./run_search_test.sh
```

Open:

- `http://localhost:8099`

Stop:

```bash
./run_search_test.sh stop
```

Optional environment overrides:

```bash
SEARCH_TEST_PORT=9001 SEARCH_API_BASE=http://host.docker.internal:8787 ./run_search_test.sh
```

Notes:

- The script expects your Rust API on `http://127.0.0.1:8787` unless you override `SEARCH_API_BASE`.
- Temporary test files are written under `/.tmp/search-test-site`.

## Fedora production deploy

1. Install/build:

```bash
sudo mkdir -p /opt/bunker-search
sudo rsync -a ./ /opt/bunker-search/
cd /opt/bunker-search
cargo build --release
```

2. Create service user and directories:

```bash
sudo useradd --system --home /opt/bunker-search --shell /sbin/nologin bunkersearch 2>/dev/null || true
sudo mkdir -p /etc/bunker-search /var/lib/bunker-search
sudo cp /opt/bunker-search/config.toml /etc/bunker-search/config.toml
sudo chown -R bunkersearch:bunkersearch /opt/bunker-search /var/lib/bunker-search
```

3. Edit `/etc/bunker-search/config.toml`:

- `bind = "127.0.0.1:8787"`
- `index_dir = "/var/lib/bunker-search/index"`
- `[kiwix].base_url = "http://fedora.akacc.net:7070"`

4. Create systemd unit `/etc/systemd/system/bunker-search.service`:

```ini
[Unit]
Description=Bunker Search API
After=network-online.target
Wants=network-online.target

[Service]
User=bunkersearch
Group=bunkersearch
WorkingDirectory=/opt/bunker-search
ExecStart=/opt/bunker-search/target/release/bunker-search serve --config /etc/bunker-search/config.toml
Restart=on-failure
RestartSec=3
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

5. Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now bunker-search
sudo systemctl status bunker-search --no-pager
```

6. Add Apache reverse proxy (`/etc/httpd/conf.d/bunker-search.conf`):

```apache
ProxyPass        /bunker-search/ http://127.0.0.1:8787/
ProxyPassReverse /bunker-search/ http://127.0.0.1:8787/
```

7. SELinux + Apache reload:

```bash
sudo setsebool -P httpd_can_network_connect 1
sudo apachectl configtest
sudo systemctl reload httpd
```

8. Verify:

```bash
curl -s https://fedora.akacc.net/bunker-search/healthz
curl -s "https://fedora.akacc.net/bunker-search/api/search?q=rust&limit=5&source=kiwix"
```

## Fedora site integration (widget)

Embed on `https://fedora.akacc.net/`:

```html
<div id="bunker-search-home"></div>
<script
  src="/bunker-search/embed/bunker-search.js"
  data-target="#bunker-search-home"
  data-api="/bunker-search"
  data-limit="10"
  data-source="kiwix"
  data-answer="false"
></script>
```

Set `data-answer="true"` only if `[ollama]` is configured.

The repo also includes a prepatched homepage file pulled from production:

- `index.html`

That file replaces the old "Chat button below is the easiest entry point" subtitle line with the search bar block.

## API

### `GET /api/search`

Params:

- `q` required: search text.
- `limit` optional.
- `offset` optional.
- `source` optional filter:
  - local source name (for Tantivy source), or
  - `kiwix`, or
  - `kiwix:<collection_id>`.
- `answer` optional bool (`true/false`): if Ollama is configured, return synthesized answer.

Response shape:

```json
{
  "total_hits": 123,
  "hits": [
    {
      "score": 10.2,
      "doc_id": "kiwix:wikipedia_en_all_mini_2025-06:/content/...",
      "source": "kiwix:wikipedia_en_all_mini_2025-06",
      "title": "Rust",
      "preview": "...",
      "location": "/content/wikipedia_en_all_mini_2025-06/Rust",
      "url": "http://fedora.akacc.net:7070/content/wikipedia_en_all_mini_2025-06/Rust"
    }
  ],
  "answer": null
}
```

### `GET /api/sources`

Lists all local and Kiwix source names currently available.

### `GET /healthz`

Returns `ok` when the service is up.

## Notes

- If Kiwix has millions of docs, federation avoids building a second giant index.
- If you still want one unified local-only index for non-Kiwix data, keep using `index` with local sources.
- Ollama integration is optional and disabled unless `[ollama]` is configured.
