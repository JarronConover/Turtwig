/*! Regions: a node marked `"region": true` draws as its own cached GPUI view, redrawn only when what it
reads changes (see SETTINGS.md); what it registers while drawing is kept for the frames that reuse it. */

use super::expr::first_field;
use crate::workspace::Workspace;
use gpui_kit::*;
use jaq_json::Val;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

thread_local! {
    /** The regions being drawn, innermost last. */
    static OWNERS: RefCell<Vec<String>> = const { RefCell::new(vec![]) };
    /** Drawers with a looping animation that wants frames (at this many a second), and when each last had one. */
    static WANTED: RefCell<HashMap<String, f64>> = RefCell::new(HashMap::new());
    static LAST_TICK: RefCell<HashMap<String, std::time::Instant>> = RefCell::new(HashMap::new());
    /** Animation frames asked for, by drawer, since the last perf summary. */
    pub(crate) static ASKED: RefCell<HashMap<String, u32>> = RefCell::new(HashMap::new());
    /** Drawers waiting on a display frame for their next tick (one wait each). */
    static WAITING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/** An animation drawn by `owner` wants frames, `fps` a second. */
pub(crate) fn want_frame(owner: &str, fps: f64) {
    WANTED.with(|w| {
        let mut w = w.borrow_mut();
        let e = w.entry(owner.to_string()).or_insert(fps);
        *e = e.max(fps);
    });
}

/** After `owner` drew: once its animation's next frame is due, the view drawing now (`owner`'s) draws
again. Display frames count the time (timers lag when the app is in the background). */
pub(crate) fn frame_if_wanted(owner: &str, window: &mut Window) {
    if let Some(fps) = WANTED.with(|w| w.borrow_mut().remove(owner)) {
        if WAITING.with(|w| w.borrow_mut().insert(owner.to_string())) {
            tick_when_due(window, owner.to_string(), window.current_view(), fps);
        }
    }
}

fn tick_when_due(window: &mut Window, owner: String, view: EntityId, fps: f64) {
    window.on_next_frame(move |window, cx| {
        let due = LAST_TICK.with(|t| t.borrow().get(&owner).is_none_or(|at| at.elapsed().as_secs_f64() >= 0.95 / fps.max(1.)));
        if due {
            WAITING.with(|w| w.borrow_mut().remove(&owner));
            LAST_TICK.with(|t| t.borrow_mut().insert(owner.clone(), std::time::Instant::now()));
            ASKED.with(|a| *a.borrow_mut().entry(owner).or_default() += 1);
            cx.notify(view);
        } else {
            tick_when_due(window, owner, view, fps);
        }
    });
}

/** The region whose drawing is under way ("" for the window's own). */
pub(crate) fn owner() -> String {
    OWNERS.with(|o| o.borrow().last().cloned().unwrap_or_default())
}

pub(crate) fn push_owner(id: &str) {
    OWNERS.with(|o| o.borrow_mut().push(id.to_string()));
}

pub(crate) fn pop_owner() {
    OWNERS.with(|o| {
        o.borrow_mut().pop();
    });
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fingerprint(pub u64);

/** Node types that draw from the workspace rather than the context, so they bring their own news. */
pub(crate) const BUILTINS: &[&str] = &["transcript", "composer", "waiting", "screen", "terminal", "diff", "attachments"];

/** The context paths a node's expressions read, the built-ins inside it, and whether it reads it all. */
#[derive(Default, Clone)]
pub(crate) struct Deps {
    pub paths: Vec<Vec<String>>,
    pub builtins: Vec<(String, Value)>,
    pub everything: bool,
}

/** Style keys that place and size a region's box rather than the node inside it. */
const OUTER: &[&str] = &["width", "height", "min_width", "min_height", "max_width", "max_height", "grow", "shrink", "basis", "margin", "position", "top", "left", "right", "bottom", "self", "aspect_ratio", "row_span"];

/** Roots read two levels deep: they hold many unrelated things (the transcript sits in `selected`). */
const DEEP: &[&str] = &["selected", "app", "state", "designer"];

pub(crate) fn deps_of(node: &Value, components: &Map<String, Value>) -> Deps {
    let mut paths = BTreeSet::new();
    let mut builtins = vec![];
    let mut everything = false;
    let mut seen = HashSet::new();
    walk(node, components, &mut paths, &mut builtins, &mut everything, &mut seen);
    Deps { paths: paths.into_iter().collect(), builtins, everything }
}

fn walk(v: &Value, comps: &Map<String, Value>, paths: &mut BTreeSet<Vec<String>>, builtins: &mut Vec<(String, Value)>, all: &mut bool, seen: &mut HashSet<String>) {
    match v {
        Value::String(s) => scan(s, paths, all),
        Value::Array(a) => a.iter().for_each(|x| walk(x, comps, paths, builtins, all, seen)),
        Value::Object(o) => {
            if let Some(kind) = o.get("type").and_then(Value::as_str) {
                if BUILTINS.contains(&kind) {
                    builtins.push((kind.to_string(), o.get("props").cloned().unwrap_or(Value::Null)));
                }
                if let Some(c) = comps.get(kind).filter(|_| seen.insert(kind.to_string())) {
                    walk(c, comps, paths, builtins, all, seen);
                }
            }
            o.values().for_each(|x| walk(x, comps, paths, builtins, all, seen));
        }
        _ => {}
    }
}

/** The paths an expression (or a template) reads: `.a.b` where a path starts. A bare `.`, `..` or `.[`
reads the whole input, so then everything counts. */
fn scan(s: &str, paths: &mut BTreeSet<Vec<String>>, all: &mut bool) {
    let b = s.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'.' {
            i += 1;
            continue;
        }
        let prev = if i == 0 { b' ' } else { b[i - 1] };
        if word(prev) || matches!(prev, b']' | b')' | b'.' | b'$' | b'"' | b'?') {
            i += 1;
            continue;
        }
        let mut parts = vec![];
        let mut j = i;
        while j < b.len() && b[j] == b'.' {
            let start = j + 1;
            let mut k = start;
            while k < b.len() && word(b[k]) {
                k += 1;
            }
            if k == start {
                break;
            }
            parts.push(s[start..k].to_string());
            j = k;
        }
        if parts.is_empty() {
            // A dot before a digit is a number; any other lone one is the whole input.
            if !b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                *all = true;
            }
            i += 1;
            continue;
        }
        let depth = if DEEP.contains(&parts[0].as_str()) { 2 } else { 1 };
        parts.truncate(depth);
        paths.insert(parts);
        i = j;
    }
}

