#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SITE_DIR="$ROOT_DIR/.tmp/search-test-site"
CONTAINER_NAME="${CONTAINER_NAME:-bunker-search-test-web}"
SEARCH_TEST_PORT="${SEARCH_TEST_PORT:-8099}"
SEARCH_API_BASE="${SEARCH_API_BASE:-http://host.docker.internal:8787}"

usage() {
  cat <<USAGE
Usage:
  ./run_search_test.sh            Start/refresh PHP Apache test container
  ./run_search_test.sh stop       Stop test container

Environment overrides:
  SEARCH_TEST_PORT   Host port for test page (default: 8099)
  SEARCH_API_BASE    Backend base URL visible from container
                     (default: http://host.docker.internal:8787)
  CONTAINER_NAME     Docker container name (default: bunker-search-test-web)
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ "${1:-}" == "stop" ]]; then
  if command -v docker >/dev/null 2>&1; then
    docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  fi
  echo "Stopped container: $CONTAINER_NAME"
  exit 0
fi

if [[ -n "${1:-}" ]]; then
  usage
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required but not found in PATH." >&2
  exit 1
fi

mkdir -p "$SITE_DIR"
cp "$ROOT_DIR/src/static/bunker-search.js" "$SITE_DIR/bunker-search.js"

cat > "$SITE_DIR/index.php" <<'PHP'
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Bunker Search Test Page</title>
  <style>
    :root {
      --bg: #0b1220;
      --panel: #0f1a30;
      --text: #ecf2ff;
      --muted: #9fb1d1;
      --border: rgba(255,255,255,.14);
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif;
      background: radial-gradient(1200px 600px at 10% -10%, rgba(125,211,252,.18), transparent 55%), var(--bg);
      color: var(--text);
    }
    .wrap { max-width: 980px; margin: 0 auto; padding: 20px; }
    h1 { margin: 0 0 8px; font-size: 28px; }
    p { color: var(--muted); }
    .grid { display: grid; gap: 14px; margin-top: 16px; }
    .card {
      border: 1px solid var(--border);
      border-radius: 14px;
      background: linear-gradient(180deg, rgba(255,255,255,.04), rgba(255,255,255,.02));
      padding: 14px;
    }
    .row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
    input[type="text"], select {
      background: rgba(255,255,255,.07);
      border: 1px solid var(--border);
      color: var(--text);
      border-radius: 8px;
      padding: 8px 10px;
      min-width: 220px;
    }
    button {
      background: rgba(125,211,252,.2);
      border: 1px solid rgba(125,211,252,.4);
      border-radius: 8px;
      color: var(--text);
      padding: 8px 12px;
      cursor: pointer;
      font-weight: 600;
    }
    button:hover { background: rgba(125,211,252,.27); }
    .mono {
      font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
      font-size: 12px;
      white-space: pre-wrap;
      border: 1px solid var(--border);
      border-radius: 10px;
      padding: 10px;
      background: rgba(0,0,0,.22);
      max-height: 280px;
      overflow: auto;
    }
    .ok { color: #34d399; }
    .warn { color: #fbbf24; }
  </style>
</head>
<body>
  <div class="wrap">
    <h1>Bunker Search Test</h1>
    <p>This page runs in a PHP Apache container and proxies API calls to your local Rust backend.</p>

    <div class="grid">
      <section class="card">
        <h2 style="margin-top:0">1) Embed Widget Test</h2>
        <p style="margin-top:0">This uses the same <code>bunker-search.js</code> embed flow you plan to use on your main site.</p>
        <div id="bunker-search"></div>
        <script
          src="/bunker-search.js"
          data-target="#bunker-search"
          data-api="/proxy.php"
          data-limit="12"
          data-answer="false"
        ></script>
      </section>

      <section class="card">
        <h2 style="margin-top:0">2) API Smoke Test</h2>
        <div class="row">
          <input id="q" type="text" value="borrow checker" aria-label="query" />
          <select id="source">
            <option value="">all sources</option>
          </select>
          <label><input id="answer" type="checkbox" /> answer=true</label>
          <button id="run">Run</button>
        </div>
        <p id="status" class="warn">Checking backend...</p>
        <div id="out" class="mono">(no query yet)</div>
      </section>
    </div>
  </div>

  <script>
    const out = document.getElementById('out');
    const statusEl = document.getElementById('status');
    const q = document.getElementById('q');
    const source = document.getElementById('source');
    const answer = document.getElementById('answer');
    const run = document.getElementById('run');

    async function loadHealth() {
      try {
        const r = await fetch('/proxy.php/healthz');
        const t = await r.text();
        if (r.ok && t.trim() === 'ok') {
          statusEl.className = 'ok';
          statusEl.textContent = 'Backend reachable through proxy: ok';
        } else {
          statusEl.className = 'warn';
          statusEl.textContent = 'Backend proxy returned: ' + t;
        }
      } catch (err) {
        statusEl.className = 'warn';
        statusEl.textContent = 'Backend unreachable. Start Rust API first.';
      }
    }

    async function loadSources() {
      try {
        const r = await fetch('/proxy.php/api/sources');
        if (!r.ok) return;
        const payload = await r.json();
        if (!Array.isArray(payload.sources)) return;
        payload.sources.forEach((name) => {
          const opt = document.createElement('option');
          opt.value = name;
          opt.textContent = name;
          source.appendChild(opt);
        });
      } catch (_) {
        // keep default option only
      }
    }

    async function runQuery() {
      const params = new URLSearchParams();
      params.set('q', q.value.trim());
      params.set('limit', '8');
      if (source.value) params.set('source', source.value);
      if (answer.checked) params.set('answer', 'true');

      try {
        const r = await fetch('/proxy.php/api/search?' + params.toString());
        const text = await r.text();
        try {
          const obj = JSON.parse(text);
          out.textContent = JSON.stringify(obj, null, 2);
        } catch (_) {
          out.textContent = text;
        }
      } catch (err) {
        out.textContent = String(err);
      }
    }

    run.addEventListener('click', runQuery);
    q.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter') runQuery();
    });

    loadHealth();
    loadSources();
  </script>
