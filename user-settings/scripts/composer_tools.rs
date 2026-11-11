// The message box's tool row: Claude's permission mode and effort level.
//
// Publishes `.data.chat_mode` ({chat id: "auto" | "plan" | "manual"}, read from the footer of the
// chat's terminal every 2.5s for the selected chat) and `.data.effort` (effortLevel in
// ~/.claude/settings.json: low | medium | high | xhigh).
//
// Actions: `chat.mode {chat, mode}` presses Shift+Tab in the chat until its footer shows that mode;
// `claude.effort {level}` writes effortLevel to ~/.claude/settings.json (Claude Code reads its
// settings file live, so the next turn uses it).

fn footer_mode(pane: &str) -> &'static str {
    let tail: String = pane.lines().rev().take(4).collect::<Vec<_>>().join("\n").to_lowercase();
    if tail.contains("plan mode") {
        "plan"
    } else if tail.contains("accept edits") {
        "edits"
    } else if tail.contains("auto mode") || tail.contains("bypass permissions") {
        "auto"
    } else {
        "manual"
    }
}

fn selected() -> Option<(String, String, String)> {
    let c = ab::context()["selected"]["chat"].clone();
    // SDK chats (transport "sdk") report mode, state and prompts as data from the app itself; reading a
    // terminal they don't have would only overwrite that.
    if c["transport"].as_str() == Some("sdk") {
        return None;
    }
    let id = c["id"].as_str()?.to_string();
    let tmux = c["tmux"].as_str()?.to_string();
    let host = c["host"].as_str().unwrap_or("mac").to_string();
    Some((id, tmux, host))
}

fn capture(tmux: &str, host: &str, f: impl FnOnce(Option<String>) + 'static) {
    // The daemon's shell may not have Homebrew on its PATH.
    let cmd = format!("PATH=/opt/homebrew/bin:/usr/local/bin:$PATH tmux capture-pane -p -t '{tmux}' 2>&1 | tail -n 4");
    ab::daemon_on(Some(host), "exec", json!({"cmd": cmd, "timeout_secs": 5}), move |r, _err| {
        f(r["stdout"].as_str().map(|s| s.to_string()))
    });
}

// A selection menu on the agent's screen ("❯ Looks good — save it / Discard and exit … Enter to confirm"):
// the options in order, which one the cursor is on, and whether they are numbered ("❯ 1. Yes"), in
// which case the number key picks one; otherwise the arrow keys move and Enter confirms.
fn parse_menu(pane: &str) -> Option<Value> {
    let lines: Vec<&str> = pane.lines().collect();
    let end = lines.iter().rposition(|l| {
        let t = l.to_lowercase();
        t.contains("enter to confirm") || t.contains("enter to select") || t.contains("enter to submit")
    })?;
    let mut items: Vec<(bool, String)> = vec![];
    let mut i = end;
    while i > 0 {
        i -= 1;
        let l = lines[i];
        let trimmed = l.trim_start();
        if trimmed.is_empty() {
            if items.is_empty() { continue } else { break }
        }
        if trimmed.contains("file edited") || trimmed.contains("files edited") {
            continue;
        }
        let indent = l.len() - trimmed.len();
        if let Some(rest) = trimmed.strip_prefix('❯') {
            items.push((true, rest.trim().to_string()));
        } else if indent >= 4 && !items.is_empty() {
            items.push((false, trimmed.to_string()));
        } else if indent >= 4 {
            items.push((false, trimmed.to_string()));
        } else {
            break;
        }
        if items.len() > 12 {
            break;
        }
    }
    items.reverse();
    if items.is_empty() || !items.iter().any(|(sel, _)| *sel) {
        return None;
    }
    let numbered = items.iter().all(|(_, t)| t.chars().next().map_or(false, |c| c.is_ascii_digit()) && t.contains('.'));
    let selected = items.iter().position(|(s, _)| *s).unwrap_or(0);
    let labels: Vec<String> = items
        .iter()
        .map(|(_, t)| if numbered { t.splitn(2, '.').nth(1).unwrap_or(t).trim().to_string() } else { t.clone() })
        .collect();
    Some(json!({ "options": labels, "selected": selected, "numbered": numbered }))
}

fn poll_mode() {
    // The selected chat, plus every chat waiting on the user, so a menu card is ready when its tab is opened.
    let mut targets: Vec<(String, String, String)> = selected().into_iter().collect();
    if let Some(chats) = ab::context()["chats"].as_array() {
        for c in chats {
            if c["state"].as_str() == Some("waiting") && c["transport"].as_str() != Some("sdk") {
                if let (Some(id), Some(tmux)) = (c["id"].as_str(), c["tmux"].as_str()) {
                    if !targets.iter().any(|t| t.0 == id) {
                        targets.push((id.to_string(), tmux.to_string(), c["host"].as_str().unwrap_or("mac").to_string()));
                    }
                }
            }
        }
    }
    for (id, tmux, host) in targets {
        poll_one(id, tmux, host);
    }
}

