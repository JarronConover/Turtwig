/*! Wire protocol between `agent-boxd` and the Agent Box app: newline-delimited JSON over ssh.
Requests get a `Response` with the same `id`; the daemon pushes `Event`s at any time. */

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub call: Call,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    Hello { client: String },
    /** Receive `Event::Model` now and on every change, plus hook events after `from_seq`. */
    Subscribe { from_seq: Option<i64> },
    /** Transcript entries from line `from` on; also starts `Event::Transcript` pushes for the chat. */
    TranscriptRead { chat_id: String, from: usize },
    TranscriptUnwatch { chat_id: String },
    ChatNew {
        name: String,
        kind: String,
        branch: Option<String>,
        base: Option<String>,
        dir: Option<String>,
        repo: Option<String>,
        prompt: Option<String>,
        model: Option<String>,
    },
    ChatStart { chat_id: String },
    ChatStop { chat_id: String },
    ChatArchive { chat_id: String },
    /** Type text into the chat and submit it. */
    ChatSend { chat_id: String, text: String },
    /** tmux key names (`Up`, `Down`, `Enter`, `Escape`, `C-c`) for prompts and questions. */
    ChatKeys { chat_id: String, keys: Vec<String> },
    SectionCreate { name: String },
    SectionRename { id: String, name: String },
    SectionDelete { id: String },
    SectionAssign { path: String, section: Option<String> },
    WorktreeDelete { path: String },
    /** Put a worktree in `section` (None: back under its project) before `before` (a path), or last. */
    WorktreeMove { path: String, section: Option<String>, before: Option<String> },
    SectionMove { id: String, index: usize },
    ProjectMove { repo: String, index: usize },
    /** `{diff, base, from, truncated}`: unified diff text; `base` "uncommitted" (default) or "branch". */
    WorktreeDiff { path: String, base: Option<String> },
    /** Attach to a tmux session: a chat's, or the shell of `cwd` when `session` is None. */
    TermOpen { session: Option<String>, cwd: Option<String>, cols: u16, rows: u16 },
    TermInput { term_id: u64, data: String },
    TermResize { term_id: u64, cols: u16, rows: u16 },
    TermClose { term_id: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerMessage {
    Response(Response),
    Event(Event),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    /** The whole model; small enough to resend on every change. */
    Model(Model),
    /** A Claude Code hook fired; stored in the daemon's event log as `seq`. */
    Hook(HookEvent),
    Transcript { chat_id: String, from: usize, entries: Vec<Value> },
    /** Terminal output, base64. */
    TermOutput { term_id: u64, data: String },
    TermExit { term_id: u64 },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub host: String,
    pub ssh_host: String,
    pub projects: Vec<Project>,
    pub sections: Vec<Section>,
    pub chats: Vec<Chat>,
    pub last_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub name: String,
    pub repo: String,
    pub hue: f32,
    pub items: Vec<Worktree>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Section {
    pub id: String,
    pub name: String,
    pub collapsed: bool,
    pub items: Vec<Worktree>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Worktree {
    pub id: String,
    pub path: String,
    pub branch: Option<String>,
    pub title: String,
    pub project: String,
    pub repo: String,
    pub hue: f32,
    pub is_primary: bool,
    pub uncommitted: u32,
    pub unpushed: u32,
    pub behind: bool,
    pub mr: Option<MergeRequest>,
    /** Newest activity (a chat transcript or the last commit), unix seconds. */
    pub age: Option<f64>,
    pub chat_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeRequest {
    pub number: u64,
    pub url: String,
    /** opened | draft | merged | closed */
    pub state: String,
    /** passing | failing | pending */
    pub checks: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub host: String,
    pub status: String,
    pub dir: Option<String>,
    pub branch: Option<String>,
    pub rc_url: Option<String>,
    pub alive: bool,
    /** idle | working | waiting | stopped */
    pub state: String,
    /** The permission or question it's waiting on, from the last hook. */
    pub waiting_on: Option<Value>,
    pub last_activity: Option<f64>,
    pub tmux: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookEvent {
    pub seq: i64,
    pub at: f64,
    pub event: String,
    pub session_id: Option<String>,
    pub payload: Value,
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).expect("protocol messages serialize");
    s.push('\n');
    s
}
