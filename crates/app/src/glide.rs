//! A mouse wheel moves in notches: GPUI jumps the whole notch at once. Here a notch sets where the scroll or the
//! zoom is going, and every frame covers part of the way that is left, so it slides there. Trackpads already send
//! smooth, small steps: they keep GPUI's own handling.
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How fast a glide closes in: about 95% of the way in 3 × TAU.
pub const TAU: f32 = 0.055;

/// `now` moved toward `to`, `dt` seconds after the last frame. Exact on any frame rate.
pub fn ease(now: f32, to: f32, dt: f32) -> f32 {
    to + (now - to) * (-dt.min(0.1) / TAU).exp()
}

/// The scroll of one container: give it `.track_scroll(&glide.handle)` and `glide.driver()` as a child.
#[derive(Clone, Default)]
pub struct Glide {
    pub handle: ScrollHandle,
    /// Where the scroll is going, and when the last frame moved it.
    to: Rc<Cell<Option<(f32, Instant)>>>,
    bar: Rc<RefCell<Bar>>,
}

/// The scroll bar, drawn over the list's right edge: it shows while the list moves and for a second after, or when
/// the pointer comes near it, then fades. It can be dragged.
#[derive(Default)]
struct Bar {
    moved: Option<Instant>,
    last_y: f32,
    alpha: f32,
    last_frame: Option<Instant>,
    /// Dragging: the pointer's distance from the top of the thumb.
    drag: Option<f32>,
    near: bool,
}

/// How long the bar stays after the list stopped, and how wide the strip where the pointer brings it is.
const BAR_STAYS: Duration = Duration::from_millis(1000);
const BAR_STRIP: f32 = 14.0;

impl Glide {
    /// The thumb: top, height and width, in window coordinates; None when everything fits.
    fn thumb(&self, wide: bool) -> Option<(Bounds<Pixels>, f32)> {
        let b = self.handle.bounds();
        let max = f32::from(self.handle.max_offset().y);
        let h = f32::from(b.size.height);
        if max <= 0.5 || h <= 0.0 {
            return None;
        }
        let th = (h * h / (h + max)).clamp(28.0_f32.min(h), h);
        let ty = f32::from(b.top()) + (h - th) * (-f32::from(self.handle.offset().y) / max).clamp(0.0, 1.0);
        let w = if wide { 9.0 } else { 6.0 };
        Some((Bounds::new(point(b.right() - px(w + 3.0), px(ty)), size(px(w), px(th))), max))
    }

    fn strip(&self) -> Bounds<Pixels> {
        let b = self.handle.bounds();
        Bounds::new(point(b.right() - px(BAR_STRIP), b.top()), size(px(BAR_STRIP), b.size.height))
    }

    /// Moves the scroll so the thumb's top is at `top` (window y).
    fn drag_to(&self, top: f32) {
        let Some((t, max)) = self.thumb(true) else { return };
        let b = self.handle.bounds();
        let room = (f32::from(b.size.height) - f32::from(t.size.height)).max(1.0);
        let frac = ((top - f32::from(b.top())) / room).clamp(0.0, 1.0);
        self.to.set(None);
        self.handle.set_offset(point(self.handle.offset().x, px(-frac * max)));
    }

    fn paint_bar(&self, window: &mut Window) {
        let now = Instant::now();
        let y = f32::from(self.handle.offset().y);
        let near = self.strip().contains(&window.mouse_position());
        let (alpha, wide, moving) = {
            let mut bar = self.bar.borrow_mut();
            if (y - bar.last_y).abs() > 0.1 {
                (bar.moved, bar.last_y) = (Some(now), y);
            }
            bar.near = near;
            let recent = bar.moved.is_some_and(|t| now - t < BAR_STAYS);
            let wide = near || bar.drag.is_some();
            let target = if wide || recent { 1.0 } else { 0.0 };
            let dt = bar.last_frame.map_or(0.0, |l| (now - l).as_secs_f32());
            bar.last_frame = Some(now);
            // Quick to show, slower to fade.
            let tau = if target > bar.alpha { 0.4 } else { 2.5 };
            bar.alpha = ease(bar.alpha, target, dt / tau);
            if (bar.alpha - target).abs() < 0.01 {
                bar.alpha = target;
            }
            (bar.alpha, wide, recent || bar.alpha != target)
        };
        if moving {
            window.request_animation_frame();
        }
        let Some((t, _)) = self.thumb(wide) else { return };
        if alpha > 0.0 {
            let w = t.size.width;
            window.paint_quad(fill(t, rgba(0xffffff00 | ((if wide { 0.45 } else { 0.28 }) * alpha * 255.0) as u32)).corner_radii(w / 2.0));
        }
        // The thumb can be grabbed; a press elsewhere on the strip jumps there.
        let glide = self.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || e.button != MouseButton::Left || !glide.strip().contains(&e.position) {
                return;
            }
            let Some((t, _)) = glide.thumb(true) else { return };
            let py = f32::from(e.position.y);
            let grab = if t.contains(&point(t.origin.x, e.position.y)) { py - f32::from(t.top()) } else { f32::from(t.size.height) / 2.0 };
            glide.bar.borrow_mut().drag = Some(grab);
            glide.drag_to(py - grab);
            cx.stop_propagation();
            window.refresh();
        });
        let glide = self.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let drag = glide.bar.borrow().drag;
            match drag {
                Some(grab) if e.pressed_button == Some(MouseButton::Left) => {
                    glide.drag_to(f32::from(e.position.y) - grab);
                    cx.stop_propagation();
                    window.refresh();
                }
                Some(_) => glide.bar.borrow_mut().drag = None,
                None => {
                    // Coming near the bar shows it; leaving lets it fade.
                    if glide.strip().contains(&e.position) != glide.bar.borrow().near {
                        window.refresh();
                    }
                }
            }
        });
        let glide = self.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _| {
            if phase == DispatchPhase::Capture && glide.bar.borrow_mut().drag.take().is_some() {
                window.refresh();
            }
        });
    }
}

