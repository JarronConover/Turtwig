/*! Live abilities: JavaScript (JavaScriptCore, macOS's JIT engine) loaded from settings and `scripts/NAME.js`, reloaded on every change.
Scripts add actions and event handlers and reach the app, the box, the shell, and files through `ab`. */

use crate::settings;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::io::{Read as _, Write as _};
use std::rc::Rc;
use std::time::Duration;

/** The JS side of the bridge: everything goes out through `__send(json)` and comes back in through
`__deliver(json)`; `__call` runs the synchronous natives (files, environment). */
const PRELUDE: &str = r#"
(() => {
  const handlers = { action: {}, event: {}, timer: {}, reply: {}, proc: {} };
  let next = 1;
  // What this delivery has set (ab.set, ab.data), visible to ab.context() before the app applies it.
  let overlay = { state: {}, data: {} };
  let cached = null;
  const send = (m) => __send(JSON.stringify(m));
  const wait = (m, cb) => { const id = next++; if (cb) handlers.reply[id] = cb; send({ ...m, id }); return id; };
  const call = (name, args, cb) => wait({ type: "call", name, args: args || {} }, cb);
  const callSync = (name, args) => {
    const r = JSON.parse(__call(name, JSON.stringify(args || {})));
    if (r.error) throw new Error(name + ": " + r.error);
    return r.value;
  };
  const noteDo = (action) => {
    if (action && action.do === "state.set" && typeof action.key === "string") { overlay.state[action.key] = action.value; cached = null; }
  };
  globalThis.ab = {
    action(name, fn) { handlers.action[name] = fn; send({ type: "register", name }); },
    on(event, fn) { (handlers.event[event] ||= []).push(fn); },
    do(action) { noteDo(action); send({ type: "do", action }); },
    context() {
      if (!cached) cached = JSON.parse(__context());
      const c = { ...cached, state: { ...cached.state, ...overlay.state }, data: { ...cached.data, ...overlay.data } };
      return c;
    },
    get state() { return this.context().state; },
    set(key, value) { this.do({ do: "state.set", key, value }); },
    data(name, value) { overlay.data[name] = value; send({ type: "data", name, value }); },
    notify(text) { send({ type: "do", action: { do: "notify", text: String(text) } }); },
    every(ms, fn) { const id = next++; handlers.timer[id] = fn; send({ type: "timer", id, ms, repeat: true }); return id; },
    after(ms, fn) { const id = next++; handlers.timer[id] = fn; send({ type: "timer", id, ms, repeat: false }); return id; },
    cancel(id) { delete handlers.timer[id]; send({ type: "cancel", id }); },
    shell(cmd, cb, opts) { return wait({ type: "shell", cmd, ...(opts || {}) }, cb); },
    box(method, params, cb, opts) { return wait({ type: "box", method, params: params || {}, host: opts && opts.host }, cb); },
    http(req, cb) { return call("http", req, cb); },
    fetch(url, cb, opts) { return call("http", { url, ...(opts || {}) }, cb); },
    read(path) { const r = __read(String(path)); return r === undefined ? null : r; },
    write(path, text, cb) { return wait({ type: "write", path: String(path), text: String(text) }, cb && ((_, err) => cb(err || null))); },
    emit(event, payload) { send({ type: "emit", event, payload }); },
    log(...parts) { send({ type: "log", text: parts.map((p) => typeof p === "string" ? p : JSON.stringify(p)).join(" ") }); },
    call, callSync,
    clipboard: { read(cb) { return call("clipboard.read", {}, cb); }, write(text, cb) { return call("clipboard.write", { text: String(text) }, cb); } },
    fs: {
      list(path) { return callSync("fs.list", { path }); },
      exists(path) { return callSync("fs.exists", { path }); },
      stat(path) { return callSync("fs.stat", { path }); },
      mkdir(path) { return callSync("fs.mkdir", { path }); },
      remove(path) { return callSync("fs.remove", { path }); },
      watch(path, fn) { const id = next++; handlers.proc[id] = { change: fn }; send({ type: "call", name: "fs.watch", args: { path, watch: id }, id: next++ }); return id; },
    },
    spawn(cmd, opts, on) {
      const id = next++;
      handlers.proc[id] = on || {};
      send({ type: "call", name: "proc.spawn", args: { cmd, ...(opts || {}), proc: id }, id: next++ });
      return { id, write: (data) => call("proc.write", { proc: id, data: String(data) }), kill: () => call("proc.kill", { proc: id }) };
    },
    inspect(cb) { return call("inspect", {}, cb); },
    patch(ops) { send({ type: "patch", ops: Array.isArray(ops) ? ops : [ops] }); },
    key(key, action, opts) { send({ type: "key", key: { key, do: action, ...(opts || {}) } }); },
    command(label, action, opts) { send({ type: "command", command: { label, do: action, ...(opts || {}) } }); },
  };
  globalThis.console = { log: ab.log, error: ab.log, warn: ab.log };
  globalThis.__deliver = (raw) => {
    const m = JSON.parse(raw);
    overlay = { state: {}, data: {} };
    cached = null;
    try {
      if (m.type === "action") return handlers.action[m.name]?.(m.args || {}, ab.context());
      if (m.type === "event") return (handlers.event[m.name] || []).forEach((fn) => fn(m.payload, ab.context()));
      if (m.type === "timer") { const fn = handlers.timer[m.id]; if (fn) fn(); if (!m.repeat) delete handlers.timer[m.id]; return; }
      if (m.type === "reply") { const fn = handlers.reply[m.id]; delete handlers.reply[m.id]; return fn?.(m.result, m.error); }
      if (m.type === "proc") {
        const h = handlers.proc[m.proc] || {};
        if (m.stream && h[m.stream]) h[m.stream](m.data);
        if ("exit" in m) { delete handlers.proc[m.proc]; h.exit?.(m.exit); }
        return;
      }
      if (m.type === "eval") return (new Function("args", "ctx", "ab", m.code))(m.args || {}, ab.context(), ab);
    } catch (e) {
      send({ type: "error", text: String(e && e.stack || e) });
    }
  };
})();
"#;

