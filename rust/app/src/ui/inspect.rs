/*! What an agent needs to see the running app: `inspect.json` (the context, layout, problems) kept
current, `activity.log` of every action run, and `app.snapshot` to capture the window. */

use crate::settings;
use crate::workspace::Workspace;
use gpui_kit::*;
use serde_json::{json, Value};
use std::io::Write as _;


/** Recent frames: when each started, how long building the element tree took, and the whole frame. */
#[derive(Default)]
pub struct Perf {
    frames: std::collections::VecDeque<(std::time::Instant, f32, f32)>,
    pub spans: std::collections::BTreeMap<&'static str, f32>,
}

impl Perf {
    pub fn record(&mut self, started: std::time::Instant, render: std::time::Duration) {
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
            "avg_render_ms": avg(|f| f.1),
            "avg_frame_ms": avg(|f| f.2),
            "max_frame_ms": recent.iter().map(|f| f.2).fold(0., f32::max),
            "last_render_ms": last.map(|f| f.1),
            "last_frame_ms": last.map(|f| f.2),
            "spans_ms": self.spans,
        })
    }
}

impl Workspace {
    /** Rewrites inspect.json when what it describes changed (checked at most twice a second). */
    pub(crate) fn write_inspect(&mut self, ctx: &jaq_json::Val, window: &Window, cx: &App) {
        if self.inspect_at.is_some_and(|t| (t.elapsed().as_millis() as f64) < self.settings.num("inspect.interval_ms", 500.)) {
            return;
        }
        let ctx = crate::ui::expr::to_json(ctx);
        self.inspect_at = Some(std::time::Instant::now());
        let mut layout: Vec<(String, [f32; 4])> = self.motion.borrow().bounds.iter().map(|(k, b)| (k.clone(), *b)).collect();
        layout.sort_by(|a, b| a.0.cmp(&b.0));
        let size = window.viewport_size();
        let doc = json!({
            "about": "The running app as of `at`: `context` is what settings expressions see; `layout` is where nodes with ids are (x, y, w, h in points); see SETTINGS.md.",
            "window": {"width": f32::from(size.width), "height": f32::from(size.height), "focused": self.focused, "number": window_number()},
            "perf": self.perf.borrow().summary(self.settings.num("inspect.perf_window_secs", 2.) as f32),
            "internal": self.internal_json(),
            "dialog": self.dialog_json(cx),
            "palette": self.palette_entries().into_iter().map(|(l, _)| l).collect::<Vec<_>>(),
            "views": {
                "transcripts": crate::ui::trace_view::inspect(self),
                "diff": crate::ui::diff_view::inspect(self),
                "terminals": crate::terminal::inspect(self, cx),
            },
            "transcripts": self.lists.iter().map(|(chat, l)| (chat.clone(), json!({"following": l.state.is_following_tail(), "at_end": l.state.is_scrolled_to_end(), "rows": l.state.item_count(), "top": l.state.logical_scroll_top().item_ix, "kinds": l.kinds()}))).collect::<serde_json::Map<_, _>>(),
            "drop": self.drop_hint.as_ref().map(|h| json!({"key": h.key, "zone": h.zone, "kind": h.kind})),
            "context": ctx,
            "clickable": self.clickables.iter().map(|(id, label, _)| json!({"id": id, "label": label})).collect::<Vec<_>>(),
            "composers": self.composers.iter().map(|(chat, c)| (chat.clone(), Value::String(c.read(cx).value().to_string()))).collect::<serde_json::Map<_, _>>(),
            "layout": layout.into_iter().map(|(k, b)| json!({"id": k, "x": b[0], "y": b[1], "w": b[2], "h": b[3]})).collect::<Vec<_>>(),
        });
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
        let digest = text.len() as u64 ^ text.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
        if digest != self.inspect_digest {
            self.inspect_digest = digest;
            let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let text = text.replacen('{', &format!("{{\n  \"at\": {at},"), 1);
            let path = settings::dir().join("inspect.json");
            let tmp = settings::dir().join(".inspect.json");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
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

    /** Captures the window to snapshot.png in the settings folder. */
    pub(crate) fn snapshot(&self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        #[cfg(target_os = "macos")]
        let id = crate::macos::window_number();
        #[cfg(not(target_os = "macos"))]
        let id: Option<i64> = None;
        let Some(id) = id else {
            window.push_notification(self.tr("no_window", "No window to capture", &[]), cx);
            return;
        };
        let path = settings::dir().join("snapshot.png");
        cx.background_spawn(async move {
            let _ = std::process::Command::new("/usr/sbin/screencapture").args(["-x", "-o", &format!("-l{id}")]).arg(path).status();
        })
        .detach();
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
