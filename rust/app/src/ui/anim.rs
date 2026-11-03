/*! Spinners drawn over the window, not in it: each leaves an empty slot, and the anim layer (a view of its
own on top) draws every slot's next turn, so a turn redraws that layer alone and nothing around it. */

use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

/** A looping animation where it was last drawn: its box, what clipped it, and its deferred layer (none:
the window's). */
#[derive(Clone)]
struct Slot {
    bounds: Bounds<Pixels>,
    mask: ContentMask<Pixels>,
    layer: Option<usize>,
    period_ms: f32,
    look: Look,
}

#[derive(Clone)]
enum Look {
    /** An icon turning (eased: it speeds up and slows down each turn). */
    Spin { icon: Icon, color: Hsla, ease: bool },
    /** One line of text fading between two opacities (a running step's title). */
    Fade { text: SharedString, style: TextStyle, min: f32, max: f32 },
}

thread_local! {
    /** The slots each region drew (by region id; "" for the window's own nodes). */
    static SLOTS: RefCell<HashMap<String, Vec<Slot>>> = RefCell::new(HashMap::new());
    /** The deferred layer being painted, for the slots inside it. */
    static LAYER: Cell<Option<usize>> = const { Cell::new(None) };
    /** Turns a second (0: spinners hold still, drawn in place). */
    static FPS: Cell<f64> = const { Cell::new(60.) };
    /** When spinners started turning, so they all turn together. */
    static EPOCH: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub(crate) fn set_fps(fps: f64) {
    FPS.set(fps);
}

/** Whether spinners go to the anim layer (else they're drawn in place, holding still). */
pub(crate) fn enabled() -> bool {
    FPS.get() > 0.
}

/** Forgets what `owner` drew: it's drawing again. */
pub(crate) fn clear(owner: &str) {
    SLOTS.with(|s| {
        s.borrow_mut().remove(owner);
    });
}

/** Forgets the slots of regions no longer on screen. */
pub(crate) fn keep(live: &HashSet<String>) {
    SLOTS.with(|s| s.borrow_mut().retain(|owner, _| owner.is_empty() || live.contains(owner)));
}

pub(crate) fn count() -> usize {
    SLOTS.with(|s| s.borrow().values().map(Vec::len).sum())
}

/** Where a spinner `icon` sits: laid out (invisibly) for its size, its turns left to the anim layer. */
pub(crate) fn slot(owner: String, icon: Icon, color: Option<Hsla>, period_ms: f32, ease: bool) -> AnyElement {
    let shown = icon.clone();
    let mark = mark(owner, period_ms, move |window| Look::Spin { icon: shown, color: color.unwrap_or_else(|| window.text_style().color), ease });
    div().relative().flex_none().child(icon.invisible()).child(mark).into_any_element()
}

/** Where a fading line of `text` sits: laid out (invisibly, cut short like it) for its size, faded by the anim layer. */
pub(crate) fn fade(owner: String, text: SharedString, min: f32, max: f32, period_ms: f32) -> AnyElement {
    let shown = text.clone();
    let mark = mark(owner, period_ms, move |window| Look::Fade { text: shown, style: window.text_style(), min, max });
    div().relative().min_w_0().child(div().min_w_0().truncate().invisible().child(text)).child(mark).into_any_element()
}

/** Notes a slot as it's placed (prepaint), before the anim layer places what it draws. */
fn mark(owner: String, period_ms: f32, look: impl FnOnce(&Window) -> Look + 'static) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let slot = Slot { bounds, mask: window.content_mask(), layer: LAYER.get(), period_ms, look: look(window) };
            SLOTS.with(|s| s.borrow_mut().entry(owner).or_default().push(slot));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/** `el` drawn in deferred layer `priority`: spinners inside it turn there too, over what it covers. */
pub(crate) fn in_layer(priority: usize, el: AnyElement) -> AnyElement {
    LayerMark { priority, child: el }.into_any_element()
}

struct LayerMark {
    priority: usize,
    child: AnyElement,
}

impl IntoElement for LayerMark {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for LayerMark {
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

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        let was = LAYER.replace(Some(self.priority));
        self.child.prepaint(window, cx);
        LAYER.set(was);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
    }
}

/** The view that turns every spinner, redrawn alone on each turn. */
#[derive(Default)]
pub struct AnimLayer {
    waiting: Rc<Cell<bool>>,
    last: Rc<Cell<Option<Instant>>>,
}

/** Asks for the next turn on the display frame it's due (one wait at a time). */
fn tick(window: &mut Window, view: EntityId, waiting: Rc<Cell<bool>>, last: Rc<Cell<Option<Instant>>>) {
    window.on_next_frame(move |window, cx| {
        let fps = FPS.get().max(1.);
        if last.get().is_none_or(|t| t.elapsed().as_secs_f64() >= 0.95 / fps) {
            last.set(Some(Instant::now()));
            waiting.set(false);
            cx.notify(view);
        } else {
            tick(window, view, waiting, last);
        }
    });
}

impl Render for AnimLayer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let epoch = EPOCH.with(|e| match e.get() {
            Some(t) => t,
            None => {
                e.set(Some(Instant::now()));
                Instant::now()
            }
        });
        let ms = epoch.elapsed().as_secs_f32() * 1000.;
        // Which slots there are is known only once the window is placed (after this), so each layer that
        // may hold some gets its own: the usual ones, and any in use.
        let mut layers: BTreeSet<Option<usize>> = [None, Some(0), Some(1), Some(2), Some(3)].into_iter().collect();
        SLOTS.with(|s| layers.extend(s.borrow().values().flatten().map(|x| x.layer)));
        let ticks = Ticks { view: cx.entity_id(), waiting: self.waiting.clone(), last: self.last.clone() };
        div().absolute().inset_0().children(layers.into_iter().map(|layer| {
            let el = Turns { layer, ms, ticks: (layer.is_none()).then(|| ticks.clone()), drawn: vec![] };
            match layer {
                None => el.into_any_element(),
                Some(p) => deferred(el).with_priority(p).into_any_element(),
            }
        }))
    }
}