/** Hashes of context values by the `Rc` they live in, so the shared bulk is hashed once per build. Each
entry holds its value, so no other value can come to live at that address while it's remembered. */
#[derive(Default)]
pub(crate) struct HashMemo {
    by_ptr: HashMap<usize, (Val, u64)>,
    generation: u64,
}

impl HashMemo {
    pub fn hash(&mut self, v: &Val) -> u64 {
        let ptr = match v {
            Val::Arr(a) => Rc::as_ptr(a) as *const () as usize,
            Val::Obj(o) => Rc::as_ptr(o) as *const () as usize,
            _ => 0,
        };
        if let Some((_, h)) = self.by_ptr.get(&ptr).filter(|_| ptr != 0) {
            return *h;
        }
        let mut s = std::collections::hash_map::DefaultHasher::new();
        v.hash(&mut s);
        let h = s.finish();
        if ptr != 0 {
            self.by_ptr.insert(ptr, (v.clone(), h));
        }
        h
    }

    /** A new context: the old one's pointers may be reused. */
    pub fn reset(&mut self, generation: u64) {
        if generation != self.generation {
            self.by_ptr.clear();
            self.generation = generation;
        }
    }
}

pub(crate) fn path_value(ctx: &Val, path: &[String]) -> Val {
    path.iter().fold(ctx.clone(), |v, k| first_field(&v, k))
}

/** What a region registered while drawing, for the frames that reuse it. */
#[derive(Default, Clone)]
pub(crate) struct Kept {
    pub clickables: Vec<(String, String, Value)>,
    pub menus: Vec<(String, String, Rc<Vec<Value>>, Val)>,
    pub tooltips: Vec<(String, String)>,
    pub drops: Vec<super::drive::DropTarget>,
    pub hovers: Vec<(String, Option<Value>, Option<Value>)>,
    pub keys: Vec<(String, Vec<(String, Value)>, Option<Value>)>,
    pub focused: Option<String>,
}

