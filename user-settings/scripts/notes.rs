// Notes tiles: a single page like Apple Notes, with a title and a checklist.
//
// A Notes tile lives in `state.term_tiles` as `{id, kind: "notes", label: "Notes"}` (grid id "term:<id>").
// Its page is in UI state, so it survives restarts:
//   note_title:<tile>          the title line
//   notes:<tile>               the checklist order: [{id, done}]
//   ni:<tile>:<item>           each item's text (what its input is bound to)
//   note_focus                 the item whose field should take focus (a new or merged line)
//
// Actions: notes.new, notes.add {tile, after?}, notes.remove {tile, item}, notes.toggle {tile, item},
//          notes.clear_done {tile}, notes.close {tile}, notes.backspace {tile, item}

fn state() -> Value {
    ab::context()["state"].clone()
}

fn tiles() -> Vec<Value> {
    state()["term_tiles"].as_array().cloned().unwrap_or_default()
}

fn items(tile: &str) -> Vec<Value> {
    state()[format!("notes:{tile}").as_str()].as_array().cloned().unwrap_or_default()
}

fn save(tile: &str, list: Vec<Value>) {
    ab::set(&format!("notes:{tile}"), Value::Array(list));
}

fn new_id() -> String {
    let n = ab::context()["now"].as_f64().unwrap_or(0.0);
    thread_local! { static SEQ: std::cell::RefCell<u64> = std::cell::RefCell::new(0); }
    let s = SEQ.with(|s| {
        *s.borrow_mut() += 1;
        *s.borrow()
    });
    format!("i{}{}", (n * 1000.0) as i64 % 100_000_000, s)
}

// A line and its sub-items: the range [p, end) of lines after it that are indented deeper.
fn family(list: &[Value], p: usize) -> (usize, usize) {
    let level = list[p]["indent"].as_i64().unwrap_or(0);
    let mut end = p + 1;
    while end < list.len() && list[end]["indent"].as_i64().unwrap_or(0) > level {
        end += 1;
    }
    (p, end)
}

// The input of one checklist line, as the app names it (node id + the tile's and item's keys).
fn input_id(tile: &str, item: &str) -> String {
    format!("note-input[term:{tile}][{item}]")
}

fn focus(tile: &str, item: &str) {
    ab::set("note_focus", json!(format!("{tile}:{item}")));
    ab::run(json!({ "do": "ui.focus", "id": input_id(tile, item) }));
}

fn add(tile: &str, after: Option<&str>) {
    let mut list = items(tile);
    let id = new_id();
    let at = after.and_then(|a| list.iter().position(|i| i["id"].as_str() == Some(a))).map(|p| p + 1).unwrap_or(list.len());
    // A new line keeps the indent of the line it follows (sub-items stay sub-items).
    let indent = if at > 0 { list[at - 1]["indent"].as_i64().unwrap_or(0) } else { 0 };
    list.insert(at, json!({ "id": id, "done": false, "indent": indent }));
    save(tile, list);
    ab::set("note_focus", json!(format!("{tile}:{id}")));
}

