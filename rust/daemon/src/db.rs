use agent_box_protocol::HookEvent;
use rusqlite::{params, Connection};
use std::io::Read;
use std::time::Duration;

use crate::util;

pub fn open() -> rusqlite::Result<Connection> {
    let _ = std::fs::create_dir_all(util::agent_home());
    let conn = Connection::open(util::db_path())?;
    conn.busy_timeout(Duration::from_secs(3))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
            seq INTEGER PRIMARY KEY, at REAL NOT NULL, event TEXT NOT NULL,
            session_id TEXT, payload TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS events_session ON events(session_id, seq);",
    )?;
    Ok(conn)
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
