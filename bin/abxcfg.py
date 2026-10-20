#!/usr/bin/env python3
"""a.box's configuration, for every script on this machine: the app's `daemon` settings as the daemon last
saved them (~/.agent-box/config.json; AGENT_BOX_OPTIONS, a JSON object, overrides them for one run), over
built-in defaults that reproduce the original setup (a Coder workspace "box" and a "mac").

This machine's host id: AGENT_BOX_HOST, else ~/.agent-box/host (one line), else the first `hosts` entry
whose `detect` matches ({env: VAR} set, {os: darwin|linux}, {hostname: regex}; all given must match),
else "box" when CODER_AGENT_URL is set, else "mac".

  abxcfg.py shell                  sh exports env.sh evals (AGENT_HOST, SESSIONS_BRANCH, GIT_AUTHOR_*, ...)
  abxcfg.py agent-env              sh code bin/agent-env evals before it runs an agent
  abxcfg.py get KEY [HOST]         a value as JSON: host.KEY (this or HOST's), repos, or an option
  abxcfg.py resolved               every resolved value, as JSON (the daemon's config_get)
  abxcfg.py aliases FROM TO FROM_HOME TO_HOME   path pairs to rewrite FROM's paths for TO, longest first
"""

import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from pathlib import Path

HOME = Path.home()
AGENT_HOME = HOME / ".agent-box"


def agent_home():
    return Path.home() / ".agent-box"


AGENT_BOX_DIR = Path(__file__).resolve().parent.parent

FOREIGN_TOOLS = {
    "sc_bin": "~/.super.engineering/bin/sc",
    "managed_bases": ["~/.superconductor/worktrees"],
    "env_prefixes": ["SUPER_ENGINEERING_", "SUPERCONDUCTOR_"],
    "path_drop": ["~/.super.engineering/bin"],
}

AGENT_ENV = {
    "secrets_file": "~/.config/agent-box/secrets.env",
    "path_prepend": [],
    "path_drop": [],
    "set": {},
    "unset": ["CLAUDECODE", "CLAUDE_PID", "CLAUDE_EFFORT", "AI_AGENT", "CLAUDE_CODE_SESSION_ID",
              "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH",
              "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_CODE_SESSION_ATTENDED",
              "CLAUDE_CODE_TUI_JUST_SWITCHED"],
    "unset_prefixes": [],
}

BOX_BOOT = {
    "kill_patterns": ["^agentapi server"],
    "packages": ["tmux", "rsync", "jq", "ttyd"],
    "install": "sudo apt-get update -qq > /dev/null 2>&1; "
               "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq {packages} > /dev/null 2>&1",
    "claude_onboarding": True,
    "git_insteadof": {"https://gitlab.com/": ["git@gitlab.com:", "ssh://git@gitlab.com/"]},
    "bashrc_hook": True,
    "tmux_status_off": True,
    "services": [
        {"name": "agent-box", "cmd": "{agent_box_dir}/bin/chat supervise 2>&1 | tee -a {agent_home}/log/supervisor.log"},
        {"name": "agent-box-web", "cmd": "{agent_box_dir}/bin/dashboard --port 8790 2>&1 | tee -a {agent_home}/log/dashboard.log"},
        {"name": "agent-box-tty", "cmd": "ttyd -i 127.0.0.1 -p 8791 -W -a -t fontSize=13 -t 'fontFamily=ui-monospace,SF Mono,Menlo,monospace' "
                                         "-t 'theme={\"background\":\"#1e1e1f\",\"foreground\":\"#d6d6d8\",\"cursor\":\"#d6d6d8\",\"selectionBackground\":\"#3a4252\"}' "
                                         "-t disableLeaveAlert=true tmux attach -t 2>&1 | tee -a {agent_home}/log/ttyd.log"},
        {"name": "agent-boxd", "cmd": "{boxd} run 2>&1 | tee -a {agent_home}/log/boxd.log", "build": True},
    ],
}

