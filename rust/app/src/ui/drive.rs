/*! Driving settings-built UI without a mouse: typing into and submitting fields, focus, hover, keys,
scrolling, drops and click menus, plus what inspect.json shows about them (`ui`). */

use super::expr::{to_val, with};
use super::render::{build_menu, loop_keys, MenuEntry};
use crate::workspace::Workspace;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::*;
use jaq_json::Val;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub(crate) const ACTIONS: &[(&str, &str)] = &[
    ("ui.type", "{id, text, replace?}: type into an `input` node's field (inspect.json `ui.inputs` lists them) as if by hand: its state and `on.change` follow; `replace` swaps the whole text"),
    ("ui.submit", "{id}: press Enter in an `input` node (runs its `on.submit` with the text as `.input`)"),
    ("ui.focus / ui.blur", "{id}: give keyboard focus to a `focusable` node or an `input` (its `focus` style and `on.focus` apply); ui.blur takes it away"),
    ("ui.hover", "{id, off?}: show a node as hovered (its `hover` style, `on.hover_start`) until another ui.hover or `off: true` (`on.hover_end`)"),
    ("ui.keystroke", "{key}: press a key (\"tab\", \"cmd-k\") through the app's own key handling, as if typed into whatever has focus (not a real key press: macOS never sees it)"),
    ("ui.key", "{key, id?}: press a key on a node (default: the focused one): its `on.key` entry, else Enter / Space click it"),
    ("ui.file_drag", "{phase: enter | move | exit | drop | sweep, x, y, paths?, to_x?, to_y?, ms?}: files dragged over the window, through the app's own drag handling (not a real drag: macOS never sees it); sweep moves from x,y to to_x,to_y over ms at 60 moves a second"),
    ("ui.text_drag", "{x, y, to_x, to_y, ms?}: press, drag and let go of the mouse's left button from x,y to to_x,to_y through the app's own mouse handling (not a real drag: macOS never sees it), selecting the text it crosses"),
    ("ui.scroll", "{id | handle, to?: top | bottom | <y> , row?, by?, x?}: scroll a `scroll` node (its `props.handle`, or its id) or a virtual list; `row` brings that row into view, `by` scrolls that many points (positive: down)"),
    ("ui.drop", "{key, value, kind?, zone?}: drop `value` on the target with that `drop_key` (inspect.json `ui.drops`) as a drag would: its matching `on.drop_item` runs if its `valid` check passes"),
];

/** A drop target drawn this frame: its key, zones and `on.drop_item` specs (accept, action, valid). */
#[derive(Clone)]
pub(crate) struct DropTarget {
    pub key: String,
    pub zones: Value,
    pub specs: Rc<Vec<(Vec<String>, Value, Option<String>)>>,
    pub ctx: Val,
}

pub(crate) struct ClickMenu {
    pub id: String,
    pub at: Point<Pixels>,
    pub menu: Entity<PopupMenu>,
    _dismiss: Subscription,
}

/** Per-node UI state that isn't settings state: kept here so the interpreter and agents share it. */
#[derive(Default)]
pub(crate) struct Ui {
    pub hovered: HashSet<String>,
    pub forced_hover: Option<String>,
    pub focus: HashMap<String, FocusHandle>,
    pub focused: Option<String>,
    pub focused_last: Option<String>,
    pub tooltips: Vec<(String, String)>,
    pub tooltips_last: Vec<(String, String)>,
    pub drops: Vec<DropTarget>,
    pub drops_last: Vec<DropTarget>,
    /** Nodes with `on.hover_start` / `on.hover_end`, and with `on.key` (key, action). */
    pub hovers: Vec<(String, Option<Value>, Option<Value>)>,
    pub hovers_last: Vec<(String, Option<Value>, Option<Value>)>,
    pub keys: Vec<(String, Vec<(String, Value)>, Option<Value>)>,
    pub keys_last: Vec<(String, Vec<(String, Value)>, Option<Value>)>,
    pub menu: Option<ClickMenu>,
    /** Each virtual list's (bottom aligned, overdraw, follow), to rebuild it when they change. */
    pub lists: HashMap<String, (bool, u32, bool)>,
    pub fonts: HashSet<String>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

pub(crate) fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(&mut u.borrow_mut()))
}

/** Starts a frame: what the last one registered becomes what agents act on. */
pub(crate) fn new_frame() {
    ui(|u| {
        u.tooltips_last = std::mem::take(&mut u.tooltips);
        u.drops_last = std::mem::take(&mut u.drops);
        u.hovers_last = std::mem::take(&mut u.hovers);
        u.keys_last = std::mem::take(&mut u.keys);
        u.focused_last = u.focused.take();
    });
}

