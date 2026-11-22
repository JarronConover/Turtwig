/*! The built-in baseline: Jarron's settings, scripts and UI state (`user-settings/` in the repo), embedded
at build time and seeded into the settings folder on first launch. A file already there is never overwritten;
"Reset to Turtwig baseline" backs the folder up and seeds again. */

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/baseline_files.rs"));

/** The three things the baseline seeds, relative to the settings folder. */
pub const PARTS: [&str; 3] = ["settings.jsonc", "scripts", "ui-state.json"];

/** Text made portable: a hard-coded Mac home folder (`/Users/NAME`) becomes `~`, which the app expands. */
pub fn portable_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/Users/") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "/Users/".len()..];
        let name_len = after.find(|c: char| c == '/' || c == '"' || c == '\'' || c.is_whitespace()).unwrap_or(after.len());
        if name_len > 0 {
            out.push('~');
            rest = &after[name_len..];
        } else {
            out.push_str("/Users/");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5 && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/** Whether a tile id names something bound to this machine: a chat (uuid), a Redo work tile (`r…`) or a
shell tile (`t…`). Notes (`n…`) and the designer travel. */
fn machine_bound_tile(id: &str) -> bool {
    let id = id.strip_prefix("term:").unwrap_or(id);
    is_uuid(id) || id.starts_with('r') && id[1..].chars().all(|c| c.is_ascii_digit()) || id.starts_with('t') && id[1..].chars().all(|c| c.is_ascii_digit())
}

/** UI state without what belongs to one machine: chats (uuids), worktree paths, shells, Redo work ids,
drafts, transient edit buffers and fetched data. Theme picks, notes, layout and non-chat tile colors stay. */
pub fn portable_state(state: &Map<String, Value>) -> Map<String, Value> {
    const DROP: [&str; 15] = [
        "selected_worktree", "tab_order", "active_tabs", "orchestrator_chat", "orchestrator_paused", "tile_pending", "tile_test",
        "tile_edit_name", "tab_edit_name", "history_query", "redo_agents_query", "redo_agents_adding", "tile_focus", "data", "setup_step",
    ];
    const DROP_PREFIX: [&str; 5] = ["terms:", "term_active:", "redo_draft:", "model_pick:", "collapsed:proj:"];
    let mut out = Map::new();
    for (k, v) in state {
        if DROP.contains(&k.as_str()) || DROP_PREFIX.iter().any(|p| k.starts_with(p)) || k.split(':').any(is_uuid) {
            continue;
        }
        if let Some(id) = k.strip_prefix("tile_color:") {
            if machine_bound_tile(id) {
                continue;
            }
        }
        let v = match (k.as_str(), v) {
            ("tile_slots", Value::Object(slots)) => Value::Object(slots.iter().filter(|(id, _)| !machine_bound_tile(id)).map(|(a, b)| (a.clone(), b.clone())).collect()),
            ("term_tiles", Value::Array(tiles)) => Value::Array(tiles.iter().filter(|t| t["kind"].as_str() == Some("notes")).cloned().collect()),
            (_, Value::String(s)) => Value::String(portable_text(s)),
            (_, v) => v.clone(),
        };
        out.insert(k.clone(), v);
    }
    out
}

/** The embedded UI state, portable, as JSON text. */
pub fn ui_state_text() -> String {
    let state: Map<String, Value> = serde_json::from_str(BASELINE_UI_STATE).unwrap_or_default();
    serde_json::to_string_pretty(&portable_state(&state)).unwrap_or_else(|_| "{}".into())
}

/** Writes whatever of the baseline `dir` lacks (each script on its own); returns what it wrote, relative to
`dir`. Files already there are left alone. */
pub fn seed(dir: &Path) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(dir.join("scripts"))?;
    let mut wrote = vec![];
    let settings = dir.join("settings.jsonc");
    if !settings.exists() {
        std::fs::write(&settings, portable_text(BASELINE_SETTINGS))?;
        wrote.push("settings.jsonc".into());
    }
    for (name, text) in BASELINE_SCRIPTS {
        let p = dir.join("scripts").join(name);
        if !p.exists() {
            std::fs::write(&p, portable_text(text))?;
            wrote.push(format!("scripts/{name}"));
        }
    }
    let state = dir.join("ui-state.json");
    if !state.exists() {
        std::fs::write(&state, ui_state_text())?;
        wrote.push("ui-state.json".into());
    }
    Ok(wrote)
}

