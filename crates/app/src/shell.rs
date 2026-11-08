//! The window: a header (counters, tabs, range, search, project, filters, panel, replay) above the tab on screen.
//! The map and the timeline share a side panel, a live feed and a replay of the range's messages.
use crate::dash::{DashEvent, DashView};
use crate::map::{MapEvent, MapView};
use crate::panel::{self, Panel, PanelEvent};
use crate::settings::{self, Settings};
use crate::state::{Filters, RANGES, is_session, norm, visible, working};
use crate::timeline::{TimelineEvent, TimelineView};
use crate::ui::*;
use crate::ui::{blue, green, red}; // over gpui's colors of the same names
use crate::view::Machine;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mdb_core::graph::{Graph, Msg, MsgKind, NodeKind, UsageView};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub type GraphFeed = watch::Receiver<Option<(Arc<Graph>, Arc<UsageView>)>>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Map,
    Timeline,
    Usage,
    Machine,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Map => "map",
            Tab::Timeline => "timeline",
            Tab::Usage => "usage",
            Tab::Machine => "machine",
        }
    }
    fn from(name: &str) -> Tab {
        match name {
            "timeline" => Tab::Timeline,
            "usage" => Tab::Usage,
            "machine" => Tab::Machine,
            _ => Tab::Map,
        }
    }
}

/// How long the side panel takes to open or close, and a feed line to slide in.
const PANEL_TAU: f32 = 0.06;
const SLIDE_IN: Duration = Duration::from_millis(450);
const FEED_MAX: usize = 7;

/// One line of the live feed.
struct FeedEvent {
    seq: u64,
    ts: u64,
    color: Rgba,
    who: String,
    what: String,
    /// The session a click selects.
    id: String,
}

/// Every message of the range again, fast-forwarded: comets on the map and lines in the feed.
struct Replay {
    msgs: Vec<usize>,
    t0: u64,
    span: u64,
    /// Seconds the whole range takes.
    dur: f32,
    /// When the replay's time 0 was.
    base: Instant,
    next: usize,
}

impl Replay {
    fn at(&self, g: &Graph, i: usize) -> f32 {
        (g.messages[self.msgs[i]].ts.saturating_sub(self.t0)) as f32 / self.span as f32 * self.dur
    }
}

pub struct Shell {
    tab: Tab,
    filters: Filters,
    graph: Option<Arc<Graph>>,
    usage: Option<Arc<UsageView>>,
    map: Entity<MapView>,
    timeline: Entity<TimelineView>,
    dash: Entity<DashView>,
    panel: Entity<Panel>,
    machine: Entity<Machine>,
    search: Entity<InputState>,
    /// The side panel: wanted open, its width on screen, when it last moved.
    panel_open: bool,
    panel_w: f32,
    panel_last: Instant,
    /// The history loader: how full its bar is on screen, how visible it is (it fades out once done), its last frame.
    load_fill: f32,
    load_alpha: f32,
    load_last: Instant,
    feed: Vec<FeedEvent>,
    feed_seq: u64,
    feed_open: bool,
    feed_clicks: u32,
    project_menu: bool,
    menu_opens: u32,
    menu_scroll: crate::glide::Glide,
    replay: Option<Replay>,
    clock_bar: Rc<Cell<Bounds<Pixels>>>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
    #[cfg(feature = "snapshot")]
    snapshot_taken: bool,
}

