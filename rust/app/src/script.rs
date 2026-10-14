/*! Live abilities: JavaScript (QuickJS) loaded from settings and `scripts/NAME.js`, reloaded on every change.
Scripts add actions and event handlers and reach the app, the box, the shell, and files through `ab`. */

use crate::settings;
use rquickjs::{Context, Function, Runtime};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;

/** The JS side of the bridge: everything goes out through `__send(json)` and comes back in through
`__deliver(json)`, so the native surface stays two functions. */
const PRELUDE: &str = r#"
(() => {
  const handlers = { action: {}, event: {}, timer: {}, reply: {} };
  let next = 1;
  const send = (m) => __send(JSON.stringify(m));
  const wait = (m, cb) => { const id = next++; if (cb) handlers.reply[id] = cb; send({ ...m, id }); return id; };
  globalThis.ab = {
    action(name, fn) { handlers.action[name] = fn; send({ type: "register", name }); },
    on(event, fn) { (handlers.event[event] ||= []).push(fn); },
    do(action) { send({ type: "do", action }); },
    context() { return JSON.parse(__context()); },
    get state() { return this.context().state; },
    set(key, value) { send({ type: "do", action: { do: "state.set", key, value } }); },
    data(name, value) { send({ type: "data", name, value }); },
    notify(text) { send({ type: "do", action: { do: "notify", text: String(text) } }); },
    every(ms, fn) { const id = next++; handlers.timer[id] = fn; send({ type: "timer", id, ms, repeat: true }); return id; },
    after(ms, fn) { const id = next++; handlers.timer[id] = fn; send({ type: "timer", id, ms, repeat: false }); return id; },
    cancel(id) { delete handlers.timer[id]; send({ type: "cancel", id }); },
    shell(cmd, cb) { return wait({ type: "shell", cmd }, cb); },
    box(method, params, cb) { return wait({ type: "box", method, params: params || {} }, cb); },
    fetch(url, cb, opts) { return wait({ type: "shell", cmd: "curl -sSL " + JSON.stringify(url) + " " + ((opts && opts.args) || "") }, cb); },
    read(path) { return __read(String(path)); },
    write(path, text) { send({ type: "write", path: String(path), text: String(text) }); },
    emit(event, payload) { send({ type: "emit", event, payload }); },
    log(...parts) { send({ type: "log", text: parts.map((p) => typeof p === "string" ? p : JSON.stringify(p)).join(" ") }); },
  };
  globalThis.console = { log: ab.log, error: ab.log, warn: ab.log };
  globalThis.__deliver = (raw) => {
    const m = JSON.parse(raw);
    try {
      if (m.type === "action") return handlers.action[m.name]?.(m.args || {}, ab.context());
      if (m.type === "event") return (handlers.event[m.name] || []).forEach((fn) => fn(m.payload, ab.context()));
      if (m.type === "timer") { const fn = handlers.timer[m.id]; if (fn) fn(); if (!m.repeat) delete handlers.timer[m.id]; return; }
      if (m.type === "reply") { const fn = handlers.reply[m.id]; delete handlers.reply[m.id]; return fn?.(m.result, m.error); }
      if (m.type === "eval") return (new Function("args", "ctx", "ab", m.code))(m.args || {}, ab.context(), ab);
    } catch (e) {
      send({ type: "error", text: String(e && e.stack || e) });
    }
  };
})();
"#;

pub struct Scripts {
    _rt: Runtime,
    ctx: Context,
    outbox: Rc<RefCell<Vec<Value>>>,
    context_json: Rc<RefCell<String>>,
    pub actions: Vec<String>,
    pub errors: Vec<String>,
}

pub fn scripts_dir() -> std::path::PathBuf {
    settings::dir().join("scripts")
}

impl Scripts {
    /** Loads `settings.scripts` (name -> source) and every `scripts/NAME.js`, in name order. */
    pub fn load(inline: &serde_json::Map<String, Value>) -> Option<Scripts> {
        let rt = Runtime::new().ok()?;
        rt.set_memory_limit(64 << 20);
        let ctx = Context::full(&rt).ok()?;
        let outbox = Rc::new(RefCell::new(vec![]));
        let context_json = Rc::new(RefCell::new("{}".to_string()));
        let mut sources: Vec<(String, String)> = inline.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
        let mut files: Vec<_> = std::fs::read_dir(scripts_dir()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "js")).collect();
        files.sort();
        for f in files {
            if let Ok(src) = std::fs::read_to_string(&f) {
                sources.push((f.file_name().unwrap_or_default().to_string_lossy().into_owned(), src));
            }
        }
        let mut errors = vec![];
        ctx.with(|c| {
            let g = c.globals();
            let out = outbox.clone();
            let _ = g.set("__send", Function::new(c.clone(), move |s: String| {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    out.borrow_mut().push(v);
                }
            }));
            let cj = context_json.clone();
            let _ = g.set("__context", Function::new(c.clone(), move || cj.borrow().clone()));
            let _ = g.set("__read", Function::new(c.clone(), |p: String| std::fs::read_to_string(expand(&p)).unwrap_or_default()));
            if let Err(e) = c.eval::<(), _>(PRELUDE) {
                errors.push(format!("script prelude: {e}"));
            }
            for (name, src) in &sources {
                if c.eval::<(), _>(src.as_str()).is_err() {
                    errors.push(format!("script {name}: {}", describe(&c.catch())));
                }
            }
        });
        let mut s = Scripts { _rt: rt, ctx, outbox, context_json, actions: vec![], errors };
        s.collect_registrations();
        Some(s)
    }

    fn collect_registrations(&mut self) {
        let mut keep = vec![];
        for m in self.outbox.borrow_mut().drain(..) {
            if m["type"] == "register" {
                if let Some(n) = m["name"].as_str() {
                    self.actions.push(n.to_string());
                }
            } else {
                keep.push(m);
            }
        }
        self.outbox.borrow_mut().extend(keep);
    }

    /** Hands a message to the scripts with `context` visible to `ab.context()`; returns what they sent back. */
    pub fn deliver(&mut self, message: Value, context: String) -> Vec<Value> {
        *self.context_json.borrow_mut() = context;
        let raw = message.to_string();
        let mut err = None;
        self.ctx.with(|c| {
            let f: rquickjs::Result<Function> = c.globals().get("__deliver");
            if let Ok(f) = f {
                if f.call::<_, ()>((raw,)).is_err() {
                    err = Some(describe(&c.catch()));
                }
            }
        });
        if let Some(e) = err {
            self.errors.push(e);
        }
        self.collect_registrations();
        self.outbox.borrow_mut().drain(..).collect()
    }

    pub fn has_action(&self, name: &str) -> bool {
        self.actions.iter().any(|a| a == name)
    }
}

fn describe(v: &rquickjs::Value) -> String {
    if let Some(e) = v.as_exception() {
        return format!("{}{}", e.message().unwrap_or_default(), e.stack().map(|s| format!("\n{s}")).unwrap_or_default());
    }
    v.as_string().and_then(|s| s.to_string().ok()).unwrap_or_else(|| format!("{v:?}"))
}

pub fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => p.to_string(),
    }
}

pub fn action_message(name: &str, args: &serde_json::Map<String, Value>) -> Value {
    json!({"type": "action", "name": name, "args": args})
}