/** Everything `ab.call(name, args, cb)` reaches, for SETTINGS.md. The `fs.*` ones also run at once
with `ab.callSync` (and `ab.fs.list` and the rest). */
pub const CALLS: &[(&str, &str)] = &[
    ("http", "{url, method?, headers?, body? (text, or JSON sent as such), timeout_secs?, args?}: an HTTP request (curl, no shell): `{status, ok, headers, body, code, stdout, stderr}`"),
    ("clipboard.read", "the clipboard's text: `{text}`"),
    ("clipboard.write", "{text}: put text on the clipboard"),
    ("fs.list", "{path}: the folder's entries `[{name, path, dir}]`"),
    ("fs.exists", "{path}: true or false"),
    ("fs.stat", "{path}: `{size, dir, modified}` or null"),
    ("fs.mkdir", "{path}: make the folder (and its parents)"),
    ("fs.remove", "{path}: remove a file or an empty folder"),
    ("fs.watch", "{path}: (via `ab.fs.watch(path, fn)`) fn(paths) runs when anything under path changes"),
    ("proc.spawn", "(via `ab.spawn(cmd, {cwd, env, stdin, shell}, {stdout, stderr, exit})`) a process on the Mac whose output streams in line by line; the handle's `write(data)` and `kill()` reach it"),
    ("proc.write / proc.kill", "{proc, data?}: write to a spawned process's stdin, or end it"),
    ("inspect", "what inspect.json holds right now (layout, transcripts, terminals, scripts, dialog, ...)"),
    ("env", "{name}: an environment variable of the app's process"),
];

pub struct Scripts {
    engine: crate::jsc::Engine,
    outbox: Rc<RefCell<Vec<Value>>>,
    context_json: Rc<RefCell<Rc<str>>>,
    pub actions: Vec<String>,
    pub errors: Vec<String>,
}