HOSTS = {
    "box": {
        "label": "box", "detect": {"env": "CODER_AGENT_URL"}, "ssh": None, "agent_box_dir": "~/agent-box",
        "sc_store": "sc.json", "supervise": True, "relay_app_actions": True, "sync_transcripts": "all", "stacks": False,
        "foreign_sessions": False, "build_unwatched": True, "env": {"REDO_GITLAB_USER_AUTH": "1"},
        "agent_env": {"path_prepend": ["{agent_box_dir}/shim", "~/redo-monorepo-worktrees/bin", "~/.local/bin"],
                      "set": {"WT_PRIMARY": "~/dev/redo", "WT_BASE": "~/dev/redo-worktrees"}},
        "claude_settings": {"enableAllProjectMcpServers": True, "permissions": {"defaultMode": "bypassPermissions"},
                            "skipDangerousModePermissionPrompt": True},
        "agent_bin_dirs": [], "boot": BOX_BOOT,
    },
    "mac": {
        "label": "Mac", "detect": {"os": "darwin"}, "ssh": None, "agent_box_dir": "~/dev/agent-box",
        "sc_store": "sc-mac.json", "supervise": False, "relay_app_actions": False, "sync_transcripts": "registry",
        "stacks": True, "foreign_sessions": True, "build_unwatched": False, "env": {}, "agent_env": {},
        "claude_settings": {}, "agent_bin_dirs": ["~/.local/bin"], "boot": {},
    },
}

# A host the defaults don't know.
HOST_FALLBACK = {
    "ssh": None, "agent_box_dir": "~/agent-box", "supervise": False, "relay_app_actions": True,
    "sync_transcripts": "registry", "stacks": True, "foreign_sessions": False, "build_unwatched": True, "env": {},
    "agent_env": {}, "claude_settings": {}, "agent_bin_dirs": [], "boot": {},
}

WT_TOOL = {
    "bin": "wt", "bin_paths": ["~/redo-monorepo-worktrees/bin/wt", "~/dev/redo-monorepo-worktrees/bin/wt"],
    "create": ["{bin}", "{branch}", "--no-start"],
    "adopt": ["{bin}", "adopt", "{path}", "--no-start"],
    "remove": ["{bin}", "remove", "{path}", ["{force}"]],
    "delete": ["{bin}", "remove", "{path}", "--force", "--purge-cache"],
    "list": ["{bin}", "list"],
    "start": ["{bin}", "start", "{branch}", "-d"],
    "partial_note": "run `wt adopt {path}` before starting its stack",
    "env": {"WT_PRIMARY": "{primary}", "WT_BASE": "{worktrees_base}"},
}

REPOS = {
    "redo": {"primary": "~/dev/redo", "worktrees": "~/dev/redo-worktrees/{branch_slug}", "hue": 210,
             "forge_project": "redo/redoapp/product/redo", "names": ["redo"], "worktree_tool": WT_TOOL},
    "internal-tools": {"primary": "~/dev/redotech-internal-tools", "worktrees": "~/dev/redotech-internal-tools-worktrees/{name}",
                       "hue": 120, "forge_project": "redo/redoapp/internal/redotech-internal-tools",
                       "names": ["redotech-internal-tools", "internal-tools"]},
    "redo-data": {"primary": "~/dev/redo-data", "worktrees": "~/dev/redo-data-worktrees/{name}", "hue": 30,
                  "forge_project": "redo/redoapp/product/redo-data", "names": ["redo-data"]},
}

# Logs the daemon's log_tail may read, by name.
LOGS = {
    "boxd": "~/.agent-box/boxd.log", "agent-box": "~/.agent-box/log/agent-box.log",
    "supervisor": "~/.agent-box/log/supervisor.log", "boxd-build": "~/.agent-box/log/boxd-build.log",
    "dashboard": "~/.agent-box/log/dashboard.log", "ttyd": "~/.agent-box/log/ttyd.log",
}

