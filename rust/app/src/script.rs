/*! Live abilities: Rust scripts (settings `scripts` and `scripts/NAME.rs`) compiled to WebAssembly and run
by wasmtime, rebuilt on every change. Each reaches the app through the `ab` crate's JSON messages. */

use crate::settings;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::io::{Read as _, Write as _};
use std::rc::Rc;
use std::time::Duration;

/** Everything `ab::call(name, args, f)` reaches, for SETTINGS.md; the `fs.*` and `env` ones also answer at once with `ab::call_sync`. */
pub const CALLS: &[(&str, &str)] = &[
    ("http", "{url, method?, headers?, body? (text, or JSON sent as such), timeout_secs?, args?}: an HTTP request (curl, no shell): `{status, ok, headers, body, code, stdout, stderr}`"),
    ("clipboard.read", "the clipboard's text: `{text}`"),
    ("clipboard.write", "{text}: put text on the clipboard"),
    ("fs.list", "{path}: the folder's entries `[{name, path, dir}]`"),
    ("fs.exists", "{path}: true or false"),
    ("fs.stat", "{path}: `{size, dir, modified}` or null"),
    ("fs.mkdir", "{path}: make the folder (and its parents)"),
    ("fs.remove", "{path}: remove a file or an empty folder"),
    ("fs.watch", "{path}: (via `ab::fs::watch(path, f)`) f(paths) runs when anything under path changes"),
    ("proc.spawn", "(via `ab::spawn(cmd, json!({cwd, env, stdin, shell}), f)`) a process on the Mac whose output streams in line by line to f; the returned handle's `write(data)` and `kill()` reach it"),
    ("proc.write / proc.kill", "{proc, data?}: write to a spawned process's stdin, or end it"),
    ("inspect", "what inspect.json holds right now (layout, transcripts, terminals, scripts, dialog, ...)"),
    ("env", "{name}: an environment variable of the app's process"),
];

/** One script's WebAssembly instance: its store (with the host side of the bridge) and the exports the
app calls. */
struct Instance {
    name: String,
    store: wasmtime::Store<Host>,
    alloc: wasmtime::TypedFunc<u32, u32>,
    deliver: wasmtime::TypedFunc<(u32, u32), ()>,
    memory: wasmtime::Memory,
    /** The script's own ids (timers, replies, processes) -> the ids the app sees. */
    ids: HashMap<u64, u64>,
}

#[derive(Default)]
struct Host {
    outbox: Vec<Value>,
    context: Rc<str>,
    /** A sync call's answer, until the script `take`s it. */
    answer: Vec<u8>,
}

/** A script still to compile: where its source goes, and what its build is cached under. */
pub struct Build {
    pub name: String,
    pub source: String,
    pub hash: String,
}

/** Where scripts build and how: from settings `scripts_options` (and `app.source_dir` for the `ab` crate). */
#[derive(Clone)]
pub struct Options {
    pub build_dir: PathBuf,
    pub sdk_dir: PathBuf,
    pub dependencies: Map<String, Value>,
    pub time_limit_ms: u64,
    pub path: String,
}

pub struct Scripts {
    instances: Vec<Instance>,
    /** App-side id -> (instance, the script's id). */
    owners: HashMap<u64, (usize, u64)>,
    next_id: u64,
    builds: Vec<Build>,
    options: Options,
    pub actions: Vec<String>,
    action_owner: HashMap<String, usize>,
    pub errors: Vec<String>,
    /** Scripts still compiling, by name. */
    pub building: Vec<String>,
    /** Bumped on every (re)load, so a build that finishes for an older load is dropped. */
    pub generation: u64,
}

pub fn scripts_dir() -> PathBuf {
    settings::dir().join("scripts")
}

const TICK_MS: u64 = 10;

/** One engine for every load: compiled code is reused, and a ticker thread lets a running script be cut off. */
fn engine() -> &'static wasmtime::Engine {
    static ENGINE: std::sync::OnceLock<wasmtime::Engine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = wasmtime::Config::new();
        config.epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config).expect("the WebAssembly engine");
        let ticker = engine.clone();
        std::thread::Builder::new()
            .name("script-epoch".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(TICK_MS));
                ticker.increment_epoch();
            })
            .ok();
        engine
    })
}

fn sanitize(name: &str) -> String {
    let stem = name.strip_suffix(".rs").unwrap_or(name);
    let s: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    if s.is_empty() { "script".into() } else { s }
}

fn hash_of(parts: &[&str]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    format!("{:016x}", h.finish())
}