pub fn scripts_dir() -> std::path::PathBuf {
    settings::dir().join("scripts")
}

impl Scripts {
    /** Loads `settings.scripts` (name -> source) and every `scripts/NAME.js`, in name order. */
    pub fn load(inline: &serde_json::Map<String, Value>) -> Option<Scripts> {
        let mut engine = crate::jsc::Engine::new();
        let outbox = Rc::new(RefCell::new(vec![]));
        let context_json: Rc<RefCell<Rc<str>>> = Rc::new(RefCell::new(Rc::from("{}")));
        let mut sources: Vec<(String, String)> = inline.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
        let mut files: Vec<_> = std::fs::read_dir(scripts_dir()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "js")).collect();
        files.sort();
        for f in files {
            if let Ok(src) = std::fs::read_to_string(&f) {
                sources.push((f.file_name().unwrap_or_default().to_string_lossy().into_owned(), src));
            }
        }
        let out = outbox.clone();
        engine.define("__send", Box::new(move |args| {
            if let Some(v) = args.first().and_then(|s| serde_json::from_str::<Value>(s).ok()) {
                out.borrow_mut().push(v);
            }
            None
        }));
        let cj = context_json.clone();
        engine.define("__context", Box::new(move |_| Some(cj.borrow().to_string())));
        engine.define("__read", Box::new(|args| args.first().and_then(|p| std::fs::read_to_string(expand(p)).ok())));
        engine.define("__call", Box::new(|args| {
            let name = args.first().map(String::as_str).unwrap_or("");
            let a = args.get(1).and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
            let out = match pure_call(name, &a) {
                Some(Ok(v)) => json!({"value": v}),
                Some(Err(e)) => json!({"error": e}),
                None => json!({"error": format!("{name} can't run synchronously; use ab.call")}),
            };
            Some(out.to_string())
        }));
        let mut errors = vec![];
        if let Err(e) = engine.eval(PRELUDE, "prelude.js") {
            errors.push(format!("script prelude: {e}"));
        }
        for (name, src) in &sources {
            if let Err(e) = engine.eval(src, name) {
                errors.push(format!("script {name}: {e}"));
            }
        }
        let mut s = Scripts { engine, outbox, context_json, actions: vec![], errors };
        s.collect_registrations();
        Some(s)
    }

    fn collect_registrations(&mut self) {
        let mut keep = vec![];
        for m in self.outbox.borrow_mut().drain(..) {
            if m["type"] == "register" {
                if let Some(n) = m["name"].as_str() {
                    if !self.actions.iter().any(|a| a == n) {
                        self.actions.push(n.to_string());
                    }
                }
            } else {
                keep.push(m);
            }
        }
        self.outbox.borrow_mut().extend(keep);
    }

    /** Hands a message to the scripts with `context` visible to `ab.context()`; returns what they sent back. */
    pub fn deliver(&mut self, message: Value, context: Rc<str>) -> Vec<Value> {
        *self.context_json.borrow_mut() = context;
        if let Err(e) = self.engine.call("__deliver", &message.to_string()) {
            self.errors.push(e);
        }
        self.collect_registrations();
        self.outbox.borrow_mut().drain(..).collect()
    }

    pub fn has_action(&self, name: &str) -> bool {
        self.actions.iter().any(|a| a == name)
    }
}

pub fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None if p == "~" => std::env::var("HOME").unwrap_or_default(),
        None => p.to_string(),
    }
}

pub fn action_message(name: &str, args: &serde_json::Map<String, Value>) -> Value {
    json!({"type": "action", "name": name, "args": args})
}

