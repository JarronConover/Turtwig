# Brief: independent shells for terminal tiles

## Problem

Turtwig's middle area is now a grid of tiles (settings `TermTile`, `scripts/tiles.rs`). A terminal tile
renders `{"type": "terminal", "props": {"path": <worktree>}}`. In `rust/app/src/ui/render.rs` around line 421,
the `terminal` node always shows `active_terminal(path)`, so every terminal tile for the same worktree
mirrors one shell. A made-up path such as `~/redo#tile-ID` renders blank, because no shell is spawned for it.

## Fix

- **`terminal` node:** accept `props.id`, a terminal tab id under `path`. When it's set, render that tab's
  terminal, not the active one, and spawn it lazily on first render if it doesn't exist. Use `props.cwd` and
  `props.command` if given, otherwise the worktree folder and a login shell.
- **Focus and input:** clicking a terminal node focuses that exact terminal. Typing, the cursor, selection,
  scrollback and resize all go to it. Show a visible blinking cursor while it's focused, like Terminal.app;
  the user reports not seeing where they type.
- **Keep the existing behavior** when `props.id` is absent (the bottom terminal card and others).
- **inspect.json `views.terminals`:** key per path plus id, so agents can verify each tile's screen.

## Acceptance, checked by hand in Turtwig

- Two terminal tiles on the grid show two different shells. Typing in one doesn't appear in the other.
- The focused tile shows a blinking cursor at the prompt. `ls`, `vim` and `htop` work, and so does resizing
  the window.
- Closing a tile ends its shell (`terminal.close {path, id}`).

The designer will switch `TermTile` to `{"path": cwd, "id": tile id}` once this lands. Work on a branch,
rebuild and install, and report against each line.
