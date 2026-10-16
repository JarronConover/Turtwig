# agent-box

Tooling and durable state for TJ's remote agent box (one always-on Coder
workspace running Claude Code chats under Remote Control). Design:
`~/.local/state/redo/designs/remote-agent-workspace/design.md`.

## Branches

- `main`: this tooling.
- `state`: durable orchestrator state (designs, handoffs, review JSON,
  registry), committed on write.
- `sessions/<host>`: the latest snapshot of that host's Claude transcripts
  and WIP bundles. One amended commit, force-pushed after every turn, so
  the branch never grows history.

## Write-through

`bin/turn-sync` is a Claude Code `Stop`/`SubagentStop` hook. It queues the
session's worktree and starts `bin/sync-worker` in the background, which:

1. snapshots each queued worktree (uncommitted changes, untracked files,
   unpushed commits) as a git bundle in `sessions/<host>:wip/<repo>/<branch>`,
   without touching HEAD, the index, or any branch, and without pushing to
   the code repo (no pre-push hook, no CI);
2. copies `~/.claude/projects` into `sessions/<host>:hosts/<host>/projects`;
3. amends and force-pushes `sessions/<host>`.

Nothing waits for a shutdown, so the box can stop at any moment.

## Boot

`bin/boot` is idempotent: installs tmux/rsync/jq, clones the `state` and
`sessions/<host>` branches into `~/.agent-box/`, restores transcripts this
machine is missing, and installs the hooks. On the box, the
`SessionStart` hook runs it after every workspace start, because the Coder
template launches a `claude` process at startup.

## Restore WIP

`bin/wip-restore <worktree> [box|mac]` applies the other host's latest WIP
for that branch to a clean worktree: unpushed commits become the branch
head, uncommitted changes stay uncommitted.

## Chats and sections (the API)

Agents on the box and the Mac app drive everything through three layers,
all over the same state:

- `bin/chat`: `new NAME [--kind primary|feature|orchestrator] [--branch B
  [--base B]] [--dir D] [--repo R] [--prompt TEXT] [--agent claude|codex]`,
  `list`, `start|stop|archive|send ID`, `move ID --to mac|box`,
  `test-local [ID | --branch B]`. An orchestrator without a branch gets its
  own worktree (`sc-<name>`). Agents are data (settings `daemon.agents`):
  launch/resume/fork templates, where transcripts live, how state shows. A
  Codex chat's session id is found from its rollout after the first message.
  `bin/codex-hooks [--file F] [--dry-run]` adds agent-boxd's hook to Codex's
  hooks.json (Codex asks once to trust it), like `bin/claude-hooks`.
- `shim/sc` (box only): super.engineering's CLI for the factory skills.
  `sc section create|edit|assign|unassign|delete`, `sc worktree
  create|delete|status|rename-branch|set-target-branch|set-label`, `sc layout
  run tabs` (starts a chat). So `/ship-feature` running on the box creates its
  feature section, puts each leg's worktree in it, and labels the rows itself.
- `bin/dashboard` HTTP API on the box's 127.0.0.1:8790 (the app tunnels it to
  the Mac's 127.0.0.1:18790): `GET /api` lists the endpoints: sidebar,
  chats, sections (create/rename/delete/assign), worktree delete, terminals.

## Setup on a new box

```bash
git clone https://gitlab.com/tj15/agent-box.git ~/agent-box
~/agent-box/bin/boot --force
```
