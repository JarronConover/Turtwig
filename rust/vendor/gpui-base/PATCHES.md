# gpui-base 0.7.3, patched for a.box

Copied from crates.io and wired in by `[patch.crates-io]` in `rust/Cargo.toml`. Changes:

- `src/input/base/state.rs`: in a soft-wrapped textarea, Home / End (cmd-left / cmd-right) go to the start or
  end of the row the caret is drawn on, every time, as a macOS text view does. The code editor is unchanged.
- `src/input/base/kind.rs`, `state.rs`, `element.rs`, `editor/decorations.rs`: a textarea paints styles its
  owner sets with `set_text_decorations` (the message box's live markdown); clipped to the text at paint.
- `src/text/markdown_ext.rs`, `src/text/format/markdown.rs`: `MarkdownExtensions::soft_breaks_as_newlines` renders a
  soft break as a line break (GitHub's "breaks" mode) instead of a space, and maps it to the source line ending for
  selection. The chat transcript turns it on (`chat.soft_breaks_as_newlines`).

- `src/text/text_view.rs`, `state.rs`, `node.rs`, `src/scrollable_mask.rs`: `TextView::table_overlay` lays an
  element (Turtwig: a scrollbar and a Shift + wheel layer) over each horizontally scrolling markdown table, with
  the table's scroll handle; and in that layout a definite `max_width` on `table_cell` caps every column at it
  (natural width up to the cap, wrapping beyond), instead of the adaptive floors.

- `src/text_selection.rs`, `element_ext.rs`, `lib.rs`: selection roots. `TextSelectionRootId`,
  `text_selection_root(root, element)` / `ElementExt::text_selection_root`, `TextSelectionRegistration::with_root`:
  a selection that starts on text inside a root takes in only that root's text (endpoint resolution, snapshots,
  touch handles and copy all skip the rest); `TextSelection::active_root` and `has_geometry_selection` read it.
  Unlike scopes, roots need no activation, so the component Root's per-frame scope reset leaves them alone.

To move to a newer gpui-base: copy it here and redo these, or drop the patch once upstream has both.
