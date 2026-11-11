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

fn save(m: &BTreeMap<String, i64>) {
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
    ab::on("start", |_, _| ab::run(json!("tiles.pin_all")));
    ab::on("chat.added", |_, _| ab::run(json!("tiles.pin_all")));
}
