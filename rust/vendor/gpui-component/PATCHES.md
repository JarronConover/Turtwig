# gpui-component 0.7.3, patched for Turtwig

Copied from crates.io and wired in by `[patch.crates-io]` in `rust/Cargo.toml`. Changes:

- `src/text/compat.rs`: `TextView::with_base` maps the gpui-base text view underneath, so Turtwig can install
  Base hooks this facade has no method for (`table_overlay`, from the gpui-base patch).

To move to a newer gpui-component: copy it here and redo this, or drop the patch once upstream has it.
