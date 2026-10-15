# Sourced by every agent-box script.

AGENT_BOX_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -n "${CODER_AGENT_URL:-}" ]; then
  AGENT_HOST=box
  # The image's glab wrapper uses TJ's Coder GitLab token only with this set;
  # without it glab falls back to a config with no token and gets a 401.
  export REDO_GITLAB_USER_AUTH=1
  AGENT_BOX_REPO_URL="${AGENT_BOX_REPO_URL:-https://gitlab.com/tj15/agent-box.git}"
else
  AGENT_HOST=mac
  AGENT_BOX_REPO_URL="${AGENT_BOX_REPO_URL:-git@gitlab.com:tj15/agent-box.git}"
fi

AGENT_HOME="$HOME/.agent-box"
AGENT_STATE_DIR="$AGENT_HOME/state"
AGENT_SESSIONS_DIR="$AGENT_HOME/sessions"
AGENT_QUEUE_DIR="$AGENT_HOME/queue"
AGENT_LOG="$AGENT_HOME/log/agent-box.log"
SESSIONS_BRANCH="sessions/$AGENT_HOST"
CLAUDE_PROJECTS_DIR="$HOME/.claude/projects"

export GIT_AUTHOR_NAME="agent-box ($AGENT_HOST)" GIT_AUTHOR_EMAIL="tj@getredo.com"
export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME" GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"

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

# Clone one branch of the agent-box repo into $1, or start it as an orphan
# when the branch doesn't exist on the remote yet.
ensure_branch_clone() {
  local dir=$1 branch=$2
  [ -d "$dir/.git" ] && return 0
  mkdir -p "$(dirname "$dir")"
  if git ls-remote --exit-code --heads "$AGENT_BOX_REPO_URL" "$branch" > /dev/null 2>&1; then
    git clone -q --single-branch --branch "$branch" "$AGENT_BOX_REPO_URL" "$dir"
  else
    git init -q "$dir"
    git -C "$dir" remote add origin "$AGENT_BOX_REPO_URL"
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

# Is this session id a registry chat that lives on the Mac?
mac_chat() {
  local f="$AGENT_STATE_DIR/registry/chats/$1.json"
  [ -n "$1" ] && [ -f "$f" ] && [ "$(jq -r .host "$f")" = mac ]
}