ENV_PUSH = {
    # Target hosts (null: every other host with ssh); files only flow toward the newer copy.
    "hosts": None,
    "paths": [
        {"path": "~/.agents/skills/", "two_way": True, "deletes": True, "exclude": [".git", "__pycache__"]},
        {"path": "~/.claude/CLAUDE.md", "two_way": True},
        {"path": "~/.agent-box/uploads/"},
        {"path": "~/.config/redo/cursor-environments.json"},
        {"path": "~/.config/redo/cursor-typecheck-agents.json"},
        {"path": "~/.claude/hooks/block-squash.py", "replace": True},
    ],
    "memory": True, "memory_deletes": True, "max_deletes": 25,
    "skills_link": True,
    # PreToolUse hooks copied over (the remote's copies of them replaced): this exact matcher, or a command matching this regex.
    "hooks_match": {"matcher": "mcp__.+__save_issue", "command": "block-squash"},
}


def options():
    try:
        saved = json.loads((agent_home() / "config.json").read_text())
    except (OSError, ValueError):
        saved = {}
    try:
        over = json.loads(os.environ.get("AGENT_BOX_OPTIONS") or "{}")
    except ValueError:
        over = {}
    saved = saved if isinstance(saved, dict) else {}
    return {**saved, **(over if isinstance(over, dict) else {})}


def option(key, default=None):
    value = options().get(key)
    return default if value is None else value


def expand(path, home=None):
    path = str(path)
    if path == "~" or path.startswith("~/"):
        return str(home or Path.home()) + path[1:]
    return path


def merged(base, over):
    """`base` with `over`'s keys: a dict of dicts merges per entry; a null entry removes it."""
    out = {k: dict(v) if isinstance(v, dict) else v for k, v in base.items()}
    for k, v in (over or {}).items():
        if v is None:
            out.pop(k, None)
        elif isinstance(v, dict) and isinstance(out.get(k), dict):
            out[k].update(v)
        else:
            out[k] = v
    return out


def fill(template, values):
    """argv from a template: {key} replaced from values; a nested list goes when any of its keys is empty."""
    out = []
    for part in template:
        if isinstance(part, list):
            keys = [k for p in part for k in re.findall(r"\{(\w+)\}", p) if k in values]
            if all(values[k] for k in keys):
                out += fill(part, values)
            continue
        out.append(subst(part, values))
    return out


def subst(text, values):
    for k, v in values.items():
        text = text.replace("{" + k + "}", str(v if v is not None else ""))
    return text


def hosts():
    return merged(HOSTS, option("hosts", {}))


def _detects(rule):
    if not isinstance(rule, dict) or not rule:
        return False
    if rule.get("env") and not os.environ.get(rule["env"]):
        return False
    if rule.get("os") and not sys.platform.startswith(rule["os"]):
        return False
    if rule.get("hostname"):
        import socket
        if not re.search(rule["hostname"], socket.gethostname()):
            return False
    return True


def host_id():
    if os.environ.get("AGENT_BOX_HOST"):
        return os.environ["AGENT_BOX_HOST"]
    try:
        named = (agent_home() / "host").read_text().strip()
        if named:
            return named
    except OSError:
        pass
    for hid, h in hosts().items():
        if _detects((h or {}).get("detect")):
            return hid
    return "box" if os.environ.get("CODER_AGENT_URL") else "mac"


def host(hid=None):
    """A host's settings, every key filled in (`id`, `label`, `ssh`, `sessions_branch`, `sc_store`, ...)."""
    hid = hid or host_id()
    h = merged(HOST_FALLBACK, hosts().get(hid) or {})
    h["id"] = hid
    h.setdefault("label", hid)
    h["ssh"] = ssh_of(hid, h)
    h["sessions_branch"] = h.get("sessions_branch") or subst(option("sessions_branch", "sessions/{host}"), {"host": hid})
    h["sc_store"] = h.get("sc_store") or f"sc-{hid}.json"
    if "sync_transcripts" not in h:
        h["sync_transcripts"] = "registry"
    return h


