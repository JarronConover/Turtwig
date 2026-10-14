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
    Spring,
}

impl Easing {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("linear") => Self::Linear,
            Some("ease_out") => Self::EaseOut,
            _ => Self::Spring,
        }
    }

    pub fn apply(self, t: f32) -> f32 {
        match self {
            Self::Linear => t,
            Self::EaseOut => 1. - (1. - t).powi(3),
            Self::Spring => {
                let (c1, c3) = (1.2_f32, 2.2_f32);
                1. + c3 * (t - 1.).powi(3) + c1 * (t - 1.).powi(2)
            }
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
}

impl Default for MotionStore {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            stack: vec![],
            frame: 0,
            trigger: 0,
            duration_ms: 280.,
            easing: Easing::Spring,
            enabled: true,
            viewport: Size::default(),
            snap_frame: 0,
        }
    }
}

impl MotionStore {
    /** Was this node on screen in the previous render? */
    pub fn was_visible(&self, key: &str) -> bool {
        self.entries.get(key).is_some_and(|e| e.seen + 1 >= self.frame)
    }

    pub fn has_history(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn prune(&mut self) {
        let frame = self.frame;
        self.entries.retain(|_, e| e.seen + 2 >= frame);
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
        if e.seen + 1 < frame {
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
