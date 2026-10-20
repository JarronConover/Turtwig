/*! Layout motion: a node whose position changes after a setting, toggle, selection, or new data
glides there instead of jumping. Positions are relative to the nearest moving ancestor. */

use gpui_kit::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq)]
pub enum Easing {
    Linear,
    EaseOut,
    /** Ease out, stronger: what enter animations use unless told otherwise. */
    EaseOutQuart,
    /** Overshoot and settle: `c1` and `c3` shape it (1.2, 2.2 by default; `motion.move.spring`). */
    Spring(f32, f32),
}

impl Easing {
    pub fn parse(s: Option<&str>) -> Self {
        Self::parse_or(s, Self::Spring(1.2, 2.2))
    }

    /** "linear", "ease_out", "ease_out_quart", or "spring"; anything else is `default`. */
    pub fn parse_or(s: Option<&str>, default: Self) -> Self {
        match s {
            Some("linear") => Self::Linear,
            Some("ease_out") => Self::EaseOut,
            Some("ease_out_quart") => Self::EaseOutQuart,
            Some("spring") => Self::Spring(1.2, 2.2),
            _ => default,
        }
    }

    /** A spring's shape from `{"c1", "c3"}`; other easings unchanged. */
    pub fn with_spring(self, spring: Option<&serde_json::Value>) -> Self {
        match (self, spring) {
            (Self::Spring(a, b), Some(v)) => Self::Spring(
                v.get("c1").and_then(serde_json::Value::as_f64).map(|x| x as f32).unwrap_or(a),
                v.get("c3").and_then(serde_json::Value::as_f64).map(|x| x as f32).unwrap_or(b),
            ),
            (e, _) => e,
        }
    }

    pub fn apply(self, t: f32) -> f32 {
        match self {
            Self::Linear => t,
            Self::EaseOut => 1. - (1. - t).powi(3),
            Self::EaseOutQuart => 1. - (1. - t).powi(4),
            Self::Spring(c1, c3) => 1. + c3 * (t - 1.).powi(3) + c1 * (t - 1.).powi(2),
        }
    }
}

struct Entry {
    rel: Point<f32>,
    from: Point<f32>,
    start: Instant,
    seen: u64,
}

pub struct MotionStore {
    entries: HashMap<String, Entry>,
    stack: Vec<Point<f32>>,
    /** The current render; moves only start on a `trigger` render, so scrolling never animates. */
    pub frame: u64,
    pub trigger: u64,
    pub duration_ms: f32,
    pub easing: Easing,
    pub enabled: bool,
    viewport: Size<Pixels>,
    snap_frame: u64,
    /** Where nodes with their own `id` ended up this frame, for `inspect.json`. */
    pub bounds: HashMap<String, [f32; 4]>,
    /** Nodes whose bounds a `Track` recorded (they don't move), by the frame it last did. */
    tracked: HashMap<String, u64>,
    /** The region that drew each node, and the regions still on screen: a reused one's nodes stay. */
    owners: HashMap<String, String>,
    pub live_owners: std::collections::HashSet<String>,
    /** Each region's last two draws (frames): a reused region's nodes count as shown since its last. */
    draws: HashMap<String, (u64, u64)>,
}

impl Default for MotionStore {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            stack: vec![],
            frame: 0,
            trigger: 0,
            duration_ms: 280.,
            easing: Easing::Spring(1.2, 2.2),
            enabled: true,
            viewport: Size::default(),
            snap_frame: 0,
            bounds: HashMap::new(),
            tracked: HashMap::new(),
            owners: HashMap::new(),
            live_owners: Default::default(),
            draws: HashMap::new(),
        }
    }
}

impl MotionStore {
    /** Was this node on screen in the previous render? */
    pub fn was_visible(&self, key: &str) -> bool {
        self.entries.get(key).is_some_and(|e| self.recent(e.seen))
    }

    /** Whether something last drawn at frame `seen` was drawn by the previous draw of the region drawing
    now (for the window, the previous frame; a reused region's last draw may be long ago). */
    pub fn recent(&self, seen: u64) -> bool {
        let owner = crate::ui::region::owner();
        seen + 1 >= self.frame || self.draws.get(&owner).is_some_and(|(_, prev)| seen >= *prev)
    }

    /** Notes that region `owner` draws in this frame. */
    pub fn drew(&mut self, owner: &str) {
        let frame = self.frame;
        let d = self.draws.entry(owner.to_string()).or_insert((frame, frame));
        if d.0 != frame {
            *d = (frame, d.0);
        }
    }