fn init() {
    ab::action("notes.new", |_, _| {
        let n = ab::context()["now"].as_f64().unwrap_or(0.0);
        let id = format!("n{}", (n * 1000.0) as i64 % 1_000_000_000);
        let mut list = tiles();
        list.push(json!({ "id": id, "kind": "notes", "label": "Notes", "cwd": "" }));
        ab::set("term_tiles", Value::Array(list));
        ab::set(&format!("note_title:{id}"), json!(""));
        let first = new_id();
        save(&id, vec![json!({ "id": first, "done": false })]);
        ab::run(json!("tiles.pin_all"));
    });
    ab::action("notes.add", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let after = args["after"].as_str().map(|s| s.to_string());
        add(&tile, after.as_deref());
    });
    // ↑ / ↓: move to the line above / below (↑ on the first line goes to the title).
    ab::action("notes.move", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let item = args["item"].as_str().unwrap_or("");
        let dir = args["dir"].as_i64().unwrap_or(1);
        let list = items(&tile);
        let Some(p) = list.iter().position(|i| i["id"].as_str() == Some(item)) else { return };
        let target = p as i64 + dir;
        if target < 0 {
            ab::run(json!({ "do": "ui.focus", "id": format!("note-title[term:{tile}]") }));
            return;
        }
        if let Some(id) = list.get(target as usize).and_then(|i| i["id"].as_str()) {
            focus(&tile, id);
        }
    });
    // ↓ or Enter on the title: into the first line.
    ab::action("notes.from_title", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        match items(&tile).first().and_then(|i| i["id"].as_str().map(|s| s.to_string())) {
            Some(id) => focus(&tile, &id),
            None => add(&tile, None),
        }
    });
    // Tab / ⇧Tab: make the line a sub-item of the one above, or move it back out (up to 3 levels).
    ab::action("notes.indent", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let item = args["item"].as_str().unwrap_or("").to_string();
        let delta = args["delta"].as_i64().unwrap_or(1);
        let list = items(&tile);
        let Some(p) = list.iter().position(|i| i["id"].as_str() == Some(item.as_str())) else { return };
        let above = if p > 0 { list[p - 1]["indent"].as_i64().unwrap_or(0) } else { -1 };
        let cur = list[p]["indent"].as_i64().unwrap_or(0);
        // One level deeper than the line above at most; never below zero; 3 levels deep at most.
        let next = (cur + delta).clamp(0, (above + 1).clamp(0, 3));
        let shift = next - cur;
        if shift == 0 {
            // Nothing to move, but Tab took the cursor to a focus catcher: put it back in the line.
            let (t2, i2) = (tile.clone(), item.clone());
            ab::after(30, move || focus(&t2, &i2));
            return;
        }
        // The line's own sub-items move with it.
        let (s, e) = family(&list, p);
        let mut list = list;
        for i in &mut list[s..e] {
            let lvl = i["indent"].as_i64().unwrap_or(0);
            i["indent"] = json!((lvl + shift).clamp(0, 6));
        }
        save(&tile, list);
        // Keep typing in the same line.
        let (t2, i2) = (tile.clone(), item.clone());
        ab::after(30, move || focus(&t2, &i2));
    });
    // Tab / ⇧Tab from the app's key map (the field itself never sees Tab: the app uses it to move focus).
    // Finds the focused checklist line and indents it.
    ab::action("notes.tab", |args, _| {
        let delta = args["delta"].as_i64().unwrap_or(1);
        ab::inspect(move |i: Value, _err| {
            let Some(inputs) = i["ui"]["inputs"].as_object().or(i["inputs"].as_object()) else { return };
            for (path, v) in inputs {
                if v["focused"].as_bool() != Some(true) || !path.starts_with("note-input[") {
                    continue;
                }
                // "note-input[term:n123][i456]": tile n123, item i456.
                let tile = path.split("[term:").nth(1).and_then(|s| s.split(']').next()).unwrap_or("");
                let item = path.rsplit('[').next().and_then(|s| s.split(']').next()).unwrap_or("");
                if tile.is_empty() || !item.starts_with('i') {
                    return;
                }
                ab::run(json!({ "do": "notes.indent", "tile": tile, "item": item, "delta": delta }));
                return;
            }
        });
    });
    ab::action("notes.toggle", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let item = args["item"].as_str().unwrap_or("");
        let mut list = items(&tile);
        let Some(p) = list.iter().position(|i| i["id"].as_str() == Some(item)) else { return };
        let done = !list[p]["done"].as_bool().unwrap_or(false);
        // Checking a parent checks its sub-items too (and unchecking unchecks them).
        let (start, end) = family(&list, p);
        for i in &mut list[start..end] {
            i["done"] = json!(done);
        }
        save(&tile, list);
    });
    ab::action("notes.remove", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let item = args["item"].as_str().unwrap_or("").to_string();
        let list = items(&tile);
        let pos = list.iter().position(|i| i["id"].as_str() == Some(item.as_str()));
        // Deleting a parent deletes its sub-items with it.
        let gone: Vec<String> = match pos {
            Some(p) => {
                let (s, e) = family(&list, p);
                list[s..e].iter().filter_map(|i| i["id"].as_str().map(|x| x.to_string())).collect()
            }
            None => vec![],
        };
        for g in gone.iter().filter(|g| **g != item) {
            ab::set(&format!("ni:{tile}:{g}"), Value::Null);
        }
        let mut kept: Vec<Value> = list.iter().filter(|i| !gone.iter().any(|g| i["id"].as_str() == Some(g.as_str()))).cloned().collect();
        if kept.is_empty() {
            kept.push(json!({ "id": new_id(), "done": false, "indent": 0 }));
        }
        // Focus moves to the line above, like deleting an empty line in Notes.
        if let Some(p) = pos {
            if p > 0 {
                if let Some(prev) = kept.get(p - 1).and_then(|i| i["id"].as_str()) {
                    ab::set("note_focus", json!(format!("{tile}:{prev}")));
                }
            }
        }
        ab::set(&format!("ni:{tile}:{item}"), Value::Null);
        save(&tile, kept);
    });
    // Backspace on an empty line removes it (never the last line).
    ab::action("notes.backspace", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let item = args["item"].as_str().unwrap_or("").to_string();
        let text = state()[format!("ni:{tile}:{item}").as_str()].as_str().unwrap_or("").to_string();
        if text.is_empty() && items(&tile).len() > 1 {
            ab::run(json!({ "do": "notes.remove", "tile": tile, "item": item }));
        }
    });
    ab::action("notes.clear_done", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let list = items(&tile);
        for i in list.iter().filter(|i| i["done"].as_bool().unwrap_or(false)) {
            ab::set(&format!("ni:{tile}:{}", i["id"].as_str().unwrap_or("")), Value::Null);
        }
        let mut kept: Vec<Value> = list.into_iter().filter(|i| !i["done"].as_bool().unwrap_or(false)).collect();
        if kept.is_empty() {
            kept.push(json!({ "id": new_id(), "done": false }));
        }
        save(&tile, kept);
    });
    ab::action("notes.close", |args, _| {
        let tile = args["tile"].as_str().unwrap_or("").to_string();
        let list: Vec<Value> = tiles().into_iter().filter(|t| t["id"].as_str() != Some(tile.as_str())).collect();
        ab::set("term_tiles", Value::Array(list));
        let mut slots = state()["tile_slots"].clone();
        if let Some(o) = slots.as_object_mut() {
            o.remove(&format!("term:{tile}"));
        }
        ab::set("tile_slots", slots);
        if state()["focus_term"].as_str() == Some(tile.as_str()) {
            ab::set("focus_term", Value::Null);
        }
    });
}
