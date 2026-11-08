//! The window: a header (counters, tabs, range, search, filters) above the tab on screen.
use crate::map::{MapEvent, MapView};
use crate::state::{Filters, RANGES, norm, visible};
use crate::ui::blue; // over gpui's color of the same name
use crate::ui::*;
use crate::view::Machine;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mdb_core::graph::{Graph, NodeKind, UsageView};
use std::sync::Arc;
use tokio::sync::watch;

pub type GraphFeed = watch::Receiver<Option<(Arc<Graph>, Arc<UsageView>)>>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Map,
    Timeline,
    Usage,
    Machine,
}

pub struct Shell {
    tab: Tab,
    filters: Filters,
    graph: Option<Arc<Graph>>,
    usage: Option<Arc<UsageView>>,
    map: Entity<MapView>,
    machine: Entity<Machine>,
    search: Entity<InputState>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
    #[cfg(feature = "snapshot")]
    snapshot_taken: bool,
}

impl Shell {
    pub fn new(machine: Entity<Machine>, mut feed: GraphFeed, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let map = cx.new(MapView::new);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search   /"));
        let subs = vec![
            cx.subscribe(&map, |_, _, e: &MapEvent, cx| match e {
                MapEvent::Select => cx.notify(),
            }),
            cx.subscribe_in(&search, window, |s, input, e: &InputEvent, _, cx| {
                if let InputEvent::Change = e {
                    s.filters.query = norm(input.read(cx).value().trim());
                    s.push(cx);
                }
            }),
        ];
        let updates = cx.spawn(async move |this, cx| {
            while feed.changed().await.is_ok() {
                let Some((graph, usage)) = feed.borrow_and_update().clone() else { continue };
                let alive = this.update(cx, |s, cx| {
                    (s.graph, s.usage) = (Some(graph), Some(usage));
                    s.push(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let tab = match std::env::var("MDB_TAB").as_deref() {
            Ok("timeline") => Tab::Timeline,
            Ok("usage") => Tab::Usage,
            Ok("machine") => Tab::Machine,
            _ => Tab::Map,
        };
        let mut filters = Filters::default();
        if let Ok(range) = std::env::var("MDB_RANGE") {
            filters.range_h = RANGES.iter().find(|(label, _)| *label == range).map_or(filters.range_h, |r| r.1);
        }
        Shell {
            tab,
            filters,
            graph: None,
            usage: None,
            map,
            machine,
            search,
            _tasks: vec![updates],
            _subs: subs,
            #[cfg(feature = "snapshot")]
            snapshot_taken: false,
        }
    }

    /// Hands the latest data and filters to the views.
    fn push(&mut self, cx: &mut Context<Self>) {
        if let Some(g) = self.graph.clone() {
            let filters = self.filters.clone();
            self.map.update(cx, |m, cx| m.set_graph(g, &filters, cx));
        }
        cx.notify();
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.map.update(cx, |m, _| m.active = tab == Tab::Map);
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let seg = |items: Vec<Stateful<Div>>| {
            div().flex().items_center().p(px(2.0)).rounded(px(8.0)).bg(panel()).border_1().border_color(line()).children(items)
        };
        let button = |id: SharedString, label: SharedString, on: bool| {
            div()
                .id(id)
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_size(px(12.0))
                .text_color(if on { text() } else { muted() })
                .when(on, |d| d.bg(panel_hi()))
                .child(label)
        };
        let tabs = [(Tab::Map, "Map"), (Tab::Timeline, "Timeline"), (Tab::Usage, "Usage"), (Tab::Machine, "Machine")]
            .into_iter()
            .map(|(tab, label)| {
                button(format!("tab-{label}").into(), label.into(), self.tab == tab)
                    .on_click(cx.listener(move |s, _, _, cx| s.set_tab(tab, cx)))
            })
            .collect();
        let ranges = RANGES
            .into_iter()
            .map(|(label, h)| {
                button(format!("range-{label}").into(), label.into(), self.filters.range_h == h).on_click(cx.listener(
                    move |s, _, _, cx| {
                        s.filters.range_h = h;
                        s.push(cx);
                    },
                ))
            })
            .collect();
        let toggle = |id: &'static str, label: &'static str, on: bool, flip: fn(&mut Filters)| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(30.0))
                .px(px(10.0))
                .rounded(px(8.0))
                .bg(panel())
                .border_1()
                .border_color(line())
                .cursor_pointer()
                .text_size(px(12.0))
                .child(
                    div()
                        .size(px(12.0))
                        .rounded(px(3.0))
                        .border_1()
                        .border_color(if on { blue() } else { muted() })
                        .when(on, |d| d.bg(blue())),
                )
                .child(label)
                .on_click(cx.listener(move |s, _, _, cx| {
                    flip(&mut s.filters);
                    s.push(cx);
                }))
        };
        let stats = self.graph.as_ref().map(|g| {
            let v = visible(g, &self.filters, now_ms());
            let sessions = v.nodes.iter().filter(|&&i| crate::state::is_session(&g.nodes[i])).count();
            let agents = v.nodes.iter().filter(|&&i| g.nodes[i].kind == NodeKind::Agent).count();
            let messages = v.messages.iter().filter(|&&m| g.messages[m].kind == mdb_core::graph::MsgKind::Message).count();
            let stat = |n: usize, label: &str| {
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(n.to_string()))
                    .child(div().text_color(muted()).child(label.to_string()))
            };
            div()
                .flex()
                .gap(px(12.0))
                .text_size(px(12.0))
                .child(stat(sessions, "sessions"))
                .child(stat(messages, "messages"))
                .child(stat(agents, "subagents"))
        });
        let map_tab = matches!(self.tab, Tab::Map | Tab::Timeline);
        div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(line())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(dot(blue()).size(px(12.0)))
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).child("my-dev-business")),
            )
            .children(stats.filter(|_| map_tab))
            .child(div().flex_1())
            .child(seg(tabs))
            .when(map_tab, |d| {
                d.child(seg(ranges))
                    .child(div().w(px(150.0)).child(Input::new(&self.search)))
                    .child(toggle("archived", "Archived", self.filters.archived, |f| f.archived = !f.archived))
                    .child(toggle("agents", "Subagents", self.filters.agents, |f| f.agents = !f.agents))
            })
    }
}