    pub fn has_history(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn prune(&mut self) {
        let frame = self.frame;
        let (owners, live) = (&self.owners, &self.live_owners);
        let kept = |k: &String| owners.get(k).is_some_and(|o| !o.is_empty() && live.contains(o));
        self.entries.retain(|k, e| e.seen + 2 >= frame || kept(k));
        self.tracked.retain(|k, seen| *seen + 2 >= frame || kept(k));
        let (entries, tracked) = (&self.entries, &self.tracked);
        self.bounds.retain(|k, _| entries.contains_key(k) || tracked.contains_key(k));
        self.owners.retain(|k, _| entries.contains_key(k) || tracked.contains_key(k));
        let live = &self.live_owners;
        self.draws.retain(|o, _| live.contains(o));
    }

    /** Notes which region is drawing `key`. */
    pub fn own(&mut self, key: &str) {
        self.owners.insert(key.to_string(), crate::ui::region::owner());
    }

    fn offset(&mut self, key: &str, origin: Point<Pixels>, window: &mut Window) -> Point<f32> {
        let viewport = window.viewport_size();
        if viewport != self.viewport {
            self.viewport = viewport;
            self.snap_frame = self.frame;
        }
        let parent = self.stack.last().copied().unwrap_or_default();
        let rel = point(f32::from(origin.x) - parent.x, f32::from(origin.y) - parent.y);
        let now = Instant::now();
        let frame = self.frame;
        let may_move = self.enabled && self.frame == self.trigger && self.snap_frame != self.frame;
        let (duration, easing) = (self.duration_ms.max(1.), self.easing);
        let current = |e: &Entry| {
            let t = (now.duration_since(e.start).as_secs_f32() * 1000. / duration).min(1.);
            let k = 1. - easing.apply(t);
            point(e.from.x * k, e.from.y * k)
        };
        let e = self.entries.entry(key.to_string()).or_insert(Entry { rel, from: Point::default(), start: now, seen: frame });
        if !(e.seen + 1 >= frame || self.draws.get(&crate::ui::region::owner()).is_some_and(|(_, prev)| e.seen >= *prev)) {
            *e = Entry { rel, from: Point::default(), start: now, seen: frame };
        } else if (rel.x - e.rel.x).abs() > 0.5 || (rel.y - e.rel.y).abs() > 0.5 {
            let cur = if may_move { current(e) } else { Point::default() };
            e.from = if may_move { point(e.rel.x + cur.x - rel.x, e.rel.y + cur.y - rel.y) } else { Point::default() };
            e.start = now;
            e.rel = rel;
        }
        e.seen = frame;
        let off = current(e);
        if off.x.abs() > 0.1 || off.y.abs() > 0.1 {
            window.request_animation_frame();
            off
        } else {
            Point::default()
        }
    }
}

/** Shares its child's layout node and shifts the child's prepaint (hitboxes, painting) by the glide offset. */
pub struct Motion {
    key: String,
    child: AnyElement,
    store: Rc<RefCell<MotionStore>>,
}

impl Motion {
    pub fn new(key: String, child: AnyElement, store: Rc<RefCell<MotionStore>>) -> Self {
        store.borrow_mut().own(&key);
        Self { key, child, store }
    }
}

impl IntoElement for Motion {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Motion {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        let off = self.store.borrow_mut().offset(&self.key, bounds.origin, window);
        if !self.key.contains('/') {
            let b = [f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height)];
            self.store.borrow_mut().bounds.insert(self.key.clone(), b);
        }
        let actual = point(f32::from(bounds.origin.x) + off.x, f32::from(bounds.origin.y) + off.y);
        self.store.borrow_mut().stack.push(actual);
        window.with_element_offset(point(px(off.x), px(off.y)), |window| {
            self.child.prepaint(window, cx);
        });
        self.store.borrow_mut().stack.pop();
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
    }
}

/** Records where a node that doesn't glide ended up (`inspect.json` layout), without moving it. */
pub struct Track {
    key: String,
    child: AnyElement,
    store: Rc<RefCell<MotionStore>>,
}

impl Track {
    pub fn new(key: String, child: AnyElement, store: Rc<RefCell<MotionStore>>) -> Self {
        store.borrow_mut().own(&key);
        Self { key, child, store }
    }
}

impl IntoElement for Track {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Track {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        {
            let mut store = self.store.borrow_mut();
            let frame = store.frame;
            store.tracked.insert(self.key.clone(), frame);
            let b = [f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height)];
            store.bounds.insert(self.key.clone(), b);
        }
        self.child.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
    }
}
