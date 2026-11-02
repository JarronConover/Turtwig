/*! What an agent needs to see the running app: `inspect.json` (the context, layout, problems) kept
current, `activity.log` of every action run, and `app.snapshot` to capture the window. */

use crate::settings;
use crate::remote::Remote;
use crate::workspace::Workspace;
use agent_box_protocol::Call;
use gpui_kit::*;
use serde_json::{json, Value};
use std::io::Write as _;


/** Recent frames: when each started, how long building the element tree took, and the whole frame. */
#[derive(Default)]
pub struct Perf {
    frames: std::collections::VecDeque<(std::time::Instant, f32, f32)>,
    pub spans: std::collections::BTreeMap<&'static str, f32>,
    /** Regions placed fresh (their fingerprint moved) and drawn, since the last summary. */
    pub regions_fresh: Vec<String>,
    pub regions_drawn: Vec<String>,
    /** A chat being opened (selected, its transcript not drawn yet), and when that began. */
    pub opening: Option<(String, std::time::Instant, bool)>,
    /** Frames drawn since launch. */
    pub frames_total: u64,
    drawn: Option<(std::time::Duration, Option<String>)>,
    /** Recent first loads of a transcript: chat, ms waiting on its machine, ms taking it in, entries. */
    fetches: std::collections::VecDeque<(String, f32, f32, usize)>,
    /** Recent opens: chat, ms from selecting it to the first frame painted with its transcript, and
    whether its transcript was already loaded. */
    pub opens: std::collections::VecDeque<(String, f32, bool, f32, f32, Value)>,
}

fn counts(ids: &[String]) -> Value {
    let mut m = std::collections::BTreeMap::new();
    for id in ids {
        *m.entry(id.as_str()).or_insert(0) += 1;
    }
    json!(m)
}

impl Perf {
    /** Starts counting region draws afresh (each inspect write reports the ones since the last). */
    pub fn reset_regions(&mut self) {
        self.regions_fresh.clear();
        self.regions_drawn.clear();
    }

    pub fn fetched(&mut self, chat: &str, wait: std::time::Duration, apply: std::time::Duration, entries: usize) {
        self.fetches.push_back((chat.to_string(), wait.as_secs_f32() * 1000., apply.as_secs_f32() * 1000., entries));
        while self.fetches.len() > 20 {
            self.fetches.pop_front();
        }
    }

    /** The workspace drew this frame: how long that took, and the chat it showed open. */
    pub fn drew(&mut self, render: std::time::Duration, opened: Option<String>) {
        self.drawn = Some((render, opened));
    }

    /** A frame was painted, `started` when the window's content began drawing. */
    pub fn record(&mut self, started: std::time::Instant) {
        let (render, opened) = self.drawn.take().unwrap_or_default();
        if let Some((chat, at, cached)) = self.opening.take_if(|o| opened.as_ref() == Some(&o.0)) {
            let frame = started.elapsed().as_secs_f32() * 1000.;
            self.opens.push_back((chat, at.elapsed().as_secs_f32() * 1000., cached, frame, render.as_secs_f32() * 1000., json!(self.spans)));
            while self.opens.len() > 20 {
                self.opens.pop_front();
            }
        }
        self.frames_total += 1;
        self.frames.push_back((started, render.as_secs_f32() * 1000., started.elapsed().as_secs_f32() * 1000.));
        while self.frames.len() > 240 {
            self.frames.pop_front();
        }
    }

    pub fn summary(&self, window_secs: f32) -> Value {
        let window_secs = window_secs.max(0.1);
        let recent: Vec<_> = self.frames.iter().filter(|f| f.0.elapsed().as_secs_f32() < window_secs).collect();
        let avg = |k: fn(&(std::time::Instant, f32, f32)) -> f32| if recent.is_empty() { 0. } else { recent.iter().map(|f| k(f)).sum::<f32>() / recent.len() as f32 };
        let last = self.frames.back();
        json!({
            "frames_per_sec": recent.len() as f32 / window_secs,
            "frames_total": self.frames_total,
            "avg_render_ms": avg(|f| f.1),
            "avg_frame_ms": avg(|f| f.2),
            "max_frame_ms": recent.iter().map(|f| f.2).fold(0., f32::max),
            "last_render_ms": last.map(|f| f.1),
            "last_frame_ms": last.map(|f| f.2),
            "spans_ms": self.spans,
            "regions_fresh": counts(&self.regions_fresh),
            "regions_drawn": counts(&self.regions_drawn),
            "opens": self.opens.iter().map(|(c, ms, cached, frame, render, spans)| json!({"chat": c, "ms": ms, "cached": cached, "frame_ms": frame, "render_ms": render, "spans": spans})).collect::<Vec<_>>(),
            "fetches": self.fetches.iter().map(|(c, wait, apply, n)| json!({"chat": c, "wait_ms": wait, "apply_ms": apply, "entries": n})).collect::<Vec<_>>(),
            "animation_frames": crate::ui::region::ASKED.with(|a| json!(std::mem::take(&mut *a.borrow_mut()))),
        })
    }
}

