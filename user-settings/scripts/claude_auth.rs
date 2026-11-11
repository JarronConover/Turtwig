// Claude sign-in watcher. Publishes `.data.claude_auth`: { ok, expired, email, checked }.
// Expired when `claude auth status` says signed out, or a chat reports "OAuth session expired" /
// "Failed to authenticate". Checked every 20 min, on start, and right after such a message.
// Action `claude.relogin` opens a terminal tab running `claude auth login`; `claude.auth_check` rechecks.

fn set(expired: bool, email: Option<String>) {
    let mut v = ab::context()["data"]["claude_auth"].clone();
    if !v.is_object() {
        v = json!({});
    }
    let was = v["expired"].as_bool().unwrap_or(false);
    v["expired"] = json!(expired);
    v["ok"] = json!(!expired);
    if let Some(e) = email {
        v["email"] = json!(e);
    }
    v["checked"] = ab::context()["now"].clone();
    ab::data("claude_auth", v);
    if expired && !was {
        ab::run(json!({ "do": "notify.system", "title": "Claude sign-in expired",
            "text": "Click “Sign in again” in Turtwig, or run: claude auth login", "sound": true,
            "on_click": "claude.relogin" }));
    }
}

fn check() {
    ab::shell("claude auth status 2>&1", json!({ "timeout": 30 }), |r, _err| {
        let out = r["stdout"].as_str().unwrap_or("").to_string();
        let v = out.parse::<Value>().unwrap_or(Value::Null);
        if v.is_object() {
            let logged = v["loggedIn"].as_bool().unwrap_or(false);
            set(!logged, v["email"].as_str().map(|s| s.to_string()));
        } else {
            let low = out.to_lowercase();
            if low.contains("expired") || low.contains("not logged in") || low.contains("log in") {
                set(true, None);
            }
        }
    });
}

fn is_auth_error(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("oauth session expired") || t.contains("failed to authenticate") || t.contains("please run /login")
        || t.contains("invalid api key") || t.contains("oauth token has expired")
}

fn init() {
    ab::action("claude.auth_check", |_, _| check());
    ab::action("claude.relogin", |_, _| {
        ab::run(json!({ "do": "terminal.new", "label": "Claude sign-in", "command": "claude auth login; echo; echo 'Signed in. You can close this tab.'" }));
        ab::run(json!({ "do": "state.set", "key": "terminal_open", "value": true }));
        // Recheck a few times while the user signs in.
        for ms in [20_000u64, 45_000, 90_000, 180_000] {
            ab::after(ms, || check());
        }
    });
    ab::action("claude.auth_dismiss", |_, _| {
        let mut v = ab::context()["data"]["claude_auth"].clone();
        if v.is_object() {
            v["dismissed"] = ab::context()["now"].clone();
            ab::data("claude_auth", v);
        }
    });
    ab::on("chat.message", |p, _| {
        if is_auth_error(p["message"]["text"].as_str().unwrap_or("")) {
            set(true, None);
            check();
        }
    });
    ab::on("start", |_, _| {
        check();
        ab::every(20 * 60_000, || check());
    });
}
