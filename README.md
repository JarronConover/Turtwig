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

## Setup on a new box

```bash
git clone https://gitlab.com/tj15/agent-box.git ~/agent-box
~/agent-box/bin/boot --force
```