/** Where each registry stood when a region began drawing. */
pub(crate) struct Marks {
    clickables: usize,
    menus: usize,
    tooltips: usize,
    drops: usize,
    hovers: usize,
    keys: usize,
    focused: Option<String>,
}

/** A region's view: draws its node through the workspace when GPUI asks, and only then. */
pub struct Region {
    ws: WeakEntity<Workspace>,
    id: String,
    node: Value,
    ctx: Val,
    path: String,
    motion_off: usize,
    /** The size it came out at when last drawn, for laying it out while it's reused. */
    size: Rc<std::cell::Cell<Option<(f32, f32)>>>,
}

impl Render for Region {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(ws) = self.ws.upgrade() else { return div().into_any_element() };
        let me = cx.entity().downgrade();
        let (id, node, ctx, path, motion_off) = (self.id.clone(), self.node.clone(), self.ctx.clone(), self.path.clone(), self.motion_off);
        let el = ws.update(cx, |ws, cx| ws.draw_region(&id, me, &node, &ctx, &path, motion_off, window, cx));
        frame_if_wanted(&self.id, window);
        let size = self.size.clone();
        let measure = canvas(move |b, _, _| size.set(Some((f32::from(b.size.width), f32::from(b.size.height)))), |_, _, _, _| {}).absolute().inset_0();
        div().relative().size_full().flex().flex_col().child(el).child(measure).into_any_element()
    }
}

impl Workspace {
    pub(crate) fn begin_kept(&self) -> Marks {
        let (tooltips, drops, hovers, keys, focused) = super::drive::ui(|u| (u.tooltips.len(), u.drops.len(), u.hovers.len(), u.keys.len(), u.focused.take()));
        Marks { clickables: self.clickables.len(), menus: self.menus.len(), tooltips, drops, hovers, keys, focused }
    }

    /** Draws a list row (lists draw them as they lay out, after their region's drawing) as `owner`'s:
    what it registers is kept with that region, and its animations redraw that region. */
    pub(crate) fn as_owner<R>(&mut self, owner: &str, window: &mut Window, f: impl FnOnce(&mut Self, &mut Window) -> R) -> R {
        let mark = (!owner.is_empty()).then(|| {
            push_owner(owner);
            self.begin_kept()
        });
        let out = f(self, window);
        if let Some(m) = mark {
            pop_owner();
            self.end_kept(owner, m);
        }
        frame_if_wanted(owner, window);
        out
    }

    /** Moves what was registered since `m` to region `id` (added to: a list draws its rows later). */
    pub(crate) fn end_kept(&mut self, id: &str, m: Marks) {
        let kept = self.kept.entry(id.to_string()).or_default();
        kept.clickables.extend(self.clickables.drain(m.clickables.min(self.clickables.len())..));
        kept.menus.extend(self.menus.drain(m.menus.min(self.menus.len())..));
        super::drive::ui(|u| {
            kept.tooltips.extend(u.tooltips.drain(m.tooltips.min(u.tooltips.len())..));
            kept.drops.extend(u.drops.drain(m.drops.min(u.drops.len())..));
            kept.hovers.extend(u.hovers.drain(m.hovers.min(u.hovers.len())..));
            kept.keys.extend(u.keys.drain(m.keys.min(u.keys.len())..));
            if let Some(f) = u.focused.take() {
                kept.focused = Some(f);
            }
            u.focused = m.focused;
        });
    }

    /** Changes when anything a region draws from does: the values at the paths it reads, its built-ins'
    news, and what all regions share (settings, window size, interactions the context doesn't hold). */
    fn region_fingerprint(&self, deps: &Deps, ctx: &Val) -> Fingerprint {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        (self.settings_rev, self.window_size.0.to_bits(), self.window_size.1.to_bits()).hash(&mut s);
        if deps.everything {
            self.ctx_generation.get().hash(&mut s);
        }
        let mut memo = self.hash_memo.borrow_mut();
        memo.reset(self.ctx_generation.get());
        for p in &deps.paths {
            memo.hash(&path_value(ctx, p)).hash(&mut s);
        }
        drop(memo);
        for (kind, props) in &deps.builtins {
            let props = self.exprs.resolve(props, ctx);
            self.builtin_news(kind, &props).hash(&mut s);
        }
        Fingerprint(s.finish())
    }