/** What was last written and mirrored, shared by the writes made here and on the file thread. */
#[derive(Default)]
struct Written {
    inspect: u64,
    mirrored: std::collections::HashMap<String, u64>,
}

static WRITTEN: std::sync::LazyLock<std::sync::Mutex<Written>> = std::sync::LazyLock::new(Default::default);

/** inspect.json and its copies to other machines go out from one thread, in order, off the main one. */
static FILES: std::sync::LazyLock<flume::Sender<(Option<Value>, Vec<String>, Vec<Remote>)>> = std::sync::LazyLock::new(|| {
    let (tx, rx) = flume::unbounded::<(Option<Value>, Vec<String>, Vec<Remote>)>();
    std::thread::spawn(move || {
        for (doc, names, hosts) in rx {
            save_files(doc, &names, &hosts);
        }
    });
    tx
});

/** A machine that starts using the app again gets every file, changed or not. */
pub(crate) fn forget_mirrored() {
    WRITTEN.lock().unwrap().mirrored.clear();
}

fn digest(bytes: &[u8]) -> u64 {
    bytes.len() as u64 ^ bytes.iter().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(*b as u64))
}

/** Writes inspect.json when it changed, then sends `names` that changed to `hosts`. */
fn save_files(doc: Option<Value>, names: &[String], hosts: &[Remote]) {
    if let Some(doc) = doc {
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
        let d = digest(text.as_bytes());
        if std::mem::replace(&mut WRITTEN.lock().unwrap().inspect, d) != d {
            let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let text = text.replacen('{', &format!("{{\n  \"at\": {at},"), 1);
            let path = settings::dir().join("inspect.json");
            let tmp = settings::dir().join(".inspect.json");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
    if hosts.is_empty() {
        return;
    }
    use base64::Engine as _;
    for name in names {
        let Ok(bytes) = std::fs::read(settings::dir().join(name)) else { continue };
        let d = digest(&bytes);
        if WRITTEN.lock().unwrap().mirrored.insert(name.clone(), d) == Some(d) {
            continue;
        }
        // Compact: inspect.json is a third smaller without its indentation.
        let bytes = match name.ends_with(".json").then(|| serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| serde_json::to_vec(&v).ok())).flatten() {
            Some(compact) => compact,
            None => bytes,
        };
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        for r in hosts {
            r.send_latest(name, Call::AppFile { name: name.clone(), data: data.clone() });
        }
    }
}

impl Workspace {
    /** Rewrites inspect.json when what it describes changed (checked at most every `inspect.interval_ms`),
    on the file thread; with `inspect_at` cleared (an agent's request just ran), here and now, so the
    answer it reads next is current. */
    pub(crate) fn write_inspect(&mut self, ctx: &jaq_json::Val, window: &Window, cx: &App) {
        let now = self.inspect_at.is_none();
        if self.inspect_at.is_some_and(|t| (t.elapsed().as_millis() as f64) < self.settings.num("inspect.interval_ms", 2000.)) {
            return;
        }
        self.inspect_at = Some(std::time::Instant::now());
        let doc = self.inspect_doc_with(ctx, window, cx);
        let names: Vec<String> = ["inspect.json", "activity.log", "problems.txt"].map(String::from).to_vec();
        let hosts = self.mirror_hosts(false);
        if now {
            save_files(Some(doc), &names, &hosts);
        } else {
            let _ = FILES.send((Some(doc), names, hosts));
        }
    }

    /** What inspect.json holds, as of now (`ab.inspect` for scripts). */
    pub(crate) fn inspect_doc(&self, window: &Window, cx: &App) -> Value {
        let ctx = self.context();
        self.inspect_doc_with(&ctx, window, cx)
    }

    fn inspect_doc_with(&self, ctx: &jaq_json::Val, window: &Window, cx: &App) -> Value {
        let mut ctx = crate::ui::expr::to_json(ctx);
        // The settings are in settings.jsonc and SETTINGS.md already; the layout has its own list below.
        if let Value::Object(o) = &mut ctx {
            o.remove("settings");
            if let Some(Value::Object(app)) = o.get_mut("app") {
                app.remove("layout");
            }
        }
        let mut layout: Vec<(String, [f32; 4])> = self.motion.borrow().bounds.iter().map(|(k, b)| (k.clone(), *b)).collect();
        layout.sort_by(|a, b| a.0.cmp(&b.0));
        let size = window.viewport_size();
        let perf = self.perf.borrow().summary(self.settings.num("inspect.perf_window_secs", 2.) as f32);
        self.perf.borrow_mut().reset_regions();
        json!({
            "about": "The running app as of `at`: `context` is what settings expressions see; `layout` is where nodes with ids are (x, y, w, h in points); see SETTINGS.md.",
            "window": {"width": f32::from(size.width), "height": f32::from(size.height), "focused": self.focused, "number": window_number()},
            "perf": perf,
            "internal": self.internal_json(),
            "dialog": self.dialog_json(cx),
            "palette": self.palette_entries().into_iter().map(|(l, _)| l).collect::<Vec<_>>(),
            "views": {
                "transcripts": crate::ui::trace_view::inspect(self),
                "diff": crate::ui::diff_view::inspect(self),
                "terminals": crate::terminal::inspect(self, cx),
            },
            "transcripts": self.lists.iter().map(|(chat, l)| (chat.clone(), json!({"following": l.state.is_following_tail(), "at_end": l.state.is_scrolled_to_end(), "rows": l.state.item_count(), "top": l.state.logical_scroll_top().item_ix, "top_offset": f32::from(l.state.logical_scroll_top().offset_in_item), "kinds": l.kinds(),
                "items": self.transcripts.get(chat).map(|t| t.items.len()), "lines": self.transcripts.get(chat).map(|t| t.lines), "start": self.transcripts.get(chat).map(|t| t.start)}))).collect::<serde_json::Map<_, _>>(),
            "drop": self.drop_hint.as_ref().map(|h| json!({"key": h.key, "zone": h.zone, "kind": h.kind})),
            "context": ctx,
            "clickable": self.clickables_last.iter().map(|(id, label, _)| json!({"id": id, "label": label})).collect::<Vec<_>>(),
            "menus": self.menus_last.iter().map(|(id, tip, _, _)| json!({"id": id, "label": tip})).collect::<Vec<_>>(),
            "composers": self.composers.iter().map(|(chat, c)| (chat.clone(), Value::String(c.read(cx).value().to_string()))).collect::<serde_json::Map<_, _>>(),
            "composer_markdown": self.composers.keys().map(|chat| (chat.clone(), self.composer_markdown(chat, cx))).collect::<serde_json::Map<_, _>>(),
            "ui": crate::ui::drive::inspect(self, window, cx),
            "layout": layout.into_iter().map(|(k, b)| json!({"id": k, "x": b[0], "y": b[1], "w": b[2], "h": b[3]})).collect::<Vec<_>>(),
            "scripts": self.scripts_json(),
        })
    }

    /** Sends the app's files (inspect.json and the rest) to the other machines whose agents relayed an action
    in the last `inspect.mirror_active_secs`, at most every `inspect.mirror_ms`, each only when it changed:
    they read them as they would here. Only those: inspect.json runs to megabytes, and it would hold up
    everything else on the connection (a chat's transcript as it opens). */
    pub(crate) fn mirror_app_files(&mut self, names: &[&str]) {
        let hosts = self.mirror_hosts(names == ["snapshot.png"]);
        if !hosts.is_empty() {
            let _ = FILES.send((None, names.iter().map(|n| n.to_string()).collect(), hosts));
        }
    }

    /** The machines to mirror to now (none if it's too soon since the last time, unless `anyway`). */
    fn mirror_hosts(&mut self, anyway: bool) -> Vec<Remote> {
        let every = self.settings.num("inspect.mirror_ms", 1500.);
        if every <= 0. || self.mirror_at.is_some_and(|t| (t.elapsed().as_millis() as f64) < every) && !anyway {
            return vec![];
        }
        let active = self.settings.num("inspect.mirror_active_secs", 300.);
        let using = |h: &str| self.relayed_at.get(h).is_some_and(|t| t.elapsed().as_secs_f64() < active);
        let hosts: Vec<Remote> = self.remotes.iter().filter(|(h, _)| !self.is_local_host(h) && self.host_up.get(*h) == Some(&true) && using(h)).map(|(_, r)| r.clone()).collect();
        if !hosts.is_empty() {
            self.mirror_at = Some(std::time::Instant::now());
        }
        hosts
    }

    /** One line per action run, newest last, trimmed to the last few hundred. */
    pub(crate) fn log_action(&mut self, action: &Value) {
        if matches!(action, Value::Null) {
            return;
        }
        let at = chrono_like_now();
        let mut line = format!("{at} {}", serde_json::to_string(action).unwrap_or_default());
        let chars = self.settings.num("inspect.activity_chars", 600.) as usize;
        if line.len() > chars {
            let mut at = chars;
            while !line.is_char_boundary(at) {
                at -= 1;
            }
            line.truncate(at);
        }
        self.activity.push(line);
        let lines = self.settings.num("inspect.activity_lines", 400.) as usize;
        if self.activity.len() > lines {
            let drop = self.activity.len() - lines;
            self.activity.drain(..drop);
        }
        if let Ok(mut f) = std::fs::File::create(settings::dir().join("activity.log")) {
            let _ = f.write_all((self.activity.join("\n") + "\n").as_bytes());
        }
    }

    /** Captures the window to `path` (default snapshot.png in the settings folder); then `then` runs with
    `.path`, and `app.snapshot_done` fires (`.path`, `.ok`). */
    pub(crate) fn snapshot(&mut self, path: Option<String>, then: Option<Value>, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        let id = crate::macos::window_number();
        #[cfg(not(target_os = "macos"))]
        let id: Option<i64> = None;
        let Some(id) = id else {
            let t = self.tr("no_window", "No window to capture", &[]);
            self.toast(t, window, cx);
            return;
        };
        let default = settings::dir().join("snapshot.png");
        let path = path.map(|p| std::path::PathBuf::from(crate::script::expand(&p))).unwrap_or(default.clone());
        let mirror = path == default;
        cx.spawn_in(window, async move |this, cx| {
            let target = path.clone();
            let ok = cx
                .background_spawn(async move { std::process::Command::new("/usr/sbin/screencapture").args(["-x", "-o", &format!("-l{id}")]).arg(target).status() })
                .await
                .is_ok_and(|s| s.success());
            this.update_in(cx, |ws, window, cx| {
                if mirror {
                    ws.mirror_app_files(&["snapshot.png"]);
                }
                let p = Value::String(path.to_string_lossy().into_owned());
                if let Some(then) = then {
                    let c = crate::ui::expr::with(&ws.context(), vec![("path".into(), crate::ui::expr::to_val(&p))]);
                    let a = ws.exprs.resolve_action(&then, &c);
                    ws.run_action(&a, window, cx);
                }
                ws.fire_event("app.snapshot_done", vec![("path".into(), crate::ui::expr::to_val(&p)), ("ok".into(), crate::ui::expr::to_val(&Value::Bool(ok)))], window, cx);
            })
            .ok();
        })
        .detach();
    }

    /** Scripts' registered actions, live timers and problems, for inspect.json. */
    fn scripts_json(&self) -> Value {
        json!({
            "actions": self.scripts.as_ref().map(|s| s.actions.clone()).unwrap_or_default(),
            "timers": self.script_timer_ids(),
            "errors": self.scripts.as_ref().map(|s| s.errors.clone()).unwrap_or_default(),
            "processes": self.script_proc_ids(),
        })
    }
}

fn window_number() -> Option<i64> {
    #[cfg(target_os = "macos")]
    return crate::macos::window_number();
    #[cfg(not(target_os = "macos"))]
    None
}

/** Unix seconds; agents compare it with `at` in inspect.json. */
fn chrono_like_now() -> String {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string()
}
