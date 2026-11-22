# Text selection stays inside one tile (2026-11-16)

## Problem
Selecting text (drag, double/triple click, shift-click) or copying in the tile grid runs across tiles:
a drag that starts in one agent tile's transcript extends into neighboring tiles, and copy takes all of it.
Jarron wants selection and copy limited to the tile where the drag started.

## Change
1. Add a node option `selection_root` (bool). A selection that starts inside a node with
   `selection_root: true` is clamped to that node's selectable text: dragging past its edge extends only to
   the start/end of text inside it; nothing outside it gets highlighted or copied.
   Nested roots: the innermost one wins.
2. Treat these as selection roots by default even without the option: each `transcript` node, each
   `markdown` node with `props.selectable`, each `terminal` node.
3. `selection.copy` / ⌘C copies only the clamped selection.
4. Document `selection_root` in SETTINGS.md's node table and expose the current selection's root id in
   inspect.json (so the designer can verify).
5. Tests: a drag from tile A's transcript to tile B's selects only tile A's text; ⌘C copies only it.

## After it lands
The designer will set `selection_root: true` on each tile component root (AgentTile, TermTile, RedoTile,
NotesTile, DesignerTile, SlackTile).

Build, test, relaunch, push to origin, and reply with what changed.