impl Shell {
    pub fn new(machine: Entity<Machine>, mut feed: GraphFeed, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let map = cx.new(MapView::new);
        let timeline = cx.new(TimelineView::new);
        let dash = cx.new(DashView::new);
        let panel = cx.new(Panel::new);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search   /"));
        let subs = vec![
            // One selection for the map, the timeline and the panel.
            cx.subscribe(&map, |s, map, e: &MapEvent, cx| match e {
                MapEvent::Select => s.select(map.read(cx).selected.clone(), cx),
            }),
            cx.subscribe(&timeline, |s, timeline, e: &TimelineEvent, cx| match e {
                TimelineEvent::Select => s.select(timeline.read(cx).selected.clone(), cx),
            }),
            cx.subscribe(&panel, |s, panel, e: &PanelEvent, cx| match e {
                PanelEvent::Select => s.select(panel.read(cx).selected.clone(), cx),
            }),
            // "Top sessions": open the session on the map.
            cx.subscribe(&dash, |s, _, e: &DashEvent, cx| match e {
                DashEvent::Open(id) => {
                    s.select(Some(id.clone()), cx);
                    s.set_tab(Tab::Map, cx);
                }
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
                    if let Some(old) = s.graph.clone()
                        && s.replay.is_none()
                    {
                        s.news(&old, &graph);
                    }
                    (s.graph, s.usage) = (Some(graph), Some(usage));
                    s.push(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let saved = cx.global::<Settings>().clone();
        let tab = Tab::from(&std::env::var("MDB_TAB").unwrap_or(saved.tab.clone()));
        let range = std::env::var("MDB_RANGE").unwrap_or(saved.range.clone());
        let filters = Filters {
            range_h: RANGES.iter().find(|(label, _)| *label == range).map_or(Some(24.0), |r| r.1),
            project: saved.project.clone(),
            archived: saved.archived,
            agents: saved.agents,
            ..Filters::default()
        };
        map.update(cx, |m, _| m.active = tab == Tab::Map);
        timeline.update(cx, |t, _| t.active = tab == Tab::Timeline);
        dash.update(cx, |d, _| d.active = tab == Tab::Usage);
        Shell {
            tab,
            filters,
            graph: None,
            usage: None,
            map,
            timeline,
            dash,
            panel,
            machine,
            search,
            panel_open: saved.panel,
            panel_w: if saved.panel { panel::WIDTH } else { 0.0 },
            panel_last: Instant::now(),
            load_fill: 0.0,
            load_alpha: 1.0,
            load_last: Instant::now(),
            feed: Vec::new(),
            feed_seq: 0,
            feed_open: saved.feed,
            feed_clicks: 0,
            project_menu: false,
            menu_opens: 0,
            menu_scroll: crate::glide::Glide::default(),
            replay: None,
            clock_bar: Rc::default(),
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
            self.map.update(cx, |m, cx| m.set_graph(g.clone(), &filters, cx));
            self.timeline.update(cx, |t, cx| t.set_graph(g.clone(), &filters, cx));
            self.panel.update(cx, |p, cx| p.set_graph(g, &filters, cx));
        }
        if let Some(u) = self.usage.clone() {
            self.dash.update(cx, |d, cx| d.set_usage(u, cx));
        }
        cx.notify();
    }

    /// Saves the filters and hands them to the views.
    fn filter(&mut self, f: impl FnOnce(&mut Filters), cx: &mut Context<Self>) {
        f(&mut self.filters);
        let (range, project, archived, agents) = (
            RANGES.iter().find(|r| r.1 == self.filters.range_h).map_or("24h", |r| r.0).to_string(),
            self.filters.project.clone(),
            self.filters.archived,
            self.filters.agents,
        );
        settings::change(cx, |s| (s.range, s.project, s.archived, s.agents) = (range, project, archived, agents));
        self.push(cx);
    }

    fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.map.update(cx, |m, cx| {
            m.selected = id.clone();
            cx.notify();
        });
        self.timeline.update(cx, |t, cx| {
            t.selected = id.clone();
            cx.notify();
        });
        self.panel.update(cx, |p, cx| p.show(id, cx));
        cx.notify();
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.map.update(cx, |m, _| m.active = tab == Tab::Map);
        self.timeline.update(cx, |t, _| t.active = tab == Tab::Timeline);
        self.dash.update(cx, |d, _| d.active = tab == Tab::Usage);
        settings::change(cx, |s| s.tab = tab.name().into());
        cx.notify();
    }

    fn feed_push(&mut self, ts: u64, color: Rgba, who: String, what: String, id: String) {
        self.feed_seq += 1;
        self.feed.insert(0, FeedEvent { seq: self.feed_seq, ts, color, who, what, id });
        self.feed.truncate(FEED_MAX);
    }

    fn msg_event(&mut self, g: &Graph, m: &Msg) {
        let title = |id: &str| g.nodes.iter().find(|n| n.id == id).map_or_else(|| id.chars().take(12).collect(), |n| n.title.clone());
        let color = if m.ok == Some(false) {
            red()
        } else if m.kind == MsgKind::Report {
            green()
        } else {
            blue()
        };
        let what = format!(
            "{}{}",
            if m.kind == MsgKind::Report { "report · " } else { "" },
            if m.summary.is_empty() { &m.text } else { &m.summary }
        );
        self.feed_push(m.ts, color, format!("{} → {}", clip(&title(&m.from), 26), clip(&title(&m.to), 26)), what, m.from.clone());
    }

    /// What changed since the last data: sessions starting or finishing work, new sessions and subagents, messages.
    /// Only what just happened: the history read after launch is not news.
    fn news(&mut self, old: &Graph, new: &Graph) {
        let before: HashMap<&str, &mdb_core::graph::Node> = old.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let now = now_ms();
        let recent = |ts: u64| now.saturating_sub(ts) < 120_000;
        for n in &new.nodes {
            match before.get(n.id.as_str()) {
                Some(o) if is_session(n) && n.live && o.status != n.status => {
                    if working(n) && !working(o) {
                        let what = n.now.as_ref().map_or("started working".into(), |m| format!("{} · {}", m.tool, m.detail));
                        self.feed_push(now, amber(), n.title.clone(), what, n.id.clone());
                    } else if !working(n) && working(o) {
                        self.feed_push(now, text(), n.title.clone(), "finished its turn".into(), n.id.clone());
                    }
                }
                Some(_) => {}
                None if is_session(n) && recent(n.created_at) => {
                    self.feed_push(n.created_at.max(1), violet(), n.title.clone(), "new session".into(), n.id.clone())
                }
                None if n.kind == NodeKind::Agent && recent(n.created_at) => {
                    let parent = n.parent.clone().unwrap_or_default();
                    let who = new.nodes.iter().find(|p| p.id == parent).map_or_else(|| parent.clone(), |p| p.title.clone());
                    self.feed_push(n.created_at.max(1), violet(), who, format!("spawned subagent · {}", n.title), parent);
                }
                None => {}
            }
        }
        let seen: HashSet<&str> = old.messages.iter().map(|m| m.id.as_str()).collect();
        for m in new.messages.iter().filter(|m| !seen.contains(m.id.as_str()) && recent(m.ts)) {
            self.msg_event(new, m);
        }
    }

    fn start_replay(&mut self, cx: &mut Context<Self>) {
        let Some(g) = self.graph.clone() else { return };
        let mut msgs = visible(&g, &self.filters, now_ms()).messages;
        if msgs.is_empty() {
            return;
        }
        msgs.sort_by_key(|&m| g.messages[m].ts);
        let t0 = g.messages[msgs[0]].ts;
        let span = now_ms().saturating_sub(t0).max(1);
        let dur = (4.0 + msgs.len() as f32 * 0.09).min(45.0);
        self.feed.clear();
        self.replay = Some(Replay { msgs, t0, span, dur, base: Instant::now(), next: 0 });
        if self.tab != Tab::Map {
            self.set_tab(Tab::Map, cx);
        }
        cx.notify();
    }

    /// Jump to `frac` (0 to 1) of the replay.
    fn seek(&mut self, frac: f32, cx: &mut Context<Self>) {
        let Some(g) = self.graph.clone() else { return };
        if let Some(r) = self.replay.as_mut() {
            let from = frac.clamp(0.0, 1.0) * r.dur;
            r.base = Instant::now() - Duration::from_secs_f32(from);
            r.next = (0..r.msgs.len()).find(|&i| r.at(&g, i) >= from).unwrap_or(r.msgs.len());
            self.feed.clear();
            cx.notify();
        }
    }

    /// Fires the replayed messages that are due; stops 1.5 s after the last.
    fn tick_replay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(g), Some(r)) = (self.graph.clone(), self.replay.as_mut()) else { return };
        let el = r.base.elapsed().as_secs_f32();
        let mut due = Vec::new();
        while r.next < r.msgs.len() && r.at(&g, r.next) <= el {
            due.push(r.msgs[r.next]);
            r.next += 1;
        }
        let done = el > r.dur + 1.5;
        for m in due {
            let msg = &g.messages[m];
            self.map.update(cx, |map, _| map.queue_comet(&msg.from, &msg.to, msg.kind == MsgKind::Report));
            self.msg_event(&g, msg);
        }
        if done {
            self.replay = None;
        } else {
            crate::glide::again(window, cx);
        }
    }

    fn header(&self, wide: bool, cx: &mut Context<Self>) -> Div {
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
                .hover(|s| s.text_color(text()))
                .when(on, |d| d.bg(panel_hi()))
                .child(label)
        };
        // A control with a border, lit when on.
        let ctl = |id: &'static str, on: bool| {
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
                .border_color(if on { rgba(0x38bdf866) } else { line() })
                .text_color(if on { blue() } else { text() })
                .cursor_pointer()
                .text_size(px(12.0))
                .hover(|s| s.bg(panel_hi()))
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
                button(format!("range-{label}").into(), label.into(), self.filters.range_h == h)
                    .on_click(cx.listener(move |s, _, _, cx| s.filter(|f| f.range_h = h, cx)))
            })
            .collect();
        let toggle = |id: &'static str, label: &'static str, on: bool, flip: fn(&mut Filters)| {
            ctl(id, false)
                .child(
                    div()
                        .size(px(12.0))
                        .rounded(px(3.0))
                        .border_1()
                        .border_color(if on { blue() } else { muted() })
                        .when(on, |d| d.bg(blue())),
                )
                .child(label)
                .on_click(cx.listener(move |s, _, _, cx| s.filter(flip, cx)))
        };
        let stats = self.graph.as_ref().filter(|_| wide).map(|g| {
            let v = visible(g, &self.filters, now_ms());
            let sessions = v.nodes.iter().filter(|&&i| is_session(&g.nodes[i])).count();
            let agents = v.nodes.iter().filter(|&&i| g.nodes[i].kind == NodeKind::Agent).count();
            let messages = v.messages.iter().filter(|&&m| g.messages[m].kind == MsgKind::Message).count();
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

        // Projects, the busiest first, in a menu under their button.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for n in self.graph.iter().flat_map(|g| g.nodes.iter()).filter(|n| is_session(n)) {
            *counts.entry(n.project.as_str()).or_default() += 1;
        }
        let mut projects: Vec<(&str, usize)> = counts.into_iter().collect();
        projects.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let current = self.filters.project.clone();
        let menu = self.project_menu.then(|| {
            let row = |id: String, label: String, pick: Option<String>, on: bool| {
                div()
                    .id(SharedString::from(id))
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(if on { blue() } else { text() })
                    .whitespace_nowrap()
                    .hover(|s| s.bg(panel_hi()))
                    .child(label)
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.project_menu = false;
                        let pick = pick.clone();
                        s.filter(|f| f.project = pick, cx);
                    }))
            };
            let mut rows = vec![row("p-all".into(), "All projects".into(), None, current.is_none())];
            rows.extend(
                projects
                    .iter()
                    .map(|(p, c)| row(format!("p-{p}"), format!("{p} · {c}"), Some(p.to_string()), current.as_deref() == Some(*p))),
            );
            deferred(
                div()
                    .id("project-menu")
                    .absolute()
                    .top(px(34.0))
                    .left_0()
                    .min_w(px(180.0))
                    .max_h(px(360.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.menu_scroll.handle)
                    .child(self.menu_scroll.driver())
                    .p(px(4.0))
                    .rounded(px(10.0))
                    .bg(panel())
                    .border_1()
                    .border_color(rgba(0xffffff1f))
                    .shadow_lg()
                    .children(rows)
                    .on_mouse_down_out(cx.listener(|s, _, _, cx| {
                        s.project_menu = false;
                        cx.notify();
                    }))
                    .with_animation(
                        SharedString::from(format!("menu-{}", self.menu_opens)),
                        Animation::new(Duration::from_millis(140)).with_easing(ease_in_out),
                        |el, t| el.opacity(t).mt(px(-4.0 * (1.0 - t))),
                    ),
            )
            .with_priority(1)
        });
        let project = div()
            .relative()
            .child(
                ctl("project", current.is_some())
                    .max_w(px(150.0))
                    .child(div().overflow_hidden().child(clip(current.as_deref().unwrap_or("All projects"), 16)))
                    .child(div().text_color(muted()).child("▾"))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.project_menu = !s.project_menu;
                        s.menu_opens += 1;
                        cx.notify();
                    })),
            )
            .children(menu);
        // The side panel's button: a box with its right part drawn.
        let panel_icon = |on: bool| {
            let c = if on { blue() } else { muted() };
            div()
                .relative()
                .w(px(14.0))
                .h(px(12.0))
                .rounded(px(2.0))
                .border_1()
                .border_color(c)
                .child(div().absolute().top_0().bottom_0().right(px(3.0)).w(px(1.0)).bg(c))
        };
        let map_tab = matches!(self.tab, Tab::Map | Tab::Timeline);
        let replaying = self.replay.is_some();
        // The tabs come first and never move; the window's title already names the app.
        div()
            .relative()
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(line())
            .child(seg(tabs))
            .children(self.loader(wide))
            .children(stats.filter(|_| map_tab))
            .child(div().flex_1())
            .when(map_tab, |d| {
                d.child(seg(ranges))
                    .child(div().w(px(120.0)).child(Input::new(&self.search)))
                    .child(project)
                    .child(toggle("archived", "Archived", self.filters.archived, |f| f.archived = !f.archived))
                    .child(toggle("agents", "Subagents", self.filters.agents, |f| f.agents = !f.agents))
                    .child(ctl("panel-toggle", self.panel_open).child(panel_icon(self.panel_open)).on_click(cx.listener(|s, _, _, cx| {
                        s.panel_open = !s.panel_open;
                        s.panel_last = Instant::now();
                        let open = s.panel_open;
                        settings::change(cx, |st| st.panel = open);
                        cx.notify();
                    })))
                    .child(ctl("replay", replaying).child(if replaying { "■ Stop" } else { "▶ Replay" }).on_click(cx.listener(
                        |s, _, _, cx| {
                            if s.replay.take().is_none() {
                                s.start_replay(cx);
                            }
                            cx.notify();
                        },
                    )))
            })
    }

    /// While the history is read after launch: how far it is, in the free room right of the tabs (laid over the bar,
    /// so nothing moves when it goes). The bar fills smoothly and the whole fades out once everything is read.
    fn loader(&self, wide: bool) -> Option<Div> {
        let u = self.usage.as_ref()?;
        if self.load_alpha <= 0.01 {
            return None;
        }
        let label = if u.ready { "History read".to_string() } else { "Reading history".to_string() };
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(if wide { 560.0 } else { 276.0 }))
                .flex()
                .items_center()
                .opacity(self.load_alpha)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(5.0))
                        .w(px(156.0))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_size(px(11.0))
                                .child(div().text_color(muted()).child(label))
                                .child(div().font_family(MONO).text_color(faint()).child(format!("{:.0}%", self.load_fill * 100.0))),
                        )
                        .child(
                            div().h(px(3.0)).w_full().rounded(px(2.0)).bg(rgba(0xffffff1a)).child(
                                div().h_full().rounded(px(2.0)).w(relative(self.load_fill)).bg(if u.ready { green() } else { blue() }),
                            ),
                        ),
                ),
        )
    }

    /// The live feed, at the bottom left of the map: the newest line slides in at the top.
    fn feed(&self, cx: &mut Context<Self>) -> Div {
        let lines = self.feed.iter().map(|e| {
            let id = e.id.clone();
            div()
                .id(SharedString::from(format!("ev-{}", e.seq)))
                .flex()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff0a)))
                .on_click(cx.listener(move |s, _, _, cx| s.select(Some(id.clone()), cx)))
                .child(div().w(px(42.0)).flex_none().font_family(MONO).text_size(px(11.0)).text_color(faint()).child(clock(e.ts)))
                .child(div().w(px(10.0)).flex_none().pt(px(5.0)).child(div().size(px(8.0)).rounded_full().bg(e.color).shadow(vec![
                    BoxShadow {
                        color: alpha(e.color, 0.9).into(),
                        offset: point(px(0.0), px(0.0)),
                        blur_radius: px(8.0),
                        spread_radius: px(0.0),
                        inset: false,
                    },
                ])))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(div().truncate().child(e.who.clone()))
                        .child(div().truncate().text_size(px(12.0)).text_color(muted()).child(e.what.clone())),
                )
                .with_animation(
                    SharedString::from(format!("ev-in-{}", e.seq)),
                    Animation::new(SLIDE_IN).with_easing(ease_out_quint()),
                    |el, t| el.opacity(t).mt(px(6.0 * (1.0 - t))),
                )
        });
        let open = self.feed_open;
        let height = px(16.0 + self.feed.len().max(1) as f32 * 44.0);
        let body = div()
            .overflow_hidden()
            .children(lines)
            .when(self.feed.is_empty(), |d| {
                d.child(div().px(px(10.0)).py(px(4.0)).text_size(px(12.0)).text_color(faint()).child("Waiting for sessions to talk…"))
            })
            .with_animation(
                SharedString::from(format!("feed-{}", self.feed_clicks)),
                Animation::new(Duration::from_millis(220)).with_easing(ease_in_out),
                move |el, t| {
                    let shown = if open { t } else { 1.0 - t };
                    if open && t >= 1.0 { el } else { el.max_h(height * shown).opacity(shown) }
                },
            );
        div()
            .absolute()
            .left(px(16.0))
            .bottom(px(16.0))
            .w(px(380.0))
            .pt(px(10.0))
            .px(px(6.0))
            .pb(px(6.0))
            .rounded(px(12.0))
            .bg(rgba(0x11121ad1))
            .border_1()
            .border_color(line())
            .child(
                div()
                    .id("feed-toggle")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .mx(px(10.0))
                    .mb(px(6.0))
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(faint())
                    .cursor_pointer()
                    .hover(|s| s.text_color(text()))
                    .child("LIVE FEED")
                    .child(if open { "▾" } else { "▸" })
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.feed_open = !s.feed_open;
                        s.feed_clicks += 1;
                        let open = s.feed_open;
                        settings::change(cx, |st| st.feed = open);
                        cx.notify();
                    })),
            )
            .when(open || self.feed_clicks > 0, |d| d.child(body))
    }

    /// The replay's clock at the top of the map; a click on its bar jumps in time.
    fn clock(&self, cx: &mut Context<Self>) -> Option<Div> {
        let r = self.replay.as_ref()?;
        let frac = (r.base.elapsed().as_secs_f32() / r.dur).min(1.0);
        let t = r.t0 + (frac as f64 * r.span as f64) as u64;
        let label = chrono::TimeZone::timestamp_millis_opt(&chrono::Local, t as i64)
            .single()
            .map(|d| d.format("%a %-d %b · %H:%M").to_string())
            .unwrap_or_default();
        let bar = self.clock_bar.clone();
        Some(
            div().absolute().top(px(18.0)).left_0().right_0().flex().justify_center().child(
                div()
                    .px(px(18.0))
                    .py(px(8.0))
                    .rounded(px(12.0))
                    .bg(rgba(0x11121ad1))
                    .border_1()
                    .border_color(line())
                    .child(div().font_family(MONO).text_size(px(22.0)).text_color(amber()).child(label))
                    .child(
                        div()
                            .id("clock-bar")
                            .relative()
                            .mt(px(8.0))
                            .h(px(6.0))
                            .bg(rgba(0xffffff1f))
                            .cursor_pointer()
                            .child(canvas(move |b, _, _| bar.set(b), |_, _, _, _| {}).absolute().size_full())
                            .child(div().h_full().w(relative(frac)).bg(amber()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|s, e: &MouseDownEvent, _, cx| {
                                    let b = s.clock_bar.get();
                                    s.seek(f32::from(e.position.x - b.origin.x) / f32::from(b.size.width).max(1.0), cx);
                                }),
                            ),
                    ),
            ),
        )
    }

    /// Nothing to show: say why, with the one click that brings sessions back.
    fn empty(&self, cx: &mut Context<Self>) -> Option<Div> {
        let g = self.graph.as_ref()?;
        let v = visible(g, &self.filters, now_ms());
        if v.nodes.iter().any(|&i| is_session(&g.nodes[i])) {
            return None;
        }
        let (msg, act): (String, &str) = if !self.filters.query.is_empty() {
            (format!("No session matches “{}”", self.search.read(cx).value().trim()), "Clear search")
        } else if let Some(p) = &self.filters.project {
            (format!("No session in {p} for this range"), "All projects")
        } else {
            ("No session in this range".into(), "Show all")
        };
        Some(
            div().absolute().top_0().bottom_0().left_0().right_0().flex().items_center().justify_center().child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(20.0))
                    .py(px(16.0))
                    .rounded(px(12.0))
                    .bg(rgba(0x11121ad1))
                    .border_1()
                    .border_color(line())
                    .child(div().text_color(muted()).child(msg))
                    .child(
                        div()
                            .id("empty-act")
                            .px(px(12.0))
                            .py(px(6.0))
                            .rounded(px(8.0))
                            .bg(panel())
                            .border_1()
                            .border_color(line())
                            .cursor_pointer()
                            .hover(|s| s.bg(panel_hi()))
                            .text_size(px(12.0))
                            .child(act)
                            .on_click(cx.listener(|s, _, window, cx| {
                                if !s.filters.query.is_empty() {
                                    s.search.update(cx, |input, cx| input.set_value("", window, cx));
                                    s.filters.query.clear();
                                    s.push(cx);
                                } else if s.filters.project.is_some() {
                                    s.filter(|f| f.project = None, cx);
                                } else {
                                    s.filter(|f| f.range_h = None, cx);
                                }
                            })),
                    ),
            ),
        )
    }
}

