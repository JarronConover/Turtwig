# Brief: let wide Markdown tables in chats scroll sideways

## Problem

In Turtwig chats, a Markdown table wider than the reading column is clipped at the right edge, and there's no
way to scroll to the hidden columns. Settings can't fix it:
- `chat.markdown.table` is only a `StyleRefinement`, built in `rust/app/src/ui/trace_view.rs`
  `markdown_style_of`, around line 452. That goes into `gpui_kit::component::text::TextViewStyle.table`.
- `overflow_x: scroll` on a refinement does nothing in GPUI without a stateful element (an `.id(...)`) and a
  scroll handle. So the table is clipped.

## Fix

Make each Markdown table in a chat scroll horizontally on its own, the way code blocks already do. See
`trace_view.rs` around lines 715–747: `overflow_x_scroll().restrict_scroll_to_axis().track_scroll(&handle)`,
the `shift_scroll_layer`, and a horizontal `Scrollbar` with `chat.code_scroll.scrollbar`.

- **Where tables are rendered.** If `gpui_kit`'s TextView renders tables, either:
  - add an option there that wraps each table in an id'd `overflow_x_scroll` container with a per-table
    scroll handle (vendor or patch the crate if needed), or
  - give TextView a table-render hook that Turtwig fills.
- **Per-table identity.** A table's scroll position must stay stable across redraws (key it by message id
  plus table index) and must not scroll the transcript vertically.
- **Input.** A plain horizontal trackpad swipe scrolls the table. Shift + wheel also scrolls it. Vertical
  scrolling over a table still scrolls the transcript.
- **Scrollbar.** Show a horizontal bar on hover when a table overflows, following `chat.code_scroll.scrollbar`.
- **Column widths.** Columns keep their natural width up to `chat.markdown.table_cell.max_width` when set;
  beyond that, text wraps.

Current settings for reference: `chat.markdown.table` = `{width: full, max_width: full, overflow_x: hidden}`
and `table_cell` = `{padding: [6, 8], min_width: 0, shrink: 1, max_width: 360}`. Once scrolling works, tell
the designer which keys to set, likely `overflow_x: scroll` back on `table`.

## Acceptance, checked by hand in Turtwig

- A reply with a 6-column table: the hidden columns are reachable with a sideways swipe, and a scrollbar
  shows on hover.
- Vertical scrolling over the table still scrolls the chat.
- Narrow tables look unchanged.
- No measurable frame-time regression while scrolling a long chat. Check `perf.avg_frame_ms` in
  ~/.agent-box/state/settings/inspect.json.

Work on a branch, commit, rebuild and install the bundle, and report against each line.