pub(crate) fn focus_handle(id: &str, cx: &mut App) -> FocusHandle {
    if let Some(h) = ui(|u| u.focus.get(id).cloned()) {
        return h;
    }
    let h = cx.focus_handle().tab_stop(true);
    ui(|u| u.focus.insert(id.to_string(), h.clone()));
    h
}

/** `theme.fonts`: font files (absolute, ~/, or in the settings folder) loaded once each. */
pub(crate) fn load_fonts(theme: &Map<String, Value>, window: &mut Window) {
    let Some(list) = theme.get("fonts").and_then(Value::as_array) else { return };
    let mut fresh = vec![];
    for p in list.iter().filter_map(Value::as_str) {
        if ui(|u| u.fonts.insert(p.to_string())) {
            let path = std::path::PathBuf::from(crate::script::expand(p));
            let path = if path.is_absolute() { path } else { crate::settings::dir().join(path) };
            if let Ok(bytes) = std::fs::read(&path) {
                fresh.push(std::borrow::Cow::Owned(bytes));
            }
        }
    }
    if !fresh.is_empty() {
        let _ = window.text_system().add_fonts(fresh);
    }
}

/** A left-click menu (`menu_on: "click"`): opens under the node, or closes if it's already open there. */
pub(crate) fn toggle_menu(ws: &mut Workspace, id: &str, spec: &[Value], ctx: &Val, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Workspace>) {
    if ui(|u| u.menu.as_ref().is_some_and(|m| m.id == id)) {
        ui(|u| u.menu = None);
        cx.notify();
        return;
    }
    let entries = ws.menu_entries(spec, ctx);
    let theme = ws.settings.theme.clone();
    let view = cx.entity();
    let menu = PopupMenu::build(window, cx, move |m, window, cx| build_menu(m, entries, &view, &theme, window, cx));
    menu.read(cx).focus_handle(cx).focus(window, cx);
    let dismiss = cx.subscribe_in(&menu, window, |_: &mut Workspace, _, _: &DismissEvent, _, cx| {
        ui(|u| u.menu = None);
        cx.notify();
    });
    ui(|u| u.menu = Some(ClickMenu { id: id.to_string(), at, menu, _dismiss: dismiss }));
    cx.notify();
}

fn field_text(ws: &Workspace, id: &str, cx: &App) -> Option<String> {
    if let Some(e) = ws.inputs.get(id) {
        return Some(e.read(cx).value().to_string());
    }
    super::widgets::textarea(id).map(|e| e.read(cx).value().to_string())
}

/** `ui.click` on a row of a virtual list that isn't drawn: scroll it into view and click again once it is. */
pub(crate) fn reveal_and_click(ws: &mut Workspace, id: Option<&str>, a: &Map<String, Value>) -> bool {
    let Some(id) = id else { return false };
    let tries = a.get("_tries").and_then(Value::as_u64).unwrap_or(0);
    let Some(at) = id.find('[').filter(|_| tries < 3) else { return false };
    let wanted = &id[at..];
    let hit = ws.virtual_lists.values().find_map(|(state, _, rows)| {
        rows.iter().position(|r| {
            let keys = loop_keys(&r.2);
            !keys.is_empty() && wanted.starts_with(&keys)
        })
        .map(|ix| (state.clone(), ix))
    });
    let Some((state, ix)) = hit else { return false };
    state.scroll_to_reveal_item(ix);
    let mut again = a.clone();
    again.insert("_tries".into(), Value::from(tries + 1));
    ws.pending_actions.push(Value::Object(again));
    true
}

fn fill(action: &mut Value, extra: &[(&str, Value)]) {
    let put = |o: &mut Map<String, Value>| {
        for (k, v) in extra {
            o.insert(k.to_string(), v.clone());
        }
    };
    match action {
        Value::Object(o) => put(o),
        Value::Array(list) => list.iter_mut().filter_map(Value::as_object_mut).for_each(put),
        _ => {}
    }
}