    fn builtin_news(&self, kind: &str, props: &Value) -> u64 {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        (kind, props.to_string()).hash(&mut s);
        match kind {
            "transcript" | "waiting" | "composer" | "attachments" => {
                let id = props.get("chat").and_then(Value::as_str).map(str::to_string).or_else(|| self.current_chat().map(|c| c.id.clone()));
                id.hash(&mut s);
                if let Some(id) = &id {
                    self.transcripts.get(id).map(|t| t.revision).hash(&mut s);
                    self.chat(id).map(|c| serde_json::to_string(c).unwrap_or_default()).hash(&mut s);
                    self.composer_texts.get(id).hash(&mut s);
                    self.attachments_json(id).to_string().hash(&mut s);
                    self.queued.get(id).hash(&mut s);
                }
            }
            "diff" => {
                self.selected.hash(&mut s);
                for (k, d) in &self.diffs {
                    (k, Rc::as_ptr(&d.files) as *const () as usize, d.loading, &d.error).hash(&mut s);
                }
            }
            _ => self.selected.hash(&mut s),
        }
        s.finish()
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_region(&mut self, id: &str, me: WeakEntity<Region>, node: &Value, ctx: &Val, path: &str, motion_off: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.kept.remove(id);
        self.region_views.insert(id.to_string(), me);
        self.perf.borrow_mut().regions_drawn.push(id.to_string());
        self.motion.borrow_mut().drew(id);
        let m = self.begin_kept();
        push_owner(id);
        let saved = std::mem::replace(&mut self.motion_off, motion_off);
        let mut els = self.render(node, ctx, path, window, cx);
        self.motion_off = saved;
        pop_owner();
        self.end_kept(id, m);
        if els.len() == 1 {
            els.pop().unwrap_or_else(|| div().into_any_element())
        } else {
            div().size_full().flex().flex_col().children(els).into_any_element()
        }
    }

    /** A node marked as a region: a box with the node's own size and placement holding its view, which is
    drawn again only when its fingerprint moved (the node itself fills the box). */
    pub(crate) fn place_region(&mut self, obj: &Map<String, Value>, ctx: &Val, path: &str, id: &str, st: &Map<String, Value>, cx: &mut Context<Self>) -> AnyElement {
        let mut node = obj.clone();
        node.remove("region");
        // Where the node sits and how big it is belong to the box; inside, the node fills it.
        if let Value::Object(style) = node.entry("style").or_insert_with(|| Value::Object(Map::new())) {
            for k in OUTER {
                style.remove(*k);
            }
            style.insert("width".into(), "full".into());
            style.insert("height".into(), "full".into());
        }
        let mut outer: Map<String, Value> = OUTER.iter().filter_map(|k| st.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        let node = Value::Object(node);
        let deps = match self.region_deps.get(path) {
            Some((rev, d)) if *rev == self.settings_rev => d.clone(),
            _ => {
                let d = deps_of(&node, &self.settings.components);
                self.region_deps.insert(path.to_string(), (self.settings_rev, d.clone()));
                d
            }
        };
        let fp = self.region_fingerprint(&deps, ctx);
        if std::env::var("ABX_REGION_DEBUG").is_ok() {
            let mut memo = self.hash_memo.borrow_mut();
            let now: Vec<(String, u64)> = deps.paths.iter().map(|p| (p.join("."), memo.hash(&path_value(ctx, p)))).collect();
            drop(memo);
            let before = self.region_debug.insert(id.to_string(), now.clone()).unwrap_or_default();
            for (k, h) in &now {
                if before.iter().any(|(bk, bh)| bk == k && bh != h) {
                    self.perf.borrow_mut().regions_fresh.push(format!("{id} <- {k}"));
                }
            }
        }
        let ws = cx.entity().downgrade();
        let (entity, fresh) = match self.regions.get(id).cloned() {
            Some((e, old)) if old == fp && !self.refresh_now => (e, false),
            Some((e, _)) => {
                let (n, c, p, mo) = (node, ctx.clone(), path.to_string(), self.motion_off);
                e.update(cx, |r, _| (r.node, r.ctx, r.path, r.motion_off) = (n, c, p, mo));
                (e, true)
            }
            None => {
                let (n, c, p, mo, i) = (node, ctx.clone(), path.to_string(), self.motion_off, id.to_string());
                (cx.new(|_| Region { ws, id: i, node: n, ctx: c, path: p, motion_off: mo, size: Default::default() }), true)
            }
        };
        self.regions.insert(id.to_string(), (entity.clone(), fp));
        self.regions_placed.insert(id.to_string());
        let measured = entity.read(cx).size.get();
        let view = AnyView::from(entity);
        // Along its parent's axis a region's size is its content's; across it the parent stretches it.
        let (axis, along) = if self.in_row { ("width", 2) } else { ("height", 3) };
        let sized_by_content = !outer.contains_key(axis) && !outer.contains_key("grow") && !outer.contains_key("basis");
        let inside = if fresh {
            self.perf.borrow_mut().regions_fresh.push(id.to_string());
            view.into_any_element()
        } else {
            // Reused, it's laid out from the box alone: one sized by its content keeps the height it had.
            if let Some((w, h)) = measured.filter(|_| sized_by_content) {
                outer.insert(axis.into(), Value::from(if along == 2 { w } else { h } as f64));
            }
            let mut full = StyleRefinement::default();
            full.size.width = Some(relative(1.).into());
            full.size.height = Some(relative(1.).into());
            view.cached(full).into_any_element()
        };
        let mut boxed = super::style::apply(div(), &outer, &self.settings.theme).flex().flex_col();
        if fresh && sized_by_content {
            boxed = if self.in_row { boxed.w_auto() } else { boxed.h_auto() };
        }
        boxed.child(inside).into_any_element()
    }

    /** Before a frame: what last frame's regions registered joins the window's own, and regions no longer
    placed are let go. */
    pub(crate) fn settle_regions(&mut self) {
        let placed = std::mem::take(&mut self.regions_placed);
        self.regions.retain(|id, _| placed.contains(id));
        self.kept.retain(|id, _| placed.contains(id));
        self.region_views.retain(|id, _| placed.contains(id));
        for k in self.kept.values() {
            self.clickables.extend(k.clickables.iter().cloned());
            self.menus.extend(k.menus.iter().cloned());
        }
        let kept: Vec<Kept> = self.kept.values().cloned().collect();
        super::drive::ui(|u| {
            for k in &kept {
                u.tooltips.extend(k.tooltips.iter().cloned());
                u.drops.extend(k.drops.iter().cloned());
                u.hovers.extend(k.hovers.iter().cloned());
                u.keys.extend(k.keys.iter().cloned());
                if u.focused.is_none() {
                    u.focused = k.focused.clone();
                }
            }
        });
        self.motion.borrow_mut().live_owners = placed.clone();
        self.live_regions = placed;
        self.refresh_now = std::mem::take(&mut self.refresh_regions);
    }

    /** Asks `owner` (a region, or "" for the window) to draw again. */
    pub(crate) fn notify_owner(&self, owner: &str, cx: &mut Context<Self>) {
        match self.region_views.get(owner).and_then(WeakEntity::upgrade) {
            Some(r) => r.update(cx, |_, cx| cx.notify()),
            None => cx.notify(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::deps_of;
    use serde_json::{json, Map};

    #[test]
    fn reads_the_paths_its_expressions_start() {
        let node = json!({"for": ".app.drag as $d | .sections | map(.x)", "as": "s", "children": [
            {"if": ".state.sidebar_open and (.s.items | length) > 0", "props": {"text": "{{ $d.kind // .selected.chat.name }} 1.5"}}]});
        let d = deps_of(&node, &Map::new());
        let paths: Vec<String> = d.paths.iter().map(|p| p.join(".")).collect();
        assert_eq!(paths, ["app.drag", "s", "sections", "selected.chat", "state.sidebar_open", "x"]);
        assert!(!d.everything);
        assert!(deps_of(&json!({"props": {"text": "{{ . | length }}"}}), &Map::new()).everything);
        assert!(deps_of(&json!({"for": ".items[] | select(.ok)"}), &Map::new()).paths.iter().any(|p| p == &["ok"]));
        assert_eq!(deps_of(&json!({"type": "transcript"}), &Map::new()).builtins.len(), 1);
    }
}
