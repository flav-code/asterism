//! A mouse wheel moves in notches: GPUI jumps the whole notch at once. Here a notch sets where the scroll or the
//! zoom is going, and every frame covers part of the way that is left, so it slides there. Trackpads already send
//! smooth, small steps: they keep GPUI's own handling.
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

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
}

impl Glide {
    /// An invisible child that catches the wheel and moves the scroll each frame.
    pub fn driver(&self) -> impl IntoElement {
        let glide = self.clone();
        canvas(|_, _, _| {}, move |_, _, window, _| glide.paint(window)).absolute().size_0()
    }

    fn paint(&self, window: &mut Window) {
        let glide = self.clone();
        let line = window.line_height();
        // Capture runs before GPUI's own scroll (in the bubble phase), which then never sees the notch.
        window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || !glide.handle.bounds().contains(&e.position) || off() {
                return;
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