/** Moves the folder's settings.jsonc, scripts/ and ui-state.json into `settings.bak-<stamp>/` beside them
and seeds the baseline afresh; returns the backup folder. */
pub fn reset(dir: &Path) -> std::io::Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let bak = dir.join(format!("settings.bak-{stamp}"));
    std::fs::create_dir_all(&bak)?;
    for part in PARTS {
        let from = dir.join(part);
        if from.exists() {
            std::fs::rename(&from, bak.join(part))?;
        }
    }
    seed(dir)?;
    Ok(bak)
}

static SEEDED_SETTINGS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/** Seeds the app's settings folder at launch, remembering whether settings.jsonc was new (a fresh install). */
pub fn seed_at_launch() {
    let dir = crate::settings::dir();
    let fresh = !dir.join("settings.jsonc").exists();
    match seed(&dir) {
        Ok(wrote) if !wrote.is_empty() => eprintln!("baseline: seeded {}", wrote.join(", ")),
        Ok(_) => {}
        Err(e) => eprintln!("baseline: could not seed {}: {e}", dir.display()),
    }
    let _ = SEEDED_SETTINGS.set(fresh);
}

/** Whether this launch wrote settings.jsonc for the first time (the setup guide opens then). */
pub fn fresh_install() -> bool {
    SEEDED_SETTINGS.get().copied().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("turtwig-baseline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn seeding_an_empty_folder_writes_settings_scripts_and_ui_state() {
        let dir = scratch("empty");
        let wrote = seed(&dir).unwrap();
        assert!(wrote.contains(&"settings.jsonc".to_string()) && wrote.contains(&"ui-state.json".to_string()));
        assert!(wrote.iter().filter(|w| w.starts_with("scripts/")).count() >= 5, "{wrote:?}");
        for part in PARTS {
            assert!(dir.join(part).exists(), "{part}");
        }
        assert!(dir.join("scripts/tiles.rs").exists());
        assert!(seed(&dir).unwrap().is_empty(), "a second seed writes nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_settings_file_is_never_overwritten() {
        let dir = scratch("keep");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.jsonc"), "{ \"mine\": true }").unwrap();
        let wrote = seed(&dir).unwrap();
        assert!(!wrote.contains(&"settings.jsonc".to_string()));
        assert_eq!(std::fs::read_to_string(dir.join("settings.jsonc")).unwrap(), "{ \"mine\": true }");
        assert!(dir.join("ui-state.json").exists(), "the missing parts still arrive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeded_files_hold_no_mac_home_folder() {
        let dir = scratch("portable");
        seed(&dir).unwrap();
        for part in ["settings.jsonc", "ui-state.json"] {
            assert!(!std::fs::read_to_string(dir.join(part)).unwrap().contains("/Users/"), "{part}");
        }
        for (name, _) in BASELINE_SCRIPTS {
            assert!(!std::fs::read_to_string(dir.join("scripts").join(name)).unwrap().contains("/Users/"), "{name}");
        }
        assert_eq!(portable_text("cd /Users/jarron.conover/redo && ls \"/Users/x/y\""), "cd ~/redo && ls \"~/y\"");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeded_ui_state_keeps_notes_and_layout_but_no_chats_or_shells() {
        let state: Map<String, Value> = serde_json::from_str(&ui_state_text()).unwrap();
        for k in state.keys() {
            assert!(!k.split(':').any(is_uuid), "chat id in {k}");
            assert!(!k.starts_with("terms:") && !k.starts_with("redo_draft:") && !k.starts_with("model_pick:"), "{k}");
        }
        for k in ["selected_worktree", "tab_order", "active_tabs", "orchestrator_chat", "tile_pending"] {
            assert!(!state.contains_key(k), "{k} should be dropped");
        }
        assert!(state.keys().any(|k| k.starts_with("notes:")), "notes stay");
        assert!(state.contains_key("sidebar_rail") && state.contains_key("scroll_lock"), "layout prefs stay");
        for t in state["term_tiles"].as_array().unwrap() {
            assert_eq!(t["kind"], "notes");
        }
        for id in state["tile_slots"].as_object().unwrap().keys() {
            assert!(!machine_bound_tile(id), "{id}");
        }
    }

    #[test]
    fn reset_backs_the_folder_up_then_seeds() {
        let dir = scratch("reset");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("settings.jsonc"), "{ \"old\": 1 }").unwrap();
        std::fs::write(dir.join("scripts/mine.rs"), "fn init() {}").unwrap();
        let bak = reset(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(bak.join("settings.jsonc")).unwrap(), "{ \"old\": 1 }");
        assert!(bak.join("scripts/mine.rs").exists());
        assert!(!dir.join("scripts/mine.rs").exists(), "the folder holds only the baseline now");
        assert!(std::fs::read_to_string(dir.join("settings.jsonc")).unwrap().len() > 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
