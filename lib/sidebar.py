"""One host's sidebar (projects and feature sections of worktrees), built from
`sc workspace list --json` and `sc section list --json`: super.engineering's
own output on the Mac, the agent-box shim's on the box."""

import os
import re
import subprocess
from pathlib import Path

PROJECTS = Path.home() / ".claude" / "projects"
HUES = {"redo": 210.0, "redo-data": 30.0, "internal-tools": 120.0, "redotech-internal-tools": 120.0}
ITEM_KEYS = ("id", "title", "branch", "path", "worktree", "is_primary", "agent_status", "attention", "pr", "local")


def last_activity(path):
    """Newest Claude transcript for this worktree, as a unix time. Sessions can
    record the path as given, resolved, or under super.engineering's new name
    for its storage (~/.super.engineering links to ~/.superconductor)."""
    paths = {path, os.path.realpath(path)}
    paths |= {p.replace("/.superconductor/", "/.super.engineering/") for p in list(paths)}
    times = []
    for p in paths:
        d = PROJECTS / re.sub(r"[^A-Za-z0-9]", "-", p)
        times += [f.stat().st_mtime for f in d.glob("*.jsonl")] if d.is_dir() else []
    if times:
        return max(times)
    # No chat of its own (a dispatcher leg): the last commit there.
    r = subprocess.run(["git", "-C", path, "log", "-1", "--format=%ct"], capture_output=True, text=True)
    return int(r.stdout.strip()) if r.returncode == 0 and r.stdout.strip() else None


def walk(o):
    vals = o.values() if isinstance(o, dict) else o if isinstance(o, list) else ()
    return ([o] if isinstance(o, dict) else []) + [d for v in vals for d in walk(v)]


def item(d, project, hue):
    out = {k: d.get(k) for k in ITEM_KEYS}
    out.update(project=project, hue=hue, letter=(project or "?")[:1].upper(), age=last_activity(d["path"]))
    return out


def build(ws, secs, host):
    workspaces = (ws.get("response") or {}).get("workspaces") or []
    names = {s["section_id"]: s for s in secs.get("sections") or []}
    projects, sections, seen = {}, [], set()

    def project_of(d):
        name = d.get("project_name") or d.get("project_id") or "?"
        return name, HUES.get(name, 0.0)

    for w in workspaces:
        for sec in w.get("sections") or []:
            if sec.get("kind") == "projects":
                for p in sec.get("projects") or []:
                    hue = (p.get("icon") or {}).get("hue", HUES.get(p["name"], 0.0))
                    proj = projects.setdefault(p["name"], {"name": p["name"], "hue": hue, "items": []})
                    for d in p.get("items") or []:
                        if d.get("path") and d["path"] not in seen:
                            seen.add(d["path"])
                            proj["items"].append(item(d, p["name"], hue))
                for d in sec.get("items") or []:  # the shim lists project items flat
                    name, hue = project_of(d)
                    proj = projects.setdefault(name, {"name": name, "hue": hue, "items": []})
                    if d.get("path") and d["path"] not in seen:
                        seen.add(d["path"])
                        proj["items"].append(item(d, name, hue))
            elif sec.get("kind") == "worktrees":
                meta = names.get(sec.get("id")) or {}
                rows = []
                for d in sec.get("items") or []:
                    if d.get("path"):
                        name, hue = project_of(d)
                        rows.append(item(d, name, hue))
                sections.append({"id": sec.get("id"), "name": meta.get("name") or sec.get("name") or "Section",
                                 "collapsed": bool(meta.get("collapsed")), "items": rows})
    for p in projects.values():
        p["items"].sort(key=lambda i: (not i["is_primary"], i["title"] or ""))
    return {"host": host, "projects": list(projects.values()), "sections": sections}