/// "18:32".
fn clock(ms: u64) -> String {
    chrono::TimeZone::timestamp_millis_opt(&chrono::Local, ms as i64).single().map(|d| d.format("%H:%M").to_string()).unwrap_or_default()
}

/// Quick at first, then gently to rest (CSS ease-out).
fn ease_out_quint() -> impl Fn(f32) -> f32 {
    |t: f32| 1.0 - (1.0 - t).powi(5)
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
        if std::env::var("MDB_DEBUG").is_ok() {
            eprintln!("snapshot ready={ready} usage={:?}", self.usage.as_ref().map(|u| (u.ready, u.scanned, u.total)));
        }
        if self.snapshot_taken || !ready {
            crate::glide::again(window, cx);
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
        crate::glide::again(window, cx); // the frame that saves it, even if nothing moves
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
            // MDB_WHEEL_EARLY: start while the history is still being read.
            Tab::Usage => self.usage.as_ref().is_some_and(|u| u.ready || std::env::var("MDB_WHEEL_EARLY").is_ok()),
            Tab::Timeline => self.graph.is_some(),
        };
        if self.snapshot_taken || !ready {
            crate::glide::again(window, cx);
            return;
        }
        self.snapshot_taken = true;
        // MDB_SELECT: select the first session whose title has this text.
        if let (Ok(want), Some(g)) = (std::env::var("MDB_SELECT"), self.graph.clone())
            && let Some(n) = g.nodes.iter().filter(|n| is_session(n) && n.title.contains(want.as_str())).max_by_key(|n| (n.live, n.last_at))
        {
            self.select(Some(n.id.clone()), cx);
        }
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

/// `MDB_LAUNCH_FRAMES=<dir>` with `MDB_LAUNCH_MARKS=ms,ms,…`: frames at those times after the window opened, to see
/// how the first data arrives; then it quits.
#[cfg(feature = "snapshot")]
fn launch_frames(window: &mut Window) {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let Ok(dir) = std::env::var("MDB_LAUNCH_FRAMES") else { return };
    let start = *START.get_or_init(std::time::Instant::now);
    let marks: Vec<u128> = std::env::var("MDB_LAUNCH_MARKS").unwrap_or_default().split(',').filter_map(|x| x.parse().ok()).collect();
    let ms = start.elapsed().as_millis();
    let next = LAUNCH_SAVED.with(|n| n.get());
    if let Some(&mark) = marks.get(next)
        && ms >= mark
    {
        LAUNCH_SAVED.with(|n| n.set(next + 1));
        window.on_next_frame(move |window, cx| {
            if let Ok(image) = window.render_to_image() {
                image.save(format!("{dir}/{mark:05}.png")).ok();
            }
            if next + 1 >= marks.len() {
                cx.quit();
            }
        });
    }
    window.request_animation_frame();
}

#[cfg(feature = "snapshot")]
thread_local! {
    static LAUNCH_SAVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CLICKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DRAG_STEP: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "snapshot")]
