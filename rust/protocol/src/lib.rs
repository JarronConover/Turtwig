/*! Wire protocol between `agent-boxd` and the a.box app: newline-delimited JSON over ssh.
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
    /** With `tail` (and `from` 0), only the last `tail` entries; `from` in the reply says where they start. */
    TranscriptRead {
        chat_id: String,
        from: usize,
        #[serde(default)]
        tail: Option<usize>,
        /** With `tail`: the `tail` lines before this line instead (an older page); the watch is left alone. */
        #[serde(default)]
        before: Option<usize>,
    },
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
        /** Which agent CLI runs it (a key of the daemon's `agents`); None: the default (claude). */
        #[serde(default)]
        agent: Option<String>,
        /** `name` is a placeholder: the chat takes its agent's own title (auto_title). */
        #[serde(default)]
        auto_name: bool,
    },
    ChatStart { chat_id: String },
    /** A new chat in the same folder that starts with a copy of this one's conversation; replies like ChatNew. */
    ChatFork { chat_id: String },
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
    /** The worktree's name in the sidebar (a leading emoji becomes its icon). */
    WorktreeLabel { path: String, label: String },
    WorktreeRenameBranch { path: String, branch: String },
    /** A worktree on `branch` (checked out if it exists here or on origin, else new from origin's default
    branch); replies `{path, branch}`. `WorktreeCreateWith` also takes a base and a folder name. */
    WorktreeCreate { repo: String, branch: String },
    /** Run a shell command on this host (bash -lc, in bin/agent-env): `{code, stdout, stderr}`. For scripts that
    add host-side abilities; off when the `exec_allowed` option is false. */
    Exec { cmd: String, cwd: Option<String>, timeout_secs: Option<u64> },
    ProjectMove { repo: String, index: usize },
    /** `{diff, base, from, truncated}`: unified diff text; `base` "uncommitted" (default) or "branch". */
    WorktreeDiff { path: String, base: Option<String> },
    /** Attach to a tmux session: a chat's, or the shell of `cwd` when `session` is None. */
    /** With `cwd`: `name` picks one of the folder's shells (none: the first), and `command` runs in a new
    one instead of a login shell. */
    TermOpen {
        session: Option<String>,
        cwd: Option<String>,
        cols: u16,
        rows: u16,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        command: Option<String>,
    },
    /** The app's `daemon` settings for this host's daemon and scripts (timings, limits, parsing); sent on
    connect and whenever settings change. `own` is what that app's user set themselves and `app` names the
    app, so several apps on one daemon merge instead of each replacing the others' (see config.rs). */
    Configure {
        options: Value,
        #[serde(default)]
        own: Option<Value>,
        #[serde(default)]
        app: Option<String>,
    },
    /** Scrolls a terminal's tmux history without tmux mouse mode: positive lines go back (entering copy
    mode), negative come forward; 0 leaves copy mode. */
    TermScroll { session: String, lines: i32 },
    /** Token usage of every Claude Code session on this machine (see the daemon's usage.rs). */
    Usage,
    /** Ends a terminal's tmux session (closing a terminal tab). */
    TermKill { session: String },
    /** The Mac app's view of itself for agents on this machine: a file of the settings folder
    (inspect.json, activity.log, problems.txt, snapshot.png), base64. */
    AppFile { name: String, data: String },
    TermInput { term_id: u64, data: String },
    TermResize { term_id: u64, cols: u16, rows: u16 },
    TermClose { term_id: u64 },
    /** Past chats, newest first: `{chats: [{id, name, dir, host, agent, kind, branch, last_activity, closed}]}`.
    `ChatStart` reopens any of them. */
    History { limit: Option<usize> },
    /** `History` for `host` (None: this one; "*": every host in the shared registry). */
    HistoryQuery {
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        host: Option<String>,
    },
    /** `WorktreeCreate` from `origin/<base>` (None: the default branch), in a folder named `name` where the repo's
    worktree template uses one. */
    WorktreeCreateWith {
        repo: String,
        branch: String,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    /** The options in effect: `{options, agents, resolved: {host_id, host, hosts, repos, ...}, host_id}`. */
    ConfigGet,
    /** Hook events from the log, oldest first: after `from_seq`, at or after `since` (unix seconds), of one
    session; at most `limit` (default `hook_events_limit`). `{events, last_seq}`. */
    HookEvents {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        from_seq: Option<i64>,
        #[serde(default)]
        since: Option<f64>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /** Snapshot every queued worktree and push this host's sessions branch now (bin/sync-worker, detached). */
    SyncNow,
    /** WIP snapshots on every host's sessions branch (one worktree's with `path`): `{wips: [{host, key, path,
    branch, head, at}]}`. */
    WipList {
        #[serde(default)]
        path: Option<String>,
    },
    /** Apply `from`'s (None: the other host's) latest WIP for the worktree's branch (bin/wip-restore). */
    WipRestore {
        path: String,
        #[serde(default)]
        from: Option<String>,
    },
    /** The last `lines` lines of a log named in the `logs` option (boxd, agent-box, supervisor, boxd-build, ...):
    `{name, path, text}`. */
    LogTail {
        name: String,
        #[serde(default)]
        lines: Option<usize>,
    },
    ChatRename { chat_id: String, name: String },
    /** Terminal shells (tmux sessions `sh-*`), of one folder with `cwd`: `{terms: [{session, name, cwd, attached,
    created, command}]}`. */
    TermList {
        #[serde(default)]
        cwd: Option<String>,
    },
    /** The branch a worktree targets (its merge request's target, the diff base). */
    WorktreeSetTarget { path: String, branch: String },
    SectionCollapse { id: String, collapsed: bool },
    /** Move a chat (with its worktree or whole section, `scope`) to host `to`; this host runs bin/chat move, so
    it must reach the other end over ssh (an error says when it can't). */
    ChatMove {
        chat_id: String,
        to: String,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        dry_run: bool,
    },
    /** Check a chat's branch (or `branch` of `repo`) out on this host with its latest WIP from its host (or
    `from`), for manual testing. */
    TestLocal {
        #[serde(default)]
        chat_id: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        from: Option<String>,
    },
    /** Start the chat branch's stack here (its repo's worktree_tool `start`), detached in tmux. */
    StackStart { chat_id: String },
    /** (Re)install the daemon's hooks for an agent CLI (bin/agent-hooks). */
    HooksInstall { agent: String },
    /** Copy the personal agent setup to `host` (None: every other host with ssh) and back (bin/env-push). */
    EnvPush {
        #[serde(default)]
        host: Option<String>,
    },
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
    /** An action an agent on this machine asked the app to run (its settings folder's run.json). */
    AppAction { action: Value },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub host: String,
    pub ssh_host: String,
    pub projects: Vec<Project>,
    pub sections: Vec<Section>,
    pub chats: Vec<Chat>,
    pub last_seq: i64,
    /** The host's display name (`hosts.HOST.label`). */
    #[serde(default)]
    pub host_label: Option<String>,
    /** This host's transcript and WIP backup (bin/sync-worker). */
    #[serde(default)]
    pub sync: Option<SyncStatus>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncStatus {
    /** The last push failed and waits for the next turn. */
    pub pending: bool,
    /** Worktrees waiting to be snapshotted. */
    pub queued: u32,
    pub last_push_at: Option<String>,
    pub error: Option<String>,
    /** The sessions branch it pushes. */
    #[serde(default)]
    pub branch: Option<String>,
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
    /** The machine it's on: a host id (the daemon's `hosts`; originally "box" or "mac"). */
    #[serde(default = "default_host")]
    pub host: String,
    /** Made outside a.box and super.engineering (by hand with `git worktree add`, or another tool). */
    #[serde(default)]
    pub external: bool,
    /** The branch it targets (`WorktreeSetTarget`), when one was set. */
    #[serde(default)]
    pub target_branch: Option<String>,
    /** Commits its upstream has that it doesn't. */
    #[serde(default)]
    pub behind_count: u32,
    /** Its upstream branch (`origin/x`). */
    #[serde(default)]
    pub upstream: Option<String>,
}

fn default_host() -> String {
    "box".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeRequest {
    pub number: u64,
    pub url: String,
    /** opened | draft | merged | closed */
    pub state: String,
    /** passing | failing | pending */
    pub checks: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub pipeline_url: Option<String>,
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
    /** Claude Code's prompt suggestion (the dim text in its empty input box) while idle. */
    #[serde(default)]
    pub suggestion: Option<String>,
    /** The agent CLI it runs: a key of the daemon's `agents` (claude, codex). */
    #[serde(default = "default_agent")]
    pub agent: String,
    /** The agent's own session id when it picks one (Codex); usage is filed under it. */
    #[serde(default)]
    pub session: Option<String>,
    /** The rest of its registry entry. */
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /** The orchestrator chat that started it. */
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub feature: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub forked_from: Option<String>,
    /** When its agent last started (unix seconds). */
    #[serde(default)]
    pub launched_at: Option<f64>,
    /** Its transcript file on this host. */
    #[serde(default)]
    pub transcript: Option<String>,
    /** What the agent's own status line says while it works, read from its pane. */
    #[serde(default)]
    pub status_line: Option<Activity>,
    /** Its agent shows a screen of its own (a menu like Claude Code's /artifacts) in place of its input box. */
    #[serde(default)]
    pub screen: bool,
}

/** A working agent's status line, like Claude Code's "✻ Concocting… (4m 17s · ↓ 6.5k tokens)". */
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Activity {
    /** "Concocting" */
    pub verb: String,
    /** When the turn started, from the time it shows (unix seconds). */
    pub since: Option<f64>,
    /** "↓ 6.5k tokens" */
    pub tokens: Option<String>,
    /** When `tokens` last changed (or the line first showed): how long the agent has sent nothing. */
    pub changed_at: f64,
    /** The line's other parts ("thinking with high effort"). */
    pub notes: Vec<String>,
}

fn default_agent() -> String {
    "claude".into()
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
