// Agents that look idle but are really waiting: their turn ended, yet they still have background work
// running (a Cursor cloud agent they delegated to, a background shell, a subagent), or their last
// message says they're waiting on a cloud agent. Publishes `.data.bg_wait`:
//   { <chat id>: { n, label } }  (only chats that are idle and waiting)
// so tiles can show a yellow light instead of a green "idle" one. Checked every 3 s.

fn looks_like_cloud_wait(msg: &str) -> Option<String> {
    let m = msg.to_lowercase();
    let cues = [
        "cursor.com/agents",
        "cloud agent",
        "cloud-delegate",
        "background agent",
        "waiting for the agent",
        "waiting on the agent",
        "i'll check back",
        "will check back",
        "when it finishes",
        "once it finishes",
    ];
    cues.iter().find(|c| m.contains(*c)).map(|_| "a cloud agent".to_string())
}

fn check() {
    ab::inspect(|i: Value, _err| {
        let ctx = ab::context();
        let mut out = json!({});
        for c in ctx["chats"].as_array().cloned().unwrap_or_default() {
            if c["open"].as_bool() != Some(true) || c["alive"].as_bool() == Some(false) {
                continue;
            }
            if c["state"].as_str() != Some("idle") {
                continue;
            }
            let id = c["id"].as_str().unwrap_or("").to_string();
            let bg = i["views"]["transcripts"][id.as_str()]["background"].as_array().cloned().unwrap_or_default();
            if !bg.is_empty() {
                let label = bg.iter().filter_map(|b| b["label"].as_str()).next().unwrap_or("background work").to_string();
                out[id.as_str()] = json!({ "n": bg.len(), "label": label });
                continue;
            }
            if let Some(label) = looks_like_cloud_wait(c["last_message"].as_str().unwrap_or("")) {
                out[id.as_str()] = json!({ "n": 1, "label": label });
            }
        }
        if ab::context()["data"]["bg_wait"] != out {
            ab::data("bg_wait", out);
        }
    });
}

fn init() {
    ab::on("start", |_, _| {
        check();
        ab::every(3000, check);
    });
    ab::on("chat.finished", |_, _| {
        ab::after(500, check);
    });
}