/** The calls that need nothing but the filesystem and environment; None for any other name. */
pub fn pure_call(name: &str, args: &Value) -> Option<Result<Value, String>> {
    let path = || expand(args["path"].as_str().unwrap_or(""));
    let r = match name {
        "fs.list" => std::fs::read_dir(path()).map_err(|e| e.to_string()).map(|d| {
            let mut out: Vec<Value> = d
                .flatten()
                .map(|e| json!({"name": e.file_name().to_string_lossy(), "path": e.path().to_string_lossy(), "dir": e.path().is_dir()}))
                .collect();
            out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            Value::Array(out)
        }),
        "fs.exists" => Ok(Value::Bool(std::path::Path::new(&path()).exists())),
        "fs.stat" => Ok(std::fs::metadata(path()).map(|m| {
            let modified = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64());
            json!({"size": m.len(), "dir": m.is_dir(), "modified": modified})
        }).unwrap_or(Value::Null)),
        "fs.mkdir" => std::fs::create_dir_all(path()).map(|_| Value::Bool(true)).map_err(|e| e.to_string()),
        "fs.remove" => {
            let p = path();
            let r = if std::path::Path::new(&p).is_dir() { std::fs::remove_dir(&p) } else { std::fs::remove_file(&p) };
            r.map(|_| Value::Bool(true)).map_err(|e| e.to_string())
        }
        "env" => Ok(args["name"].as_str().and_then(|n| std::env::var(n).ok()).map(Value::String).unwrap_or(Value::Null)),
        _ => return None,
    };
    Some(r)
}

/** Runs a command to completion, feeding it `stdin`, killing it after `timeout`. */
pub fn run_output(mut cmd: std::process::Command, stdin: Option<String>, timeout: Option<Duration>) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let read_out = std::thread::spawn(move || {
        let mut b = vec![];
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut b);
        }
        b
    });
    let read_err = std::thread::spawn(move || {
        let mut b = vec![];
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut b);
        }
        b
    });
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if timeout.is_some_and(|t| started.elapsed() > t) {
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    Ok(std::process::Output { status, stdout: read_out.join().unwrap_or_default(), stderr: read_err.join().unwrap_or_default() })
}

const STATUS_MARK: &str = "\n__ABX_HTTP_STATUS__";

/** curl's arguments for a request (no shell in between) and the body for its stdin. */
pub fn curl_args(req: &Value) -> Result<(Vec<String>, Option<String>), String> {
    let url = req["url"].as_str().filter(|u| !u.is_empty()).ok_or("http needs a url")?;
    let mut args: Vec<String> = vec!["-sS".into(), "-L".into(), "-D".into(), "-".into(), "-w".into(), format!("{STATUS_MARK}%{{http_code}}")];
    let method = req["method"].as_str().map(str::to_uppercase);
    let timeout = req["timeout_secs"].as_f64().unwrap_or(30.);
    args.extend(["-m".into(), format!("{timeout}")]);
    let mut has_type = false;
    if let Some(h) = req["headers"].as_object() {
        for (k, v) in h {
            has_type |= k.eq_ignore_ascii_case("content-type");
            args.extend(["-H".into(), format!("{k}: {}", crate::ui::render::value_text(v))]);
        }
    }
    let body = match &req["body"] {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => {
            if !has_type {
                args.extend(["-H".into(), "Content-Type: application/json".into()]);
            }
            Some(other.to_string())
        }
    };
    if body.is_some() {
        args.extend(["--data-binary".into(), "@-".into()]);
    }
    if let Some(m) = method.or_else(|| body.as_ref().map(|_| "POST".into())) {
        args.extend(["-X".into(), m]);
    }
    match &req["args"] {
        Value::Array(a) => args.extend(a.iter().map(crate::ui::render::value_text)),
        Value::String(s) => args.extend(s.split_whitespace().map(str::to_string)),
        _ => {}
    }
    args.push("--".into());
    args.push(url.to_string());
    Ok((args, body))
}