impl Scripts {
    /** Loads `settings.scripts` (name -> Rust source) and every `scripts/NAME.rs`: built ones start now,
    the rest wait in `take_builds` for the app to compile them in the background. */
    pub fn load(inline: &Map<String, Value>, options: Options, generation: u64) -> Option<Scripts> {
        let mut sources: Vec<(String, String)> = inline.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
        let mut files: Vec<_> = std::fs::read_dir(scripts_dir()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "rs")).collect();
        files.sort();
        for f in files {
            if let Ok(src) = std::fs::read_to_string(&f) {
                sources.push((f.file_name().unwrap_or_default().to_string_lossy().into_owned(), src));
            }
        }
        let sdk = std::fs::read_to_string(options.sdk_dir.join("src/lib.rs")).unwrap_or_default();
        let deps = Value::Object(options.dependencies.clone()).to_string();
        let mut s = Scripts {
            instances: vec![],
            owners: HashMap::new(),
            next_id: 0,
            builds: vec![],
            options,
            actions: vec![],
            action_owner: HashMap::new(),
            errors: vec![],
            building: vec![],
            generation,
        };
        for (name, source) in sources {
            let hash = hash_of(&[&source, &sdk, &deps]);
            match std::fs::read(s.cached(&name, &hash)) {
                Ok(wasm) => {
                    s.start(&name, &wasm, Rc::from("{}"));
                }
                Err(_) => {
                    s.building.push(name.clone());
                    s.builds.push(Build { name, source, hash });
                }
            }
        }
        Some(s)
    }

    fn cached(&self, name: &str, hash: &str) -> PathBuf {
        self.options.build_dir.join("cache").join(format!("{}-{hash}.wasm", sanitize(name)))
    }

    /** The scripts that need compiling (each once), with what `build` needs for them. */
    pub fn take_builds(&mut self) -> Vec<(Build, Options)> {
        let opts = self.options.clone();
        self.builds.drain(..).map(|b| (b, opts.clone())).collect()
    }

    /** A build finished: start the script (its `init` runs; what it sends comes back), or note why not. */
    pub fn built(&mut self, name: &str, result: Result<Vec<u8>, String>, context: Rc<str>) -> Vec<Value> {
        self.building.retain(|n| n != name);
        match result {
            Ok(wasm) => self.start(name, &wasm, context),
            Err(e) => {
                self.errors.push(format!("script {name}: {e}"));
                vec![]
            }
        }
    }

    fn start(&mut self, name: &str, wasm: &[u8], context: Rc<str>) -> Vec<Value> {
        match self.instantiate(name, wasm, context) {
            Ok(mut inst) => {
                let index = self.instances.len();
                let out: Vec<Value> = std::mem::take(&mut inst.store.data_mut().outbox);
                self.instances.push(inst);
                self.route_out(index, out)
            }
            Err(e) => {
                self.errors.push(format!("script {name}: {e}"));
                vec![]
            }
        }
    }

    fn instantiate(&self, name: &str, wasm: &[u8], context: Rc<str>) -> Result<Instance, String> {
        use wasmtime::{Caller, Extern, Linker, Module, Store};
        let engine = engine();
        let module = Module::new(engine, wasm).map_err(|e| e.to_string())?;
        let mut linker: Linker<Host> = Linker::new(engine);
        fn memory(caller: &mut Caller<'_, Host>) -> Option<wasmtime::Memory> {
            match caller.get_export("memory") {
                Some(Extern::Memory(m)) => Some(m),
                _ => None,
            }
        }
        fn text(caller: &mut Caller<'_, Host>, ptr: u32, len: u32) -> String {
            let Some(mem) = memory(caller) else { return String::new() };
            let mut buf = vec![0u8; len as usize];
            let _ = mem.read(&*caller, ptr as usize, &mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        }
        linker
            .func_wrap("ab", "send", |mut caller: Caller<'_, Host>, ptr: u32, len: u32| {
                let t = text(&mut caller, ptr, len);
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    caller.data_mut().outbox.push(v);
                }
            })
            .map_err(|e| e.to_string())?;
        linker
            .func_wrap("ab", "sync", |mut caller: Caller<'_, Host>, np: u32, nl: u32, ap: u32, al: u32| -> u32 {
                let name = text(&mut caller, np, nl);
                let args: Value = serde_json::from_str(&text(&mut caller, ap, al)).unwrap_or(Value::Null);
                let answer = match name.as_str() {
                    "context" => caller.data().context.to_string(),
                    "read" => std::fs::read_to_string(expand(args["path"].as_str().unwrap_or(""))).map(|t| Value::String(t).to_string()).unwrap_or_else(|_| "null".into()),
                    other => match pure_call(other, &args) {
                        Some(Ok(v)) => json!({"value": v}).to_string(),
                        Some(Err(e)) => json!({"error": e}).to_string(),
                        None => json!({"error": format!("{other} can't answer at once; use ab::call")}).to_string(),
                    },
                };
                caller.data_mut().answer = answer.into_bytes();
                caller.data().answer.len() as u32
            })
            .map_err(|e| e.to_string())?;
        linker
            .func_wrap("ab", "take", |mut caller: Caller<'_, Host>, ptr: u32| {
                let answer = std::mem::take(&mut caller.data_mut().answer);
                if let Some(mem) = memory(&mut caller) {
                    let _ = mem.write(&mut caller, ptr as usize, &answer);
                }
            })
            .map_err(|e| e.to_string())?;
        let mut store = Store::new(engine, Host { context, ..Default::default() });
        store.set_epoch_deadline(self.ticks());
        let instance = linker.instantiate(&mut store, &module).map_err(|e| e.to_string())?;
        let memory = instance.get_memory(&mut store, "memory").ok_or("the script exports no memory")?;
        let alloc = instance.get_typed_func::<u32, u32>(&mut store, "ab_alloc").map_err(|e| format!("{e} (is `fn init()` defined?)"))?;
        let deliver = instance.get_typed_func::<(u32, u32), ()>(&mut store, "ab_deliver").map_err(|e| e.to_string())?;
        let init = instance.get_typed_func::<(), ()>(&mut store, "ab_init").map_err(|e| e.to_string())?;
        store.set_epoch_deadline(self.ticks());
        init.call(&mut store, ()).map_err(|e| self.trap_text(&e))?;
        Ok(Instance { name: name.to_string(), store, alloc, deliver, memory, ids: HashMap::new() })
    }

    fn ticks(&self) -> u64 {
        (self.options.time_limit_ms / TICK_MS).max(1)
    }

    fn trap_text(&self, e: &wasmtime::Error) -> String {
        if e.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::Interrupt) {
            format!("ran longer than {} ms (scripts_options.time_limit_ms) and was stopped", self.options.time_limit_ms)
        } else {
            format!("{e:#}")
        }
    }

    /** What a script sent, with its ids swapped for app-wide ones and its actions noted as its own. */
    fn route_out(&mut self, index: usize, out: Vec<Value>) -> Vec<Value> {
        let mut back = vec![];
        for mut m in out {
            if m["type"] == "register" {
                if let Some(n) = m["name"].as_str() {
                    if !self.actions.iter().any(|a| a == n) {
                        self.actions.push(n.to_string());
                    }
                    self.action_owner.insert(n.to_string(), index);
                }
                continue;
            }
            if m["type"] == "error" {
                let name = self.instances.get(index).map(|i| i.name.clone()).unwrap_or_default();
                if let Some(t) = m["text"].as_str() {
                    m["text"] = json!(format!("script {name}: {t}"));
                }
            }
            for field in ["id"] {
                if let Some(local) = m[field].as_u64() {
                    m[field] = json!(self.global(index, local));
                }
            }
            for field in ["proc", "watch"] {
                if let Some(local) = m["args"][field].as_u64() {
                    m["args"][field] = json!(self.global(index, local));
                }
            }
            back.push(m);
        }
        back
    }

    fn global(&mut self, index: usize, local: u64) -> u64 {
        if let Some(g) = self.instances.get(index).and_then(|i| i.ids.get(&local)) {
            return *g;
        }
        self.next_id += 1;
        let g = self.next_id;
        self.owners.insert(g, (index, local));
        if let Some(i) = self.instances.get_mut(index) {
            i.ids.insert(local, g);
        }
        g
    }

    /** Hands a message to the scripts it's for (an action to its owner, an event to all, a reply or timer
    or process line to the one that asked) with `context` visible to `ab::context()`; returns what they sent. */
    /** Whether any script is running (none: nothing to deliver to, no context to build). */
    pub fn any(&self) -> bool {
        !self.instances.is_empty()
    }

    pub fn deliver(&mut self, message: Value, context: Rc<str>) -> Vec<Value> {
        let targets: Vec<(usize, Value)> = match message["type"].as_str().unwrap_or("") {
            "action" => message["name"].as_str().and_then(|n| self.action_owner.get(n)).map(|i| vec![(*i, message.clone())]).unwrap_or_default(),
            "event" => (0..self.instances.len()).map(|i| (i, message.clone())).collect(),
            kind => {
                let field = if kind == "proc" { "proc" } else { "id" };
                let Some((i, local)) = message[field].as_u64().and_then(|g| self.owners.get(&g).copied()) else { return vec![] };
                let mut m = message.clone();
                m[field] = json!(local);
                vec![(i, m)]
            }
        };
        let mut out = vec![];
        for (i, m) in targets {
            out.extend(self.deliver_to(i, &m, context.clone()));
        }
        out
    }

    /** Delivers to the script named `name` only (its `start` after a late build). */
    pub fn deliver_named(&mut self, name: &str, message: Value, context: Rc<str>) -> Vec<Value> {
        match self.instances.iter().position(|i| i.name == name) {
            Some(i) => self.deliver_to(i, &message, context),
            None => vec![],
        }
    }

    fn deliver_to(&mut self, index: usize, message: &Value, context: Rc<str>) -> Vec<Value> {
        let ticks = self.ticks();
        let Some(inst) = self.instances.get_mut(index) else { return vec![] };
        let bytes = message.to_string().into_bytes();
        inst.store.data_mut().context = context;
        inst.store.set_epoch_deadline(ticks);
        let result = (|| -> Result<(), wasmtime::Error> {
            let ptr = inst.alloc.call(&mut inst.store, bytes.len() as u32)?;
            inst.memory.write(&mut inst.store, ptr as usize, &bytes)?;
            inst.deliver.call(&mut inst.store, (ptr, bytes.len() as u32))
        })();
        let out = std::mem::take(&mut inst.store.data_mut().outbox);
        let name = inst.name.clone();
        if let Err(e) = result {
            let text = self.trap_text(&e);
            self.errors.push(format!("script {name}: {text}"));
        }
        self.route_out(index, out)
    }

    pub fn has_action(&self, name: &str) -> bool {
        self.actions.iter().any(|a| a == name)
    }
}

