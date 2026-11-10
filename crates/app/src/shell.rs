//! The window: a header (counters, tabs, range, search, project, filters, panel, replay) above the tab on screen.
//! The map and the timeline share a side panel, a live feed and a replay of the range's messages.
use crate::dash::{DashEvent, DashView};
use crate::machine_view::MachineView;
use crate::map::{MapEvent, MapView};
use crate::menus::Said;
use crate::panel::{self, Panel, PanelEvent};
use crate::settings::{self, Settings};
use crate::state::{Filters, RANGES, RangeChoice, is_session, norm, visible, working};
use crate::timeline::{TimelineEvent, TimelineView};
use crate::ui::*;
use crate::ui::{blue, green, red}; // over gpui's colors of the same names
use asterism_core::graph::{Graph, Msg, MsgKind, NodeKind, UsageView};
use asterism_core::i18n;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use rust_i18n::t;
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
}

/// A saved tab's name; the map for a name that is none.
impl From<&str> for Tab {
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
/// Lines the feed keeps: its height is the member's, so it can show many.
const FEED_MAX: usize = 50;
/// The feed's lines are never shorter than this; the panel above keeps at least `PANEL_MIN_H`.
const FEED_MIN_H: f32 = 90.0;
const PANEL_MIN_H: f32 = 200.0;
/// From the feed's top edge (its grip) to its first line: the grip and the title.
const FEED_HEAD_H: f32 = 34.0;
/// Below this window width, the side panel lies over the map instead of next to it.
const NARROW: f32 = 1100.0;

/// One line of the live feed.
struct FeedEvent {
    seq: u64,
    ts: u64,
    color: Rgba,
    who: String,
    what: What,
    /// The session a click selects.
    id: String,
}

/// What a feed line says: written when it is drawn, so it follows a change of language.
enum What {
    /// A message's text, or what a session works on: from the data, never translated.
    Data(String),
    Report(String),
    StartedWorking,
    Finished,
    NewSession,
    Subagent(String),
}

impl What {
    fn say(&self) -> String {
        match self {
            What::Data(s) => s.clone(),
            What::Report(s) => t!("shell.feed.report", text = s).into_owned(),
            What::StartedWorking => t!("shell.feed.started_working").into_owned(),
            What::Finished => t!("shell.feed.finished").into_owned(),
            What::NewSession => t!("shell.feed.new_session").into_owned(),
            What::Subagent(name) => t!("shell.feed.subagent", name = name).into_owned(),
        }
    }
}

/// Every message of the range again, fast-forwarded: comets on the map and lines in the feed.
struct Replay {
    /// The graph it started from: `msgs` are indices into it, and a newer graph numbers its messages again.
    graph: Arc<Graph>,
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
    fn at(&self, i: usize) -> f32 {
        (self.graph.messages[self.msgs[i]].ts.saturating_sub(self.t0)) as f32 / self.span as f32 * self.dur
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
    machine: Entity<MachineView>,
    search: Entity<InputState>,
    /// The side panel: wanted open, its width on screen, when it last moved.
    panel_open: bool,
    /// Narrow windows: the panel over the map, closed unless opened there.
    panel_narrow_open: bool,
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
    /// The height of the feed's lines; its grip changes it, saved once the mouse lets go.
    feed_h: f32,
    feed_resized: bool,
    feed_scroll: crate::glide::Glide,
    project_menu: bool,
    range_menu: bool,
    /// The settings window, and what a check for updates from it found (title, detail, whether it failed).
    settings_open: bool,
    settings_page: SettingsPage,
    settings_focus: FocusHandle,
    update_checking: bool,
    update_answer: Option<Said>,
    /// The machines menu's form, and the machine whose removal waits for a yes.
    machine_name: Entity<InputState>,
    machine_target: Entity<InputState>,
    machine_remove: Option<String>,
    /// The menu a press outside it just closed: when that press was on its own button, the click must not open it
    /// again.
    menu_closed: Option<(&'static str, Instant)>,
    menu_opens: u32,
    menu_scroll: crate::glide::Glide,
    tab_seg: crate::seg::Seg,
    replay: Option<Replay>,
    /// A newer version: downloading, or ready to install (the arrow at the top right).
    update: crate::update::State,
    clock_bar: Rc<Cell<Bounds<Pixels>>>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
    #[cfg(feature = "snapshot")]
    snapshot_taken: bool,
}

impl Shell {
    pub fn new(
        machine: Entity<MachineView>,
        mut feed: GraphFeed,
        mut updates: watch::Receiver<crate::update::State>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let map = cx.new(MapView::new);
        let timeline = cx.new(TimelineView::new);
        let dash = cx.new(DashView::new);
        let panel = cx.new(Panel::new);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("shell.search").to_string()));
        let machine_name = cx.new(|cx| InputState::new(window, cx).placeholder(t!("machines.name").to_string()));
        let machine_target = cx.new(|cx| InputState::new(window, cx).placeholder(t!("machines.target").to_string()));
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
            // The form's Add button follows what is typed; Enter adds.
            cx.subscribe_in(&machine_name, window, |s, _, e: &InputEvent, window, cx| s.machine_form(e, window, cx)),
            cx.subscribe_in(&machine_target, window, |s, _, e: &InputEvent, window, cx| s.machine_form(e, window, cx)),
        ];
        let updates_task = cx.spawn(async move |this, cx| {
            while feed.changed().await.is_ok() {
                let Some((graph, usage)) = feed.borrow_and_update().clone() else { continue };
                #[cfg(feature = "snapshot")]
                let graph = if crate::demo::on() { Arc::new(crate::demo::graph(&graph)) } else { graph };
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
        let new_version = cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
                let state = updates.borrow_and_update().clone();
                if this
                    .update(cx, |s, cx| {
                        s.update = state;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let saved = cx.global::<Settings>().clone();
        let tab = Tab::from(std::env::var("MDB_TAB").unwrap_or(saved.tab.clone()).as_str());
        let range = std::env::var("MDB_RANGE").unwrap_or(saved.range.clone());
        let filters = Filters {
            range_h: RANGES.iter().find(|r| r.label == range).map_or(Some(24.0), |r| r.hours),
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
            panel_narrow_open: false,
            panel_w: if saved.panel { panel::WIDTH } else { 0.0 },
            panel_last: Instant::now(),
            load_fill: 0.0,
            load_alpha: 1.0,
            load_last: Instant::now(),
            feed: Vec::new(),
            feed_seq: 0,
            feed_open: saved.feed,
            feed_clicks: 0,
            feed_h: saved.feed_h,
            feed_resized: false,
            feed_scroll: crate::glide::Glide::default(),
            project_menu: false,
            range_menu: false,
            settings_open: false,
            settings_page: SettingsPage::Language,
            settings_focus: cx.focus_handle(),
            update_checking: false,
            update_answer: None,
            machine_name,
            machine_target,
            machine_remove: None,
            menu_closed: None,
            menu_opens: 0,
            menu_scroll: crate::glide::Glide::default(),
            tab_seg: crate::seg::Seg::default(),
            replay: None,
            update: crate::update::State::Idle,
            clock_bar: Rc::default(),
            _tasks: vec![updates_task, new_version],
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
            RANGES.iter().find(|r| r.hours == self.filters.range_h).map_or("24h", |r| r.label).to_string(),
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

    fn feed_push(&mut self, ts: u64, color: Rgba, who: String, what: What, id: String) {
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
        let text = if m.summary.is_empty() { &m.text } else { &m.summary }.clone();
        let what = if m.kind == MsgKind::Report { What::Report(text) } else { What::Data(text) };
        self.feed_push(m.ts, color, format!("{} → {}", clip(&title(&m.from), 26), clip(&title(&m.to), 26)), what, m.from.clone());
    }

    /// What changed since the last data: sessions starting or finishing work, new sessions and subagents, messages.
    /// Only what just happened: the history read after launch is not news.
    fn news(&mut self, old: &Graph, new: &Graph) {
        let before: HashMap<&str, &asterism_core::graph::Node> = old.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let now = now_ms();
        let recent = |ts: u64| now.saturating_sub(ts) < 120_000;
        for n in &new.nodes {
            match before.get(n.id.as_str()) {
                Some(o) if is_session(n) && n.live && o.status != n.status => {
                    if working(n) && !working(o) {
                        let what = n
                            .now
                            .as_ref()
                            .map_or(What::StartedWorking, |m| What::Data(format!("{} · {}", crate::state::tool_name(&m.tool), m.detail)));
                        self.feed_push(now, amber(), n.title.clone(), what, n.id.clone());
                    } else if !working(n) && working(o) {
                        self.feed_push(now, text(), n.title.clone(), What::Finished, n.id.clone());
                    }
                }
                Some(_) => {}
                None if is_session(n) && recent(n.created_at) => {
                    self.feed_push(n.created_at.max(1), violet(), n.title.clone(), What::NewSession, n.id.clone())
                }
                None if n.kind == NodeKind::Agent && recent(n.created_at) => {
                    let parent = n.parent.clone().unwrap_or_default();
                    let who = new.nodes.iter().find(|p| p.id == parent).map_or_else(|| parent.clone(), |p| p.title.clone());
                    self.feed_push(n.created_at.max(1), violet(), who, What::Subagent(n.title.clone()), parent);
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
        self.replay = Some(Replay { graph: g, msgs, t0, span, dur, base: Instant::now(), next: 0 });
        if self.tab != Tab::Map {
            self.set_tab(Tab::Map, cx);
        }
        cx.notify();
    }

    /// Jump to `frac` (0 to 1) of the replay.
    fn seek(&mut self, frac: f32, cx: &mut Context<Self>) {
        if let Some(r) = self.replay.as_mut() {
            let from = frac.clamp(0.0, 1.0) * r.dur;
            r.base = Instant::now() - Duration::from_secs_f32(from);
            r.next = (0..r.msgs.len()).find(|&i| r.at(i) >= from).unwrap_or(r.msgs.len());
            self.feed.clear();
            cx.notify();
        }
    }

    /// Fires the replayed messages that are due; stops 1.5 s after the last.
    fn tick_replay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.replay.as_mut() else { return };
        let g = r.graph.clone();
        let el = r.base.elapsed().as_secs_f32();
        let mut due = Vec::new();
        while r.next < r.msgs.len() && r.at(r.next) <= el {
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

    fn header(&self, wide: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
        let tabs = [
            (Tab::Map, t!("shell.tab.map")),
            (Tab::Timeline, t!("shell.tab.timeline")),
            (Tab::Usage, t!("shell.tab.usage")),
            (Tab::Machine, t!("shell.tab.machine")),
        ]
        .into_iter()
        .map(|(tab, label)| {
            button(format!("tab-{}", tab.name()).into(), label.to_string().into(), self.tab == tab)
                .on_click(cx.listener(move |s, _, _, cx| s.set_tab(tab, cx)))
                .into_any_element()
        })
        .collect();
        let tab_index = [Tab::Map, Tab::Timeline, Tab::Usage, Tab::Machine].iter().position(|t| *t == self.tab).unwrap_or(0);
        // Rendered now: the closures below keep `cx` borrowed.
        let tabs = self.tab_seg.render(tab_index, tabs, window, cx);
        // A menu under its button, in the bar's look.
        let popup = |id: &'static str, menu: Div, closed: &'static str| {
            deferred(
                menu.id(id)
                    .absolute()
                    .top(px(34.0))
                    .left_0()
                    // An open menu takes the pointer: nothing behind it scrolls or reacts.
                    .occlude()
                    .p(px(4.0))
                    .rounded(px(10.0))
                    .bg(panel())
                    .border_1()
                    .border_color(rgba(0xffffff1f))
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(move |s, _, _, cx| {
                        (s.project_menu, s.range_menu) = (false, false);
                        s.menu_closed = Some((closed, Instant::now()));
                        cx.notify();
                    }))
                    .with_animation(
                        SharedString::from(format!("{id}-{}", self.menu_opens)),
                        Animation::new(Duration::from_millis(140)).with_easing(ease_in_out),
                        |el, t| el.opacity(t).mt(px(-4.0 * (1.0 - t))),
                    ),
            )
            .with_priority(1)
        };
        let item = |id: String, on: bool| {
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
        };
        // The range: its button says which; the others are in its menu.
        let range_now = RANGES.iter().find(|r| r.hours == self.filters.range_h).map_or("24h", |r| r.label);
        let range_menu = self.range_menu.then(|| {
            let rows = RANGES.into_iter().map(|RangeChoice { label, hours: h }| {
                item(format!("range-{label}"), self.filters.range_h == h).child(range_label(label)).on_click(cx.listener(
                    move |s, _, _, cx| {
                        s.range_menu = false;
                        s.filter(|f| f.range_h = h, cx);
                    },
                ))
            });
            popup("range-menu", div().min_w(px(110.0)).children(rows), "range")
        });
        let range = div()
            .relative()
            .child(
                ctl("range", self.range_menu)
                    .child(crate::icons::line(gpui_kit::assets::IconName::Clock, muted(), 13.0))
                    .child(range_label(range_now))
                    .child(div().text_color(muted()).child("▾"))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.range_menu = !s.range_menu && s.reopens("range");
                        s.menu_opens += 1;
                        cx.notify();
                    })),
            )
            .children(range_menu);
        let stats = self.graph.as_ref().filter(|_| wide).map(|g| {
            let v = visible(g, &self.filters, now_ms());
            let sessions = v.nodes.iter().filter(|&&i| is_session(&g.nodes[i])).count();
            let agents = v.nodes.iter().filter(|&&i| g.nodes[i].kind == NodeKind::Agent).count();
            let messages = v.messages.iter().filter(|&&m| g.messages[m].kind == MsgKind::Message).count();
            let stat = |n: usize, key: &str| {
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(n.to_string()))
                    .child(div().text_color(muted()).child(i18n::count(key, n as u64)))
            };
            div()
                .flex()
                .gap(px(12.0))
                .text_size(px(12.0))
                .child(stat(sessions, "shell.stats.sessions"))
                .child(stat(messages, "shell.stats.messages"))
                .child(stat(agents, "shell.stats.subagents"))
        });

        // Projects, the busiest first, in a menu under their button.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        let sessions = self.graph.iter().flat_map(|g| g.nodes.iter()).filter(|n| is_session(n));
        for n in sessions {
            *counts.entry(n.project.as_str()).or_default() += 1;
        }
        let mut projects: Vec<(&str, usize)> = counts.into_iter().collect();
        projects.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let current = self.filters.project.clone();
        let hidden = !self.filters.archived || !self.filters.agents;
        let menu = self.project_menu.then(|| {
            let row = |id: String, label: String, pick: Option<String>, on: bool| {
                item(id, on).child(label).on_click(cx.listener(move |s, _, _, cx| {
                    s.project_menu = false;
                    let pick = pick.clone();
                    s.filter(|f| f.project = pick, cx);
                }))
            };
            let mut rows = vec![row("p-all".into(), t!("shell.all_projects").into_owned(), None, current.is_none())];
            rows.extend(
                projects
                    .iter()
                    .map(|(p, c)| row(format!("p-{p}"), format!("{p} · {c}"), Some(p.to_string()), current.as_deref() == Some(*p))),
            );
            // What else the map and the timeline show; the menu stays open to flip both.
            let check = |id: &'static str, label: String, on: bool, flip: fn(&mut Filters)| {
                item(id.into(), false)
                    .flex()
                    .items_center()
                    .gap(px(8.0))
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
            let list = div()
                .id("project-list")
                .max_h(px(300.0))
                .overflow_y_scroll()
                .track_scroll(&self.menu_scroll.handle)
                .children(rows)
                .child(self.menu_scroll.driver());
            let checks = div()
                .mt(px(4.0))
                .pt(px(4.0))
                .border_t_1()
                .border_color(line())
                .child(check("archived", t!("shell.archived").into_owned(), self.filters.archived, |f| f.archived = !f.archived))
                .child(check("agents", t!("shell.subagents").into_owned(), self.filters.agents, |f| f.agents = !f.agents));
            popup("project-menu", div().min_w(px(200.0)).child(list).child(checks), "project")
        });
        let project = div()
            .relative()
            .child(
                ctl("project", current.is_some())
                    .relative()
                    .max_w(px(150.0))
                    .child(div().overflow_hidden().child(clip(current.as_deref().unwrap_or(&t!("shell.all_projects")), 16)))
                    .child(div().text_color(muted()).child("▾"))
                    // Archived sessions or subagents hidden (from its menu): a dot says the map shows less.
                    .when(hidden, |d| d.child(div().absolute().top(px(4.0)).right(px(4.0)).size(px(6.0)).rounded_full().bg(blue())))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.project_menu = !s.project_menu && s.reopens("project");
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
        // The tabs come first and never move; the window's title already names the app. When the window is too
        // narrow for one row, the map's controls go to a second row, which wraps again if it must.
        let one_row = !map_tab || f32::from(window.viewport_size().width) >= 900.0 + labels_extra(window);
        let narrow = f32::from(window.viewport_size().width) < NARROW;
        let panel_on = if narrow { self.panel_narrow_open } else { self.panel_open };
        let controls: Vec<AnyElement> = if map_tab {
            vec![
                range.into_any_element(),
                div().w(px(120.0)).flex_shrink_0().child(Input::new(&self.search)).into_any_element(),
                project.into_any_element(),
                ctl("replay", replaying)
                    .child(if replaying { t!("shell.stop") } else { t!("shell.replay") }.into_owned())
                    .on_click(cx.listener(|s, _, _, cx| {
                        if s.replay.take().is_none() {
                            s.start_replay(cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            ]
        } else {
            Vec::new()
        };
        let update = self.update_button(cx);
        // The side panel's button, at the right end: above the panel it opens and closes.
        let panel_toggle = map_tab.then(|| {
            ctl("panel-toggle", panel_on).child(panel_icon(panel_on)).on_click(cx.listener(move |s, _, _, cx| {
                s.panel_last = Instant::now();
                if narrow {
                    // Narrow, the panel lies over the map: opening it there is not a saved choice.
                    s.panel_narrow_open = !s.panel_narrow_open;
                } else {
                    s.panel_open = !s.panel_open;
                    let open = s.panel_open;
                    settings::change(cx, |st| st.panel = open);
                }
                cx.notify();
            }))
        });
        let first = div()
            .relative()
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(tabs)
            .children(stats.filter(|_| map_tab))
            // The loader lies in the free room after the tabs and counters, cut to it when the window is narrow.
            .child(div().flex_1().min_w_0().self_stretch().relative().overflow_hidden().children(self.loader()));
        let settings = self.settings_button(cx);
        let bar = div().px(px(16.0)).py(px(10.0)).border_b_1().border_color(line());
        if one_row {
            bar.child(first.children(controls).children(update).child(settings).children(panel_toggle))
        } else {
            bar.flex()
                .flex_col()
                .gap(px(8.0))
                .child(first.children(update).child(settings).children(panel_toggle))
                .child(div().flex().flex_wrap().items_center().gap(px(8.0)).children(controls))
        }
    }

    /// While the history is read after launch: how far it is, in the free room right of the tabs (laid over the bar,
    /// so nothing moves when it goes). The bar fills smoothly and the whole fades out once everything is read.
    fn loader(&self) -> Option<Div> {
        let u = self.usage.as_ref()?;
        #[cfg(feature = "snapshot")]
        if crate::demo::on() {
            return None; // screenshots show the app at rest
        }
        if self.load_alpha <= 0.01 {
            return None;
        }
        let label = if u.ready { t!("shell.history_read") } else { t!("shell.reading_history") }.into_owned();
        Some(
            div().absolute().top_0().bottom_0().left_0().flex().items_center().opacity(self.load_alpha).child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .w(px(180.0))
                    .max_w(relative(1.0))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(11.0))
                            .child(div().text_color(muted()).child(label))
                            .child(div().font_family(MONO).text_color(faint()).child(format!("{:.0}%", self.load_fill * 100.0))),
                    )
                    .child(
                        div()
                            .h(px(3.0))
                            .w_full()
                            .rounded(px(2.0))
                            .bg(rgba(0xffffff1a))
                            .child(div().h_full().rounded(px(2.0)).w(relative(self.load_fill)).bg(if u.ready { green() } else { blue() })),
                    ),
            ),
        )
    }

    /// A newer version, at the top right: a grey arrow with its progress while it downloads, then green; a click
    /// installs it and starts the app again.
    fn update_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::update::State;
        let (ready, label, done) = match &self.update {
            State::Idle => return None,
            State::Downloading { version, done } => {
                (false, t!("shell.update.downloading", version = version, percent = format!("{:.0}", done * 100.0)).into_owned(), *done)
            }
            State::Ready { version, .. } => (true, t!("shell.update.ready", version = version).into_owned(), 1.0),
        };
        let sub = if ready { t!("shell.update.click_to_restart") } else { t!("shell.update.installs_on_click") }.into_owned();
        let color = if ready { green() } else { muted() };
        Some(
            div()
                .id("update")
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .w(px(32.0))
                .h(px(30.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(if ready { rgba(0x4ade8059) } else { line() })
                .bg(if ready { rgba(0x4ade801f) } else { panel() })
                .cursor_pointer()
                .hover(|s| s.bg(if ready { rgba(0x4ade802e) } else { panel_hi() }))
                .child(crate::icons::line(gpui_kit::assets::IconName::Download, color, 16.0))
                .when(!ready, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(7.0))
                            .right(px(7.0))
                            .bottom(px(4.0))
                            .h(px(2.0))
                            .rounded(px(1.0))
                            .bg(rgba(0xffffff1f))
                            .child(div().h_full().rounded(px(1.0)).w(relative(done)).bg(muted())),
                    )
                })
                .tooltip(move |_, cx| cx.new(|_| Tip { title: label.clone(), detail: sub.clone() }).into())
                .on_click(cx.listener(|s, _, _, cx| {
                    if let State::Ready { file, .. } = &s.update {
                        match crate::update::apply(file) {
                            Ok(()) => cx.quit(),
                            Err(e) => eprintln!("update: {e}"),
                        }
                    }
                }))
                .with_animation("update-in", Animation::new(Duration::from_millis(300)).with_easing(ease_in_out), |el, t| el.opacity(t))
                .into_any_element(),
        )
    }

    /// Whether a click on `menu`'s button may open it: not when that same press just closed it.
    fn reopens(&self, menu: &str) -> bool {
        !self.menu_closed.is_some_and(|(m, at)| m == menu && at.elapsed() < Duration::from_millis(400))
    }

    /// Why the machine typed in the form can't be added yet (None: it can).
    fn machine_invalid(&self, cx: &App) -> Option<String> {
        let (name, target) = (self.machine_name.read(cx).value(), self.machine_target.read(cx).value());
        crate::machines::check(&name, &target, &cx.global::<Settings>().machines).err()
    }

    fn machine_form(&mut self, e: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e {
            InputEvent::PressEnter { .. } => self.add_machine(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        }
    }

    fn add_machine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.machine_invalid(cx).is_some() {
            return;
        }
        let name = self.machine_name.read(cx).value().trim().to_string();
        let target = self.machine_target.read(cx).value().trim().to_string();
        let mut list = cx.global::<Settings>().machines.clone();
        list.push(settings::Machine { name, target });
        settings::set_machines(cx, list);
        for input in [&self.machine_name, &self.machine_target] {
            input.update(cx, |i, cx| i.set_value("", window, cx));
        }
        cx.notify();
    }

    /// How the other machines are, for the dot on the settings button: red when one failed, amber while one is on its
    /// way, green when all are connected; none without machines.
    fn machines_dot(&self, cx: &App) -> Option<Rgba> {
        use crate::machines::Status;
        let states: Vec<Status> = cx
            .global::<Settings>()
            .machines
            .iter()
            .map(|m| crate::machines::status(&m.name).map_or(Status::Connecting, |s| s.status))
            .collect();
        if states.iter().any(|s| matches!(s, Status::Failed(_))) {
            Some(red())
        } else if states.iter().any(|s| *s != Status::Connected) {
            Some(amber())
        } else {
            (!states.is_empty()).then(green)
        }
    }

    /// The other machines, in the settings: each with what it says, removed after a yes, and the form that adds one.
    fn machines_section(&self, cx: &mut Context<Self>) -> Div {
        use crate::machines::{MachineState, Status};
        let machines = cx.global::<Settings>().machines.clone();
        let connecting = || MachineState { status: Status::Connecting, sessions: 0 };
        let states: Vec<MachineState> = machines.iter().map(|m| crate::machines::status(&m.name).unwrap_or_else(connecting)).collect();
        let color_of = |s: &Status| match s {
            Status::Connected => green(),
            Status::Failed(_) => red(),
            _ => amber(),
        };
        let rows = machines.iter().zip(&states).map(|(m, MachineState { status, sessions })| {
            let name = m.name.clone();
            let confirming = self.machine_remove.as_deref() == Some(m.name.as_str());
            let said = match status {
                Status::Connecting => t!("machines.connecting").into_owned(),
                Status::Installing => t!("machines.installing").into_owned(),
                Status::Connected => i18n::count("machines.sessions", *sessions as u64),
                Status::Failed(e) => e.clone(),
            };
            let failed = matches!(status, Status::Failed(_));
            let small = |id: String, label: String, strong: bool| {
                div()
                    .id(SharedString::from(id))
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(if strong { red() } else { muted() })
                    .border_1()
                    .border_color(if strong { rgba(0xf8717159) } else { line() })
                    .hover(|s| s.bg(panel_hi()))
                    .child(label)
            };
            div()
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(8.0))
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().size(px(8.0)).flex_none().rounded_full().bg(color_of(status)))
                        .child(div().flex_1().text_size(px(13.0)).text_color(text()).child(m.name.clone()))
                        .child(
                            div()
                                .id(SharedString::from(format!("machine-remove-{}", m.name)))
                                .px(px(6.0))
                                .rounded(px(6.0))
                                .cursor_pointer()
                                .text_size(px(13.0))
                                .text_color(faint())
                                .hover(|s| s.text_color(text()).bg(panel_hi()))
                                .child("✕")
                                .tooltip(|_, cx| {
                                    cx.new(|_| Tip { title: t!("machines.remove").into_owned(), detail: String::new() }).into()
                                })
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.machine_remove = Some(name.clone());
                                    cx.notify();
                                })),
                        ),
                )
                .child(div().pl(px(16.0)).font_family(MONO).text_size(px(11.0)).text_color(faint()).child(m.target.clone()))
                .child(
                    div()
                        .pl(px(16.0))
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .text_color(if failed { red() } else { muted() })
                        .child(said),
                )
                .when(confirming, |d| {
                    let name = m.name.clone();
                    d.child(
                        div()
                            .mt(px(4.0))
                            .ml(px(16.0))
                            .p(px(8.0))
                            .rounded(px(8.0))
                            .bg(rgba(0xf871711a))
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(div().text_size(px(12.0)).text_color(text()).child(t!("machines.confirm", name = m.name).into_owned()))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.0))
                                    .child(
                                        small(format!("machine-yes-{}", m.name), t!("machines.confirm_yes").into_owned(), true).on_click(
                                            cx.listener(move |s, _, _, cx| {
                                                let list =
                                                    cx.global::<Settings>().machines.iter().filter(|m| m.name != name).cloned().collect();
                                                settings::set_machines(cx, list);
                                                s.machine_remove = None;
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(small(format!("machine-no-{}", m.name), t!("machines.cancel").into_owned(), false).on_click(
                                        cx.listener(|s, _, _, cx| {
                                            s.machine_remove = None;
                                            cx.notify();
                                        }),
                                    )),
                            ),
                    )
                })
        });
        let invalid = self.machine_invalid(cx);
        let typed = !self.machine_name.read(cx).value().is_empty() || !self.machine_target.read(cx).value().is_empty();
        let add = div()
            .id("machine-add")
            .flex_none()
            .px(px(12.0))
            .h(px(30.0))
            .flex()
            .items_center()
            .rounded(px(8.0))
            .text_size(px(12.0))
            .border_1()
            .border_color(if invalid.is_none() { rgba(0x38bdf866) } else { line() })
            .text_color(if invalid.is_none() { blue() } else { faint() })
            .when(invalid.is_none(), |d| d.cursor_pointer().hover(|s| s.bg(panel_hi())))
            .child(t!("machines.add").into_owned())
            // A grey button says why it is grey.
            .when_some(invalid.clone(), |d, why| {
                d.tooltip(move |_, cx| cx.new(|_| Tip { title: why.clone(), detail: String::new() }).into())
            })
            .on_click(cx.listener(|s, _, window, cx| s.add_machine(window, cx)));
        div().flex().flex_col().gap(px(2.0)).children(rows).child(
            div()
                .mt(px(4.0))
                .p(px(8.0))
                .border_t_1()
                .border_color(line())
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(Input::new(&self.machine_name))
                .child(div().flex().gap(px(6.0)).child(div().flex_1().child(Input::new(&self.machine_target))).child(add))
                // Typed but not addable: why, under the form, until it is fixed.
                .when_some(invalid.filter(|_| typed), |d, why| d.child(div().text_size(px(11.0)).text_color(faint()).child(why))),
        )
    }

    /// The settings button, at the top right: its menu became the settings window. The dot says how the other
    /// machines are.
    fn settings_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let dot = self.machines_dot(cx);
        div()
            .id("settings-button")
            .relative()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .w(px(32.0))
            .h(px(30.0))
            .rounded(px(8.0))
            .bg(panel())
            .border_1()
            .border_color(line())
            .cursor_pointer()
            .hover(|s| s.bg(panel_hi()))
            .child(crate::icons::line(gpui_kit::assets::IconName::Settings, muted(), 15.0))
            .children(dot.map(|c| div().absolute().top(px(5.0)).right(px(5.0)).size(px(6.0)).rounded_full().bg(c)))
            .tooltip(|_, cx| {
                cx.new(|_| Tip { title: t!("settings.title").into_owned(), detail: t!("settings.button_sub").into_owned() }).into()
            })
            .on_click(cx.listener(|s, _, window, cx| s.open_settings(window, cx)))
            .into_any_element()
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        (self.settings_open, self.machine_remove) = (true, None);
        // A machine that failed: that is what the red dot on the button was about.
        if self.machines_dot(cx) == Some(red()) {
            self.settings_page = SettingsPage::Machines;
        }
        self.settings_focus.focus(window, cx);
        cx.notify();
    }