/** An action with event data added to it (the dropped item, the wheel delta, the key). */
pub(crate) fn with_args(action: &Value, extra: &[(&str, Value)]) -> Value {
    let mut a = action.clone();
    fill(&mut a, extra);
    a
}

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(ws: &mut Workspace, name: &str, a: &Map<String, Value>, window: &mut Window, cx: &mut Context<Workspace>) -> bool {
    let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string).filter(|x| !x.is_empty());
    match name {
        "ui.type" => {
            let (Some(id), text) = (s("id"), s("text").unwrap_or_default()) else {
                ws.exprs.note("ui.type: needs an id".into());
                return true;
            };
            let replace = a.get("replace").and_then(Value::as_bool) == Some(true);
            let next = |old: &str| if replace { text.clone() } else { format!("{old}{text}") };
            if let Some(e) = ws.inputs.get(&id).cloned() {
                e.update(cx, |st, cx| {
                    let v = next(&st.value());
                    st.set_value(v, window, cx);
                });
            } else if let Some(e) = super::widgets::textarea(&id) {
                e.update(cx, |st, cx| {
                    let v = next(&st.value());
                    st.set_value(v, window, cx);
                });
            } else {
                ws.exprs.note(format!("ui.type: no input {id}"));
            }
        }
        "ui.submit" => {
            let Some(id) = s("id") else { return true };
            let text = field_text(ws, &id, cx).unwrap_or_default();
            match ws.input_submits.get(&id).cloned() {
                Some((action, ctx)) => {
                    let c = with(&ctx, vec![("input".into(), to_val(&Value::String(text)))]);
                    let resolved = ws.exprs.resolve_action(&action, &c);
                    ws.run_action(&resolved, window, cx);
                }
                None => ws.exprs.note(format!("ui.submit: {id} has no on.submit")),
            }
        }
        "ui.focus" => {
            let Some(id) = s("id") else { return true };
            if let Some(e) = ws.inputs.get(&id).cloned() {
                e.update(cx, |st, cx| st.focus(window, cx));
            } else if let Some(e) = super::widgets::textarea(&id) {
                e.update(cx, |st, cx| st.focus(window, cx));
            } else if let Some(h) = ui(|u| u.focus.get(&id).cloned()) {
                h.focus(window, cx);
            } else {
                ws.exprs.note(format!("ui.focus: {id} isn't focusable (give it \"focusable\": true)"));
            }
        }
        "ui.blur" => window.blur(cx),
        "ui.hover" => {
            let off = a.get("off").and_then(Value::as_bool) == Some(true);
            let next = if off { None } else { s("id") };
            let prev = ui(|u| std::mem::replace(&mut u.forced_hover, next.clone()));
            let hovers = ui(|u| u.hovers.iter().chain(u.hovers_last.iter()).map(|(i, st, en)| (i.clone(), st.clone(), en.clone())).collect::<Vec<_>>());
            if let Some(p) = prev.filter(|p| Some(p) != next.as_ref()) {
                if let Some(end) = hovers.iter().find(|h| h.0 == p).and_then(|h| h.2.clone()) {
                    ws.run_action(&end, window, cx);
                }
            }
            if let Some(n) = next {
                if let Some(start) = hovers.iter().find(|h| h.0 == n).and_then(|h| h.1.clone()) {
                    ws.run_action(&start, window, cx);
                }
            }
        }
        "ui.key" => {
            let key = s("key").unwrap_or_default();
            let id = s("id").or_else(|| ui(|u| u.focused_last.clone()));
            let Some(id) = id else {
                ws.exprs.note("ui.key: no id and nothing focused".into());
                return true;
            };
            let entry = ui(|u| u.keys.iter().chain(u.keys_last.iter()).find(|k| k.0 == id).map(|k| (k.1.clone(), k.2.clone())));
            let (map, click) = entry.unwrap_or_default();
            match map.iter().find(|(k, _)| *k == key).map(|(_, a)| a.clone()) {
                Some(action) => ws.run_action(&action, window, cx),
                None => match click.filter(|_| key == "enter" || key == "space") {
                    Some(c) => ws.run_action(&c, window, cx),
                    None => ws.exprs.note(format!("ui.key: {id} has no `{key}` key")),
                },
            }
        }
        "ui.keystroke" => {
            // Through the app's own key handling, from the focused element out, as a key press would go.
            // Once this action is done: the key's own handlers update the workspace too.
            match gpui::Keystroke::parse(&s("key").unwrap_or_default()) {
                Ok(k) => window.defer(cx, move |window, cx| {
                    window.dispatch_keystroke(k, cx);
                }),
                Err(e) => ws.exprs.note(format!("ui.keystroke: {e}")),
            }
        }
        "ui.file_drag" => {
            let n = |k: &str| a.get(k).and_then(Value::as_f64).unwrap_or(0.) as f32;
            let position = point(px(n("x")), px(n("y")));
            let paths = a.get("paths").and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_str).map(std::path::PathBuf::from).collect()).unwrap_or_default();
            if s("phase").as_deref() == Some("sweep") {
                let (to, ms) = (point(px(n("to_x")), px(n("to_y"))), a.get("ms").and_then(Value::as_u64).unwrap_or(1000));
                let steps = (ms / 16).max(1);
                cx.spawn_in(window, async move |_, cx| {
                    for i in 1..=steps {
                        cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                        let t = i as f32 / steps as f32;
                        let at = point(position.x + (to.x - position.x) * t, position.y + (to.y - position.y) * t);
                        let _ = cx.update(|window, cx| window.dispatch_event(gpui::PlatformInput::FileDrop(gpui::FileDropEvent::Pending { position: at }), cx));
                    }
                })
                .detach();
                return true;
            }
            let event = match s("phase").as_deref() {
                Some("enter") => Some(gpui::FileDropEvent::Entered { position, paths: gpui::ExternalPaths(paths) }),
                Some("move") => Some(gpui::FileDropEvent::Pending { position }),
                Some("drop") => Some(gpui::FileDropEvent::Submit { position }),
                Some("exit") => Some(gpui::FileDropEvent::Exited),
                other => {
                    ws.exprs.note(format!("ui.file_drag: phase {other:?} isn't enter, move, exit or drop"));
                    None
                }
            };
            if let Some(e) = event {
                window.defer(cx, move |window, cx| {
                    window.dispatch_event(gpui::PlatformInput::FileDrop(e), cx);
                });
            }
        }
        "ui.text_drag" => {
            let n = |k: &str| a.get(k).and_then(Value::as_f64).unwrap_or(0.) as f32;
            let (from, to) = (point(px(n("x")), px(n("y"))), point(px(n("to_x")), px(n("to_y"))));
            let steps = (a.get("ms").and_then(Value::as_u64).unwrap_or(300) / 16).max(1);
            let modifiers = gpui::Modifiers::default();
            cx.spawn_in(window, async move |_, cx| {
                let press = gpui::MouseDownEvent { button: MouseButton::Left, position: from, modifiers, click_count: 1, first_mouse: false };
                let _ = cx.update(|window, cx| window.dispatch_event(gpui::PlatformInput::MouseDown(press), cx));
                for i in 1..=steps {
                    cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                    let t = i as f32 / steps as f32;
                    let at = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                    let moved = gpui::MouseMoveEvent { position: at, pressed_button: Some(MouseButton::Left), modifiers };
                    let _ = cx.update(|window, cx| window.dispatch_event(gpui::PlatformInput::MouseMove(moved), cx));
                }
                let up = gpui::MouseUpEvent { button: MouseButton::Left, position: to, modifiers, click_count: 1 };
                let _ = cx.update(|window, cx| window.dispatch_event(gpui::PlatformInput::MouseUp(up), cx));
            })
            .detach();
        }
        "ui.scroll" => {
            let target = s("handle").or_else(|| s("id")).unwrap_or_default();
            let to = a.get("to");
            let by = a.get("by").and_then(Value::as_f64).map(|x| x as f32);
            let row = a.get("row").and_then(Value::as_u64).map(|r| r as usize);
            if let Some((state, _, _)) = ws.virtual_lists.get(&target) {
                match (row, to.and_then(Value::as_str), by) {
                    (Some(r), _, _) => state.scroll_to_reveal_item(r),
                    (_, Some("bottom"), _) => state.scroll_to_end(),
                    (_, Some("top"), _) => state.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) }),
                    (_, _, Some(d)) => state.scroll_by(px(d)),
                    _ => {}
                }
            } else if let Some(h) = ws.scroll_handles.get(&target).cloned() {
                let max = h.max_offset();
                let mut o = h.offset();
                match (to, by) {
                    (Some(Value::String(t)), _) if t == "top" => o.y = px(0.),
                    (Some(Value::String(t)), _) if t == "bottom" => o.y = -max.y,
                    (Some(Value::Number(n)), _) => o.y = -px(n.as_f64().unwrap_or(0.) as f32),
                    (_, Some(d)) => o.y -= px(d),
                    _ => {}
                }
                if let Some(x) = a.get("x").and_then(Value::as_f64) {
                    o.x = -px(x as f32);
                }
                o.x = o.x.clamp(-max.x, px(0.));
                o.y = o.y.clamp(-max.y, px(0.));
                h.set_offset(o);
                if let Some(r) = row {
                    h.scroll_to_item(r);
                }
            } else {
                ws.exprs.note(format!("ui.scroll: no scroll node or list `{target}` (inspect.json ui.scrolls)"));
            }
        }
        "ui.drop" => {
            let key = s("key").unwrap_or_default();
            let value = a.get("value").cloned().unwrap_or(Value::Null);
            let kind = s("kind").unwrap_or_else(|| "item".into());
            let found = ui(|u| {
                u.drops.iter().chain(u.drops_last.iter()).find(|d| d.key == key).map(|d| {
                    let spec = d.specs.iter().find(|(accept, _, _)| accept.is_empty() || accept.contains(&kind)).cloned();
                    (spec, d.zones.clone(), d.ctx.clone())
                })
            });
            let Some((spec, zones, ctx)) = found else {
                ws.exprs.note(format!("ui.drop: no drop target `{key}` (inspect.json ui.drops)"));
                return true;
            };
            let Some((_, action, valid)) = spec else {
                ws.exprs.note(format!("ui.drop: `{key}` doesn't accept {kind}"));
                return true;
            };
            let default_zone = match &zones {
                Value::String(m) if m.starts_with("edges") && m != "edges+inside" => "after".to_string(),
                _ => "inside".to_string(),
            };
            let zone = s("zone").unwrap_or(default_zone);
            if let Some(code) = valid {
                let c = with(&ctx, vec![("dragged".into(), to_val(&value)), ("zone".into(), to_val(&Value::from(zone.as_str())))]);
                if !ws.exprs.truthy(&code, &c) {
                    ws.exprs.note(format!("ui.drop: `{key}` refuses that drop (its valid check)"));
                    return true;
                }
            }
            ws.run_action(&with_args(&action, &[("dragged", value), ("zone", Value::String(zone))]), window, cx);
        }
        _ => return false,
    }
    true
}

