/*! Pure rules behind settings-driven behavior: which daemons to connect to, which chat state changes
are which events, which relayed actions may run, how agent requests and their results are shaped. */

use serde_json::{json, Value};

/** One daemon connection: `ssh_host` empty runs `daemon` here instead of over ssh. */
#[derive(Debug, Clone, PartialEq)]
pub struct HostSpec {
    pub name: String,
    pub ssh_host: String,
    pub daemon: String,
    pub ssh_options: Vec<String>,
    pub retry_secs: u64,
    pub watch_ms: u64,
}

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

/** `app.local_host` / `app.default_host` from merged settings. */
pub fn host_name(raw: &Value, key: &str, default: &str) -> String {
    raw.pointer(&format!("/app/{key}")).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(default).to_string()
}

/** Every daemon to connect to: each `hosts.<name>.connection`, plus `connection` as the default host and
`local` as the local host when those have no `connection` of their own. */
pub fn host_specs(raw: &Value) -> Vec<HostSpec> {
    let default_host = host_name(raw, "default_host", "box");
    let local_host = host_name(raw, "local_host", "mac");
    let base = &raw["connection"];
    let base_options = strings(base.get("ssh_options")).unwrap_or_else(|| ["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"].map(String::from).to_vec());
    let base_retry = base.get("retry_seconds").and_then(Value::as_u64).unwrap_or(3);
    let base_watch = base.get("watch_ms").and_then(Value::as_u64).unwrap_or(500);
    let spec = |name: &str, c: &Value| -> Option<HostSpec> {
        if c.get("enabled").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let daemon = c.get("daemon").and_then(Value::as_str).filter(|d| !d.is_empty())?.to_string();
        Some(HostSpec {
            name: name.to_string(),
            ssh_host: c.get("ssh_host").and_then(Value::as_str).unwrap_or("").to_string(),
            daemon,
            ssh_options: strings(c.get("ssh_options")).unwrap_or_else(|| base_options.clone()),
            retry_secs: c.get("retry_seconds").and_then(Value::as_u64).unwrap_or(base_retry),
            watch_ms: c.get("watch_ms").and_then(Value::as_u64).unwrap_or(base_watch),
        })
    };
    let mut out: Vec<HostSpec> = vec![];
    if let Some(hosts) = raw.get("hosts").and_then(Value::as_object) {
        for (name, h) in hosts {
            if let Some(s) = h.get("connection").filter(|c| c.is_object()).and_then(|c| spec(name, c)) {
                out.push(s);
            }
        }
    }
    let has_own = |name: &str| raw.pointer(&format!("/hosts/{name}/connection")).is_some_and(Value::is_object);
    if !has_own(&local_host) && raw["local"]["enabled"].as_bool() == Some(true) {
        let local = json!({"daemon": raw["local"]["daemon"], "ssh_host": ""});
        if let Some(s) = spec(&local_host, &local) {
            out.insert(0, s);
        }
    }
    if !has_own(&default_host) && base.is_object() {
        if let Some(s) = spec(&default_host, base) {
            out.push(s);
        }
    }
    out
}

/** Event names for a chat going from `from` to `to`, by `events_options.transitions`:
`[{from: state | [states] | "*", to: state, event}]` (None: the built-in rules). */
pub fn transition_events(rules: Option<&Value>, from: &str, to: &str) -> Vec<String> {
    let builtin = json!([
        {"from": "*", "to": "waiting", "event": "chat.waiting"},
        {"from": "working", "to": "idle", "event": "chat.finished"},
        {"from": ["working", "waiting", "idle"], "to": "stopped", "event": "chat.stopped"},
    ]);
    let rules = rules.filter(|r| r.is_array()).unwrap_or(&builtin);
    if from == to {
        return vec![];
    }
    let matches = |pat: &Value, state: &str| match pat {
        Value::String(s) => s == "*" || s == state,
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(state) || x.as_str() == Some("*")),
        Value::Null => true,
        _ => false,
    };
    rules
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| matches(r.get("from").unwrap_or(&Value::Null), from) && matches(r.get("to").unwrap_or(&Value::Null), to))
        .filter_map(|r| r.get("event").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/** An event's handlers: one `{if, do}` object or a list of them. */
pub fn handlers(spec: &Value) -> Vec<Value> {
    match spec {
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        other => vec![other.clone()],
    }
}

/** A handler's `if` and action: `{if?, do, id?}`; anything else in a list (a name, a list, an action
object with its own arguments) is the action itself. */
pub fn handler_parts(h: &Value) -> (Option<&str>, &Value) {
    match h.as_object() {
        Some(o) if o.contains_key("do") && o.keys().all(|k| k == "if" || k == "do" || k == "id") => (o.get("if").and_then(Value::as_str), &o["do"]),
        _ => (None, h),
    }
}

/** Every action name an action runs, including nested `then`, `else`, and lists. */
pub fn action_names(v: &Value) -> Vec<String> {
    let mut out = vec![];
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => {
                match o.get("do") {
                    Some(Value::String(s)) => out.push(s.clone()),
                    Some(other) => walk(other, out),
                    None => {}
                }
                for k in ["then", "else"] {
                    if let Some(x) = o.get(k) {
                        walk(x, out);
                    }
                }
            }
            _ => {}
        }
    }
    walk(v, &mut out);
    out
}

