// Sticky prompt: once your latest message in the open chat scrolls off the top of the transcript, it
// pins to the top of the chat (like Claude in VS Code). Publishes `.data.sticky_prompt`:
// { chat, text, show }. Checked on transcript scroll (action `sticky.check`), on new messages, on
// selection and every 1.5 s as a fallback.

fn check() {
    let chat = ab::context()["selected"]["chat"]["id"].as_str().unwrap_or("").to_string();
    if chat.is_empty() {
        ab::data("sticky_prompt", json!({ "show": false }));
        return;
    }
    ab::inspect(move |i: Value, _err| {
        let t = &i["views"]["transcripts"][chat.as_str()];
        let items = t["items"].as_array().cloned().unwrap_or_default();
        let last_user = items.iter().rev().find(|it| it["kind"].as_str() == Some("user")).cloned();
        let Some(u) = last_user else {
            ab::data("sticky_prompt", json!({ "chat": chat, "show": false }));
            return;
        };
        let u_index = u["index"].as_i64().unwrap_or(-1);
        let text = u["text"].as_str().unwrap_or("").to_string();
        let top = i["layout"]
            .as_array()
            .and_then(|l| l.iter().find(|n| n["id"].as_str() == Some("transcript")))
            .and_then(|n| n["y"].as_f64())
            .unwrap_or(0.0);
        let rows = t["rows"].as_array().cloned().unwrap_or_default();
        // A row's `row` is the transcript item index it shows.
        let own = rows.iter().find(|r| r["row"].as_i64() == Some(u_index));
        let show = match own {
            // Visible row: pinned once its bottom has passed the transcript's top edge.
            Some(r) => r["y"].as_f64().unwrap_or(0.0) + r["h"].as_f64().unwrap_or(0.0) <= top + 6.0,
            // Not drawn: pinned when everything drawn comes after it (scrolled past it).
            None => {
                let first = rows.iter().filter_map(|r| r["row"].as_i64()).min();
                first.map_or(false, |f| f > u_index)
            }
        };
        let prev = ab::context()["data"]["sticky_prompt"].clone();
        let next = json!({ "chat": chat, "text": text, "show": show, "index": u_index });
        if prev != next {
            ab::data("sticky_prompt", next);
        }
    });
}

fn init() {
    ab::action("sticky.check", |_, _| check());
    ab::action("sticky.jump", |_, _| {
        // Scroll back to the pinned message.
        let p = ab::context()["data"]["sticky_prompt"].clone();
        if let (Some(chat), Some(index)) = (p["chat"].as_str(), p["index"].as_i64()) {
            let chat = chat.to_string();
            ab::inspect(move |i: Value, _err| {
                let rows = i["views"]["transcripts"][chat.as_str()]["rows"].as_array().cloned().unwrap_or_default();
                let _ = rows;
                ab::run(json!({ "do": "transcript.scroll", "chat": chat, "row": index }));
                ab::after(300, || check());
            });
        }
    });
    ab::on("start", |_, _| {
        check();
        // A timer, not a scroll handler: reading the app's inspect state on every scroll event made
        // scrolling lag.
        // Only the full-screen chat shows the pinned prompt, so skip the (costly) inspect read otherwise.
        ab::every(2000, || {
            if ab::context()["state"]["tile_focus"].as_bool() == Some(true) {
                check();
            }
        });
    });
    ab::on("select", |_, _| check());
    ab::on("chat.message", |_, _| check());
}