def ssh_of(hid, h=None):
    """How other machines reach a host over ssh (None: they can't). The app's own box (`remote_host`,
    default box) takes the app's connection.ssh_host (option ssh_host), then AGENT_BOX_SSH."""
    own = ((option("hosts", {}) or {}).get(hid) or {}).get("ssh")
    if own:
        return own
    if hid == option("remote_host", "box"):
        found = option("ssh_host") or os.environ.get("AGENT_BOX_SSH")
        if found:
            return found
    return (h if h is not None else hosts().get(hid) or {}).get("ssh")


def other_hosts():
    here = host_id()
    return [hid for hid in hosts() if hid != here]


def remote_host():
    """The other machine scripts reach by default: the first other host with ssh."""
    return next((hid for hid in other_hosts() if ssh_of(hid)), None)


def label(hid):
    return host(hid).get("label") or hid


def remote_path(path):
    """A path for a remote shell: ~/ becomes $HOME/."""
    path = str(path)
    return "$HOME" + path[1:] if path == "~" or path.startswith("~/") else path


def agent_box_dir(hid=None):
    """This checkout here; another host's from its `agent_box_dir` (as ~/..., for a remote shell)."""
    if not hid or hid == host_id():
        return str(AGENT_BOX_DIR)
    return host(hid).get("agent_box_dir") or "~/agent-box"


def repos():
    return merged(REPOS, option("repos", {}))


def default_repo():
    names = list(repos())
    want = option("default_repo", "redo")
    return want if want in names else (names[0] if names else None)


def primary(repo):
    r = repos().get(repo)
    return Path(expand(r["primary"])) if r and r.get("primary") else None


def repo_of(directory):
    """The repo whose primary checkout is this folder."""
    real = os.path.realpath(str(directory))
    for key, r in repos().items():
        if r.get("primary") and os.path.realpath(expand(r["primary"])) == real:
            return key
    return None


def repo_for_folder(name):
    """A repo key for a checkout folder name: a repo's key or one of its `names`, else the name itself."""
    for key, r in repos().items():
        if name == key or name in (r.get("names") or []) or (r.get("primary") and Path(expand(r["primary"])).name == name):
            return key
    return name


def hue(repo):
    r = repos().get(repo) or {}
    if r.get("hue") is not None:
        return float(r["hue"])
    return float(int(hashlib.sha1(repo.encode()).hexdigest()[:8], 16) % 360)


def branch_slug(branch):
    return branch.replace("/", "-")


def worktree_path(repo, branch=None, name=None):
    r = repos().get(repo) or {}
    template = r.get("worktrees") or f"{r.get('primary', '~/dev/' + repo)}-worktrees/{{name}}"
    slug = branch_slug(branch) if branch else ""
    return Path(expand(subst(template, {"branch_slug": slug, "branch": branch or "", "name": name or slug, "repo": repo})))


def worktrees_base(repo):
    r = repos().get(repo) or {}
    template = r.get("worktrees") or f"{r.get('primary', '~/dev/' + repo)}-worktrees/{{name}}"
    return Path(expand(template.split("{")[0].rstrip("/")))


def worktree_tool(repo):
    """The repo's worktree tool ({bin, create, adopt, remove, delete, list, start, env}), its bin resolved;
    None: plain `git worktree`. adopt: set up a folder plain git checked out (so several check out at once)."""
    tool = (repos().get(repo) or {}).get("worktree_tool")
    if not tool:
        return None
    tool = dict(tool)
    found = next((expand(p) for p in tool.get("bin_paths") or [] if os.path.exists(expand(p))), None)
    tool["bin"] = found or shutil.which(tool.get("bin") or "") or tool.get("bin")
    return tool


def tool_values(repo, **extra):
    p = primary(repo)
    return {"primary": str(p) if p else "", "worktrees_base": str(worktrees_base(repo)), "repo": repo, **extra}


def tool_env(repo, tool, **extra):
    values = tool_values(repo, **extra)
    return dict(os.environ, **{k: expand(subst(str(v), values)) for k, v in (tool.get("env") or {}).items()})