fn menu_json(entries: Vec<MenuEntry>) -> Vec<Value> {
    entries
        .into_iter()
        .filter_map(|e| match e {
            MenuEntry::Item { label, checked, disabled, .. } => Some(json!({"label": label, "checked": checked, "disabled": disabled})),
            MenuEntry::Submenu { label, children, .. } => Some(json!({"label": label, "items": menu_json(children)})),
            MenuEntry::Separator => None,
        })
        .collect()
}

/** What inspect.json shows under `ui`. */
pub(crate) fn inspect(ws: &Workspace, window: &Window, cx: &App) -> Value {
    let mut inputs = Map::new();
    for (id, e) in &ws.inputs {
        let st = e.read(cx);
        inputs.insert(id.clone(), json!({"value": st.value().to_string(), "focused": st.focus_handle(cx).is_focused(window), "submit": ws.input_submits.contains_key(id)}));
    }
    for (id, e) in super::widgets::textareas() {
        let st = e.read(cx);
        inputs.insert(id.clone(), json!({"value": st.value().to_string(), "focused": st.focus_handle(cx).is_focused(window), "submit": ws.input_submits.contains_key(&id), "multiline": true}));
    }
    let mut scrolls = Map::new();
    for (name, h) in &ws.scroll_handles {
        let (o, m) = (h.offset(), h.max_offset());
        scrolls.insert(name.clone(), json!({"x": -f32::from(o.x), "y": -f32::from(o.y), "max_x": f32::from(m.x), "max_y": f32::from(m.y)}));
    }
    for (id, (state, keys, _)) in &ws.virtual_lists {
        let top = state.logical_scroll_top();
        scrolls.insert(id.clone(), json!({"virtual": true, "rows": keys.len(), "top_row": top.item_ix, "top_offset": f32::from(top.offset_in_item), "following": state.is_following_tail()}));
    }
    let menus: Vec<Value> = ws
        .menus_last
        .iter()
        .map(|(id, tip, spec, ctx)| json!({"id": id, "label": tip, "items": menu_json(ws.menu_entries(spec, ctx))}))
        .collect();
    ui(|u| {
        json!({
            "focused": u.focused_last,
            "hovered": u.hovered.iter().collect::<Vec<_>>(),
            "forced_hover": u.forced_hover,
            "open_menu": u.menu.as_ref().map(|m| m.id.clone()),
            "inputs": inputs,
            "scrolls": scrolls,
            "tooltips": u.tooltips_last.iter().map(|(id, t)| json!({"id": id, "text": t})).collect::<Vec<_>>(),
            "menus": menus,
            "drops": u.drops_last.iter().map(|d| json!({"key": d.key, "zones": d.zones, "accepts": d.specs.iter().flat_map(|s| s.0.clone()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "keys": u.keys_last.iter().map(|(id, k, click)| json!({"id": id, "keys": k.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(), "clicks": click.is_some()})).collect::<Vec<_>>(),
            "hover_events": u.hovers_last.iter().map(|h| h.0.clone()).collect::<Vec<_>>(),
        })
    })
}