    /// The inputs keep the placeholders they were made with: they get this language's.
    fn relabel_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        for (input, key) in
            [(&self.search, "shell.search"), (&self.machine_name, "machines.name"), (&self.machine_target, "machines.target")]
        {
            input.update(cx, |i, cx| i.set_placeholder(t!(key).to_string(), window, cx));
        }
        self.machine.update(cx, |m, cx| m.relabel(window, cx));
    }

    /// Checks for a newer version now; the answer shows under the button.
    fn check_updates(&mut self, cx: &mut Context<Self>) {
        if self.update_checking {
            return;
        }
        (self.update_checking, self.update_answer) = (true, None);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async { crate::update::check_now() }).await;
            this.update(cx, |s, cx| {
                (s.update_checking, s.update_answer) = (false, Some(crate::menus::checked(found)));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The settings, over the window: the language, the other machines, the updates. Esc, ✕ or a click outside it
    /// closes it.
    fn settings_window(&self, cx: &mut Context<Self>) -> AnyElement {
        let close = |s: &mut Shell, cx: &mut Context<Shell>| {
            (s.settings_open, s.machine_remove) = (false, None);
            cx.notify();
        };
        let section = |title: String, sub: String| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .pb(px(8.0))
                .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(text()).child(title))
                .when(!sub.is_empty(), |d| d.child(div().text_size(px(12.0)).line_height(px(17.0)).text_color(muted()).child(sub)))
        };
        let chosen = cx.global::<Settings>().language.clone();
        let choice = |id: &'static str, label: String, pick: Option<&'static str>| {
            let on = chosen.as_deref() == pick;
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .hover(|s| s.bg(panel_hi()))
                .text_size(px(13.0))
                .text_color(if on { text() } else { muted() })
                .child(
                    div()
                        .size(px(14.0))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(if on { blue() } else { muted() })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(on, |d| d.child(div().size(px(6.0)).rounded_full().bg(blue()))),
                )
                .child(label)
                .on_click(cx.listener(move |s, _, window, cx| {
                    settings::set_language(cx, pick.map(str::to_string));
                    s.relabel_inputs(window, cx);
                    cx.notify();
                }))
        };
        let system = t!("language.system", name = i18n::name(settings::system_language())).into_owned();
        let mut languages = vec![choice("lang-system", system, None)];
        languages.extend(i18n::LANGUAGES.iter().map(|l| choice(l.code, l.name.to_string(), Some(l.code))));
        let check = div()
            .id("settings-check")
            .flex_none()
            .px(px(12.0))
            .h(px(30.0))
            .flex()
            .items_center()
            .rounded(px(8.0))
            .text_size(px(12.0))
            .border_1()
            .border_color(if self.update_checking { line() } else { rgba(0x38bdf866) })
            .text_color(if self.update_checking { faint() } else { blue() })
            .when(!self.update_checking, |d| d.cursor_pointer().hover(|s| s.bg(panel_hi())))
            .child(if self.update_checking { t!("settings.checking") } else { t!("settings.check") }.into_owned())
            .on_click(cx.listener(|s, _, _, cx| s.check_updates(cx)));
        let answer = self.update_answer.clone().map(|Said { level, title, detail }| {
            let warning = level == PromptLevel::Warning;
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .text_size(px(12.0))
                .child(div().text_color(if warning { red() } else { text() }).child(title))
                .child(div().text_color(muted()).child(detail))
        });
        use gpui_kit::assets::IconName;
        let page = self.settings_page;
        let pages = [
            (SettingsPage::Language, IconName::Languages, t!("settings.language")),
            (SettingsPage::Machines, IconName::Server, t!("settings.machines")),
            (SettingsPage::Updates, IconName::Download, t!("settings.updates")),
        ];
        let dot = self.machines_dot(cx);
        let nav = div()
            .w(px(210.0))
            .flex_none()
            .h_full()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .border_r_1()
            .border_color(line())
            .bg(rgba(0xffffff05))
            .child(
                div()
                    .px(px(10.0))
                    .pt(px(6.0))
                    .pb(px(12.0))
                    .text_size(px(16.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t!("settings.title").into_owned()),
            )
            .children(pages.iter().map(|(p, icon, label)| {
                let (p, on) = (*p, *p == page);
                div()
                    .id(SharedString::from(format!("settings-page-{p:?}")))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .text_size(px(13.0))
                    .text_color(if on { text() } else { muted() })
                    .when(on, |d| d.bg(panel_hi()))
                    .hover(|s| s.bg(panel_hi()))
                    .child(crate::icons::line(*icon, if on { blue() } else { muted() }, 15.0))
                    .child(div().flex_1().child(label.to_string()))
                    .children(dot.filter(|_| p == SettingsPage::Machines).map(|c| div().size(px(6.0)).rounded_full().bg(c)))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        (s.settings_page, s.machine_remove) = (p, None);
                        cx.notify();
                    }))
            }));
        let content = match page {
            SettingsPage::Language => {
                div().child(section(t!("settings.language").into_owned(), t!("settings.language_sub").into_owned())).children(languages)
            }
            SettingsPage::Machines => div()
                .child(section(t!("settings.machines").into_owned(), t!("machines.intro").into_owned()))
                .child(self.machines_section(cx)),
            SettingsPage::Updates => div()
                .child(section(
                    t!("settings.updates").into_owned(),
                    t!("settings.version", version = env!("CARGO_PKG_VERSION")).into_owned(),
                ))
                .child(div().flex().items_start().gap(px(14.0)).child(check).children(answer)),
        };
        let card = div()
            .id("settings")
            .track_focus(&self.settings_focus)
            .on_key_down(cx.listener(move |s, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    close(s, cx);
                }
            }))
            .on_mouse_down_out(cx.listener(move |s, _, _, cx| close(s, cx)))
            .w(px(880.0))
            .max_w(relative(0.94))
            .h(px(580.0))
            .max_h(relative(0.88))
            .flex()
            .overflow_hidden()
            .rounded(px(14.0))
            .bg(panel())
            .border_1()
            .border_color(rgba(0xffffff1f))
            .shadow_lg()
            .child(nav)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(div().id("settings-body").size_full().overflow_y_scroll().px(px(28.0)).pt(px(24.0)).pb(px(24.0)).child(content))
                    .child(
                        div()
                            .id("settings-close")
                            .absolute()
                            .top(px(14.0))
                            .right(px(14.0))
                            .px(px(8.0))
                            .py(px(2.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .text_color(muted())
                            .hover(|s| s.text_color(text()).bg(panel_hi()))
                            .child("✕")
                            .on_click(cx.listener(move |s, _, _, cx| close(s, cx))),
                    ),
            );
        deferred(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .occlude()
                .bg(rgba(0x0000008c))
                .flex()
                .items_center()
                .justify_center()
                .child(card)
                .with_animation("settings-in", Animation::new(Duration::from_millis(160)).with_easing(ease_in_out), |el, t| el.opacity(t)),
        )
        .with_priority(2)
        .into_any_element()
    }

    /// The live feed, at the bottom of the side panel: the newest line slides in at the top. Its grip, the top edge,
    /// sets its height; its title folds it.
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
                        .child(div().truncate().text_size(px(12.0)).text_color(muted()).child(e.what.say())),
                )
                .with_animation(
                    SharedString::from(format!("ev-in-{}", e.seq)),
                    Animation::new(SLIDE_IN).with_easing(ease_out_quint()),
                    |el, t| el.opacity(t).mt(px(6.0 * (1.0 - t))),
                )
        });
        let open = self.feed_open;
        let height = px(self.feed_h);
        let body = div()
            .id("feed-lines")
            .h(height)
            .overflow_y_scroll()
            .track_scroll(&self.feed_scroll.handle)
            .px(px(6.0))
            .pb(px(6.0))
            .children(lines)
            .when(self.feed.is_empty(), |d| {
                d.child(
                    div()
                        .px(px(10.0))
                        .py(px(4.0))
                        .text_size(px(12.0))
                        .text_color(faint())
                        .truncate()
                        .child(t!("shell.feed.waiting").into_owned()),
                )
            })
            .child(self.feed_scroll.driver())
            .with_animation(
                SharedString::from(format!("feed-{}", self.feed_clicks)),
                Animation::new(Duration::from_millis(220)).with_easing(ease_in_out),
                move |el, t| {
                    let shown = if open { t } else { 1.0 - t };
                    if open && t >= 1.0 { el } else { el.max_h(height * shown).opacity(shown) }
                },
            );
        // The grip: a short bar on the top edge, and the cursor that says it moves up and down.
        let grip = div()
            .id("feed-grip")
            .h(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor(CursorStyle::ResizeUpDown)
            .hover(|s| s.bg(rgba(0x38bdf81f)))
            .child(div().w(px(32.0)).h(px(3.0)).rounded_full().bg(line()))
            .on_drag(FeedGrip, |_, _, _, cx| cx.new(|_| Nothing));
        div()
            .flex_none()
            .border_t_1()
            .border_color(line())
            .bg(panel())
            .when(open, |d| d.child(grip))
            .child(
                div()
                    .id("feed-toggle")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(16.0))
                    .pt(px(if open { 2.0 } else { 10.0 }))
                    .pb(px(8.0))
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(faint())
                    .cursor_pointer()
                    .hover(|s| s.text_color(text()))
                    .child(t!("shell.feed.title").into_owned())
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
            .map(|d| d.format(&i18n::names(&d, "%a %-d %b · %H:%M")).to_string())
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
        let (msg, act) = if !self.filters.query.is_empty() {
            (t!("shell.empty.no_match", query = self.search.read(cx).value().trim()), t!("shell.empty.clear_search"))
        } else if let Some(p) = &self.filters.project {
            (t!("shell.empty.none_in_project", project = p), t!("shell.all_projects"))
        } else {
            (t!("shell.empty.none"), t!("shell.empty.show_all"))
        };
        let (msg, act) = (msg.into_owned(), act.into_owned());
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

/// A range's button: "3d" says "3 j" in French.
fn range_label(label: &str) -> String {
    match label {
        "6h" => t!("shell.range.6h"),
        "24h" => t!("shell.range.24h"),
        "3d" => t!("shell.range.3d"),
        "7d" => t!("shell.range.7d"),
        "14d" => t!("shell.range.14d"),
        "30d" => t!("shell.range.30d"),
        "All" => t!("shell.range.all"),
        other => other.into(),
    }
    .into_owned()
}

/// How wide `s` is drawn at `size`.
fn text_w(window: &mut Window, s: String, size: f32, weight: FontWeight) -> f32 {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = TextRun { len: s.len(), font, color: text().into(), background_color: None, underline: None, strikethrough: None };
    f32::from(window.text_system().shape_line(s.into(), px(size), &[run], None).width())
}

/// How much wider the top bar's labels are in this language than in English, whose bar fits on one row from 1320 px.
fn labels_extra(window: &mut Window) -> f32 {
    const KEYS: [&str; 7] = [
        "shell.tab.map",
        "shell.tab.timeline",
        "shell.tab.usage",
        "shell.tab.machine",
        "shell.range.24h",
        "shell.all_projects",
        "shell.replay",
    ];
    let mut extra = 0.0;
    for key in KEYS {
        extra += text_w(window, t!(key).into_owned(), 12.0, FontWeight::NORMAL)
            - text_w(window, t!(key, locale = "en").into_owned(), 12.0, FontWeight::NORMAL);
    }
    extra.max(0.0)
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
        let timed = clicks.split(';').filter(|c| !c.is_empty()).enumerate();
        for (i, c) in timed {
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
        // MDB_WHEEL_DELAY: the first notch comes this many ms in (after a click, say).
        let delay: f32 = std::env::var("MDB_WHEEL_DELAY").ok().and_then(|d| d.parse().ok()).unwrap_or(0.0);
        while sent < w.count && ms >= delay + sent as f32 * w.gap {
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
        let narrow = f32::from(window.viewport_size().width) < NARROW;
        let target = if (narrow && self.panel_narrow_open) || (!narrow && self.panel_open) { panel::WIDTH } else { 0.0 };
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
            Tab::Map => {
                div().size_full().relative().child(self.map.clone()).children(self.clock(cx)).children(self.empty(cx)).into_any_element()
            }
            Tab::Timeline => div().size_full().relative().child(self.timeline.clone()).children(self.empty(cx)).into_any_element(),
            Tab::Machine => self.machine.clone().into_any_element(),
            Tab::Usage => self.dash.clone().into_any_element(),
        };
        self.panel.update(cx, |p, _| p.active = map_tab && self.panel_w > 0.0);
        let wide = f32::from(window.viewport_size().width) >= 1700.0;
        // The panel: next to the map and the timeline, or over them when the window is narrow. The live feed is at
        // its bottom; dragging the feed's grip shares the height between them.
        let side = (map_tab && self.panel_w > 0.0).then(|| {
            div().h_full().flex_none().w(px(self.panel_w)).overflow_hidden().border_l_1().border_color(line()).child(
                div()
                    .w(px(panel::WIDTH))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().child(self.panel.clone()))
                    .child(self.feed(cx))
                    .on_drag_move(cx.listener(|s, e: &DragMoveEvent<FeedGrip>, _, cx| {
                        let room = f32::from(e.bounds.size.height) - PANEL_MIN_H - FEED_HEAD_H;
                        let h = f32::from(e.bounds.bottom() - e.event.position.y) - FEED_HEAD_H;
                        s.feed_h = h.clamp(FEED_MIN_H, room.max(FEED_MIN_H));
                        s.feed_resized = true;
                        cx.notify();
                    })),
            )
        });
        let content = div().flex_1().min_w_0().h_full().child(body);
        let main = if narrow {
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .flex()
                .child(content)
                .children(side.map(|p| div().absolute().top_0().bottom_0().right_0().shadow_lg().bg(panel()).child(p)))
        } else {
            div().flex_1().min_h_0().flex().child(content).children(side)
        };
        let settings = self.settings_open.then(|| self.settings_window(cx));
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg())
            .text_color(text())
            .text_size(px(13.0))
            // The feed's new height is saved once the mouse lets go of its grip.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|s, _, _, cx| {
                    if std::mem::take(&mut s.feed_resized) {
                        let h = s.feed_h;
                        settings::change(cx, |st| st.feed_h = h);
                    }
                }),
            )
            .child(self.header(wide, window, cx))
            .child(main)
            .children(settings)
    }
}