/** Splits curl's output (header blocks, the body, then the status mark) into the result. */
pub fn parse_curl_output(stdout: &str, stderr: &str, code: Option<i32>) -> Value {
    let (rest, status) = match stdout.rfind(STATUS_MARK) {
        Some(i) => (&stdout[..i], stdout[i + STATUS_MARK.len()..].trim().parse::<u16>().unwrap_or(0)),
        None => (stdout, 0),
    };
    let mut rest = rest;
    let mut headers = Map::new();
    while rest.starts_with("HTTP/") {
        let Some(end) = rest.find("\r\n\r\n").map(|i| (i, 4)).or_else(|| rest.find("\n\n").map(|i| (i, 2))) else { break };
        headers.clear();
        for line in rest[..end.0].lines().skip(1) {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_lowercase(), Value::String(v.trim().to_string()));
            }
        }
        rest = &rest[end.0 + end.1..];
    }
    json!({
        "status": status, "ok": (200..300).contains(&status), "headers": headers, "body": rest,
        "code": code, "stdout": rest, "stderr": stderr,
    })
}

/** An HTTP request through curl (blocking; run it off the UI thread). */
pub fn http(req: &Value, path: &str) -> Value {
    let (args, body) = match curl_args(req) {
        Ok(x) => x,
        Err(e) => return json!({"status": 0, "ok": false, "error": e, "code": -1, "stdout": "", "stderr": e, "body": "", "headers": {}}),
    };
    let mut cmd = std::process::Command::new("curl");
    cmd.args(&args).env("PATH", path);
    match run_output(cmd, body, None) {
        Ok(o) => parse_curl_output(&String::from_utf8_lossy(&o.stdout), &String::from_utf8_lossy(&o.stderr), o.status.code()),
        Err(e) => json!({"status": 0, "ok": false, "error": e.to_string(), "code": -1, "stdout": "", "stderr": e.to_string(), "body": "", "headers": {}}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_gets_arguments_not_a_shell_line() {
        let (args, body) = curl_args(&json!({"url": "https://x.test/a?b=1;rm -rf", "method": "put", "headers": {"X-A": "1"}, "body": {"k": 1}})).unwrap();
        assert_eq!(args.last().map(String::as_str), Some("https://x.test/a?b=1;rm -rf"));
        assert!(args.windows(2).any(|w| w[0] == "-X" && w[1] == "PUT"));
        assert!(args.iter().any(|a| a == "Content-Type: application/json"));
        assert_eq!(body.as_deref(), Some(r#"{"k":1}"#));
        let (args, body) = curl_args(&json!({"url": "https://x.test", "body": "raw"})).unwrap();
        assert!(args.windows(2).any(|w| w[0] == "-X" && w[1] == "POST"));
        assert_eq!(body.as_deref(), Some("raw"));
        assert!(curl_args(&json!({})).is_err());
    }

    #[test]
    fn curl_output_splits_into_status_headers_and_body() {
        let out = "HTTP/1.1 301 Moved\r\nLocation: /b\r\n\r\nHTTP/2 200\r\nContent-Type: text/plain\r\n\r\nhello\r\n\r\nworld\n__ABX_HTTP_STATUS__200";
        let r = parse_curl_output(out, "", Some(0));
        assert_eq!(r["status"], 200);
        assert_eq!(r["ok"], true);
        assert_eq!(r["headers"]["content-type"], "text/plain");
        assert!(r["headers"].get("location").is_none());
        assert_eq!(r["body"], "hello\r\n\r\nworld");
    }

    #[test]
    fn pure_calls_reach_the_filesystem() {
        let dir = std::env::temp_dir().join(format!("abx-pure-{}", std::process::id()));
        let d = dir.to_string_lossy().to_string();
        assert_eq!(pure_call("fs.mkdir", &json!({"path": d})).unwrap().unwrap(), json!(true));
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        let list = pure_call("fs.list", &json!({"path": d})).unwrap().unwrap();
        assert_eq!(list[0]["name"], "f.txt");
        assert_eq!(pure_call("fs.exists", &json!({"path": format!("{d}/nope")})).unwrap().unwrap(), json!(false));
        assert!(pure_call("clipboard.read", &json!({})).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
