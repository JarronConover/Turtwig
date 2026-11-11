# gpui-pre 0.3.9, patched for a.box

Copied from crates.io (without its examples, docs and tests) and wired in by `[patch.crates-io]` in
`rust/Cargo.toml`. Changes:

- `src/app.rs`: `App::redraw_on_update(entity)`: every update of that entity made outside a draw notifies
  it, as if it ended with `cx.notify()`. a.box's window keeps the workspace as a cached view (so a spinner
  turning doesn't redraw it), and this way no change to the workspace can wait for some later redraw.
- `src/window.rs`: `Window::draw` counts the windows drawing (`App::drawing`), for the above.
- `src/app.rs`: `App::updates_of(entity)` counts those updates, so the workspace can tell a frame it asked
  for from one a view inside it asked for.
- `src/view.rs`: `ViewElement::reuse_nested()`: a cached view drawn again reuses the cached views inside
  it that weren't notified, instead of drawing them all again (upstream marks the whole subtree as
  refreshing). The workspace's regions decide themselves when to draw again.

To move to a newer gpui-pre: copy it here and redo these, or drop the patch once upstream has an equivalent.