/// What the feed's grip carries while it is dragged.
struct FeedGrip;

/// Nothing to draw under the mouse while the grip is dragged.
struct Nothing;

impl Render for Nothing {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The settings window's categories, listed on its left.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SettingsPage {
    Language,
    Machines,
    Updates,
}

/// The window's shell, for the menus that open its settings.
pub struct ShellHandle(pub WeakEntity<Shell>);

impl Global for ShellHandle {}

/// Opens the settings window (from a menu).
pub fn open_settings(cx: &mut App) {
    let Some(shell) = cx.try_global::<ShellHandle>().and_then(|h| h.0.upgrade()) else { return };
    let Some(w) = cx.windows().into_iter().next() else { return };
    w.update(cx, |_, window, cx| shell.update(cx, |s, cx| s.open_settings(window, cx))).ok();
}

/// A small hover card: a title and a line under it.
struct Tip {
    title: String,
    detail: String,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p(px(10.0))
            .rounded(px(10.0))
            .bg(rgba(0x11121ad0))
            .border_1()
            .border_color(line())
            .text_size(px(12.0))
            .text_color(text())
            .child(div().font_weight(FontWeight::SEMIBOLD).child(self.title.clone()))
            .when(!self.detail.is_empty(), |d| d.child(div().text_color(muted()).child(self.detail.clone())))
    }
}