/** Whether a relayed action may run, by `inspect.remote_actions`: "all" (default), "none", or a list of
action names (every action it would run must be listed). */
pub fn remote_allowed(policy: Option<&Value>, action: &Value) -> Result<(), String> {
    match policy {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(s)) if s == "all" => Ok(()),
        Some(Value::String(s)) if s == "none" => Err("inspect.remote_actions is \"none\"".into()),
        Some(Value::Array(allowed)) => {
            let ok = |n: &str| allowed.iter().any(|a| a.as_str() == Some(n));
            match action_names(action).into_iter().find(|n| !ok(n)) {
                Some(n) => Err(format!("`{n}` is not in inspect.remote_actions")),
                None => Ok(()),
            }
        }
        Some(other) => Err(format!("inspect.remote_actions: unknown value {other}")),
    }
}

/** An agent's request: a plain action, or `{"id", "action", "read"?}` (`read`: a jq expression whose value
goes in the result). A file in `run.d` names its request by its file name. */
#[derive(Debug, PartialEq)]
pub struct Request {
    pub id: Option<String>,
    pub action: Value,
    pub read: Option<String>,
}

pub fn parse_request(file_id: Option<&str>, v: Value) -> Request {
    let envelope = v.as_object().filter(|o| o.contains_key("action") && !o.contains_key("do"));
    match envelope {
        Some(o) => Request {
            id: o.get("id").map(|i| i.as_str().map(str::to_string).unwrap_or_else(|| i.to_string())).or(file_id.map(str::to_string)),
            action: o.get("action").cloned().unwrap_or(Value::Null),
            read: o.get("read").and_then(Value::as_str).map(str::to_string),
        },
        None => Request { id: file_id.map(str::to_string), action: v, read: None },
    }
}

/** A request id safe to use as a file name. */
pub fn safe_id(id: &str) -> Option<String> {
    let ok = !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && !id.starts_with('.');
    ok.then(|| id.to_string())
}

/** `PATH` from `app.path`, with `~/` expanded. */
pub fn path_env(list: &[String], home: &str) -> String {
    list.iter().map(|p| match p.strip_prefix("~/") {
        Some(rest) => format!("{home}/{rest}"),
        None => p.clone(),
    }).collect::<Vec<_>>().join(":")
}

/** The keys a chat's agent takes for `kind` (`interrupt_keys`, `cancel_keys`), from `daemon.agents.<agent>`. */
pub fn agent_keys(agents: &Value, agent: &str, kind: &str) -> Vec<String> {
    strings(agents.get(agent).and_then(|a| a.get(kind))).filter(|k| !k.is_empty()).unwrap_or_else(|| vec!["Escape".into()])
}

/** The key that picks option `n` (1-based) of a waiting chat's question: `daemon.agents.<agent>.answer_key` ("{n}"). */
pub fn answer_key(agents: &Value, agent: &str, n: usize) -> String {
    agents.get(agent).and_then(|a| a.get("answer_key")).and_then(Value::as_str).unwrap_or("{n}").replace("{n}", &n.to_string())
}

/** Moving through sent messages: `cur` None is the composer's own text; `by` -1 goes to older ones.
Returns the index counted from the newest (0), or None for back at the composer. */
pub fn recall_step(count: usize, cur: Option<usize>, by: i64) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let at: i64 = cur.map(|c| c as i64).unwrap_or(-1) - by;
    if at < 0 {
        None
    } else {
        Some((at as usize).min(count - 1))
    }
}