impl Glide {
    /// An invisible child that catches the wheel and moves the scroll each frame.
    pub fn driver(&self) -> impl IntoElement {
        let glide = self.clone();
        // Painted after the list's content (deferred), so the scroll bar is drawn over it.
        deferred(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    glide.paint(window);
                    glide.paint_bar(window);
                },
            )
            .absolute()
            .size_0(),
        )
    }

    fn paint(&self, window: &mut Window) {
        let glide = self.clone();
        let line = window.line_height();
        // Capture runs before GPUI's own scroll (in the bubble phase), which then never sees the notch.
        window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
            let m = e.modifiers;
            if phase != DispatchPhase::Capture || !glide.handle.bounds().contains(&e.position) || off() || m.platform || m.control || m.alt
            {
                return; // with a modifier, the wheel zooms
            }
            if e.delta.precise() {
                glide.to.set(None); // a trackpad took over: stop gliding against it
                return;
            }
            let max = f32::from(glide.handle.max_offset().y);
            if max <= 0.0 {
                return; // nothing to scroll here: an outer scroll may want it
            }
            let from = glide.to.get().map_or(f32::from(glide.handle.offset().y), |t| t.0);
            let to = (from + f32::from(e.delta.pixel_delta(line).y)).clamp(-max, 0.0);
            glide.to.set(Some((to, Instant::now())));
            cx.stop_propagation();
            window.refresh();
        });
        let y = f32::from(self.handle.offset().y);
        trace("scroll", y);
        let Some((to, last)) = self.to.get() else { return };
        let now = Instant::now();
        let next = ease(y, to, (now - last).as_secs_f32());
        if (next - to).abs() < 0.5 {
            self.handle.set_offset(point(self.handle.offset().x, px(to)));
            self.to.set(None);
        } else {
            self.handle.set_offset(point(self.handle.offset().x, px(next)));
            self.to.set(Some((to, now)));
        }
        window.request_animation_frame();
    }
}

/// `MDB_GLIDE_OFF=1` (snapshot builds) turns gliding off, to measure the notches it replaces.
pub fn off() -> bool {
    #[cfg(feature = "snapshot")]
    return std::env::var("MDB_GLIDE_OFF").is_ok();
    #[cfg(not(feature = "snapshot"))]
    false
}

/// Snapshot builds print `trace <what> <ms> <value>` once a wheel test has started, to chart the motion.
#[cfg(feature = "snapshot")]
pub static TRACE_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn trace(what: &str, value: f32) {
    #[cfg(feature = "snapshot")]
    if let Some(start) = TRACE_START.get() {
        eprintln!("trace {what} {:.1} {value:.3}", start.elapsed().as_secs_f32() * 1000.0);
    }
    #[cfg(not(feature = "snapshot"))]
    let _ = (what, value);
}

#[cfg(test)]
mod tests {
    use super::ease;

    #[test]
    fn same_path_at_any_frame_rate() {
        let (mut a, mut b) = (0.0, 0.0);
        for _ in 0..12 {
            a = ease(a, 100.0, 1.0 / 60.0);
        }
        for _ in 0..24 {
            b = ease(b, 100.0, 1.0 / 120.0);
        }
        assert!((a - b).abs() < 0.01 && a > 90.0 && a < 100.0, "{a} {b}");
    }
}

/// Opacities that fade to where they should be instead of switching: titles appearing on hover, the rest of the map
/// dimming around a focus. Each frame asks for every value it draws; values nobody asked for are dropped.
#[derive(Default)]
pub struct Fades {
    shown: std::collections::HashMap<u64, (f32, u64)>,
    frame: u64,
    dt: f32,
    last: Option<Instant>,
    /// Something is still fading: draw another frame.
    pub moving: bool,
}

/// How fast a fade closes in (about 95% in 3 × FADE_TAU).
const FADE_TAU: f32 = 0.06;

impl Fades {
    pub fn begin(&mut self) {
        let now = Instant::now();
        self.dt = self.last.map_or(0.0, |l| (now - l).as_secs_f32().min(0.1));
        (self.last, self.moving) = (Some(now), false);
        self.frame += 1;
    }

    /// The value of `key` this frame, on its way to `to`; a new key starts at `from`.
    pub fn get(&mut self, key: u64, to: f32, from: f32) -> f32 {
        let (frame, k) = (self.frame, (-self.dt / FADE_TAU).exp());
        let e = self.shown.entry(key).or_insert((from, frame));
        let mut v = to + (e.0 - to) * k;
        if off() || (v - to).abs() < 0.005 {
            v = to;
        } else {
            self.moving = true;
        }
        *e = (v, frame);
        v
    }

    pub fn end(&mut self) {
        let frame = self.frame;
        self.shown.retain(|_, v| v.1 == frame);
    }
}

/// Draws `cx`'s view again on the next frame. `Window::request_animation_frame` asks for the view being painted,
/// which during `render` may be another one (the window's root): then the frame never comes and animations stall.
pub fn again<V: 'static>(window: &Window, cx: &mut Context<V>) {
    let view = cx.entity().downgrade();
    window.on_next_frame(move |_, cx| {
        view.update(cx, |_, cx| cx.notify()).ok();
    });
}
