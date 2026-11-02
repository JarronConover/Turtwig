# Turtwig

A Mac app for running Claude Code and Codex chats, on the Mac and on an
always-on Coder workspace (the box), plus the tooling both machines share.

## What it believes

1. **Just a box.** This is just "a box" for an ADE (agentic development
   environment). The app itself is a framework, built in Rust; otherwise the
   software is liquid and takes whatever shape you want to give the box. Open
   a designer chat to shape it, its look and what it does (with scripts),
   live in the chat, with no reloads. Everything is configurable.
2. **Agents are full users.** All app state is exposed, so agents can see and
   drive the app and your chats directly. They're full users of the box: an
   orchestrator chat creates sections, worktrees and other chats, sends them
   messages, and moves them between machines.
3. **Work syncs.** Work syncs seamlessly between a remote box you control and
   your own machine. Chats outlive the app and the machine: closing the app
   doesn't stop them, you can reach them from your phone, a move carries the
   branch and uncommitted work along, and a new machine gets everything back
   from your backups.
4. **Git-native.** Projects are repos and the unit of work is a worktree on
   its branch, with its merge request, checks and unpushed work right on its
   row. How tasks map to worktrees, and how they're shown, is up to your
   settings.
5. **Ships as source.** The app builds from its own git checkout and updates
   and rebuilds itself. Agents can change Turtwig's own code, not just its
   settings.

## Install on a Mac

```bash
git clone git@github.com:JarronConover/Turtwig.git ~/dev/turtwig
~/dev/turtwig/bin/install
```

It needs Rust (`bin/install` says how to get it). The app opens on a setup
guide: your agents, an optional Coder workspace to run chats on (it installs
Turtwig there for you), and a private repo for backups. "Set up Turtwig…" in the
command palette (⌘K) brings the guide back; `bin/setup` is what it runs.

## Branches

This repo holds only the tooling (`main`). The backups live in a separate
private repo, `daemon.state_repo_url` in settings, because they hold every
transcript and uncommitted change. Never point it at this repo, which other
people can read. Without it nothing is backed up: chats stay on their
machine and can't move between machines.

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

The setup guide does this for you; by hand:

```bash
git clone https://github.com/JarronConover/Turtwig.git ~/agent-box
~/agent-box/bin/boot --force
```