#[derive(Clone)]
struct Ticks {
    view: EntityId,
    waiting: Rc<Cell<bool>>,
    last: Rc<Cell<Option<Instant>>>,
}

/** The slots of one layer, turned to where they are at `ms`. */
struct Turns {
    layer: Option<usize>,
    ms: f32,
    /** The window's own layer keeps the turns coming while there are spinners. */
    ticks: Option<Ticks>,
    drawn: Vec<(AnyElement, ContentMask<Pixels>, Option<TextStyleRefinement>)>,
}

/** Everything a text style sets, as a refinement to draw under. */
fn whole(s: TextStyle) -> TextStyleRefinement {
    TextStyleRefinement {
        color: Some(s.color),
        font_family: Some(s.font_family),
        font_features: Some(s.font_features),
        font_fallbacks: s.font_fallbacks,
        font_size: Some(s.font_size),
        line_height: Some(s.line_height),
        font_weight: Some(s.font_weight),
        font_style: Some(s.font_style),
        background_color: s.background_color,
        underline: s.underline,
        strikethrough: s.strikethrough,
        white_space: Some(s.white_space),
        text_overflow: s.text_overflow,
        text_align: Some(s.text_align),
        line_clamp: s.line_clamp,
    }
}

impl IntoElement for Turns {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Turns {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let style = Style { position: Position::Absolute, inset: Edges::all(px(0.).into()), ..Default::default() };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        let slots: Vec<Slot> = SLOTS.with(|s| s.borrow().values().flatten().filter(|x| x.layer == self.layer).cloned().collect());
        for s in slots {
            let period = s.period_ms.max(1.);
            let t = (self.ms % period) / period;
            let (mut el, text) = match s.look {
                Look::Spin { icon, color, ease } => {
                    let t = if ease { ease_in_out(t) } else { t };
                    let size = s.bounds.size.width.min(s.bounds.size.height);
                    (icon.with_size(size).text_color(color).rotate(percentage(t)).into_any_element(), None)
                }
                Look::Fade { text, style, min, max } => {
                    let o = pulsating_between(min, max)(t);
                    (div().size_full().min_w_0().truncate().opacity(o).child(text).into_any_element(), Some(whole(style)))
                }
            };
            window.with_content_mask(Some(s.mask.clone()), |window| {
                window.with_text_style(text.clone(), |window| el.prepaint_as_root(s.bounds.origin, s.bounds.size.into(), window, cx))
            });
            self.drawn.push((el, s.mask, text));
        }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        for (el, mask, text) in &mut self.drawn {
            window.with_content_mask(Some(mask.clone()), |window| window.with_text_style(text.clone(), |window| el.paint(window, cx)));
        }
        if let Some(t) = self.ticks.take().filter(|_| enabled() && count() > 0) {
            if !t.waiting.replace(true) {
                tick(window, t.view, t.waiting, t.last);
            }
        }
    }
}