def tool_argv(repo, tool, action, **extra):
    template = tool.get(action)
    if not template:
        return None
    return fill(template, tool_values(repo, bin=tool["bin"], **extra))


def default_base(checkout=None):
    """The default branch: the checkout's origin/HEAD, else option default_base."""
    if checkout:
        r = subprocess.run(["git", "-C", str(checkout), "rev-parse", "--abbrev-ref", "origin/HEAD"],
                           capture_output=True, text=True)
        name = r.stdout.strip()
        if r.returncode == 0 and name.startswith("origin/") and name != "origin/HEAD":
            return name[len("origin/"):]
    return option("default_base", "main")


def state_branch():
    return option("state_branch", "state")


def state_repo_url():
    """Where the state and sessions branches live: AGENT_BOX_REPO_URL, option state_repo_url (or this host's).
    Empty: no backup repo, so state and transcripts stay on this machine. Never the code repo's origin, which
    other people can read."""
    return os.environ.get("AGENT_BOX_REPO_URL") or host().get("state_repo_url") or option("state_repo_url") or ""


NO_BACKUP = ("no backup repo: set daemon.state_repo_url in settings to a private git repo to back up chats "
             "and move them between machines")


def git_author():
    """(name, email) for the state commits: option git_author {name, email}, else agent-box (<host>) and
    git's user.email."""
    a = option("git_author", {}) or {}
    name = subst(a.get("name") or "agent-box ({host})", {"host": host_id()})
    email = a.get("email")
    if not email:
        email = subprocess.run(["git", "config", "user.email"], capture_output=True, text=True).stdout.strip()
    return name, email or "agent-box@localhost"


def foreign_tools():
    """Another tool sharing this machine's sessions and worktrees (super.engineering); None: none."""
    if "foreign_tools" in options() and options()["foreign_tools"] is None:
        return None
    return {**FOREIGN_TOOLS, **(option("foreign_tools", {}) or {})}


def path_aliases():
    """[{host: path}]: folders that are the same checkout on each host (the box's ~/project is the Mac's
    ~/dev/redo). A 2-list is [mac path, box path]."""
    out = []
    for a in option("path_aliases", [{"mac": "~/dev/redo", "box": "~/project"}]) or []:
        if isinstance(a, list) and len(a) == 2:
            a = {"mac": a[0], "box": a[1]}
        if isinstance(a, dict):
            out.append(a)
    return out


def alias_pairs(src, dst, src_home, dst_home):
    """(src path, dst path) for every alias both hosts have, longest first, then src's home for dst's."""
    pairs = []
    for a in path_aliases():
        if a.get(src) and a.get(dst):
            pairs.append((expand(a[src], src_home).rstrip("/"), expand(a[dst], dst_home).rstrip("/")))
    pairs.sort(key=lambda p: -len(p[0]))
    return pairs + [(str(src_home).rstrip("/"), str(dst_home).rstrip("/"))]


def translate(path, src, dst, src_home, dst_home):
    """A path on src as the same place on dst."""
    for a, b in alias_pairs(src, dst, src_home, dst_home):
        if path == a or path.startswith(a + "/"):
            return b + path[len(a):]
    return path


def agent_env(h=None):
    """Top-level agent_env with this host's added: lists extend, `set` merges."""
    h = h or host()
    top = {**AGENT_ENV, **(option("agent_env", {}) or {})}
    own = h.get("agent_env") or {}
    out = dict(top)
    for k in ("path_prepend", "path_drop", "unset", "unset_prefixes"):
        out[k] = list(top.get(k) or []) + list(own.get(k) or [])
    out["set"] = {**(h.get("env") or {}), **(top.get("set") or {}), **(own.get("set") or {})}
    if own.get("secrets_file"):
        out["secrets_file"] = own["secrets_file"]
    ft = foreign_tools()
    if ft:
        out["unset_prefixes"] += ft.get("env_prefixes") or []
        out["path_drop"] += ft.get("path_drop") or []
    return out


