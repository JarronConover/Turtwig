use agent_box_protocol::{HookEvent, InboxItem};
use rusqlite::{params, Connection, OptionalExtension};
use std::io::Read;

use crate::util;

pub fn open() -> rusqlite::Result<Connection> {
    let _ = std::fs::create_dir_all(util::agent_home());
    let conn = Connection::open(util::db_path())?;
    conn.busy_timeout(crate::config::ms("db_busy_ms", 3000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
            seq INTEGER PRIMARY KEY, at REAL NOT NULL, event TEXT NOT NULL,
            session_id TEXT, payload TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS events_session ON events(session_id, seq);",
    )?;
    init_inbox(&conn)?;
    Ok(conn)
}

pub fn init_inbox(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS inbox (
            id INTEGER PRIMARY KEY, key TEXT UNIQUE, at REAL NOT NULL, updated_at REAL NOT NULL,
            rev INTEGER NOT NULL DEFAULT 1, source TEXT, title TEXT NOT NULL, text TEXT NOT NULL DEFAULT '',
            chat TEXT, read_at REAL);
         CREATE INDEX IF NOT EXISTS inbox_unread ON inbox(read_at, updated_at);",
    )
}

pub struct Post<'a> {
    pub title: &'a str,
    pub text: &'a str,
    pub chat: Option<&'a str>,
    pub source: Option<&'a str>,
    pub key: Option<&'a str>,
    pub force: bool,
}

/** Read items older than this are deleted on the next post. */
const INBOX_KEEP_SECS: f64 = 30.0 * 86400.0;

/** Add an item, or update the one with the same key. An update that changes the title or text, or a forced one,
makes it unread again (rev + 1); one that changes neither leaves it as it was. Returns (id, changed). */
pub fn inbox_post(conn: &Connection, p: &Post, now: f64) -> rusqlite::Result<(i64, bool)> {
    let _ = conn.execute("DELETE FROM inbox WHERE read_at IS NOT NULL AND read_at < ?1", params![now - INBOX_KEEP_SECS]);
    if let Some(key) = p.key {
        let old: Option<(i64, String, String)> = conn
            .query_row("SELECT id, title, text FROM inbox WHERE key = ?1", params![key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        if let Some((id, title, text)) = old {
            if !p.force && title == p.title && text == p.text {
                conn.execute("UPDATE inbox SET chat = COALESCE(?2, chat), source = COALESCE(?3, source) WHERE id = ?1",
                             params![id, p.chat, p.source])?;
                return Ok((id, false));
            }
            conn.execute(
                "UPDATE inbox SET title = ?2, text = ?3, chat = COALESCE(?4, chat), source = COALESCE(?5, source),
                 updated_at = ?6, rev = rev + 1, read_at = NULL WHERE id = ?1",
                params![id, p.title, p.text, p.chat, p.source, now],
            )?;
            return Ok((id, true));
        }
    }
    conn.execute(
        "INSERT INTO inbox (key, at, updated_at, source, title, text, chat) VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)",
        params![p.key, now, p.source, p.title, p.text, p.chat],
    )?;
    Ok((conn.last_insert_rowid(), true))
}

/** Unread items, newest first. */
pub fn inbox_unread(conn: &Connection) -> Vec<InboxItem> {
    let mut st = match conn.prepare(
        "SELECT id, key, at, updated_at, rev, source, title, text, chat FROM inbox WHERE read_at IS NULL
         ORDER BY updated_at DESC, id DESC LIMIT 500",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    st.query_map([], |r| {
        Ok(InboxItem {
            id: r.get(0)?,
            key: r.get(1)?,
            at: r.get(2)?,
            updated_at: r.get(3)?,
            rev: r.get(4)?,
            source: r.get(5)?,
            title: r.get(6)?,
            text: r.get(7)?,
            chat: r.get(8)?,
        })
    })
    .map(|it| it.flatten().collect())
    .unwrap_or_default()
}

/** Mark read: these ids, every item of `chat`, or all of them. Returns how many rows changed. */
pub fn inbox_read(conn: &Connection, ids: &[i64], chat: Option<&str>, key: Option<&str>, all: bool, now: f64) -> usize {
    let mut n = 0;
    if all {
        return conn.execute("UPDATE inbox SET read_at = ?1 WHERE read_at IS NULL", params![now]).unwrap_or(0);
    }
    for id in ids {
        n += conn.execute("UPDATE inbox SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL", params![id, now]).unwrap_or(0);
    }
    if let Some(c) = chat {
        n += conn.execute("UPDATE inbox SET read_at = ?2 WHERE chat = ?1 AND read_at IS NULL", params![c, now]).unwrap_or(0);
    }
    if let Some(k) = key {
        n += conn.execute("UPDATE inbox SET read_at = ?2 WHERE key = ?1 AND read_at IS NULL", params![k, now]).unwrap_or(0);
    }
    n
}

/** `agent-boxd inbox post|list|read`: this host's inbox, straight from the database (no daemon needed).
post reads `{title, text, chat, source, key, force}` on stdin and prints `{id, changed}`; list prints the unread
items; read takes ids, `--chat C`, `--key K`, or `--all`. */
pub fn inbox_cli(args: &[String]) -> i32 {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("inbox: {e}");
            return 1;
        }
    };
    match args.first().map(String::as_str) {
        Some("post") => {
            let mut input = String::new();
            let _ = std::io::stdin().read_to_string(&mut input);
            let v: serde_json::Value = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);
            let s = |k: &str| v.get(k).and_then(|x| x.as_str());
            let Some(title) = s("title").filter(|t| !t.is_empty()) else {
                eprintln!("inbox post: stdin needs {{\"title\": ...}}");
                return 2;
            };
            let post = Post { title, text: s("text").unwrap_or(""), chat: s("chat"), source: s("source"), key: s("key"),
                              force: v.get("force").and_then(|x| x.as_bool()).unwrap_or(false) };
            match inbox_post(&conn, &post, util::now()) {
                Ok((id, changed)) => {
                    println!("{}", serde_json::json!({"id": id, "changed": changed}));
                    0
                }
                Err(e) => {
                    eprintln!("inbox post: {e}");
                    1
                }
            }
        }
        Some("list") => {
            println!("{}", serde_json::to_string(&inbox_unread(&conn)).unwrap_or_else(|_| "[]".into()));
            0
        }
        Some("read") => {
            let (mut ids, mut chat, mut key, mut all) = (vec![], None, None, false);
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--all" => all = true,
                    "--chat" => chat = it.next().cloned(),
                    "--key" => key = it.next().cloned(),
                    x => ids.extend(x.parse::<i64>().ok()),
                }
            }
            println!("{}", serde_json::json!({"read": inbox_read(&conn, &ids, chat.as_deref(), key.as_deref(), all, util::now())}));
            0
        }
        _ => {
            eprintln!("usage: agent-boxd inbox post (JSON on stdin) | list | read [IDS...] [--chat C] [--key K] [--all]");
            2
        }
    }
}