#[cfg(feature = "snapshot")]
impl Shell {
    /// `MDB_SNAPSHOT=<file.png>` with `MDB_TAB` (map, timeline, usage): once the tab has its data and stands still,
    /// save the frame and quit. The Machine tab takes its own snapshots.
    fn snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(path) = std::env::var("MDB_SNAPSHOT") else { return };
        let ready = match self.tab {
            Tab::Map => self.map.read(cx).settled(),
            Tab::Machine => return,
            _ => self.usage.as_ref().is_some_and(|u| u.ready),
        };
        if self.snapshot_taken || !ready {
            window.request_animation_frame();
            return;
        }
        self.snapshot_taken = true;
        window.on_next_frame(move |window, cx| {
            match window.render_to_image() {
                Ok(image) => {
                    if let Err(e) = image.save(&path) {
                        eprintln!("snapshot: {e}");
                    }
                }
                Err(e) => eprintln!("snapshot: {e}"),
            }
            cx.quit();
        });
    }
}

/// `MDB_WHEEL=x,y,lines,count,gap_ms`: once the tab is ready, put the mouse at (x, y) and send `count` mouse-wheel
/// notches of `lines` each, `gap_ms` apart. The views print their motion (`trace …` on stderr) and frames are saved
/// as `MDB_WHEEL_FRAMES/<ms>.png` at the `MDB_WHEEL_MARKS` (ms after the first notch); then it quits.
#[cfg(feature = "snapshot")]
#[derive(Clone)]
struct Wheel {
    at: Point<Pixels>,
    lines: f32,
    count: usize,
    gap: f32,
    frames: Option<String>,
    marks: Vec<f32>,
}

#[cfg(feature = "snapshot")]
impl Shell {
    fn wheel_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(spec) = std::env::var("MDB_WHEEL") else { return };
        let ready = match self.tab {
            Tab::Map => self.map.read(cx).settled(),
            Tab::Machine => self.machine.read(cx).ready(),
            _ => false,
        };
        if self.snapshot_taken || !ready {
            window.request_animation_frame();
            return;
        }
        self.snapshot_taken = true;
        let v: Vec<f32> = spec.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        let marks = std::env::var("MDB_WHEEL_MARKS").unwrap_or_default();
        let wheel = Wheel {
            at: point(px(v[0]), px(v[1])),
            lines: v[2],
            count: v[3] as usize,
            gap: v[4],
            frames: std::env::var("MDB_WHEEL_FRAMES").ok(),
            marks: marks.split(',').filter_map(|x| x.trim().parse().ok()).collect(),
        };
        let at = wheel.at;
        window.on_next_frame(move |window, cx| {
            let moved = MouseMoveEvent { position: at, pressed_button: None, modifiers: Modifiers::default() };
            window.dispatch_event(PlatformInput::MouseMove(moved), cx);
            window.refresh();
            // A few frames later, so the move has been hit-tested and the views have drawn under the mouse.
            window.on_next_frame(move |window, _| {
                window.refresh();
                window.on_next_frame(move |window, _| {
                    crate::glide::TRACE_START.set(std::time::Instant::now()).ok();
                    wheel_frames(window, wheel, 0, 0);
                });
            });
        });
    }
}

#[cfg(feature = "snapshot")]
fn wheel_frames(window: &mut Window, w: Wheel, sent: usize, saved: usize) {
    window.on_next_frame(move |window, cx| {
        let ms = crate::glide::TRACE_START.get().map_or(0.0, |s| s.elapsed().as_secs_f32() * 1000.0);
        let mut sent = sent;
        while sent < w.count && ms >= sent as f32 * w.gap {
            let e = ScrollEvent {
                position: w.at,
                delta: ScrollDelta::Lines(point(0.0, w.lines)),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            };
            eprintln!("trace notch {ms:.1} {}", w.lines);
            window.dispatch_event(PlatformInput::Scroll(e), cx);
            sent += 1;
        }
        let mut saved = saved;
        if let Some(dir) = &w.frames
            && saved < w.marks.len()
            && ms >= w.marks[saved]
        {
            if let Ok(image) = window.render_to_image() {
                image.save(format!("{dir}/{:04}.png", w.marks[saved] as u32)).ok();
            }
            saved += 1;
        }
        window.refresh();
        if ms < (w.count as f32 - 1.0) * w.gap + 700.0 { wheel_frames(window, w, sent, saved) } else { cx.quit() }
    });
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "snapshot")]
        {
            self.snapshot(window, cx);
            self.wheel_test(window, cx);
        }
        #[cfg(not(feature = "snapshot"))]
        let _ = window;
        let body: AnyElement = match self.tab {
            Tab::Map => self.map.clone().into_any_element(),
            Tab::Machine => self.machine.clone().into_any_element(),
            Tab::Timeline | Tab::Usage => {
                div().size_full().flex().items_center().justify_center().text_color(muted()).child("Coming next").into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg())
            .text_color(text())
            .text_size(px(13.0))
            .child(self.header(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
