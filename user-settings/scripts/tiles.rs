// Agent tile grid: where each open chat's tile sits (state `tile_slots`: {chat id: cell index}).
// Open chats without a cell take the lowest free cells, in `.chats` order (the same rule the grid's jq
// uses, so both agree). Action `tiles.move {dragged, to}` puts a tile in cell `to`; a tile already there
// swaps into the dragged tile's old cell.

use std::collections::BTreeMap;

fn term_tiles() -> Vec<Value> {
    ab::context()["state"]["term_tiles"].as_array().cloned().unwrap_or_default()
}

fn assignment() -> BTreeMap<String, i64> {
    let ctx = ab::context();
    // Same item order as the grid's jq: open chats, then terminal tiles ("term:ID").
    let mut open: Vec<String> = ctx["chats"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c["open"].as_bool().unwrap_or(false))
                .filter_map(|c| c["id"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    for t in term_tiles() {
        if let Some(id) = t["id"].as_str() {
            open.push(format!("term:{id}"));
        }
    }
    // The designer is a tile too while it's open (state.designer_open).
    if ctx["state"]["designer_open"].as_bool() == Some(true) {
        open.push("term:designer".to_string());
    }
    let saved = ctx["state"]["tile_slots"].clone();
    let mut m: BTreeMap<String, i64> = BTreeMap::new();
    for id in &open {
        if let Some(i) = saved[id.as_str()].as_i64() {
            m.insert(id.clone(), i);
        }
    }
    for id in &open {
        if m.contains_key(id) {
            continue;
        }
        let mut free = 0i64;
        while m.values().any(|v| *v == free) {
            free += 1;
        }
        m.insert(id.clone(), free);
    }
    m
}

// Each tile keeps its own title color (state `tile_color:<id>`), whatever cell it sits in. A tile
// without one gets the least-used of the 8 palette colors among the open tiles, so all 8 are used
// before any repeats.
fn assign_colors(m: &BTreeMap<String, i64>) {
    let st = ab::context()["state"].clone();
    let n_colors = ab::context()["theme"]["tile_tints"].as_array().map(|a| a.len()).unwrap_or(8).max(1);
    let mut used = vec![0usize; n_colors];
    let mut missing: Vec<(i64, String)> = vec![];
    for (id, slot) in m {
        match st[format!("tile_color:{id}").as_str()].as_i64() {
            Some(c) => used[(c.max(0) as usize) % n_colors] += 1,
            None => missing.push((*slot, id.clone())),
        }
    }
    // In grid order, so a fresh grid still reads left-to-right through the palette.
    missing.sort();
    for (_, id) in missing {
        let (c, _) = used.iter().enumerate().min_by_key(|(i, n)| (**n, *i)).unwrap();
        used[c] += 1;
        ab::set(&format!("tile_color:{id}"), json!(c));
    }
}

fn save(m: &BTreeMap<String, i64>) {
    assign_colors(m);
    let mut obj = json!({});
    for (k, v) in m {
        obj[k.as_str()] = json!(v);
    }
    ab::set("tile_slots", obj);
}

fn init() {
    ab::action("tiles.move", |args, _| {
        let dragged = args["dragged"].as_str().unwrap_or("").to_string();
        let to = args["to"].as_i64().or(args["to"].as_str().and_then(|s| s.parse().ok())).unwrap_or(-1);
        if dragged.is_empty() || to < 0 {
            return;
        }
        let mut m = assignment();
        let Some(from) = m.get(&dragged).copied() else { return };
        if from == to {
            return;
        }
        let occupant = m.iter().find(|(_, v)| **v == to).map(|(k, _)| k.clone());
        if let Some(o) = occupant {
            m.insert(o, from);
        }
        m.insert(dragged, to);
        save(&m);
    });
    // A new shell tile in ~/redo: its own terminal key, so it never shares a screen with another tile.
    ab::action("tiles.new_terminal", |args, _| {
        let cwd = args["cwd"].as_str().unwrap_or("/Users/jarron.conover/redo").to_string();
        let now = ab::context()["now"].as_f64().unwrap_or(0.0);
        let id = format!("t{}", (now * 1000.0) as i64 % 100_000_000);
        let mut list = term_tiles();
        let n = list.len() + 1;
        let label = format!("Terminal {n}");
        list.push(json!({ "id": id, "label": label, "cwd": cwd }));
        ab::set("term_tiles", Value::Array(list));
        ab::run(json!({ "do": "terminal.new", "path": cwd.clone(), "id": id.clone(), "label": label, "cwd": cwd, "host": "mac" }));
        ab::run(json!({ "do": "focus", "area": "terminal" }));
        ab::run(json!("tiles.pin_all"));
    });
    ab::action("tiles.close_terminal", |args, _| {
        let id = args["id"].as_str().unwrap_or("").to_string();
        let list: Vec<Value> = term_tiles().into_iter().filter(|t| t["id"].as_str() != Some(id.as_str())).collect();
        if let Some(t) = term_tiles().iter().find(|t| t["id"].as_str() == Some(id.as_str())) {
            let cwd = t["cwd"].as_str().unwrap_or("").to_string();
            ab::run(json!({ "do": "terminal.close", "path": cwd, "id": id.clone() }));
        }
        ab::set("term_tiles", Value::Array(list));
        let mut m = assignment();
        m.remove(&format!("term:{id}"));
        save(&m);
    });
    // Lock in today's positions so tiles don't reshuffle as chats open and close.
    ab::action("tiles.pin_all", |_, _| {
        let m = assignment();
        save(&m);
    });
    // Wheel over the page gutter or the sidebar: scroll the tile grid. state.grid_thumb is the scrollbar
    // thumb's position along its track; the page offset follows from it (same math as the grid's jq).
    ab::action("tiles.wheel", |args, _| {
        let ctx = ab::context();
        // The tile page moves only while scroll lock is on.
        if ctx["state"]["tile_focus"].as_bool() == Some(true) || ctx["state"]["scroll_lock"].as_bool() != Some(true) {
            return;
        }
        let dy = args["dy"].as_f64().unwrap_or(0.0);
        if dy == 0.0 {
            return;
        }
        // Sizes come with the event (scripts don't see the layout); same defaults as the jq.
        let w = args["w"].as_f64().or(ctx["app"]["layout"]["main-area"]["w"].as_f64()).unwrap_or(1200.0);
        let cols = if w < 900.0 { 1.0 } else if w < 1400.0 { 2.0 } else if w < 2000.0 { 3.0 } else { 4.0 };
        let max_slot = ctx["state"]["tile_slots"].as_object().map(|o| o.values().filter_map(|v| v.as_f64()).fold(-1.0, f64::max)).unwrap_or(-1.0);
        let n = max_slot + 1.0;
        let rows = (n / cols).ceil() + 1.0;
        let wh = args["wh"].as_f64().or(ctx["app"]["window"]["height"].as_f64()).unwrap_or(1000.0);
        let ch = (((wh - 110.0) / 2.0).floor()).max(340.0);
        let content = rows * ch + (rows - 1.0) * 10.0 + 16.0;
        let view = wh - 58.0;
        if content <= view {
            return;
        }
        let max = (content - view).max(1.0);
        let th = (view * view / content).max(40.0);
        let track = (view - 16.0 - th).max(1.0);
        let cur = ctx["state"]["grid_thumb"].as_f64().unwrap_or(0.0).clamp(0.0, track);
        // Wheel deltas arrive large and spiky (momentum bursts up to thousands of points), so each step is
        // capped and scaled down; with the grid's short ease on `top` that reads as a smooth glide.
        let step = dy.clamp(-260.0, 260.0) * 0.45;
        let next = (cur - step * track / max).clamp(0.0, track);
        if (next - cur).abs() > 0.01 {
            ab::set("grid_thumb", json!(next));
        }
    });
    ab::action("tiles.open_designer", |_, _| {
        ab::set("designer_open", json!(true));
        ab::run(json!("tiles.pin_all"));
    });
    ab::action("tiles.close_designer", |_, _| {
        ab::set("designer_open", json!(false));
        let mut m = assignment();
        m.remove("term:designer");
        save(&m);
        if ab::context()["state"]["focus_term"].as_str() == Some("designer") {
            ab::set("focus_term", Value::Null);
        }
    });
    ab::on("start", |_, _| {
        ab::run(json!("tiles.pin_all"));
        // Pin the designer's cell whenever it opens (⌘⇧D, the palette button or the New tile menu).
        ab::every(700, || {
            let ctx = ab::context();
            let open = ctx["state"]["designer_open"].as_bool() == Some(true);
            let placed = ctx["state"]["tile_slots"]["term:designer"].is_i64();
            if open && !placed {
                ab::run(json!("tiles.pin_all"));
                return;
            }
            // Any open tile still without its own color (a chat opened elsewhere, a new terminal) gets one.
            let st = &ctx["state"];
            let slots = st["tile_slots"].as_object().cloned().unwrap_or_default();
            if slots.keys().any(|id| st[format!("tile_color:{id}").as_str()].is_null()) {
                let m: BTreeMap<String, i64> = slots.iter().filter_map(|(k, v)| v.as_i64().map(|i| (k.clone(), i))).collect();
                assign_colors(&m);
            }
        });
    });
    ab::on("chat.added", |_, _| ab::run(json!("tiles.pin_all")));
}
