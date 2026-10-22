# gpui-base 0.7.3, patched for a.box

Copied from crates.io and wired in by `[patch.crates-io]` in `rust/Cargo.toml`. Changes:

- `src/input/base/state.rs`: in a soft-wrapped textarea, Home / End (cmd-left / cmd-right) go to the start or
  end of the row the caret is drawn on, every time, as a macOS text view does. The code editor is unchanged.
- `src/input/base/kind.rs`, `state.rs`, `element.rs`, `editor/decorations.rs`: a textarea paints styles its
  owner sets with `set_text_decorations` (the message box's live markdown); clipped to the text at paint.

To move to a newer gpui-base: copy it here and redo these, or drop the patch once upstream has both.
