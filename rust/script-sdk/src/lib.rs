/*! What an a.box script (`scripts/NAME.rs` with a `fn init()`, compiled to WebAssembly) uses to reach the
app: every call becomes a JSON message to the app, or a reply coming back. See SETTINGS.md, Scripts. */

pub use serde_json::{self, json, Value};

use std::cell::RefCell;
use std::collections::HashMap;

type Handler = Box<dyn FnMut(Value, Value)>;
type Reply = Box<dyn FnOnce(Value, Option<String>)>;

#[derive(Default)]
struct Registry {
    next: u64,
    actions: HashMap<String, Handler>,
    events: HashMap<String, Vec<Handler>>,
    timers: HashMap<u64, (Box<dyn FnMut()>, bool)>,
    replies: HashMap<u64, Reply>,
    procs: HashMap<u64, Box<dyn FnMut(Proc, Value)>>,
    /** What this delivery has set (`set`, `data`), visible to `context()` before the app applies it. */
    overlay_state: serde_json::Map<String, Value>,
    overlay_data: serde_json::Map<String, Value>,
    context: Option<Value>,
}

thread_local! {
    static REG: RefCell<Registry> = RefCell::new(Registry::default());
}

fn next_id() -> u64 {
    REG.with(|r| {
        let mut r = r.borrow_mut();
        r.next += 1;
        r.next
    })
}

mod host {
    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "ab")]
    extern "C" {
        pub fn send(ptr: *const u8, len: usize);
        pub fn sync(name: *const u8, name_len: usize, args: *const u8, args_len: usize) -> usize;
        pub fn take(ptr: *mut u8);
    }

    /** Off WebAssembly (tests), messages collect here and sync calls answer null. */
    #[cfg(not(target_arch = "wasm32"))]
    thread_local! {
        pub static SENT: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    pub fn send_text(text: &str) {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            send(text.as_ptr(), text.len())
        }
        #[cfg(not(target_arch = "wasm32"))]
        SENT.with(|s| s.borrow_mut().push(text.to_string()));
    }

    pub fn sync_text(name: &str, args: &str) -> String {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            let len = sync(name.as_ptr(), name.len(), args.as_ptr(), args.len());
            let mut buf = vec![0u8; len];
            take(buf.as_mut_ptr());
            String::from_utf8_lossy(&buf).into_owned()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (name, args);
            "null".into()
        }
    }
}

fn send(message: Value) {
    host::send_text(&message.to_string());
}

fn wait(mut message: Value, reply: Option<Reply>) -> u64 {
    let id = next_id();
    if let Some(f) = reply {
        REG.with(|r| r.borrow_mut().replies.insert(id, f));
    }
    message["id"] = json!(id);
    send(message);
    id
}

fn val(v: impl serde::Serialize) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/** A new action, usable anywhere one is (buttons, keys, the palette, run.json, other scripts): `f(args, ctx)`. */
pub fn action(name: &str, f: impl FnMut(Value, Value) + 'static) {
    REG.with(|r| r.borrow_mut().actions.insert(name.to_string(), Box::new(f)));
    send(json!({"type": "register", "name": name}));
}

/** Handles an event (`start` on each load, `model`, the settings events, your own `emit`s): `f(payload, ctx)`. */
pub fn on(event: &str, f: impl FnMut(Value, Value) + 'static) {
    REG.with(|r| r.borrow_mut().events.entry(event.to_string()).or_default().push(Box::new(f)));
}

/** Runs any action: `json!({"do": "select", ...})`, or a name like `json!("nav.back")`. */
pub fn run(action: Value) {
    if action["do"] == "state.set" {
        if let Some(k) = action["key"].as_str() {
            let v = action["value"].clone();
            REG.with(|r| r.borrow_mut().overlay_state.insert(k.to_string(), v));
        }
    }
    send(json!({"type": "do", "action": action}));
}