/** Compiles a script to WebAssembly (blocking; run it off the UI thread): a small crate per script in
`build_dir`, all sharing one target folder, so a rebuild after an edit takes seconds. */
pub fn build(b: &Build, o: &Options) -> Result<Vec<u8>, String> {
    let crate_name = format!("abx_{}", sanitize(&b.name));
    let dir = o.build_dir.join(sanitize(&b.name));
    std::fs::create_dir_all(dir.join("src")).map_err(|e| e.to_string())?;
    let mut deps = format!("ab = {{ path = {:?} }}\n", o.sdk_dir.to_string_lossy());
    for (k, v) in &o.dependencies {
        let spec = match v {
            Value::String(s) => format!("{s:?}"),
            other => toml_inline(other),
        };
        deps.push_str(&format!("{k} = {spec}\n"));
    }
    let manifest = format!(
        "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\npath = \"src/lib.rs\"\n\n[dependencies]\n{deps}\n[profile.release]\nopt-level = 2\nincremental = true\ncodegen-units = 16\npanic = \"abort\"\ndebug = false\n\n[workspace]\n"
    );
    std::fs::write(dir.join("Cargo.toml"), manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("src/script.rs"), &b.source).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("src/lib.rs"), "#![allow(unused_imports, dead_code)]\nuse ab::{json, Value};\ninclude!(\"script.rs\");\nab::script!(init);\n").map_err(|e| e.to_string())?;
    let target = o.build_dir.join("target");
    let run = || {
        let mut cmd = std::process::Command::new("cargo");
        cmd.args(["build", "--release", "--quiet", "--target", "wasm32-unknown-unknown", "--message-format", "short", "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", &target)
            .env("PATH", &o.path);
        run_output(cmd, None, Some(Duration::from_secs(600)))
    };
    let mut out = run().map_err(|e| format!("couldn't run cargo: {e}"))?;
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() && (err.contains("may not be installed") || err.contains("can't find crate for `core`") || err.contains("can't find crate for `std`")) {
        let mut add = std::process::Command::new("rustup");
        add.args(["target", "add", "wasm32-unknown-unknown"]).env("PATH", &o.path);
        let _ = run_output(add, None, Some(Duration::from_secs(600)));
        out = run().map_err(|e| format!("couldn't run cargo: {e}"))?;
    }
    if !out.status.success() {
        let shown = format!("scripts/{}", b.name);
        let src_path = dir.join("src/script.rs").to_string_lossy().into_owned();
        let lines: Vec<String> = String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| l.contains("error") || l.contains("warning: unused"))
            .filter(|l| !l.starts_with("error: could not compile"))
            .map(|l| l.replace(&src_path, &shown).replace("src/script.rs", &shown))
            .take(12)
            .collect();
        return Err(if lines.is_empty() { String::from_utf8_lossy(&out.stderr).trim().to_string() } else { lines.join("\n") });
    }
    let wasm_path = target.join("wasm32-unknown-unknown/release").join(format!("{crate_name}.wasm"));
    let wasm = std::fs::read(&wasm_path).map_err(|e| format!("{}: {e}", wasm_path.display()))?;
    let cache = o.build_dir.join("cache");
    let _ = std::fs::create_dir_all(&cache);
    if let Ok(old) = std::fs::read_dir(&cache) {
        for f in old.flatten() {
            if f.file_name().to_string_lossy().starts_with(&format!("{}-", sanitize(&b.name))) {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
    let _ = std::fs::write(cache.join(format!("{}-{}.wasm", sanitize(&b.name), b.hash)), &wasm);
    Ok(wasm)
}

/** A JSON object as a TOML inline table (for `scripts_options.dependencies` entries like `{"version": "1", "features": [...]}`). */
fn toml_inline(v: &Value) -> String {
    match v {
        Value::Object(o) => format!("{{ {} }}", o.iter().map(|(k, v)| format!("{k} = {}", toml_inline(v))).collect::<Vec<_>>().join(", ")),
        Value::Array(a) => format!("[{}]", a.iter().map(toml_inline).collect::<Vec<_>>().join(", ")),
        Value::String(s) => format!("{s:?}"),
        other => other.to_string(),
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

    /** Compiles and runs a real script (needs cargo and the wasm32 target): `cargo test -- --ignored`. */
    #[test]
    #[ignore]
    fn a_rust_script_builds_registers_and_answers() {
        let dir = std::env::temp_dir().join(format!("abx-script-{}", std::process::id()));
        let home = std::env::var("HOME").unwrap_or_default();
        let options = Options {
            build_dir: dir.clone(),
            sdk_dir: std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../script-sdk"),
            dependencies: Map::new(),
            time_limit_ms: 200,
            path: format!("{home}/.cargo/bin:/usr/bin:/bin"),
        };
        let source = r#"
fn init() {
    ab::action("greet", |args, ctx| {
        ab::set("greeting", format!("hi {} from {}", args["who"].as_str().unwrap_or("?"), ctx["host"].as_str().unwrap_or("?")));
        ab::shell("echo hi", json!({}), |r, _| ab::data("echo", r["stdout"].clone()));
    });
    ab::action("spin", |_, _| loop {});
    ab::on("start", |_, _| ab::log("started"));
}
"#;
        let mut inline = Map::new();
        inline.insert("hello.rs".into(), json!(source));
        let mut s = Scripts::load(&inline, options.clone(), 1).unwrap();
        let (b, o) = s.take_builds().pop().expect("a build");
        let wasm = build(&b, &o).expect("it compiles");
        let out = s.built("hello.rs", Ok(wasm), Rc::from("{}"));
        assert!(out.is_empty(), "{out:?}");
        assert!(s.has_action("greet"));
        let out = s.deliver(json!({"type": "action", "name": "greet", "args": {"who": "tj"}}), Rc::from(r#"{"host":"mac"}"#));
        assert_eq!(out[0]["action"], json!({"do": "state.set", "key": "greeting", "value": "hi tj from mac"}));
        let id = out[1]["id"].as_u64().unwrap();
        let out = s.deliver(json!({"type": "reply", "id": id, "result": {"stdout": "hi\n"}}), Rc::from("{}"));
        assert_eq!(out[0], json!({"type": "data", "name": "echo", "value": "hi\n"}));
        let out = s.deliver(json!({"type": "event", "name": "start"}), Rc::from("{}"));
        assert_eq!(out[0]["text"], "started");
        s.deliver(json!({"type": "action", "name": "spin"}), Rc::from("{}"));
        assert!(s.errors.last().is_some_and(|e| e.contains("ran longer than 200 ms")), "{:?}", s.errors);
        let again = Scripts::load(&inline, options, 2).unwrap();
        assert!(again.building.is_empty(), "the second load uses the cached build");
        assert!(again.has_action("greet"));
        let _ = std::fs::remove_dir_all(dir);
    }
}