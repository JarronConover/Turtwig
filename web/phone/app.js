/** The a.box phone app: the box daemon's model over /ws, shown as the sidebar, inbox, chats, and a new-chat form. */
(() => {
  const $app = document.getElementById("app");
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  const now = () => Date.now() / 1000;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  const state = {
    model: null,
    config: null,
    conn: "connecting",
    route: { name: "home" },
    scope: localStorage.getItem("scope") || "active",
    collapsed: JSON.parse(localStorage.getItem("collapsed") || "{}"),
    tx: null,
    picks: {},
    answered: {},
    readSent: new Set(),
    starting: false,
    pushOn: false,
  };

  function ago(t) {
    if (!t) return "";
    const s = Math.max(0, now() - t);
    if (s < 60) return "now";
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    if (s < 86400) return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
  }

  function duration(since) {
    const s = Math.max(0, Math.floor(now() - since));
    return s < 60 ? `${s}s` : s < 3600 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  }

  function toast(text) {
    const el = document.createElement("div");
    el.className = "toast";
    el.textContent = text;
    document.body.appendChild(el);
    setTimeout(() => el.remove(), 4000);
  }

  class Conn {
    constructor() {
      this.next = 1;
      this.pending = new Map();
      this.ws = null;
      this.backoff = 500;
    }

    open() {
      const z = "DecompressionStream" in window;
      const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws${z ? "?z=1" : ""}`);
      ws.binaryType = "arraybuffer";
      this.ws = ws;
      const feed = z ? this.inflater() : null;
      ws.onopen = () => {
        this.backoff = 500;
        setConn("open");
        onOpen();
      };
      ws.onmessage = (ev) => (typeof ev.data === "string" ? ev.data.split("\n").forEach((l) => l && this.line(l)) : feed(ev.data));
      ws.onclose = () => {
        if (this.ws !== ws) return;
        this.ws = null;
        this.failAll();
        setConn("closed");
        setTimeout(() => !this.ws && this.open(), this.backoff);
        this.backoff = Math.min(this.backoff * 2, 8000);
      };
    }

    inflater() {
      const ds = new DecompressionStream("deflate");
      const writer = ds.writable.getWriter();
      const reader = ds.readable.pipeThrough(new TextDecoderStream()).getReader();
      let buf = "";
      (async () => {
        for (;;) {
          const { value, done } = await reader.read();
          if (done) return;
          buf += value;
          let i;
          while ((i = buf.indexOf("\n")) >= 0) {
            const line = buf.slice(0, i);
            buf = buf.slice(i + 1);
            if (line) this.line(line);
          }
        }
      })().catch((e) => console.error("a.box stream", e));
      return (data) => writer.write(new Uint8Array(data)).catch((e) => console.error("a.box stream", e));
    }

    line(l) {
      let m;
      try {
        m = JSON.parse(l);
      } catch {
        return;
      }
      if (m.event) return onEvent(m.event, m.data);
      const p = this.pending.get(m.id);
      if (!p) return;
      this.pending.delete(m.id);
      m.error ? p.reject(new Error(m.error)) : p.resolve(m.result);
    }

    call(method, params) {
      return new Promise((resolve, reject) => {
        if (!this.ws || this.ws.readyState !== 1) return reject(new Error("Not connected to the box"));
        const id = this.next++;
        this.pending.set(id, { resolve, reject });
        this.ws.send(JSON.stringify(params === undefined ? { id, method } : { id, method, params }));
      });
    }

    failAll() {
      for (const p of this.pending.values()) p.reject(new Error("Lost the connection to the box"));
      this.pending.clear();
    }

    reconnect() {
      const old = this.ws;
      this.ws = null;
      this.failAll();
      if (old) old.close();
      this.backoff = 500;
      this.open();
    }
  }

  const conn = new Conn();
  const call = (m, p) => conn.call(m, p);

  function setConn(s) {
    state.conn = s;
    const dot = document.querySelector(".conn");
    if (dot) dot.className = `conn ${s}`;
  }

  async function onOpen() {
    try {
      await call("hello", { client: "phone" });
      await call("subscribe", { from_seq: null });
      state.config = await call("config_get");
      if (state.route.name === "new") {
        state.forceNew = true;
        refresh();
      }
    } catch (e) {
      toast(e.message);
    }
    if (state.route.name === "chat") loadTranscript(state.route.id);
  }

  let renderQueued = false;
  function onEvent(name, data) {
    if (name === "model") {
      state.model = data;
      if (!renderQueued) {
        renderQueued = true;
        requestAnimationFrame(() => {
          renderQueued = false;
          refresh();
        });
      }
    } else if (name === "transcript" && state.tx && state.tx.id === data.chat_id) {
      appendEntries(data.from, data.entries);
    }
  }

  const chatsById = () => Object.fromEntries((state.model?.chats || []).map((c) => [c.id, c]));
  const chatOf = (id) => (state.model?.chats || []).find((c) => c.id === id);
  const allWorktrees = () => [...(state.model?.sections || []).flatMap((s) => s.items), ...(state.model?.projects || []).flatMap((p) => p.items)];

  function chatMeta(c) {
    if (c.state === "waiting") return "Needs you";
    if (c.state === "working") {
      const sl = c.status_line;
      return sl ? `${sl.verb}${sl.since ? ` · ${duration(sl.since)}` : ""}` : "Working";
    }
    const when = c.last_activity ? ` · ${ago(c.last_activity)}` : "";
    return c.state === "stopped" || !c.alive ? `Stopped${when}` : `Idle${when}`;
  }

  const dotClass = (c) => `st-${c.state === "waiting" ? "waiting" : c.state === "working" ? "working" : c.alive && c.state !== "stopped" ? "idle" : "stopped"}`;

  function chatRow(c) {
    return `<button class="chat-row" data-go="#/chat/${esc(c.id)}"><span class="dot ${dotClass(c)}"></span><span class="name">${esc(c.name)}</span><span class="meta">${esc(chatMeta(c))}</span></button>`;
  }

  function mrBadge(mr) {
    if (!mr) return "";
    const checks = mr.checks ? `<span class="checks ${esc(mr.checks)}"></span>` : "";
    return `<a class="mr ${esc(mr.state)}" href="${esc(mr.url)}" target="_blank" rel="noopener">!${mr.number}${checks}</a>`;
  }

  function isActive(w, byId) {
    const chats = w.chat_ids.map((id) => byId[id]).filter(Boolean);
    return chats.some((c) => c.alive || c.state === "waiting") || (w.age && now() - w.age < 86400);
  }

  function worktreeHtml(w, byId) {
    const chats = w.chat_ids.map((id) => byId[id]).filter(Boolean);
    const counts = [w.uncommitted ? `<span class="count">±${w.uncommitted}</span>` : "", w.unpushed ? `<span class="count">↑${w.unpushed}</span>` : ""].join("");
    const sub = [w.branch, w.host !== "box" ? w.host : "", ago(w.age)].filter(Boolean).join(" · ");
    return `<div class="wt">
      <div class="wt-head"><span class="wt-title">${esc(w.title)}</span>${counts}${mrBadge(w.mr)}<button class="wt-new" data-new-dir="${esc(w.path)}" data-new-repo="${esc(w.repo)}" aria-label="New chat here">+</button></div>
      <div class="wt-sub">${esc(sub)}</div>
      ${chats.map(chatRow).join("")}
    </div>`;
  }

  function groupHtml(key, name, items, byId) {
    const shown = state.scope === "all" ? items : items.filter((w) => isActive(w, byId));
    if (!shown.length) return "";
    const open = !state.collapsed[key];
    return `<section class="group">
      <button class="group-head" data-toggle="${esc(key)}"><span class="chev ${open ? "open" : ""}">›</span>${esc(name)}<span class="group-count">${shown.length}</span></button>
      ${open ? shown.map((w) => worktreeHtml(w, byId)).join("") : ""}
    </section>`;
  }

  function header(left, title, right) {
    return `<header class="bar">${left}<div class="bar-title">${title}</div>${right}</header>`;
  }

  function homeHeader() {
    const n = state.model?.inbox?.length || 0;
    return header(
      `<span class="logo">a.box</span><span class="conn ${state.conn}"></span>`,
      "",
      `<button class="icon-btn" data-go="#/inbox" aria-label="Inbox">${bell}${n ? `<span class="badge">${n}</span>` : ""}</button><button class="icon-btn" data-go="#/new" aria-label="New chat">${plus}</button>`
    );
  }

  const bell = `<svg viewBox="0 0 24 24" width="22" height="22"><path fill="currentColor" d="M12 22a2.5 2.5 0 0 0 2.45-2h-4.9A2.5 2.5 0 0 0 12 22Zm7-6V11a7 7 0 0 0-5.5-6.84V3.5a1.5 1.5 0 0 0-3 0v.66A7 7 0 0 0 5 11v5l-2 2v1h18v-1Z"/></svg>`;
  const plus = `<svg viewBox="0 0 24 24" width="22" height="22"><path fill="currentColor" d="M11 5h2v6h6v2h-6v6h-2v-6H5v-2h6z"/></svg>`;
  const back = `<button class="icon-btn back" data-go="#/" aria-label="Back"><svg viewBox="0 0 24 24" width="24" height="24"><path fill="currentColor" d="M15.4 4.6 14 3.2 5.2 12l8.8 8.8 1.4-1.4L8 12z"/></svg></button>`;

  function renderHome() {
    const m = state.model;
    if (!m) return `${homeHeader()}<main class="scroll"><div class="empty">Connecting to the box…</div></main>`;
    const byId = chatsById();
    const seen = new Set(allWorktrees().flatMap((w) => w.chat_ids));
    const waiting = m.chats.filter((c) => c.state === "waiting").sort((a, b) => (b.last_activity || 0) - (a.last_activity || 0));
    const loose = m.chats
      .filter((c) => !seen.has(c.id) && (c.alive || now() - (c.last_activity || 0) < 2 * 86400))
      .sort((a, b) => (b.last_activity || 0) - (a.last_activity || 0));
    const groups = [
      ...m.projects.map((p) => groupHtml(`p:${p.repo}`, p.name, p.items, byId)),
      ...m.sections.map((s) => groupHtml(`s:${s.id}`, s.name, s.items, byId)),
    ].join("");
    return `${homeHeader()}
      <main class="scroll">
        <div class="scope"><button class="${state.scope === "active" ? "on" : ""}" data-scope="active">Active</button><button class="${state.scope === "all" ? "on" : ""}" data-scope="all">All</button></div>
        ${waiting.length ? `<section class="group needs"><div class="group-head static">Needs you<span class="group-count">${waiting.length}</span></div>${waiting.map(chatRow).join("")}</section>` : ""}
        ${groups}
        ${loose.length ? `<section class="group"><div class="group-head static">Other chats<span class="group-count">${loose.length}</span></div>${loose.map(chatRow).join("")}</section>` : ""}
      </main>`;
  }

  function renderInbox() {
    const items = state.model?.inbox || [];
    const pushBtn = state.pushOn ? "" : `<button class="wide" data-act="push">Turn on notifications</button>`;
    const list = items.length
      ? items
          .map(
            (it) => `<div class="inbox-item">
          <div class="inbox-title">${esc(it.title)}</div>
          ${it.text ? `<div class="inbox-text">${md(it.text)}</div>` : ""}
          <div class="inbox-meta">${esc([it.source, ago(it.updated_at || it.at)].filter(Boolean).join(" · "))}</div>
          <div class="inbox-actions">${it.chat ? `<button data-act="inbox-open" data-id="${it.id}" data-chat="${esc(it.chat)}">Open chat</button>` : ""}<button data-act="inbox-done" data-id="${it.id}">Done</button></div>
        </div>`
          )
          .join("")
      : `<div class="empty">${state.model ? "Nothing waiting on you." : "Connecting to the box…"}</div>`;
    return `${header(back, "Inbox", items.length ? `<button class="text-btn" data-act="inbox-all">Clear all</button>` : `<span class="bar-pad"></span>`)}
      <main class="scroll">${pushBtn}${list}</main>`;
  }

  function renderNew() {
    const cfg = state.config?.resolved || {};
    const repos = Object.keys(cfg.repos || {});
    const preset = state.route;
    const repo = preset.repo || cfg.default_repo || repos[0] || "";
    const agents = Object.keys(state.config?.options?.agents || { claude: 1 });
    const defaultAgent = state.config?.options?.default_agent || "claude";
    const worktrees = allWorktrees().filter((w) => w.repo === repo && w.host === (state.model?.host || "box"));
    return `${header(back, "New chat", `<span class="bar-pad"></span>`)}
      <main class="scroll form">
        <label>Repo<select id="nc-repo">${repos.map((r) => `<option ${r === repo ? "selected" : ""}>${esc(r)}</option>`).join("")}</select></label>
        <label>Where<select id="nc-where"><option value="">New worktree</option>${worktrees
          .map((w) => `<option value="${esc(w.path)}" ${w.path === preset.dir ? "selected" : ""}>${esc(w.title)}${w.branch ? ` (${esc(w.branch)})` : ""}</option>`)
          .join("")}</select></label>
        <label id="nc-branch-row" class="${preset.dir ? "hidden" : ""}">Branch<input id="nc-branch" placeholder="Optional" autocapitalize="off" autocorrect="off"></label>
        <label>Prompt<textarea id="nc-prompt" rows="6" placeholder="What should it do?"></textarea></label>
        <label>Name<input id="nc-name" placeholder="Named from the prompt"></label>
        ${agents.length > 1 ? `<label>Agent<select id="nc-agent">${agents.map((a) => `<option ${a === defaultAgent ? "selected" : ""}>${esc(a)}</option>`).join("")}</select></label>` : ""}
        <button class="wide primary" data-act="new-start" ${state.starting ? "disabled" : ""}>${state.starting ? "Starting…" : "Start chat"}</button>
      </main>`;
  }

  // Chat view: the shell is built once per chat; model changes update its header, question card and status.
  function chatShell(id) {
    return `<div id="chat-head"></div>
      <main class="scroll tx" id="tx"><button class="older hidden" id="older" data-act="older">Load earlier</button><div id="tx-list"></div><div id="tx-pending"></div></main>
      <div id="waiting"></div>
      <div id="status"></div>
      <div class="composer"><textarea id="input" rows="1" placeholder="Message"></textarea><button class="send" id="send" data-act="send" aria-label="Send"><svg viewBox="0 0 24 24" width="20" height="20"><path fill="currentColor" d="M12 4 5 11l1.4 1.4L11 7.8V20h2V7.8l4.6 4.6L19 11z"/></svg></button></div>`;
  }

  function updateChat() {
    const id = state.route.id;
    const c = chatOf(id);
    const head = document.getElementById("chat-head");
    if (!head) return;
    const sub = c ? [chatMeta(c), c.branch].filter(Boolean).join(" · ") : "";
    head.innerHTML = header(
      back,
      `<div class="chat-name">${esc(c?.name || "Chat")}</div><div class="chat-sub"><span class="dot ${c ? dotClass(c) : "st-stopped"}"></span>${esc(sub)}</div>`,
      `<button class="icon-btn" data-act="menu" aria-label="More">⋯</button>`
    );
    document.getElementById("waiting").innerHTML = c ? waitingHtml(c) : "";
    const status = document.getElementById("status");
    let line = "";
    if (c?.state === "working") {
      const sl = c.status_line;
      line = `<div class="status-line"><span class="spin">✻</span> ${esc(sl ? [sl.verb, sl.since ? duration(sl.since) : "", sl.tokens].filter(Boolean).join(" · ") : "Working")}<button class="text-btn" data-act="interrupt">Stop</button></div>`;
    } else if (c && (!c.alive || c.state === "stopped")) {
      line = `<div class="status-line muted">Stopped. Sending a message starts it again.</div>`;
    } else if (c?.suggestion) {
      line = `<button class="suggestion" data-act="suggest">${esc(c.suggestion)}</button>`;
    }
    status.innerHTML = line;
    markRead(id);
  }

  function markRead(id) {
    for (const it of state.model?.inbox || []) {
      const key = `${it.id}:${it.rev}`;
      if (it.chat === id && !state.readSent.has(key)) {
        state.readSent.add(key);
        call("inbox_read", { ids: [it.id] }).catch(() => {});
      }
    }
  }

  function waitingHtml(c) {
    if (c.state !== "waiting") return "";
    const w = c.waiting_on || {};
    const stamp = JSON.stringify(w.tool_input ?? w.message ?? null);
    if (state.answered[c.id] === stamp) return `<div class="waiting"><div class="q muted">Answer sent…</div></div>`;
    const qs = (w.tool_input?.questions || []).map((q) => ({ text: q.question || "", options: (q.options || []).map((o) => ({ label: o.label, desc: o.description || "" })) }));
    const cancel = `<button class="opt ghost" data-act="cancel">Cancel</button>`;
    if (qs.length > 1) {
      const picks = state.picks[c.id]?.stamp === stamp ? state.picks[c.id].picks : [];
      return `<div class="waiting">${qs
        .map(
          (q, qi) => `<div class="q">${esc(q.text)}</div><div class="opts">${q.options
            .map((o, oi) => `<button class="opt ${picks[qi] === oi ? "picked" : ""}" data-act="pick" data-q="${qi}" data-opt="${oi}">${esc(o.label)}</button>`)
            .join("")}</div>`
        )
        .join("")}<div class="opts">${cancel}</div></div>`;
    }
    if (qs.length === 1) {
      return `<div class="waiting"><div class="q">${esc(qs[0].text)}</div><div class="opts col">${qs[0].options
        .map((o, i) => `<button class="opt" data-act="answer" data-opt="${i}"><b>${esc(o.label)}</b>${o.desc ? `<span>${esc(o.desc)}</span>` : ""}</button>`)
        .join("")}${cancel}</div></div>`;
    }
    const detail = w.tool_input?.command || w.tool_input?.file_path || w.tool_input?.url || "";
    const message = w.tool_name ? `${w.tool_name} needs your permission` : w.message || "The agent is waiting for you";
    const options = ["Yes", "Yes, don't ask again", "No"];
    return `<div class="waiting"><div class="q">${esc(message)}</div>${detail ? `<pre class="detail">${esc(detail)}</pre>` : ""}<div class="opts">${options
      .map((o, i) => `<button class="opt" data-act="answer" data-opt="${i}">${esc(o)}</button>`)
      .join("")}${cancel}</div></div>`;
  }

  const agentCfg = (agent) => state.config?.options?.agents?.[agent] || {};
  const answerKey = (agent, n) => String(agentCfg(agent).answer_key || "{n}").replace("{n}", n);
  const keysOf = (agent, kind, fallback) => {
    const k = agentCfg(agent)[kind];
    return Array.isArray(k) && k.length ? k : fallback;
  };

  async function pressInTurn(id, keys) {
    for (let i = 0; i < keys.length; i++) {
      if (i) await sleep(300);
      await call("chat_keys", { chat_id: id, keys: [keys[i]] });
    }
  }

  function stampOf(c) {
    const w = c.waiting_on || {};
    return JSON.stringify(w.tool_input ?? w.message ?? null);
  }

  async function answer(c, keys) {
    state.answered[c.id] = stampOf(c);
    updateChat();
    try {
      await pressInTurn(c.id, keys);
    } catch (e) {
      delete state.answered[c.id];
      updateChat();
      toast(e.message);
    }
  }

  // Transcript: the last page on open, live entries after it, older pages on demand.
  async function loadTranscript(id) {
    const list = document.getElementById("tx-list");
    if (!list) return;
    try {
      const r = await call("transcript_read", { chat_id: id, from: 0, tail: 120 });
      if (state.route.id !== id) return;
      state.tx = { id, start: r.from, next: r.from + r.entries.length, entries: r.entries };
      renderTranscript(true);
    } catch (e) {
      list.innerHTML = `<div class="empty">${esc(e.message)}</div>`;
    }
  }

  async function loadOlder() {
    const tx = state.tx;
    if (!tx || tx.start <= 0) return;
    const box = document.getElementById("tx");
    const fromBottom = box.scrollHeight - box.scrollTop;
    const r = await call("transcript_read", { chat_id: tx.id, from: 0, tail: 120, before: tx.start });
    if (state.tx !== tx) return;
    tx.entries = [...r.entries, ...tx.entries];
    tx.start = r.from;
    renderTranscript(false);
    box.scrollTop = box.scrollHeight - fromBottom;
  }

  function appendEntries(from, entries) {
    const tx = state.tx;
    if (!tx) return;
    if (from > tx.next) return loadTranscript(tx.id);
    const fresh = entries.slice(tx.next - from);
    if (!fresh.length) return;
    tx.next += fresh.length;
    tx.entries.push(...fresh);
    const box = document.getElementById("tx");
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 120;
    const list = document.getElementById("tx-list");
    list.querySelector(":scope > .empty")?.remove();
    for (const e of fresh) for (const el of entryEls(e, tx)) list.appendChild(el);
    settlePending(fresh);
    if (atBottom) box.scrollTop = box.scrollHeight;
  }

  function renderTranscript(toBottom) {
    const tx = state.tx;
    tx.tools = {};
    const list = document.getElementById("tx-list");
    list.innerHTML = "";
    for (const e of tx.entries) for (const el of entryEls(e, tx)) list.appendChild(el);
    document.getElementById("older").classList.toggle("hidden", tx.start <= 0);
    if (!tx.entries.length) list.innerHTML = `<div class="empty">No messages yet.</div>`;
    if (toBottom) {
      const box = document.getElementById("tx");
      box.scrollTop = box.scrollHeight;
    }
  }

  function el(html) {
    const t = document.createElement("template");
    t.innerHTML = html.trim();
    return t.content.firstChild;
  }

  function userPart(text) {
    const t = String(text).replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, "").trim();
    if (!t || t.startsWith("<local-command-caveat>")) return null;
    const cmd = t.match(/<command-name>([\s\S]*?)<\/command-name>/);
    if (cmd) {
      const args = (t.match(/<command-args>([\s\S]*?)<\/command-args>/) || [])[1] || "";
      return el(`<div class="note">${esc(`${cmd[1].trim()} ${args.trim()}`.trim())}</div>`);
    }
    if (/^<local-command-(stdout|stderr)>/.test(t)) {
      const body = t.replace(/<\/?local-command-(stdout|stderr)>/g, "").trim();
      return body ? el(`<div class="note">${esc(body.slice(0, 400))}</div>`) : null;
    }
    if (t.startsWith("<task-notification>")) {
      const summary = (t.match(/<summary>([\s\S]*?)<\/summary>/) || [])[1];
      return el(`<div class="note">${esc(summary || "A background task finished")}</div>`);
    }
    return el(`<div class="msg user">${esc(t)}</div>`);
  }

  function toolSummary(name, input = {}) {
    const base = (p) => String(p || "").split("/").pop();
    switch (name) {
      case "Bash":
        return input.description || input.command;
      case "Read":
      case "Write":
      case "Edit":
      case "MultiEdit":
      case "NotebookEdit":
        return base(input.file_path || input.notebook_path);
      case "Grep":
      case "Glob":
        return input.pattern;
      case "Agent":
      case "Task":
        return input.description;
      case "WebFetch":
        return input.url;
      case "WebSearch":
        return input.query;
      case "TodoWrite":
        return `${(input.todos || []).length} todos`;
      case "Skill":
        return input.skill;
      case "AskUserQuestion":
        return input.questions?.[0]?.question;
    }
    return Object.values(input).find((v) => typeof v === "string") || "";
  }

  function toolName(name) {
    const m = String(name).match(/^mcp__(.+?)__(.+)$/);
    return m ? `${m[1].replace(/^claude_ai_/, "")} · ${m[2]}` : name;
  }

  function resultText(content) {
    if (typeof content === "string") return content;
    if (Array.isArray(content)) return content.map((p) => (p.type === "text" ? p.text : p.type === "image" ? "[image]" : "")).join("\n");
    return "";
  }

  function entryEls(e, tx) {
    const out = [];
    if (!e || e.isMeta) return out;
    if (e.type === "system") {
      if (e.subtype === "compact_boundary") out.push(el(`<div class="divider">Conversation compacted</div>`));
      return out;
    }
    const content = e.message?.content;
    if (e.type === "user") {
      if (e.isCompactSummary) return out;
      if (typeof content === "string") {
        const p = userPart(content);
        if (p) out.push(p);
        return out;
      }
      for (const b of content || []) {
        if (b.type === "text") {
          const p = userPart(b.text);
          if (p) out.push(p);
        } else if (b.type === "image") {
          out.push(el(`<div class="msg user muted">[image]</div>`));
        } else if (b.type === "tool_result") {
          const card = tx.tools?.[b.tool_use_id];
          if (card) {
            const text = resultText(b.content);
            if (b.is_error) card.classList.add("error");
            if (text) card.appendChild(el(`<pre class="tool-out">${esc(text)}</pre>`));
          }
        }
      }
      return out;
    }
    if (e.type === "assistant") {
      for (const b of content || []) {
        if (b.type === "text" && b.text?.trim()) out.push(el(`<div class="msg assistant">${md(b.text)}</div>`));
        else if (b.type === "thinking" && b.thinking?.trim()) out.push(el(`<details class="thinking"><summary>Thinking</summary><div>${md(b.thinking)}</div></details>`));
        else if (b.type === "tool_use") {
          const input = b.input || {};
          const shown = b.name === "Bash" ? input.command : JSON.stringify(input, null, 2);
          const card = el(
            `<details class="tool"><summary><span class="tool-name">${esc(toolName(b.name))}</span><span class="tool-sum">${esc(toolSummary(b.name, input) || "")}</span></summary><pre class="tool-in">${esc(shown)}</pre></details>`
          );
          if (tx.tools) tx.tools[b.id] = card;
          out.push(card);
        }
      }
    }
    return out;
  }

  function settlePending(fresh) {
    const box = document.getElementById("tx-pending");
    if (!box) return;
    const sent = fresh.filter((e) => e.type === "user" && !e.isMeta).length;
    for (let i = 0; i < sent && box.firstChild; i++) box.firstChild.remove();
  }

  async function send() {
    const input = document.getElementById("input");
    const text = input.value.trim();
    const id = state.route.id;
    if (!text || !id) return;
    input.value = "";
    autosize(input);
    const pending = el(`<div class="msg user pending">${esc(text)}</div>`);
    document.getElementById("tx-pending").appendChild(pending);
    const box = document.getElementById("tx");
    box.scrollTop = box.scrollHeight;
    setTimeout(() => pending.remove(), 120000);
    try {
      await call("chat_send", { chat_id: id, text });
    } catch (e) {
      pending.remove();
      input.value = text;
      autosize(input);
      toast(e.message);
    }
  }

  function autosize(t) {
    t.style.height = "auto";
    t.style.height = `${Math.min(t.scrollHeight + 2, 160)}px`;
    t.style.overflowY = t.scrollHeight > 160 ? "auto" : "hidden";
  }

  function showMenu() {
    const c = chatOf(state.route.id);
    if (!c) return;
    const items = [
      c.alive ? ["stop", "Stop chat"] : ["start", "Start chat"],
      ...(c.rc_url ? [["rc", "Open in Claude"]] : []),
      ["close", "Cancel"],
    ];
    const sheet = el(`<div class="sheet-bg"><div class="sheet">${items.map(([a, l]) => `<button data-menu="${a}">${esc(l)}</button>`).join("")}</div></div>`);
    sheet.addEventListener("click", async (ev) => {
      const a = ev.target.closest("[data-menu]")?.dataset.menu;
      sheet.remove();
      try {
        if (a === "stop") await call("chat_stop", { chat_id: c.id });
        if (a === "start") await call("chat_start", { chat_id: c.id });
        if (a === "rc") window.open(c.rc_url, "_blank");
      } catch (e) {
        toast(e.message);
      }
    });
    document.body.appendChild(sheet);
  }

  async function startNew() {
    const v = (id) => document.getElementById(id)?.value.trim() || "";
    const dir = v("nc-where");
    const name = v("nc-name");
    const params = {
      name: name || state.config?.options?.default_chat_name || "New chat",
      kind: dir ? "primary" : "orchestrator",
      branch: dir ? null : v("nc-branch") || null,
      base: null,
      dir: dir || null,
      repo: v("nc-repo") || null,
      prompt: v("nc-prompt") || null,
      model: null,
      agent: v("nc-agent") || null,
      auto_name: !name,
    };
    state.starting = true;
    refresh();
    try {
      const r = await call("chat_new", params);
      const lines = String(r?.stdout || "").trim().split("\n");
      const id = lines[lines.length - 1].split(/\s+/)[0];
      state.starting = false;
      location.hash = id ? `#/chat/${id}` : "#/";
    } catch (e) {
      state.starting = false;
      refresh();
      toast(e.message);
    }
  }

  const b64urlBytes = (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4)), (c) => c.charCodeAt(0));

  async function enablePush() {
    if (!("serviceWorker" in navigator) || !("PushManager" in window)) return toast("Add a.box to your Home Screen, open it from there, then try again.");
    try {
      const perm = await Notification.requestPermission();
      if (perm !== "granted") return toast("Notifications are off for a.box. Turn them on in Settings › Notifications.");
      const reg = await navigator.serviceWorker.ready;
      const { key } = await (await fetch("/api/push/key")).json();
      const sub = (await reg.pushManager.getSubscription()) || (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64urlBytes(key) }));
      await fetch("/api/push/subscribe", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(sub) });
      state.pushOn = true;
      refresh();
      await fetch("/api/push/test", { method: "POST" });
    } catch (e) {
      toast(`Couldn't turn on notifications: ${e.message}`);
    }
  }

  async function checkPush() {
    if (!("serviceWorker" in navigator)) return;
    try {
      await navigator.serviceWorker.register("/sw.js");
      const reg = await navigator.serviceWorker.ready;
      const sub = await reg.pushManager?.getSubscription();
      if (sub && Notification.permission === "granted") {
        state.pushOn = true;
        await fetch("/api/push/subscribe", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(sub) });
      }
    } catch {}
  }

  function parseRoute() {
    const h = location.hash.replace(/^#\/?/, "");
    const [name, id] = h.split("/");
    if (name === "chat" && id) return { name: "chat", id };
    if (name === "inbox") return { name: "inbox" };
    if (name === "new") return { name: "new", dir: state.newPreset?.dir, repo: state.newPreset?.repo };
    return { name: "home" };
  }

  function route() {
    const prev = state.route;
    if (prev.name === "chat" && state.tx) call("transcript_unwatch", { chat_id: state.tx.id }).catch(() => {});
    state.tx = null;
    state.route = parseRoute();
    if (state.route.name !== "new") state.newPreset = null;
    if (state.route.name === "chat") {
      $app.innerHTML = chatShell(state.route.id);
      updateChat();
      if (conn.ws?.readyState === 1) loadTranscript(state.route.id);
      autosize(document.getElementById("input"));
    } else {
      refresh();
      document.querySelector(".scroll")?.scrollTo(0, 0);
    }
  }

  function refresh() {
    const r = state.route.name;
    if (r === "chat") return updateChat();
    if (r === "new" && document.getElementById("nc-prompt") && !state.forceNew) {
      const btn = document.querySelector('[data-act="new-start"]');
      if (btn) {
        btn.disabled = state.starting;
        btn.textContent = state.starting ? "Starting…" : "Start chat";
      }
      return;
    }
    state.forceNew = false;
    const scroll = document.querySelector(".scroll")?.scrollTop || 0;
    $app.innerHTML = r === "inbox" ? renderInbox() : r === "new" ? renderNew() : renderHome();
    const s = document.querySelector(".scroll");
    if (s) s.scrollTop = scroll;
    const n = (state.model?.inbox?.length || 0) + (state.model?.chats || []).filter((c) => c.state === "waiting").length;
    if (navigator.setAppBadge) (n ? navigator.setAppBadge(n) : navigator.clearAppBadge()).catch(() => {});
  }

  $app.addEventListener("click", async (ev) => {
    const t = ev.target.closest("[data-go],[data-act],[data-toggle],[data-scope],[data-new-dir]");
    if (!t) return;
    if (t.dataset.newDir) {
      state.newPreset = { dir: t.dataset.newDir, repo: t.dataset.newRepo };
      location.hash = "#/new";
      return;
    }
    if (t.dataset.go) {
      if (t.tagName === "A") return;
      location.hash = t.dataset.go;
      return;
    }
    if (t.dataset.toggle) {
      state.collapsed[t.dataset.toggle] = !state.collapsed[t.dataset.toggle];
      localStorage.setItem("collapsed", JSON.stringify(state.collapsed));
      return refresh();
    }
    if (t.dataset.scope) {
      state.scope = t.dataset.scope;
      localStorage.setItem("scope", state.scope);
      return refresh();
    }
    const c = chatOf(state.route.id);
    try {
      switch (t.dataset.act) {
        case "send":
          return send();
        case "older":
          return loadOlder();
        case "menu":
          return showMenu();
        case "interrupt":
          return c && call("chat_keys", { chat_id: c.id, keys: keysOf(c.agent, "interrupt_keys", ["Escape"]) });
        case "suggest": {
          const input = document.getElementById("input");
          input.value = c?.suggestion || "";
          autosize(input);
          return input.focus();
        }
        case "answer":
          return c && answer(c, [answerKey(c.agent, +t.dataset.opt + 1)]);
        case "cancel":
          delete state.picks[c?.id];
          return c && answer(c, keysOf(c.agent, "cancel_keys", ["Escape"]));
        case "pick": {
          if (!c) return;
          const stamp = stampOf(c);
          const qs = c.waiting_on?.tool_input?.questions || [];
          const p = state.picks[c.id]?.stamp === stamp ? state.picks[c.id] : (state.picks[c.id] = { stamp, picks: qs.map(() => null) });
          p.picks[+t.dataset.q] = +t.dataset.opt;
          if (p.picks.every((x) => x !== null)) {
            delete state.picks[c.id];
            const keys = [...p.picks.map((x) => answerKey(c.agent, x + 1)), ...keysOf(c.agent, "submit_keys", [answerKey(c.agent, 1)])];
            return answer(c, keys);
          }
          return updateChat();
        }
        case "inbox-open":
          await call("inbox_read", { ids: [+t.dataset.id] });
          location.hash = `#/chat/${t.dataset.chat}`;
          return;
        case "inbox-done":
          return await call("inbox_read", { ids: [+t.dataset.id] });
        case "inbox-all":
          return await call("inbox_read", { all: true });
        case "push":
          return enablePush();
        case "new-start":
          return startNew();
      }
    } catch (e) {
      toast(e.message);
    }
  });

  $app.addEventListener("change", (ev) => {
    if (ev.target.id === "nc-repo") {
      state.route.repo = ev.target.value;
      state.route.dir = null;
      state.forceNew = true;
      refresh();
    }
    if (ev.target.id === "nc-where") document.getElementById("nc-branch-row").classList.toggle("hidden", !!ev.target.value);
  });

  $app.addEventListener("input", (ev) => {
    if (ev.target.id === "input") autosize(ev.target);
  });

  // The on-screen keyboard shrinks the visual viewport, not the layout one: size the app to what's visible.
  const vv = window.visualViewport;
  if (vv) {
    const fit = () => {
      $app.style.height = `${vv.height}px`;
      $app.style.transform = `translateY(${vv.offsetTop}px)`;
    };
    vv.addEventListener("resize", fit);
    vv.addEventListener("scroll", fit);
    fit();
  }

  let hiddenAt = 0;
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) hiddenAt = Date.now();
    else if (Date.now() - hiddenAt > 5000 || !conn.ws || conn.ws.readyState !== 1) conn.reconnect();
  });

  navigator.serviceWorker?.addEventListener("message", (e) => {
    if (e.data?.go) location.hash = e.data.go;
  });

  setInterval(() => state.route.name !== "new" && refresh(), 15000);
  window.addEventListener("hashchange", route);
  route();
  conn.open();
  checkPush();
})();