</body>
</html>
PHP

cat > "$SITE_DIR/proxy.php" <<'PHP'
<?php
$base = getenv('SEARCH_API_BASE');
if (!$base) {
    $base = 'http://host.docker.internal:8787';
}

$path = $_SERVER['PATH_INFO'] ?? '';
if ($path === '/api/search') {
    $targetPath = '/api/search';
} elseif ($path === '/api/sources') {
    $targetPath = '/api/sources';
} elseif ($path === '/healthz') {
    $targetPath = '/healthz';
} else {
    http_response_code(404);
    header('Content-Type: application/json; charset=utf-8');
    echo json_encode([
        'error' => 'unknown proxy path',
        'path' => $path,
        'expected' => ['/api/search', '/api/sources', '/healthz'],
    ], JSON_PRETTY_PRINT);
    exit;
}

$query = $_SERVER['QUERY_STRING'] ?? '';
$url = rtrim($base, '/') . $targetPath;
if ($query !== '') {
    $url .= '?' . $query;
}

$context = stream_context_create([
    'http' => [
        'method' => 'GET',
        'timeout' => 20,
        'ignore_errors' => true,
        'header' => "Accept: application/json\r\nUser-Agent: bunker-search-test-page/1.0\r\n",
    ],
]);

$body = @file_get_contents($url, false, $context);
$status = 502;
if (isset($http_response_header[0]) && preg_match('/\s(\d{3})\s/', $http_response_header[0], $m)) {
    $status = (int)$m[1];
}
http_response_code($status);

if ($targetPath === '/healthz') {
    header('Content-Type: text/plain; charset=utf-8');
    if ($body === false) {
        echo 'unreachable';
    } else {
        echo $body;
    }
    exit;
}

header('Content-Type: application/json; charset=utf-8');
if ($body === false) {
    echo json_encode([
        'error' => 'unable to reach bunker-search backend',
        'target' => $url,
    ], JSON_PRETTY_PRINT);
    exit;
}

echo $body;
PHP

# Remove old container first to keep reruns simple.
docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

run_container() {
  docker run -d --rm \
    --name "$CONTAINER_NAME" \
    -p "$SEARCH_TEST_PORT:80" \
    --add-host host.docker.internal:host-gateway \
    -e "SEARCH_API_BASE=$SEARCH_API_BASE" \
    -v "$SITE_DIR:/var/www/html:ro" \
    php:8.2-apache
}

if ! run_container >/dev/null 2>&1; then
  echo "Retrying container launch without host-gateway mapping..."
  docker run -d --rm \
    --name "$CONTAINER_NAME" \
    -p "$SEARCH_TEST_PORT:80" \
    -e "SEARCH_API_BASE=$SEARCH_API_BASE" \
    -v "$SITE_DIR:/var/www/html:ro" \
    php:8.2-apache >/dev/null
fi

if command -v curl >/dev/null 2>&1; then
  if curl -fsS "http://127.0.0.1:8787/healthz" >/dev/null 2>&1; then
    backend_hint="Rust backend looks up at http://127.0.0.1:8787"
  else
    backend_hint="Rust backend not reachable at http://127.0.0.1:8787 (start it with: cargo run -- serve --config config.toml)"
  fi
else
  backend_hint="curl not found; skipped backend health check"
fi

echo ""
echo "PHP Apache test page is running: http://localhost:${SEARCH_TEST_PORT}"
echo "Proxy target (from container): ${SEARCH_API_BASE}"
echo "$backend_hint"
echo "Stop with: ./run_search_test.sh stop"
