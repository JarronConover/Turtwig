# Sourced by every agent-box script.

AGENT_BOX_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Host id, branches, git identity and the remote's ssh name, from the app's settings (bin/abxcfg.py).
eval "$(python3 "$AGENT_BOX_DIR/bin/abxcfg.py" shell 2> /dev/null)"
if [ -z "${AGENT_HOST:-}" ]; then
  if [ -n "${CODER_AGENT_URL:-}" ]; then AGENT_HOST=box AGENT_SYNC_TRANSCRIPTS=all; else AGENT_HOST=mac; fi
  SESSIONS_BRANCH="sessions/$AGENT_HOST" STATE_BRANCH=state
  AGENT_BOX_REPO_URL="${AGENT_BOX_REPO_URL:-}"
fi

AGENT_HOME="$HOME/.agent-box"
AGENT_STATE_DIR="$AGENT_HOME/state"
AGENT_SESSIONS_DIR="$AGENT_HOME/sessions"
AGENT_QUEUE_DIR="$AGENT_HOME/queue"
AGENT_LOG="$AGENT_HOME/log/agent-box.log"
CLAUDE_PROJECTS_DIR="$HOME/.claude/projects"

# A resolved value (JSON) from the app's settings: cfg KEY [HOST].
cfg() { python3 "$AGENT_BOX_DIR/bin/abxcfg.py" get "$@" 2> /dev/null; }
# A plain option from config.json: opt KEY.
opt() { jq -r --arg k "$1" '.[$k] // empty' "$AGENT_HOME/config.json" 2> /dev/null; }

log() {
  mkdir -p "$(dirname "$AGENT_LOG")"
  printf '%s [%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(basename "$0")" "$*" >> "$AGENT_LOG"
}

# Run a command detached from the hook's process group; setsid is missing on macOS.
detach() {
  if command -v setsid > /dev/null; then
    nohup setsid "$@" > /dev/null 2>&1 < /dev/null &
  else
    nohup "$@" > /dev/null 2>&1 < /dev/null &
  fi
}

NO_BACKUP="no backup repo: set daemon.state_repo_url in settings to a private git repo to back up chats and move them between machines"

# Clone one branch of the state repo into $1 (or point an existing clone at it), or start
# it as an orphan when the branch doesn't exist on the remote yet. With no state repo the
# clone is local only, and an origin left from before is dropped.
ensure_branch_clone() {
  local dir=$1 branch=$2 url
  if [ -d "$dir/.git" ]; then
    url=$(git -C "$dir" remote get-url origin 2> /dev/null)
    if [ -z "$AGENT_BOX_REPO_URL" ]; then
      [ -n "$url" ] && git -C "$dir" remote remove origin
    elif [ "$url" != "$AGENT_BOX_REPO_URL" ]; then
      git -C "$dir" remote remove origin 2> /dev/null
      git -C "$dir" remote add origin "$AGENT_BOX_REPO_URL"
    fi
    return 0
  fi
  mkdir -p "$(dirname "$dir")"
  if [ -n "$AGENT_BOX_REPO_URL" ] && git ls-remote --exit-code --heads "$AGENT_BOX_REPO_URL" "$branch" > /dev/null 2>&1; then
    git clone -q --single-branch --branch "$branch" "$AGENT_BOX_REPO_URL" "$dir"
  else
    git init -q "$dir"
    [ -n "$AGENT_BOX_REPO_URL" ] && git -C "$dir" remote add origin "$AGENT_BOX_REPO_URL"
    git -C "$dir" checkout -q --orphan "$branch"
  fi
}

# Stable key for a worktree's WIP: <repo>/<branch with / as ~>.
wip_key() {
  local top=$1 branch origin
  branch=$(git -C "$top" symbolic-ref --quiet --short HEAD) || return 1
  origin=$(git -C "$top" remote get-url origin 2> /dev/null) || return 1
  printf '%s/%s' "$(basename "${origin%.git}")" "$(printf '%s' "$branch" | tr '/' '~')"
}

# Is this session id a registry chat that a.box runs on this machine? (Chats imported
# from super.engineering and not started here are left alone.)
own_chat() {
  local f="$AGENT_STATE_DIR/registry/chats/$1.json"
  [ -n "$1" ] && [ -f "$f" ] && [ "$(jq -r '(.host // "box") + " " + .status' "$f")" = "$AGENT_HOST running" ]
}
