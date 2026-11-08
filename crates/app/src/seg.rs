//! A segmented control (tabs, ranges, sorts): its highlight slides to the chosen option instead of jumping.
use crate::glide;
use crate::ui::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

/// One control's highlight. Keep it in the view, one per control.
#[derive(Clone, Default)]
pub struct Seg(Rc<RefCell<State>>);

#[derive(Default)]
struct State {
    /// Where the control and each option were laid out last frame.
    container: Bounds<Pixels>,
    items: Vec<Bounds<Pixels>>,
    /// The highlight on screen: left and width, from the control's inner edge.
    x: f32,
    w: f32,
    placed: bool,
    last: Option<Instant>,
}

/// Records where an element was laid out.
fn probe(state: Rc<RefCell<State>>, item: Option<usize>) -> impl IntoElement {
    canvas(
        move |b, _, _| {
            let mut s = state.borrow_mut();
            match item {
                Some(i) if i < s.items.len() => s.items[i] = b,
                Some(_) => {}
                None => s.container = b,
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

impl Seg {
    /// `items` are the options, without a background of their own; the one at `selected` gets the highlight.
    pub fn render<V: 'static>(&self, selected: usize, items: Vec<AnyElement>, window: &mut Window, cx: &mut Context<V>) -> Div {
        let (x, w, placed) = {
            let mut st = self.0.borrow_mut();
            st.items.resize(items.len(), Bounds::default());
            let origin = st.container.origin.x;
            let target = st
                .items
                .get(selected)
                .filter(|b| b.size.width > px(0.0))
                .map(|b| (f32::from(b.origin.x - origin), f32::from(b.size.width)));
            let now = Instant::now();
            match target {
                Some((tx, tw)) if !st.placed => (st.x, st.w, st.placed) = (tx, tw, true),
                Some((tx, tw)) => {
                    let dt = st.last.map_or(0.0, |l| (now - l).as_secs_f32()) * 0.7;
                    (st.x, st.w) = (glide::ease(st.x, tx, dt), glide::ease(st.w, tw, dt));
                    if (st.x - tx).abs() < 0.3 && (st.w - tw).abs() < 0.3 {
                        (st.x, st.w) = (tx, tw);
                    } else {
                        glide::again(window, cx);
                    }
                }
                None => glide::again(window, cx), // not measured yet: the next frame places it
            }
            st.last = Some(now);
            (st.x, st.w, st.placed)
        };
        div()
            .relative()
            .flex()
            .items_center()
            .p(px(2.0))
            .rounded(px(8.0))
            .bg(panel())
            .border_1()
            .border_color(line())
            .child(probe(self.0.clone(), None))
            .when(placed, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .bottom(px(2.0))
                        .left(px(x))
                        .w(px(w))
                        .rounded(px(6.0))
                        .bg(panel_hi())
                        .border_1()
                        .border_color(rgba(0xffffff14)),
                )
            })
            .children(items.into_iter().enumerate().map(|(i, item)| div().relative().child(item).child(probe(self.0.clone(), Some(i)))))
    }
}