def agent_env_sh():
    e = agent_env()
    values = {"agent_box_dir": str(AGENT_BOX_DIR), "home": str(Path.home())}
    lines = []
    secrets = expand(e.get("secrets_file") or "")
    if secrets:
        lines.append(f"set -a; [ -f {shlex.quote(secrets)} ] && . {shlex.quote(secrets)}; set +a")
    prepend = [expand(subst(p, values)) for p in e["path_prepend"]]
    if prepend:
        lines.append(f"export PATH={shlex.quote(':'.join(prepend))}:\"$PATH\"")
    for k, v in e["set"].items():
        lines.append(f"export {k}={shlex.quote(expand(subst(str(v), values)))}")
    if e["unset"]:
        lines.append("unset " + " ".join(e["unset"]))
    if e["unset_prefixes"]:
        pats = " | ".join(f"{shlex.quote(p)}*" for p in e["unset_prefixes"])
        lines.append(f'for v in $(compgen -e); do case $v in {pats}) unset "$v" ;; esac; done')
    for d in e["path_drop"]:
        lines.append(f"PATH=$(printf '%s' \"$PATH\" | tr ':' '\\n' | grep -vxF {shlex.quote(expand(d))} | paste -sd: -)")
    lines.append("export PATH")
    return "\n".join(lines)


def shell():
    h = host()
    name, email = git_author()
    remote = remote_host()
    values = {
        "AGENT_HOST": h["id"], "AGENT_HOST_LABEL": h.get("label") or h["id"],
        "AGENT_BOX_REPO_URL": state_repo_url(), "STATE_BRANCH": state_branch(),
        "SESSIONS_BRANCH": h["sessions_branch"], "AGENT_REMOTE_HOST": remote or "",
        "AGENT_SYNC_TRANSCRIPTS": h.get("sync_transcripts") or "registry",
        "AGENT_BOX_SSH": ssh_of(remote) if remote else "",
        "GIT_AUTHOR_NAME": name, "GIT_AUTHOR_EMAIL": email, "GIT_COMMITTER_NAME": name, "GIT_COMMITTER_EMAIL": email,
        **{k: str(v) for k, v in (h.get("env") or {}).items()},
    }
    return "\n".join(f"export {k}={shlex.quote(str(v))}" for k, v in values.items())


def get(key, hid=None):
    h = host(hid)
    special = {
        "host": lambda: h, "hosts": lambda: {k: host(k) for k in hosts()}, "repos": repos,
        "agent_env": lambda: agent_env(h), "foreign_tools": foreign_tools, "path_aliases": path_aliases,
        "logs": lambda: {**LOGS, **(option("logs", {}) or {})},
        "env_push": lambda: {**ENV_PUSH, **(option("env_push", {}) or {})},
        "boot": lambda: {**(h.get("boot") or {})},
        "git_author": lambda: dict(zip(("name", "email"), git_author())),
        "state_repo_url": state_repo_url, "state_branch": state_branch, "default_repo": default_repo,
    }
    if key in special:
        return special[key]()
    if key in h:
        return h[key]
    return option(key)


def resolved():
    return {"host_id": host_id(), "host": host(), "hosts": {k: host(k) for k in hosts()}, "repos": repos(),
            "default_repo": default_repo(), "state_repo_url": state_repo_url(), "state_branch": state_branch(),
            "git_author": dict(zip(("name", "email"), git_author())), "agent_env": agent_env(),
            "foreign_tools": foreign_tools(), "path_aliases": path_aliases(), "env_push": get("env_push"),
            "logs": get("logs")}


def main(argv):
    if not argv or argv[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    cmd = argv[0]
    if cmd == "shell":
        print(shell())
    elif cmd == "agent-env":
        print(agent_env_sh())
    elif cmd == "get" and len(argv) >= 2:
        print(json.dumps(get(argv[1], argv[2] if len(argv) > 2 else None)))
    elif cmd == "resolved":
        print(json.dumps(resolved()))
    elif cmd == "aliases" and len(argv) == 5:
        for a, b in alias_pairs(*argv[1:5]):
            print(f"{a}\t{b}")
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