/** `agent-boxd hook`: append the hook's JSON to the event log. Prints nothing, never fails. */
pub fn hook() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let payload: serde_json::Value = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);
    let event = payload.get("hook_event_name").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
    let session = payload.get("session_id").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Ok(conn) = open() {
        let _ = conn.execute(
            "INSERT INTO events (at, event, session_id, payload) VALUES (?1, ?2, ?3, ?4)",
            params![util::now(), event, session, payload.to_string()],
        );
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<HookEvent> {
    let payload: String = r.get(4)?;
    Ok(HookEvent {
        seq: r.get(0)?,
        at: r.get(1)?,
        event: r.get(2)?,
        session_id: r.get(3)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
    })
}

pub fn since(conn: &Connection, seq: i64, limit: i64) -> Vec<HookEvent> {
    let mut st = match conn.prepare(
        "SELECT seq, at, event, session_id, payload FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    st.query_map(params![seq, limit], row).map(|it| it.flatten().collect()).unwrap_or_default()
}

/** Events after `from_seq`, at or after `since`, of one session, oldest first. */
pub fn query(conn: &Connection, session: Option<&str>, from_seq: Option<i64>, since: Option<f64>, limit: i64) -> Vec<HookEvent> {
    let mut st = match conn.prepare(
        "SELECT seq, at, event, session_id, payload FROM events
         WHERE seq > ?1 AND at >= ?2 AND (?3 IS NULL OR session_id = ?3) ORDER BY seq LIMIT ?4",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    st.query_map(params![from_seq.unwrap_or(0), since.unwrap_or(0.0), session, limit], row)
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/** The most recent event of each session, oldest first. */
pub fn latest_per_session(conn: &Connection) -> Vec<HookEvent> {
    let mut st = match conn.prepare(
        "SELECT seq, at, event, session_id, payload FROM events
         WHERE seq IN (SELECT MAX(seq) FROM events WHERE session_id IS NOT NULL
                       AND event NOT IN ('SubagentStop', 'SessionStart') GROUP BY session_id)
         ORDER BY seq",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    st.query_map([], row).map(|it| it.flatten().collect()).unwrap_or_default()
}

pub fn max_seq(conn: &Connection) -> i64 {
    conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_inbox(&c).unwrap();
        c
    }

    fn post<'a>(title: &'a str, text: &'a str, key: Option<&'a str>, force: bool) -> Post<'a> {
        Post { title, text, chat: Some("c1"), source: Some("test"), key, force }
    }

    #[test]
    fn same_key_same_content_stays_read() {
        let c = db();
        let (id, changed) = inbox_post(&c, &post("A", "x", Some("k"), false), 1.0).unwrap();
        assert!(changed);
        assert_eq!(inbox_read(&c, &[id], None, None, false, 2.0), 1);
        assert_eq!(inbox_post(&c, &post("A", "x", Some("k"), false), 3.0).unwrap(), (id, false));
        assert!(inbox_unread(&c).is_empty());
    }

    #[test]
    fn changed_or_forced_post_is_unread_again() {
        let c = db();
        let (id, _) = inbox_post(&c, &post("A", "x", Some("k"), false), 1.0).unwrap();
        inbox_read(&c, &[], Some("c1"), None, false, 2.0);
        assert_eq!(inbox_post(&c, &post("A", "y", Some("k"), false), 3.0).unwrap(), (id, true));
        let u = inbox_unread(&c);
        assert_eq!((u.len(), u[0].rev, u[0].text.as_str(), u[0].at, u[0].updated_at), (1, 2, "y", 1.0, 3.0));
        inbox_read(&c, &[], None, Some("k"), false, 4.0);
        assert_eq!(inbox_post(&c, &post("A", "y", Some("k"), true), 5.0).unwrap(), (id, true));
        assert_eq!(inbox_unread(&c)[0].rev, 3);
    }

    #[test]
    fn unkeyed_posts_are_separate_and_old_read_items_go() {
        let c = db();
        inbox_post(&c, &post("A", "", None, false), 1.0).unwrap();
        inbox_post(&c, &post("B", "", None, false), 2.0).unwrap();
        assert_eq!(inbox_unread(&c).iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["B", "A"]);
        assert_eq!(inbox_read(&c, &[], None, None, true, 3.0), 2);
        inbox_post(&c, &post("C", "", None, false), 3.0 + INBOX_KEEP_SECS + 1.0).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM inbox", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