/** The context settings expressions see, with this delivery's `set`s and `data`s already in it. */
pub fn context() -> Value {
    let mut c = REG.with(|r| r.borrow().context.clone()).unwrap_or_else(|| {
        let c: Value = serde_json::from_str(&host::sync_text("context", "{}")).unwrap_or(Value::Null);
        REG.with(|r| r.borrow_mut().context = Some(c.clone()));
        c
    });
    REG.with(|r| {
        let r = r.borrow();
        for (key, over) in [("state", &r.overlay_state), ("data", &r.overlay_data)] {
            if !c[key].is_object() {
                c[key] = json!({});
            }
            if let Some(o) = c[key].as_object_mut() {
                o.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
    });
    c
}

pub fn state() -> Value {
    context()["state"].clone()
}

/** Sets UI state (`.state.<key>`). */
pub fn set(key: &str, value: impl serde::Serialize) {
    run(json!({"do": "state.set", "key": key, "value": val(value)}));
}

/** Publishes a value as `.data.<name>` for expressions. */
pub fn data(name: &str, value: impl serde::Serialize) {
    let v = val(value);
    REG.with(|r| r.borrow_mut().overlay_data.insert(name.to_string(), v.clone()));
    send(json!({"type": "data", "name": name, "value": v}));
}

/** A notice in the window. */
pub fn notify(text: impl std::fmt::Display) {
    run(json!({"do": "notify", "text": text.to_string()}));
}

/** Raises an event for `on` handlers (here and in settings `events`). */
pub fn emit(event: &str, payload: impl serde::Serialize) {
    send(json!({"type": "emit", "event": event, "payload": val(payload)}));
}

/** A line in scripts.log. */
pub fn log(text: impl std::fmt::Display) {
    send(json!({"type": "log", "text": text.to_string()}));
}

/** Runs `f` every `ms` milliseconds; returns its id for `cancel`. */
pub fn every(ms: u64, f: impl FnMut() + 'static) -> u64 {
    let id = next_id();
    REG.with(|r| r.borrow_mut().timers.insert(id, (Box::new(f), true)));
    send(json!({"type": "timer", "id": id, "ms": ms, "repeat": true}));
    id
}

/** Runs `f` once after `ms` milliseconds. */
pub fn after(ms: u64, f: impl FnOnce() + 'static) -> u64 {
    let id = next_id();
    let mut once = Some(f);
    REG.with(|r| r.borrow_mut().timers.insert(id, (Box::new(move || if let Some(f) = once.take() { f() }), false)));
    send(json!({"type": "timer", "id": id, "ms": ms, "repeat": false}));
    id
}

pub fn cancel(id: u64) {
    REG.with(|r| r.borrow_mut().timers.remove(&id));
    send(json!({"type": "cancel", "id": id}));
}

/** A shell command on the Mac (`opts`: `{cwd, host, timeout, env}`): `f({code, stdout, stderr}, error)`. */
pub fn shell(cmd: &str, opts: Value, f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
    let mut m = json!({"type": "shell", "cmd": cmd});
    if let (Some(o), Some(extra)) = (m.as_object_mut(), opts.as_object()) {
        o.extend(extra.clone());
    }
    wait(m, Some(Box::new(f)))
}

/** A call to a daemon (`exec` runs bash there: `{cmd, cwd?, timeout_secs?}`); `host` None: where it belongs. */
pub fn daemon_on(host: Option<&str>, method: &str, params: Value, f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
    wait(json!({"type": "box", "method": method, "params": params, "host": host}), Some(Box::new(f)))
}

pub fn daemon(method: &str, params: Value, f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
    daemon_on(None, method, params, f)
}

/** One of the app's abilities by name (`http`, `clipboard.read`, `fs.list`, `inspect`, ...; see SETTINGS.md). */
pub fn call(name: &str, args: Value, f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
    wait(json!({"type": "call", "name": name, "args": args}), Some(Box::new(f)))
}

/** The abilities that answer at once (`fs.*`, `env`). */
pub fn call_sync(name: &str, args: Value) -> Result<Value, String> {
    let r: Value = serde_json::from_str(&host::sync_text(name, &args.to_string())).unwrap_or(Value::Null);
    match r.get("error").and_then(Value::as_str) {
        Some(e) => Err(format!("{name}: {e}")),
        None => Ok(r.get("value").cloned().unwrap_or(Value::Null)),
    }
}

/** An HTTP request without a shell (`{url, method, headers, body, timeout_secs}`): `f({status, ok, headers, body}, error)`. */
pub fn http(req: Value, f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
    call("http", req, f)
}

pub fn fetch(url: &str, f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
    call("http", json!({"url": url}), f)
}

/** A file's text (`~/` allowed), or None when it can't be read. */
pub fn read(path: &str) -> Option<String> {
    let r: Value = serde_json::from_str(&host::sync_text("read", &json!({"path": path}).to_string())).unwrap_or(Value::Null);
    r.as_str().map(str::to_string)
}

pub fn write(path: &str, text: &str, f: impl FnOnce(Option<String>) + 'static) -> u64 {
    wait(json!({"type": "write", "path": path, "text": text}), Some(Box::new(move |_, err| f(err))))
}

pub fn inspect(f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
    call("inspect", json!({}), f)
}

pub mod clipboard {
    use super::*;
    pub fn read(f: impl FnOnce(Value, Option<String>) + 'static) -> u64 {
        call("clipboard.read", json!({}), f)
    }
    pub fn write(text: &str) -> u64 {
        call("clipboard.write", json!({"text": text}), |_, _| {})
    }
}

pub mod fs {
    use super::*;
    pub fn list(path: &str) -> Result<Value, String> {
        call_sync("fs.list", json!({"path": path}))
    }
    pub fn exists(path: &str) -> bool {
        call_sync("fs.exists", json!({"path": path})).ok().and_then(|v| v.as_bool()).unwrap_or(false)
    }
    pub fn stat(path: &str) -> Result<Value, String> {
        call_sync("fs.stat", json!({"path": path}))
    }
    pub fn mkdir(path: &str) -> Result<Value, String> {
        call_sync("fs.mkdir", json!({"path": path}))
    }
    pub fn remove(path: &str) -> Result<Value, String> {
        call_sync("fs.remove", json!({"path": path}))
    }
    /** Calls `f(paths)` whenever anything under `path` changes. */
    pub fn watch(path: &str, mut f: impl FnMut(Value) + 'static) -> u64 {
        let id = next_id();
        REG.with(|r| r.borrow_mut().procs.insert(id, Box::new(move |p, data| if let Proc::Change = p { f(data) })));
        send(json!({"type": "call", "name": "fs.watch", "args": {"path": path, "watch": id}, "id": next_id()}));
        id
    }
}

/** What a spawned process (or a watch) reports. */
pub enum Proc {
    Stdout,
    Stderr,
    Exit,
    Change,
}

/** A spawned process: send it input, or end it. */
pub struct Process(pub u64);

impl Process {
    pub fn write(&self, data: &str) {
        call("proc.write", json!({"proc": self.0, "data": data}), |_, _| {});
    }
    pub fn kill(&self) {
        call("proc.kill", json!({"proc": self.0}), |_, _| {});
    }
}

/** A process whose output streams in (`cmd` a shell line or a list, `opts` `{cwd, env, stdin}`): `f(Stdout|Stderr, line)`, then `f(Exit, code)`. */
pub fn spawn(cmd: Value, opts: Value, f: impl FnMut(Proc, Value) + 'static) -> Process {
    let id = next_id();
    REG.with(|r| r.borrow_mut().procs.insert(id, Box::new(f)));
    let mut args = json!({"cmd": cmd, "proc": id});
    if let (Some(a), Some(o)) = (args.as_object_mut(), opts.as_object()) {
        a.extend(o.clone());
    }
    send(json!({"type": "call", "name": "proc.spawn", "args": args, "id": next_id()}));
    Process(id)
}

/** Patch ops over the settings (`{"id": ..., "set": {...}}`, ...), undone when scripts reload. */
pub fn patch(ops: Value) {
    let ops = if ops.is_array() { ops } else { json!([ops]) };
    send(json!({"type": "patch", "ops": ops}));
}

/** A key binding (`key` like "cmd-shift-k"; `opts` `{if, not_in}`). */
pub fn key(key: &str, action: Value, opts: Value) {
    let mut k = json!({"key": key, "do": action});
    if let (Some(o), Some(extra)) = (k.as_object_mut(), opts.as_object()) {
        o.extend(extra.clone());
    }
    send(json!({"type": "key", "key": k}));
}

/** A command palette entry (`opts` `{if}`). */
pub fn command(label: &str, action: Value, opts: Value) {
    let mut c = json!({"label": label, "do": action});
    if let (Some(o), Some(extra)) = (c.as_object_mut(), opts.as_object()) {
        o.extend(extra.clone());
    }
    send(json!({"type": "command", "command": c}));
}

#[doc(hidden)]
pub fn deliver(raw: &str) {
    let Ok(m) = serde_json::from_str::<Value>(raw) else { return };
    REG.with(|r| {
        let mut r = r.borrow_mut();
        r.overlay_state.clear();
        r.overlay_data.clear();
        r.context = None;
    });
    match m["type"].as_str().unwrap_or("") {
        "action" => {
            let name = m["name"].as_str().unwrap_or("").to_string();
            let f = REG.with(|r| r.borrow_mut().actions.remove(&name));
            if let Some(mut f) = f {
                f(m["args"].clone(), context());
                REG.with(|r| { r.borrow_mut().actions.entry(name).or_insert(f); });
            }
        }
        "event" => {
            let name = m["name"].as_str().unwrap_or("").to_string();
            let hs = REG.with(|r| r.borrow_mut().events.remove(&name));
            if let Some(mut hs) = hs {
                for h in hs.iter_mut() {
                    h(m["payload"].clone(), context());
                }
                REG.with(|r| {
                    let mut r = r.borrow_mut();
                    let added = r.events.remove(&name).unwrap_or_default();
                    hs.extend(added);
                    r.events.insert(name, hs);
                });
            }
        }
        "timer" => {
            let id = m["id"].as_u64().unwrap_or(0);
            let t = REG.with(|r| r.borrow_mut().timers.remove(&id));
            if let Some((mut f, repeat)) = t {
                f();
                if repeat {
                    REG.with(|r| { r.borrow_mut().timers.entry(id).or_insert((f, true)); });
                }
            }
        }
        "reply" => {
            let id = m["id"].as_u64().unwrap_or(0);
            let f = REG.with(|r| r.borrow_mut().replies.remove(&id));
            if let Some(f) = f {
                f(m["result"].clone(), m["error"].as_str().map(str::to_string));
            }
        }
        "proc" => {
            let id = m["proc"].as_u64().unwrap_or(0);
            let f = REG.with(|r| r.borrow_mut().procs.remove(&id));
            if let Some(mut f) = f {
                match m["stream"].as_str() {
                    Some("stdout") => f(Proc::Stdout, m["data"].clone()),
                    Some("stderr") => f(Proc::Stderr, m["data"].clone()),
                    Some("change") => f(Proc::Change, m["data"].clone()),
                    _ => {}
                }
                if m.get("exit").is_some() {
                    f(Proc::Exit, m["exit"].clone());
                } else {
                    REG.with(|r| { r.borrow_mut().procs.entry(id).or_insert(f); });
                }
            }
        }
        _ => {}
    }
}

/** A panic becomes a problem in the app with its message, not a bare trap. */
#[doc(hidden)]
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| send(json!({"type": "error", "text": info.to_string()}))));
}