fn poll_one(id: String, tmux: String, host: String) {
    capture_n(&tmux, &host, 30, move |pane| {
        if let Some(p) = pane {
            let mut modes = ab::context()["data"]["chat_mode"].clone();
            if !modes.is_object() {
                modes = json!({});
            }
            modes[id.clone()] = json!(footer_mode(&p));
            ab::data("chat_mode", modes);
            let menu = parse_menu(&p);
            let mut menus = ab::context()["data"]["chat_menu"].clone();
            if !menus.is_object() {
                menus = json!({});
            }
            menus[id.clone()] = menu.clone().unwrap_or(Value::Null);
            ab::data("chat_menu", menus);
            // What the screen is doing, so the waiting card only shows for a real prompt (the app's
            // "waiting" state can outlive a prompt answered on the terminal): menu | prompt | working | idle.
            let tail = p.lines().rev().take(12).collect::<Vec<_>>().join("\n").to_lowercase();
            let screen = if menu.is_some() {
                "menu"
            } else if tail.contains("enter to select") || tail.contains("esc to cancel") || tail.contains("do you want to proceed") || tail.contains("❯ 1.") || tail.contains("enter to confirm") {
                "prompt"
            } else if tail.contains("esc to interrupt") {
                "working"
            } else {
                "idle"
            };
            let mut screens = ab::context()["data"]["chat_screen"].clone();
            if !screens.is_object() {
                screens = json!({});
            }
            screens[id] = json!(screen);
            ab::data("chat_screen", screens);
        }
    });
}

fn capture_n(tmux: &str, host: &str, lines: u32, f: impl FnOnce(Option<String>) + 'static) {
    let cmd = format!("PATH=/opt/homebrew/bin:/usr/local/bin:$PATH tmux capture-pane -p -t '{tmux}' 2>&1 | tail -n {lines}");
    ab::daemon_on(Some(host), "exec", json!({"cmd": cmd, "timeout_secs": 5}), move |r, _err| {
        f(r["stdout"].as_str().map(|s| s.to_string()))
    });
}

fn set_mode(chat: String, want: String, tries: u32) {
    let c = ab::context()["chats"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["id"].as_str() == Some(chat.as_str())).cloned())
        .unwrap_or(Value::Null);
    let Some(tmux) = c["tmux"].as_str().map(|s| s.to_string()) else { return };
    let host = c["host"].as_str().unwrap_or("mac").to_string();
    let chat2 = chat.clone();
    capture(&tmux, &host, move |pane| {
        let now = pane.as_deref().map(footer_mode).unwrap_or("manual");
        let mut modes = ab::context()["data"]["chat_mode"].clone();
        if !modes.is_object() {
            modes = json!({});
        }
        modes[chat2.clone()] = json!(now);
        ab::data("chat_mode", modes);
        if now == want || tries == 0 {
            return;
        }
        // Shift+Tab cycles the modes (tmux calls it BTab).
        ab::run(json!({"do": "chat.keys", "chat": chat2, "keys": ["BTab"]}));
        let (c3, w3) = (chat2.clone(), want.clone());
        ab::after(700, move || set_mode(c3, w3, tries - 1));
    });
}

fn read_effort() {
    let text = ab::read("~/.claude/settings.json").unwrap_or_default();
    let v = text.parse::<Value>().unwrap_or(Value::Null);
    let level = v["effortLevel"].as_str().unwrap_or("high").to_string();
    ab::data("effort", json!(level));
}

fn init() {
    ab::action("chat.mode", |args, _ctx| {
        let chat = args["chat"].as_str().unwrap_or("").to_string();
        let mode = args["mode"].as_str().unwrap_or("auto").to_string();
        if !chat.is_empty() {
            set_mode(chat, mode, 5);
        }
    });
    // Pick option `index` of the menu on the chat's screen (data.chat_menu): the number key for a
    // numbered menu, else arrow presses to reach it and Enter.
    ab::action("chat.menu_pick", |args, _ctx| {
        let chat = args["chat"].as_str().unwrap_or("").to_string();
        let index = args["index"].as_i64().unwrap_or(0);
        let menu = ab::context()["data"]["chat_menu"][chat.clone()].clone();
        if chat.is_empty() || !menu.is_object() {
            return;
        }
        let selected = menu["selected"].as_i64().unwrap_or(0);
        let mut keys: Vec<String> = vec![];
        if menu["numbered"].as_bool().unwrap_or(false) {
            keys.push((index + 1).to_string());
        } else {
            let delta = index - selected;
            for _ in 0..delta.abs() {
                keys.push(if delta > 0 { "Down".into() } else { "Up".into() });
            }
            keys.push("Enter".into());
        }
        ab::run(json!({"do": "chat.keys", "chat": chat.clone(), "keys": keys}));
        // Forget the menu at once so the card doesn't linger until the next poll.
        let mut menus = ab::context()["data"]["chat_menu"].clone();
        if menus.is_object() {
            menus[chat] = Value::Null;
            ab::data("chat_menu", menus);
        }
        ab::after(1200, || poll_mode());
    });
    ab::action("claude.effort", |args, _ctx| {
        let level = args["level"].as_str().unwrap_or("high").to_string();
        let script = "import json,sys,os; p=os.path.expanduser('~/.claude/settings.json'); d=json.load(open(p)); d['effortLevel']=sys.argv[1]; json.dump(d,open(p,'w'),indent=2); open(p,'a').write('\\n')";
        ab::shell("python3 -c \"$PY\" \"$LEVEL\"", json!({"env": {"PY": script, "LEVEL": level.clone()}, "timeout": 10}), move |r, _err| {
            if r["code"].as_i64().unwrap_or(1) == 0 {
                ab::data("effort", json!(level));
                ab::notify(format!("Effort set to {level} for new turns"));
            } else {
                ab::notify(format!("Couldn't set effort: {}", r["stderr"].as_str().unwrap_or("")));
            }
        });
    });
    ab::on("start", |_, _| {
        read_effort();
        poll_mode();
        ab::every(2500, || poll_mode());
    });
    ab::on("select", |_, _| poll_mode());
}