fn wheel_frames(window: &mut Window, w: Wheel, sent: usize, saved: usize) {
    window.on_next_frame(move |window, cx| {
        let ms = crate::glide::TRACE_START.get().map_or(0.0, |s| s.elapsed().as_secs_f32() * 1000.0);
        let mut sent = sent;
        // MDB_CLICK: one click at the point; MDB_CLICKS="x,y@ms;…": clicks at their times (the first at 0 counts once).
        let clicks = std::env::var("MDB_CLICKS").unwrap_or_else(|_| {
            if std::env::var("MDB_CLICK").is_ok() { format!("{},{}@0", f32::from(w.at.x), f32::from(w.at.y)) } else { String::new() }
        });
        for (i, c) in clicks.split(';').filter(|c| !c.is_empty()).enumerate() {
            let (xy, at) = c.split_once('@').unwrap_or((c, "0"));
            let at: f32 = at.parse().unwrap_or(0.0);
            let done = CLICKED.with(|d| d.get() > i);
            if done || ms < at {
                continue;
            }
            CLICKED.with(|d| d.set(i + 1));
            let v: Vec<f32> = xy.split(',').filter_map(|x| x.parse().ok()).collect();
            let position = point(px(v[0]), px(v[1]));
            eprintln!("trace click {ms:.1} {i}");
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                cx,
            );
            let down = MouseDownEvent { button: MouseButton::Left, position, click_count: 1, ..Default::default() };
            window.dispatch_event(PlatformInput::MouseDown(down), cx);
            let up = MouseUpEvent { button: MouseButton::Left, position, click_count: 1, ..Default::default() };
            window.dispatch_event(PlatformInput::MouseUp(up), cx);
        }
        // MDB_DRAG="x1,y1,x2,y2@ms": press at the first point, move to the second over 300 ms, release.
        if let Ok(d) = std::env::var("MDB_DRAG") {
            let (xy, at) = d.split_once('@').unwrap_or((&d, "0"));
            let v: Vec<f32> = xy.split(',').filter_map(|x| x.parse().ok()).collect();
            let at: f32 = at.parse().unwrap_or(0.0);
            let step = DRAG_STEP.with(|s| s.get());
            let (a, b) = (point(px(v[0]), px(v[1])), point(px(v[2]), px(v[3])));
            if step == 0 && ms >= at {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent { position: a, pressed_button: None, modifiers: Modifiers::default() }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position: a,
                        click_count: 1,
                        ..Default::default()
                    }),
                    cx,
                );
                DRAG_STEP.with(|s| s.set(1));
            } else if step == 1 {
                let t = ((ms - at) / 300.0).clamp(0.0, 1.0);
                let p = point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: p,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
                if t >= 1.0 {
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button: MouseButton::Left,
                            position: b,
                            click_count: 1,
                            ..Default::default()
                        }),
                        cx,
                    );
                    DRAG_STEP.with(|s| s.set(2));
                }
            }
        }
        while sent < w.count && ms >= sent as f32 * w.gap {
            let e = ScrollEvent {
                position: w.at,
                delta: ScrollDelta::Lines(point(0.0, w.lines)),
                modifiers: Modifiers { platform: std::env::var("MDB_WHEEL_MODS").is_ok_and(|m| m.contains("cmd")), ..Default::default() },
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            };
            eprintln!("trace notch {ms:.1} {}", w.lines);
            // MDB_PINCH: a pinch of `lines` (0.1 = +10%) instead; MDB_WHEEL_PRECISE: a trackpad swipe of `lines` px.
            if std::env::var("MDB_PINCH").is_ok() {
                let pinch = PinchEvent { position: w.at, delta: w.lines, modifiers: Modifiers::default(), phase: TouchPhase::Moved };
                window.dispatch_event(PlatformInput::Pinch(pinch), cx);
            } else if std::env::var("MDB_WHEEL_PRECISE").is_ok() {
                let swipe = ScrollEvent { delta: ScrollDelta::Pixels(point(px(w.lines), px(w.lines / 2.0))), ..e };
                window.dispatch_event(PlatformInput::Scroll(swipe), cx);
            } else {
                window.dispatch_event(PlatformInput::Scroll(e), cx);
            }
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
        // MDB_WHEEL_HOLD: how long to keep going after the last notch (700 ms by default).
        let hold = std::env::var("MDB_WHEEL_HOLD").ok().and_then(|h| h.parse().ok()).unwrap_or(700.0);
        if ms < (w.count as f32 - 1.0) * w.gap + hold { wheel_frames(window, w, sent, saved) } else { cx.quit() }
    });
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "snapshot")]
        {
            self.snapshot(window, cx);
            self.wheel_test(window, cx);
            launch_frames(window);
        }
        self.tick_replay(window, cx);
        // The side panel opens and closes smoothly; the map and the timeline give it room as it goes.
        let target = if self.panel_open { panel::WIDTH } else { 0.0 };
        if self.panel_w != target {
            let now = Instant::now();
            self.panel_w = crate::glide::ease(self.panel_w, target, (now - self.panel_last).as_secs_f32() * crate::glide::TAU / PANEL_TAU);
            self.panel_last = now;
            if (self.panel_w - target).abs() < 0.5 {
                self.panel_w = target;
            } else {
                crate::glide::again(window, cx);
            }
        }
        // The loader's bar slides to the share of history read; once all is read it fades out.
        if let Some(u) = self.usage.clone() {
            let now = Instant::now();
            let dt = (now - self.load_last).as_secs_f32();
            self.load_last = now;
            let target = if u.total_bytes == 0 { 0.0 } else { u.read_bytes as f32 / u.total_bytes as f32 };
            self.load_fill = crate::glide::ease(self.load_fill, if u.ready { 1.0 } else { target }, dt * 0.5);
            if u.ready && (1.0 - self.load_fill) < 0.01 {
                self.load_alpha = crate::glide::ease(self.load_alpha, 0.0, dt * 0.25);
            }
            if self.load_alpha > 0.01 && (self.load_fill < 0.999 || u.ready) {
                crate::glide::again(window, cx);
            }
        }
        let map_tab = matches!(self.tab, Tab::Map | Tab::Timeline);
        let body: AnyElement = match self.tab {
            Tab::Map => div()
                .size_full()
                .relative()
                .child(self.map.clone())
                .child(self.feed(cx))
                .children(self.clock(cx))
                .children(self.empty(cx))
                .into_any_element(),
            Tab::Timeline => div().size_full().relative().child(self.timeline.clone()).children(self.empty(cx)).into_any_element(),
            Tab::Machine => self.machine.clone().into_any_element(),
            Tab::Usage => self.dash.clone().into_any_element(),
        };
        self.panel.update(cx, |p, _| p.active = map_tab && self.panel_w > 0.0);
        let wide = f32::from(window.viewport_size().width) >= 1700.0;
        div().size_full().flex().flex_col().bg(bg()).text_color(text()).text_size(px(13.0)).child(self.header(wide, cx)).child(
            div().flex_1().min_h_0().flex().child(div().flex_1().min_w_0().h_full().child(body)).when(map_tab && self.panel_w > 0.0, |d| {
                d.child(
                    div()
                        .h_full()
                        .flex_none()
                        .w(px(self.panel_w))
                        .overflow_hidden()
                        .border_l_1()
                        .border_color(line())
                        .child(div().w(px(panel::WIDTH)).h_full().child(self.panel.clone())),
                )
            }),
        )
    }
}