/** The exports the app calls (`ab_alloc`, `ab_deliver`, `ab_init` running the script's `init`); the app's build adds it. */
#[macro_export]
macro_rules! script {
    ($init:path) => {
        #[no_mangle]
        pub extern "C" fn ab_alloc(len: usize) -> *mut u8 {
            let mut v = Vec::<u8>::with_capacity(len.max(1));
            let p = v.as_mut_ptr();
            std::mem::forget(v);
            p
        }
        #[no_mangle]
        pub unsafe extern "C" fn ab_deliver(ptr: *mut u8, len: usize) {
            let bytes = Vec::from_raw_parts(ptr, len, len.max(1));
            $crate::deliver(&String::from_utf8_lossy(&bytes));
        }
        #[no_mangle]
        pub extern "C" fn ab_init() {
            $crate::install_panic_hook();
            $init();
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent() -> Vec<Value> {
        host::SENT.with(|s| s.borrow_mut().drain(..).map(|t| serde_json::from_str(&t).unwrap()).collect())
    }

    #[test]
    fn actions_register_and_run_with_their_args() {
        let hits = std::rc::Rc::new(RefCell::new(vec![]));
        let h = hits.clone();
        action("hi", move |args, _| h.borrow_mut().push(args["n"].clone()));
        assert_eq!(sent()[0], json!({"type": "register", "name": "hi"}));
        deliver(&json!({"type": "action", "name": "hi", "args": {"n": 3}}).to_string());
        deliver(&json!({"type": "action", "name": "hi", "args": {"n": 4}}).to_string());
        assert_eq!(*hits.borrow(), vec![json!(3), json!(4)]);
    }

    #[test]
    fn replies_reach_their_callback_once() {
        let got = std::rc::Rc::new(RefCell::new(None));
        let g = got.clone();
        let id = shell("uptime", json!({"cwd": "/"}), move |r, e| *g.borrow_mut() = Some((r, e)));
        let m = sent().pop().unwrap();
        assert_eq!((m["type"].clone(), m["cmd"].clone(), m["cwd"].clone(), m["id"].as_u64()), (json!("shell"), json!("uptime"), json!("/"), Some(id)));
        deliver(&json!({"type": "reply", "id": id, "result": {"code": 0}}).to_string());
        deliver(&json!({"type": "reply", "id": id, "result": {"code": 9}}).to_string());
        assert_eq!(got.borrow().clone(), Some((json!({"code": 0}), None)));
    }

    #[test]
    fn set_shows_in_context_during_the_same_delivery() {
        REG.with(|r| r.borrow_mut().context = Some(json!({"state": {"a": 1}})));
        set("b", 2);
        let c = context();
        assert_eq!((c["state"]["a"].clone(), c["state"]["b"].clone()), (json!(1), json!(2)));
        let m = sent().pop().unwrap();
        assert_eq!(m["action"], json!({"do": "state.set", "key": "b", "value": 2}));
    }

    #[test]
    fn repeating_timers_keep_running_and_one_shots_stop() {
        let n = std::rc::Rc::new(RefCell::new(0));
        let (a, b) = (n.clone(), n.clone());
        let rep = every(10, move || *a.borrow_mut() += 1);
        let once = after(10, move || *b.borrow_mut() += 10);
        for id in [rep, rep, once, once] {
            deliver(&json!({"type": "timer", "id": id}).to_string());
        }
        assert_eq!(*n.borrow(), 12);
    }
}