/** Quotes a value for a POSIX shell. */
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Value {
        serde_json_lenient::from_str(crate::settings::DEFAULTS).expect("defaults parse")
    }

    #[test]
    fn the_defaults_connect_the_box_over_ssh_and_this_mac_locally() {
        let specs = host_specs(&defaults());
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["mac", "box"]);
        assert_eq!(specs[0].ssh_host, "");
        assert_eq!(specs[1].ssh_host, "coder.tj-agents");
        assert_eq!(specs[1].retry_secs, 3);
        assert!(specs[1].ssh_options.contains(&"ServerAliveInterval=15".to_string()));
    }

    #[test]
    fn a_hosts_entry_adds_a_connection_and_overrides_the_default_one() {
        let mut v = defaults();
        v["hosts"]["gpu"] = json!({"label": "GPU", "connection": {"ssh_host": "gpu.example", "daemon": "~/agent-boxd"}});
        v["hosts"]["box"]["connection"] = json!({"ssh_host": "other", "daemon": "d", "retry_seconds": 9});
        v["local"]["enabled"] = json!(false);
        let specs = host_specs(&v);
        let box_ = specs.iter().find(|s| s.name == "box").unwrap();
        assert_eq!((box_.ssh_host.as_str(), box_.retry_secs), ("other", 9));
        assert!(specs.iter().any(|s| s.name == "gpu" && s.ssh_host == "gpu.example"));
        assert!(!specs.iter().any(|s| s.name == "mac"));
        assert_eq!(specs.iter().filter(|s| s.name == "box").count(), 1);
    }

    #[test]
    fn transitions_follow_the_rules() {
        assert_eq!(transition_events(None, "working", "idle"), ["chat.finished"]);
        assert_eq!(transition_events(None, "idle", "waiting"), ["chat.waiting"]);
        assert!(transition_events(None, "waiting", "waiting").is_empty());
        assert!(transition_events(None, "waiting", "idle").is_empty());
        assert_eq!(transition_events(None, "idle", "stopped"), ["chat.stopped"]);
        let rules = json!([{"from": ["working", "waiting"], "to": "idle", "event": "chat.finished"}]);
        assert_eq!(transition_events(Some(&rules), "waiting", "idle"), ["chat.finished"]);
    }

    #[test]
    fn handlers_are_if_do_objects_or_bare_actions() {
        let h = json!({"if": ".x", "do": {"do": "notify", "text": "hi"}});
        assert_eq!(handler_parts(&h), (Some(".x"), &json!({"do": "notify", "text": "hi"})));
        let bare = json!({"do": "notify", "text": "hi"});
        assert_eq!(handler_parts(&bare), (None, &bare));
        assert_eq!(handler_parts(&json!("nav.back")), (None, &json!("nav.back")));
        assert_eq!(handlers(&json!({"do": "a"})).len(), 1);
        assert_eq!(handlers(&json!([{"do": "a"}, "b"])).len(), 2);
    }

    #[test]
    fn relayed_actions_are_checked_through_nested_thens() {
        let a = json!([{"do": "if", "cond": true, "then": {"do": "shell", "run": "x"}}, "nav.back"]);
        assert_eq!(action_names(&a), ["if", "shell", "nav.back"]);
        assert!(remote_allowed(None, &a).is_ok());
        assert!(remote_allowed(Some(&json!("none")), &a).is_err());
        assert!(remote_allowed(Some(&json!(["if", "nav.back"])), &a).unwrap_err().contains("shell"));
        assert!(remote_allowed(Some(&json!(["if", "nav.back", "shell"])), &a).is_ok());
    }

    #[test]
    fn requests_are_plain_actions_or_envelopes() {
        assert_eq!(parse_request(Some("r1"), json!({"do": "section.rename", "id": "s"})), Request { id: Some("r1".into()), action: json!({"do": "section.rename", "id": "s"}), read: None });
        let r = parse_request(None, json!({"id": "x", "action": "nav.back", "read": ".app.nav"}));
        assert_eq!((r.id.as_deref(), r.read.as_deref()), (Some("x"), Some(".app.nav")));
        assert_eq!(safe_id("ab-1.2_c").as_deref(), Some("ab-1.2_c"));
        assert!(safe_id("../x").is_none());
        assert!(safe_id(".hidden").is_none());
    }

    #[test]
    fn keys_paths_and_recall() {
        let agents = json!({"codex": {"interrupt_keys": ["C-c"], "answer_key": "{n}\n"}});
        assert_eq!(agent_keys(&agents, "codex", "interrupt_keys"), ["C-c"]);
        assert_eq!(agent_keys(&agents, "claude", "interrupt_keys"), ["Escape"]);
        assert_eq!(answer_key(&agents, "codex", 2), "2\n");
        assert_eq!(answer_key(&agents, "claude", 3), "3");
        assert_eq!(path_env(&["~/.local/bin".into(), "/bin".into()], "/h"), "/h/.local/bin:/bin");
        assert_eq!(recall_step(3, None, -1), Some(0));
        assert_eq!(recall_step(3, Some(0), -1), Some(1));
        assert_eq!(recall_step(3, Some(2), -1), Some(2));
        assert_eq!(recall_step(3, Some(0), 1), None);
        assert_eq!(recall_step(0, None, -1), None);
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }
}
