(function () {
  const script = document.currentScript;
  const targetSelector = script?.dataset?.target || "#bunker-search";
  const resultLimit = Number(script?.dataset?.limit || "10");
  const source = script?.dataset?.source || "";
  const wantAnswer = script?.dataset?.answer === "true";

  let apiBase = script?.dataset?.api;
  if (!apiBase && script?.src) {
    try {
      apiBase = new URL(script.src, window.location.href).origin;
    } catch (_) {
      apiBase = "";
    }
  }

  if (!apiBase) {
    apiBase = window.location.origin;
  }

  const root = document.querySelector(targetSelector);
  if (!root) {
    console.warn("bunker-search: target not found", targetSelector);
    return;
  }

  if (!document.getElementById("bunker-search-style")) {
    const style = document.createElement("style");
    style.id = "bunker-search-style";
    style.textContent = `
      .bunker-search { border:1px solid rgba(255,255,255,.15); border-radius:14px; padding:12px; background:rgba(0,0,0,.16); }
      .bunker-search-input { width:100%; border:1px solid rgba(255,255,255,.2); background:rgba(255,255,255,.06); color:inherit; border-radius:10px; padding:10px 12px; font-size:14px; }
      .bunker-search-meta { margin-top:8px; font-size:12px; opacity:.8; }
      .bunker-search-answer { margin-top:10px; border:1px solid rgba(255,255,255,.14); border-radius:10px; padding:9px 10px; font-size:13px; background:rgba(255,255,255,.04); }
      .bunker-search-results { margin-top:10px; display:grid; gap:8px; }
      .bunker-search-hit { border:1px solid rgba(255,255,255,.12); border-radius:10px; padding:9px 10px; background:rgba(255,255,255,.03); }
      .bunker-search-title { font-weight:700; font-size:14px; }
      .bunker-search-title a { color:inherit; text-decoration:none; }
      .bunker-search-title a:hover { text-decoration:underline; }
      .bunker-search-preview { margin-top:4px; font-size:13px; opacity:.92; }
      .bunker-search-foot { margin-top:5px; font-size:12px; opacity:.75; display:flex; gap:8px; flex-wrap:wrap; }
      .bunker-search-empty { font-size:13px; opacity:.8; }
    `;
    document.head.appendChild(style);
  }

  const container = document.createElement("section");
  container.className = "bunker-search";
  container.innerHTML = `
    <input class="bunker-search-input" type="search" placeholder="Search your offline resources..." autocomplete="off" />
    <div class="bunker-search-meta">Type to search indexed local documents.</div>
    <div class="bunker-search-answer" style="display:none;"></div>
    <div class="bunker-search-results"></div>
  `;

  root.appendChild(container);

  const input = container.querySelector(".bunker-search-input");
  const meta = container.querySelector(".bunker-search-meta");
  const answerBox = container.querySelector(".bunker-search-answer");
  const results = container.querySelector(".bunker-search-results");

  let requestCounter = 0;
  let debounceTimer;

  function renderEmpty(message) {
    results.innerHTML = `<div class="bunker-search-empty">${message}</div>`;
  }

  function renderHits(payload) {
    if (!payload.hits || payload.hits.length === 0) {
      renderEmpty("No results.");
      return;
    }

    const html = payload.hits
      .map((hit) => {
        const title = escapeHtml(hit.title || "Untitled");
        const sourceLabel = escapeHtml(hit.source || "source");
        const preview = escapeHtml(hit.preview || "");
        const location = escapeHtml(hit.location || "");

        const titleHtml = hit.url
          ? `<a href="${escapeAttr(hit.url)}" target="_blank" rel="noopener">${title}</a>`
          : title;

        return `
          <article class="bunker-search-hit">
            <div class="bunker-search-title">${titleHtml}</div>
            <div class="bunker-search-preview">${preview}</div>
            <div class="bunker-search-foot">
              <span>${sourceLabel}</span>
              <span>${location}</span>
            </div>
          </article>
        `;
      })
      .join("");

    results.innerHTML = html;
  }

  async function runSearch() {
    const q = input.value.trim();
    if (!q) {
      renderEmpty("Start typing to search.");
      meta.textContent = "Type to search indexed local documents.";
      answerBox.style.display = "none";
      answerBox.textContent = "";
      return;
    }

    const seq = ++requestCounter;
    meta.textContent = "Searching...";

    const params = new URLSearchParams();
    params.set("q", q);
    params.set("limit", String(resultLimit));
    if (source) {
      params.set("source", source);
    }
    if (wantAnswer) {
      params.set("answer", "true");
    }

    try {
      const response = await fetch(`${apiBase}/api/search?${params.toString()}`);
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }
      const payload = await response.json();
      if (seq !== requestCounter) {
        return;
      }

      meta.textContent = `${payload.total_hits || 0} result(s)`;
      if (payload.answer) {
        answerBox.style.display = "block";
        answerBox.textContent = payload.answer;
      } else {
        answerBox.style.display = "none";
        answerBox.textContent = "";
      }
      renderHits(payload);
    } catch (err) {
      if (seq !== requestCounter) {
        return;
      }
      meta.textContent = "Search unavailable";
      answerBox.style.display = "none";
      answerBox.textContent = "";
      renderEmpty("Could not fetch results from search API.");
      console.error("bunker-search:", err);
    }
  }

  input.addEventListener("input", function () {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(runSearch, 170);
  });

  renderEmpty("Start typing to search.");

  function escapeHtml(value) {
    return String(value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/\"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }

  function escapeAttr(value) {
    return String(value).replace(/\"/g, "&quot;");
  }
})();
