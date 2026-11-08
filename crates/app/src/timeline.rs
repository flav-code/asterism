//! The Timeline tab: one row per session, newest at the top, the sessions it started or forked under it. A bar
//! runs from its start to its last activity, its subagents under the bar, and messages between sessions are arcs.
//! The wheel scrolls the rows; with ⌘, ctrl or alt it zooms the time; dragging moves the time.
use crate::glide::{self, Glide};
use crate::state::{Filters, Visible, is_session, node_tip, project_colors, visible, working};
use crate::ui::*;
use crate::ui::{blue, green, red}; // over gpui's colors of the same names
use chrono::{Datelike, Duration as Days, Local, NaiveDate, TimeZone, Timelike};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mdb_core::graph::{EdgeKind, Graph, MsgKind, NodeKind};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROW: f32 = 26.0;
const LEFT: f32 = 300.0;
const AXIS: f32 = 34.0;
/// How long a message that just arrived blinks.
const BLINK: Duration = Duration::from_secs(6);

pub enum TimelineEvent {
    Select,
}

/// What the rows show, worked out once per data update.
#[derive(Default)]
struct Layout {
    /// Graph node and indent per row.
    rows: Vec<(usize, usize)>,
    /// Graph node → row; a subagent shares its session's row.
    row_of: HashMap<usize, usize>,
    /// Subagents with a row.
    agents: Vec<usize>,
    /// Spawn and fork edges: from row, to row, the new session.
    links: Vec<(usize, usize, usize, EdgeKind)>,
    /// Messages between two rows: message, from row, to row.
    msgs: Vec<(usize, usize, usize)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Hover {
    Node(usize),
    Msg(usize),
}

pub struct TimelineView {
    graph: Option<Arc<Graph>>,
    colors: HashMap<String, Rgba>,
    v: Option<Visible>,
    layout: Rc<Layout>,
    /// Time zoom and shift, as d3: x = tx + k × x at zoom 1.
    k: f32,
    tx: f32,
    zoom: Option<(f32, f32, Instant)>,
    scroll: Glide,
    /// Where the rows were drawn last frame, in window coordinates.
    chart: Rc<Cell<Bounds<Pixels>>>,
    hover: Option<Hover>,
    hover_row: Option<usize>,
    mouse: Point<Pixels>,
    drag: Option<(Pixels, f32, bool)>,
    pub selected: Option<String>,
    pub active: bool,
    /// Messages that arrived while the tab was open, and when they first showed: they blink.
    fresh: HashMap<String, Instant>,
    known: HashSet<String>,
    started: Instant,
    last_paint: Instant,
    _tick: Task<()>,
}

impl EventEmitter<TimelineEvent> for TimelineView {}

impl TimelineView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // The working stripes move and the "now" line creeps right: a few frames a second is enough.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(83)).await;
                let alive = this.update(cx, |t, cx| {
                    let moving = t.layout.rows.iter().any(|r| t.graph.as_ref().is_some_and(|g| working(&g.nodes[r.0])));
                    if t.active && (moving || !t.fresh.is_empty() || t.last_paint.elapsed() > Duration::from_secs(5)) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        TimelineView {
            graph: None,
            colors: HashMap::new(),
            v: None,
            layout: Rc::default(),
            k: 1.0,
            tx: 0.0,
            zoom: None,
            scroll: Glide::default(),
            chart: Rc::new(Cell::new(Bounds::default())),
            hover: None,
            hover_row: None,
            mouse: Point::default(),
            drag: None,
            selected: None,
            active: false,
            fresh: HashMap::new(),
            known: HashSet::new(),
            started: Instant::now(),
            last_paint: Instant::now(),
            _tick: tick,
        }
    }

    pub fn set_graph(&mut self, g: Arc<Graph>, filters: &Filters, cx: &mut Context<Self>) {
        let v = visible(&g, filters, now_ms());
        let index: HashMap<&str, usize> = g.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        // Spawned and forked sessions go under their parent, oldest first; the others newest first.
        let sessions: HashSet<usize> = v.nodes.iter().copied().filter(|&i| is_session(&g.nodes[i])).collect();
        let (mut parent, mut kids): (HashMap<usize, usize>, HashMap<usize, Vec<usize>>) = Default::default();
        for e in g.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Spawn | EdgeKind::Fork)) {
            if let (Some(&a), Some(&b)) = (index.get(e.from.as_str()), index.get(e.to.as_str()))
                && sessions.contains(&a)
                && sessions.contains(&b)
                && !parent.contains_key(&b)
            {
                parent.insert(b, a);
                kids.entry(a).or_default().push(b);
            }
        }
        let mut rows = Vec::new();
        let mut done = HashSet::new();
        fn visit(
            n: usize,
            depth: usize,
            g: &Graph,
            kids: &HashMap<usize, Vec<usize>>,
            done: &mut HashSet<usize>,
            rows: &mut Vec<(usize, usize)>,
        ) {
            if !done.insert(n) {
                return;
            }
            rows.push((n, depth));
            let mut under = kids.get(&n).cloned().unwrap_or_default();
            under.sort_by_key(|&k| g.nodes[k].created_at);
            for k in under {
                visit(k, depth + 1, g, kids, done, rows);
            }
        }
        let mut roots: Vec<usize> = sessions.iter().copied().filter(|i| !parent.contains_key(i)).collect();
        roots.sort_by_key(|&i| std::cmp::Reverse(g.nodes[i].created_at));
        let mut all: Vec<usize> = sessions.iter().copied().collect();
        all.sort_unstable();
        for i in roots.into_iter().chain(all) {
            visit(i, 0, &g, &kids, &mut done, &mut rows); // the second pass catches cycles, if any
        }
        let mut outside: Vec<usize> =
            v.nodes.iter().copied().filter(|&i| matches!(g.nodes[i].kind, NodeKind::Ghost | NodeKind::Remote)).collect();
        outside.sort_by_key(|&i| g.nodes[i].created_at);
        rows.extend(outside.into_iter().map(|node| (node, 0)));

        let mut row_of: HashMap<usize, usize> = rows.iter().enumerate().map(|(r, row)| (row.0, r)).collect();
        for &i in &v.nodes {
            let n = &g.nodes[i];
            if n.kind == NodeKind::Agent
                && let Some(r) = n.parent.as_deref().and_then(|p| index.get(p)).and_then(|p| row_of.get(p)).copied()
            {
                row_of.insert(i, r);
            }
        }
        // Messages seen for the first time while the tab is up blink; the first data only fills `known`.
        let now = now_ms();
        let first = self.known.is_empty();
        for m in &g.messages {
            if self.known.insert(m.id.clone()) && !first && now.saturating_sub(m.ts) < 90_000 {
                self.fresh.insert(m.id.clone(), Instant::now());
            }
        }
        let shown: HashMap<&str, usize> = v.nodes.iter().map(|&i| (g.nodes[i].id.as_str(), i)).collect();
        let row = |id: &str| shown.get(id).and_then(|i| row_of.get(i)).copied();
        let agents = v.nodes.iter().copied().filter(|&i| g.nodes[i].kind == NodeKind::Agent && row_of.contains_key(&i)).collect();
        let links = v
            .edges
            .iter()
            .map(|&e| &g.edges[e])
            .filter(|e| matches!(e.kind, EdgeKind::Spawn | EdgeKind::Fork))
            .filter_map(|e| Some((row(&e.from)?, row(&e.to)?, *shown.get(e.to.as_str())?, e.kind)))
            .collect();
        let msgs = v
            .messages
            .iter()
            .filter_map(|&m| Some((m, row(&g.messages[m].from)?, row(&g.messages[m].to)?)))
            .filter(|(_, a, b)| a != b)
            .collect();
        self.colors = project_colors(&g);
        self.layout = Rc::new(Layout { rows, row_of, agents, links, msgs });
        (self.graph, self.v) = (Some(g), Some(v));
        cx.notify();
    }

    fn scale(&self, w: f32, now: u64) -> Scale {
        let start = self.v.as_ref().map_or(now.saturating_sub(86_400_000), |v| v.start) as f64;
        let end = now as f64 + (now as f64 - start) * 0.03;
        Scale { start, end: end.max(start + 1.0), r0: LEFT + 8.0, r1: (w - 16.0).max(LEFT + 9.0), k: self.k, tx: self.tx }
    }

    /// Zoom the time to `k`, keeping the time under `x` where it is.
    fn zoom_at(&mut self, k: f32, x: f32) {
        self.tx = x - (x - self.tx) * k / self.k;
        self.k = k;
    }

    /// The row under the pointer, and the session, subagent or message there.
    fn hit(&self, p: Point<Pixels>) -> (Option<usize>, Option<Hover>) {
        let (Some(g), l) = (self.graph.as_ref(), &self.layout) else { return (None, None) };
        let b = self.chart.get();
        if l.rows.is_empty() || !b.contains(&p) || !self.scroll.handle.bounds().contains(&p) {
            return (None, None);
        }
        let (x, y) = (f32::from(p.x - b.origin.x), f32::from(p.y - b.origin.y));
        let row = ((y / ROW) as usize).min(l.rows.len() - 1);
        if x < LEFT {
            return (Some(row), Some(Hover::Node(l.rows[row].0)));
        }
        let now = now_ms();
        let s = self.scale(f32::from(b.size.width), now);
        let cy = |r: usize| r as f32 * ROW + ROW / 2.0;
        // An arc within 5 px, measured along the curve.
        let mut best = (5.0, None);
        for &(m, r1, r2) in &l.msgs {
            let (y1, y2) = (cy(r1), cy(r2));
            if y < y1.min(y2) || y > y1.max(y2) {
                continue;
            }
            let mx = s.at(g.messages[m].ts as f64);
            let d = (x - arc_x(mx, bulge(y1, y2), y1, y2, y)).abs();
            if d < best.0 {
                best = (d, Some(m));
            }
        }
        if let Some(m) = best.1 {
            return (Some(row), Some(Hover::Msg(m)));
        }
        // A bar, or a subagent's line under it.
        let n = &g.nodes[l.rows[row].0];
        let (x0, x1) = (s.at(n.created_at.max(1) as f64), s.at(if n.live { now } else { n.last_at.max(n.created_at) } as f64));
        if (y - cy(row)).abs() <= 6.0 && x >= x0 - 2.0 && x <= x1.max(x0 + 3.0) + 2.0 {
            return (Some(row), Some(Hover::Node(l.rows[row].0)));
        }
        for &i in l.agents.iter().filter(|i| l.row_of.get(i) == Some(&row)) {
            let a = &g.nodes[i];
            let (a0, a1) = (s.at(a.created_at as f64), s.at(if a.live { now } else { a.last_at } as f64));
            if (y - cy(row) - 8.0).abs() <= 3.0 && x >= a0 - 2.0 && x <= a1.max(a0 + 2.0) + 2.0 {
                return (Some(row), Some(Hover::Node(i)));
            }
        }
        (Some(row), None)
    }

    fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.emit(TimelineEvent::Select);
        cx.notify();
    }
}

/// The time scale: the range shown at zoom 1 is `start`…`end` over `r0`…`r1`.
#[derive(Clone, Copy)]
struct Scale {
    start: f64,
    end: f64,
    r0: f32,
    r1: f32,
    k: f32,
    tx: f32,
}

impl Scale {
    fn at(&self, t: f64) -> f32 {
        self.tx + self.k * (self.r0 + ((t - self.start) / (self.end - self.start)) as f32 * (self.r1 - self.r0))
    }
    fn time(&self, x: f32) -> f64 {
        self.start + (((x - self.tx) / self.k - self.r0) / (self.r1 - self.r0)) as f64 * (self.end - self.start)
    }
}

/// How far right an arc between two rows bulges.
fn bulge(y1: f32, y2: f32) -> f32 {
    8.0 + (36.0f32).min((y2 - y1).abs() * 0.12)
}

/// x of the arc `x → x + bulge → x` at height `y`, between `y1` and `y2` (a cubic with both handles at x + bulge).
fn arc_x(x: f32, bulge: f32, y1: f32, y2: f32, y: f32) -> f32 {
    // y(t) = y1 + (y2 - y1)(3t² - 2t³) is monotonic: find t by halving.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let f = |t: f32| (3.0 * t * t - 2.0 * t * t * t) * (y2 - y1) + y1;
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if (f(mid) < y) == (y2 > y1) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t = (lo + hi) / 2.0;
    x + 3.0 * bulge * t * (1.0 - t)
}

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// Axis ticks between `t0` and `t1` (ms), about `count` of them, on round local times.
fn ticks(t0: f64, t1: f64, count: f32) -> Vec<i64> {
    let step = (t1 - t0) / count as f64;
    const STEPS: [i64; 17] = [
        1_000,
        5_000,
        15_000,
        30_000,
        MINUTE,
        5 * MINUTE,
        15 * MINUTE,
        30 * MINUTE,
        HOUR,
        3 * HOUR,
        6 * HOUR,
        12 * HOUR,
        DAY,
        2 * DAY,
        7 * DAY,
        30 * DAY,
        91 * DAY,
    ];
    let every = STEPS.iter().copied().find(|&s| s as f64 >= step).unwrap_or(365 * DAY);
    let local = |ms: f64| Local.timestamp_millis_opt(ms as i64).single();
    let Some(first) = local(t0) else { return Vec::new() };
    let midnight = |d: NaiveDate| Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest().map(|t| t.timestamp_millis());
    let mut out = Vec::new();
    if every < DAY {
        // shortcut: steps from local midnight, so a DST day is an hour off until the next midnight.
        let Some(m) = midnight(first.date_naive()) else { return out };
        let mut t = m + ((t0 as i64 - m) / every) * every;
        while (t as f64) <= t1 {
            if t as f64 >= t0 {
                out.push(t);
            }
            t += every;
        }
        return out;
    }
    let mut d = first.date_naive();
    let end = local(t1).map_or(d, |t| t.date_naive());
    while d <= end {
        let keep = match every {
            DAY => true,
            e if e == 2 * DAY => (d.day() - 1) % 2 == 0,
            e if e == 7 * DAY => d.weekday().num_days_from_sunday() == 0,
            e if e == 30 * DAY => d.day() == 1,
            e if e == 91 * DAY => d.day() == 1 && d.month0() % 3 == 0,
            _ => d.day() == 1 && d.month0() == 0,
        };
        if keep && let Some(t) = midnight(d).filter(|&t| t as f64 >= t0 && t as f64 <= t1) {
            out.push(t);
        }
        d += Days::days(1);
    }
    out
}

/// "18:00", "Sat 7", or "Sat 1 Nov" / "Nov" at the start of a month.
fn tick_label(t: i64, span_ms: u64) -> String {
    let Some(d) = Local.timestamp_millis_opt(t).single() else { return String::new() };
    if d.second() != 0 {
        d.format("%H:%M:%S").to_string()
    } else if d.hour() != 0 || d.minute() != 0 {
        d.format("%H:%M").to_string()
    } else if d.day() != 1 {
        d.format("%a %-d").to_string()
    } else if span_ms > 60 * DAY as u64 {
        d.format("%b").to_string()
    } else {
        d.format("%a %-d %b").to_string()
    }
}

fn spawn_color() -> Rgba {
    violet()
}
fn fork_color() -> Rgba {
    rgb(0xf472b6)
}

/// A filled triangle pointing from `from` to `tip`.
fn arrow(window: &mut Window, tip: Point<Pixels>, from: Point<Pixels>, color: Rgba) {
    let d = tip - from;
    let len = (d.magnitude() as f32).max(1e-3);
    let (ux, uy) = (f32::from(d.x) / len, f32::from(d.y) / len);
    let base = point(tip.x - px(ux * 6.0), tip.y - px(uy * 6.0));
    let mut path = PathBuilder::fill();
    path.move_to(tip);
    path.line_to(point(base.x + px(-uy * 3.0), base.y + px(ux * 3.0)));
    path.line_to(point(base.x - px(-uy * 3.0), base.y - px(ux * 3.0)));
    path.close();
    if let Ok(p) = path.build() {
        window.paint_path(p, color);
    }
}

fn rect(window: &mut Window, x: f32, y: f32, w: f32, h: f32, o: Point<Pixels>, color: Rgba) {
    window.paint_quad(fill(Bounds::new(o + point(px(x), px(y)), size(px(w), px(h))), color));
}

impl Render for TimelineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.last_paint = Instant::now();
        self.fresh.retain(|_, at| at.elapsed() < BLINK);
        if let Some((to, at, last)) = self.zoom {
            let now = Instant::now();
            let k = glide::ease(self.k.ln(), to.ln(), (now - last).as_secs_f32()).exp();
            if (k / to - 1.0).abs() < 0.002 {
                self.zoom_at(to, at);
                self.zoom = None;
            } else {
                self.zoom_at(k, at);
                self.zoom = Some((to, at, now));
                window.request_animation_frame();
            }
        }
        glide::trace("tlzoom", self.k);
        if !self.fresh.is_empty() {
            window.request_animation_frame();
        }
        let (Some(g), Some(v)) = (self.graph.clone(), self.v.as_ref()) else {
            return div()
                .id("timeline")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted())
                .child("Reading the sessions…");
        };
        let now = now_ms();
        let height = self.layout.rows.len() as f32 * ROW + 24.0;
        let drawn = f32::from(self.chart.get().size.width);
        let width = if drawn > 0.0 { drawn } else { f32::from(window.viewport_size().width) };
        let s = self.scale(width, now);
        let span = now.saturating_sub(v.start);
        let count = self.layout.rows.len();

        // The axis.
        let axis = {
            let marks = ticks(s.time(s.r0), s.time(s.r1), ((width - LEFT) / 110.0).max(3.0));
            canvas(
                |_, _, _| {},
                move |b, _, window, cx| {
                    let o = b.origin;
                    window.paint_quad(fill(b, rgba(0x07080dd9)));
                    rect(window, 0.0, 0.0, LEFT, AXIS, o, rgb(0x0b0c12));
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: Bounds::new(o + point(px(LEFT), px(0.0)), size(b.size.width - px(LEFT), b.size.height)),
                        }),
                        |window| {
                            rect(window, s.r0.max(LEFT), 28.0, s.r1 - s.r0.max(LEFT), 1.0, o, rgba(0xffffff1f));
                            for &t in &marks {
                                let x = s.at(t as f64);
                                rect(window, x, 22.0, 1.0, 6.0, o, rgba(0xffffff1f));
                                paint_text(
                                    window,
                                    cx,
                                    tick_label(t, span).into(),
                                    (o.x + px(x), o.y + px(15.0)),
                                    0.5,
                                    10.5,
                                    muted(),
                                    FontWeight::NORMAL,
                                    true,
                                );
                            }
                        },
                    );
                    paint_text(
                        window,
                        cx,
                        format!("{count} sessions · newest at top").into(),
                        (o.x + px(14.0), o.y + px(17.0)),
                        0.0,
                        10.5,
                        muted(),
                        FontWeight::NORMAL,
                        true,
                    );
                    rect(window, 0.0, AXIS - 1.0, f32::from(b.size.width), 1.0, o, line());
                },
            )
            .w_full()
            .h(px(AXIS))
            .flex_none()
        };

        // The rows.
        let l = self.layout.clone();
        let colors = self.colors.clone();
        let (selected, hover_row, hover) = (self.selected.clone(), self.hover_row, self.hover);
        let fresh: HashMap<String, f32> = self.fresh.iter().map(|(id, at)| (id.clone(), at.elapsed().as_secs_f32())).collect();
        let stripes = self.started.elapsed().as_secs_f32();
        let chart = self.chart.clone();
        let visible_area = self.scroll.handle.clone();
        let g2 = g.clone();
        let body = canvas(
            move |b, _, _| chart.set(b),
            move |b, _, window, cx| {
                let g = &g2;
                let o = b.origin;
                let w = f32::from(b.size.width);
                let s = Scale { r1: (w - 16.0).max(LEFT + 9.0), ..s };
                let shown = visible_area.bounds();
                let (top, bottom) = (f32::from(shown.top() - o.y) - ROW, f32::from(shown.bottom() - o.y) + ROW);
                let cy = |r: usize| r as f32 * ROW + ROW / 2.0;
                let on_screen = |r: usize| cy(r) >= top && cy(r) <= bottom;
                let color = |i: usize| {
                    let n = &g.nodes[i];
                    if matches!(n.kind, NodeKind::Remote | NodeKind::Ghost) {
                        rgb(0xb8b9c6)
                    } else {
                        colors.get(&n.project).copied().unwrap_or(rgb(0x93c5fd))
                    }
                };
                let at = |x: f32, y: f32| o + point(px(x), px(y));
                let chart_mask =
                    ContentMask { bounds: Bounds::new(o + point(px(LEFT), px(0.0)), size(px((w - LEFT).max(0.0)), b.size.height)) };
                window.with_content_mask(Some(chart_mask), |window| {
                    // Bars, and the stripes of the sessions working now.
                    for (r, &(i, _)) in l.rows.iter().enumerate().filter(|(r, _)| on_screen(*r)) {
                        let n = &g.nodes[i];
                        let x0 = s.at(if n.created_at > 0 { n.created_at as f64 } else { s.start });
                        let x1 = s.at(if n.live { now } else { n.last_at.max(n.created_at) } as f64);
                        let a = if n.live {
                            0.95
                        } else if n.archived {
                            0.3
                        } else {
                            0.55
                        };
                        rect(window, x0, cy(r) - 5.0, (x1 - x0).max(3.0), 10.0, o, alpha(color(i), a));
                        if working(n) {
                            let xs = s.at(if n.status_at > 0 { n.status_at } else { now } as f64) - 2.0;
                            let xe = s.at(now as f64);
                            let (x, sw) = (xs.min(xe - 10.0), (xe - xs + 2.0).max(10.0));
                            let area = Bounds::new(at(x, cy(r) - 5.0), size(px(sw), px(10.0)));
                            window.with_content_mask(Some(ContentMask { bounds: area }), |window| {
                                // 45° stripes, 4 px of every 8, sliding right.
                                let period = 8.0 * std::f32::consts::SQRT_2;
                                let shift = (stripes / 0.8).fract() * period;
                                let mut sx = x - 10.0 - period + shift;
                                while sx < x + sw + 10.0 {
                                    let mut p = PathBuilder::fill();
                                    let (yb, yt) = (cy(r) + 5.0, cy(r) - 5.0);
                                    p.move_to(at(sx, yb));
                                    p.line_to(at(sx + period / 2.0, yb));
                                    p.line_to(at(sx + period / 2.0 + 10.0, yt));
                                    p.line_to(at(sx + 10.0, yt));
                                    p.close();
                                    if let Ok(p) = p.build() {
                                        window.paint_path(p, rgba(0xfbbf248c));
                                    }
                                    sx += period;
                                }
                            });
                        }
                    }
                    // Subagents: a thin line under their session's bar.
                    for &i in &l.agents {
                        let (n, r) = (&g.nodes[i], l.row_of[&i]);
                        if !on_screen(r) {
                            continue;
                        }
                        let x0 = s.at(n.created_at as f64);
                        let x1 = s.at(if n.live { now } else { n.last_at } as f64).max(x0 + 2.0);
                        rect(window, x0, cy(r) + 7.0, x1 - x0, 2.0, o, alpha(if n.live { amber() } else { color(i) }, 0.8));
                    }
                    // Started and forked sessions: a line down to the new session's row.
                    for &(r1, r2, to, kind) in &l.links {
                        let (y1, y2) = (cy(r1), cy(r2) - 6.0);
                        if y1.max(y2) < top || y1.min(y2) > bottom {
                            continue;
                        }
                        let x = s.at(g.nodes[to].created_at as f64);
                        let c = if kind == EdgeKind::Fork { fork_color() } else { spawn_color() };
                        let mut p = PathBuilder::stroke(px(1.4));
                        if kind == EdgeKind::Fork {
                            p = p.dash_array(&[px(4.0), px(3.0)]);
                        }
                        p.move_to(at(x, y1));
                        p.line_to(at(x, y2));
                        if let Ok(p) = p.build() {
                            window.paint_path(p, c);
                        }
                        arrow(window, at(x, y2), at(x, y1), c);
                    }
                    // Messages: an arc from the sender's row to the receiver's, bulging right.
                    for &(m, r1, r2) in &l.msgs {
                        let msg = &g.messages[m];
                        let (y1, y2) = (cy(r1), cy(r2));
                        if y1.max(y2) < top || y1.min(y2) > bottom {
                            continue;
                        }
                        let x = s.at(msg.ts as f64);
                        let bulge = bulge(y1, y2);
                        let failed = msg.ok == Some(false);
                        let c = if failed {
                            red()
                        } else if msg.kind == MsgKind::Report {
                            green()
                        } else {
                            blue()
                        };
                        let hovered = hover == Some(Hover::Msg(m));
                        let mut a = if hovered { 1.0 } else { 0.75 };
                        if let Some(t) = fresh.get(&msg.id) {
                            a *= 0.15 + 0.85 * (0.5 + 0.5 * (t * std::f32::consts::TAU).cos()); // 1 s blinks
                        }
                        let mut p = PathBuilder::stroke(px(if hovered { 2.4 } else { 1.4 }));
                        if failed {
                            p = p.dash_array(&[px(3.0), px(3.0)]);
                        }
                        p.move_to(at(x, y1));
                        p.cubic_bezier_to(at(x, y2), at(x + bulge, y1), at(x + bulge, y2));
                        if let Ok(p) = p.build() {
                            window.paint_path(p, alpha(c, a));
                        }
                        arrow(window, at(x, y2), at(x + bulge * 0.5, y2 + (y1 - y2).signum() * 2.0), alpha(c, a));
                    }
                    // Now.
                    let xn = s.at(now as f64);
                    let mut p = PathBuilder::stroke(px(1.0)).dash_array(&[px(2.0), px(3.0)]);
                    p.move_to(at(xn, top.max(0.0)));
                    p.line_to(at(xn, bottom.min(f32::from(b.size.height))));
                    if let Ok(p) = p.build() {
                        window.paint_path(p, amber());
                    }
                });
                // The labels' column, the row backgrounds, then dots and titles.
                rect(window, 0.0, top.max(0.0), LEFT, bottom - top.max(0.0), o, rgb(0x0b0c12));
                for (r, &(i, depth)) in l.rows.iter().enumerate().filter(|(r, _)| on_screen(*r)) {
                    let n = &g.nodes[i];
                    let y = r as f32 * ROW;
                    let tint = if selected.as_deref() == Some(n.id.as_str()) {
                        Some(rgba(0x38bdf814))
                    } else if hover_row == Some(r) {
                        Some(rgba(0xffffff0a))
                    } else {
                        (r % 2 == 0).then_some(rgba(0xffffff05))
                    };
                    if let Some(t) = tint {
                        rect(window, 0.0, y, w, ROW, o, t);
                    }
                    let dx = 14.0 + depth as f32 * 14.0;
                    let dot_color = if working(n) {
                        amber()
                    } else if n.live {
                        green()
                    } else if n.kind == NodeKind::Remote {
                        rgba(0x00000000)
                    } else {
                        color(i)
                    };
                    let ring = if matches!(n.kind, NodeKind::Remote | NodeKind::Ghost) { rgb(0xb8b9c6) } else { rgba(0x00000000) };
                    let a = if n.live { 1.0 } else { 0.6 };
                    let db = Bounds::new(at(dx - 3.5, cy(r) - 3.5), size(px(7.0), px(7.0)));
                    window.paint_quad(quad(db, px(3.5), alpha(dot_color, a), px(1.0), alpha(ring, a), BorderStyle::Solid));
                    let room = ((LEFT - 34.0 - depth as f32 * 14.0) / 6.6) as usize;
                    let mut tx = dx + 10.0;
                    if depth > 0 {
                        // "↳", drawn: fonts draw it at very different sizes.
                        let (ax, ay) = (tx + 1.0, cy(r));
                        let mut p = PathBuilder::stroke(px(1.2));
                        p.move_to(at(ax, ay - 5.0));
                        p.line_to(at(ax, ay + 0.5));
                        p.line_to(at(ax + 7.0, ay + 0.5));
                        if let Ok(p) = p.build() {
                            window.paint_path(p, muted());
                        }
                        arrow(window, at(ax + 9.0, ay + 0.5), at(ax, ay + 0.5), muted());
                        tx += 14.0;
                    }
                    let title = clip(&n.title, room);
                    let (c, weight) = if n.live {
                        (rgb(0xffffff), FontWeight::SEMIBOLD)
                    } else if n.archived {
                        (alpha(faint(), 0.6), FontWeight::NORMAL)
                    } else {
                        (rgb(0xcfd0dc), FontWeight::NORMAL)
                    };
                    paint_text(window, cx, title.into(), (o.x + px(tx), o.y + px(cy(r))), 0.0, 11.5, c, weight, false);
                }
            },
        )
        .w_full()
        .h(px(height));

        let tip = self.hover.map(|h| {
            // The card is placed in the tab, whose top left is the axis's.
            let top_left = point(self.chart.get().origin.x, self.scroll.handle.bounds().top() - px(AXIS));
            let pos = self.mouse - top_left;
            match h {
                Hover::Node(i) => node_tip(&g.nodes[i], pos, now),
                Hover::Msg(m) => {
                    let msg = &g.messages[m];
                    let title = |id: &str| g.nodes.iter().find(|n| n.id == id).map_or_else(|| id.to_string(), |n| clip(&n.title, 40));
                    let kind = if msg.kind == MsgKind::Report { "Report" } else { "Message" };
                    let failed = if msg.ok == Some(false) { " · failed" } else { "" };
                    let what = if msg.summary.is_empty() { &msg.text } else { &msg.summary };
                    tip_card(pos)
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} → {}", title(&msg.from), title(&msg.to))))
                        .child(div().text_size(px(12.0)).text_color(muted()).child(format!("{kind} · {}{failed}", when(msg.ts))))
                        .child(div().text_size(px(12.0)).child(clip(what, 200)))
                }
            }
        });

        div()
            .id("timeline")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg())
            .child(axis)
            .child(
                div()
                    .id("timeline-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.handle)
                    .child(self.scroll.driver())
                    .child(body),
            )
            .children(tip.map(|t| t.text_size(px(13.0)).text_color(text())))
            .child(zoom_driver(cx.entity()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|t, e: &MouseDownEvent, _, _| {
                    if f32::from(e.position.x - t.chart.get().origin.x) >= LEFT {
                        t.drag = Some((e.position.x, t.tx, false));
                    }
                }),
            )
            .on_mouse_move(cx.listener(|t, e: &MouseMoveEvent, _, cx| {
                t.mouse = e.position;
                if let Some((x0, tx0, moved)) = t.drag.as_mut()
                    && e.pressed_button == Some(MouseButton::Left)
                {
                    let dx = f32::from(e.position.x - *x0);
                    if *moved || dx.abs() > 3.0 {
                        *moved = true;
                        t.tx = *tx0 + dx;
                        t.zoom = None;
                    }
                }
                let (row, hover) = t.hit(e.position);
                if (row, hover) != (t.hover_row, t.hover) {
                    (t.hover_row, t.hover) = (row, hover);
                }
                cx.notify(); // the card follows the pointer
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|t, e: &MouseUpEvent, _, cx| {
                    let moved = t.drag.take().is_some_and(|d| d.2);
                    if moved {
                        return;
                    }
                    let g = t.graph.clone();
                    let id = match t.hit(e.position) {
                        (_, Some(Hover::Node(i))) => g.map(|g| g.nodes[i].id.clone()),
                        (_, Some(Hover::Msg(m))) => g.map(|g| g.messages[m].from.clone()),
                        (Some(r), None) => g.map(|g| g.nodes[t.layout.rows[r].0].id.clone()),
                        _ => return,
                    };
                    t.select(id, cx);
                }),
            )
    }
}

/// ⌘, ctrl or alt with the wheel zooms the time around the pointer; a sideways trackpad swipe moves it. Both are
/// caught before the rows' own scroll.
fn zoom_driver(view: Entity<TimelineView>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let view = view.clone();
            let line = window.line_height();
            window.on_mouse_event(move |e: &ScrollEvent, phase, _, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                view.update(cx, |t, cx| {
                    let b = t.chart.get();
                    let x = f32::from(e.position.x - b.origin.x);
                    if !t.scroll.handle.bounds().contains(&e.position) || x < LEFT {
                        return;
                    }
                    let d = e.delta.pixel_delta(line);
                    let m = e.modifiers;
                    if m.platform || m.control || m.alt {
                        let to = (t.zoom.map_or(t.k, |z| z.0) * (f32::from(d.y) * 0.003).exp()).clamp(1.0, 400.0);
                        if e.delta.precise() || glide::off() {
                            t.zoom = None;
                            t.zoom_at(to, x);
                        } else {
                            t.zoom = Some((to, x, Instant::now()));
                        }
                    } else if e.delta.precise() && d.x.abs() > d.y.abs() {
                        t.tx += f32::from(d.x);
                    } else {
                        return;
                    }
                    cx.stop_propagation();
                    cx.notify();
                });
            });
        },
    )
    .absolute()
    .size_0()
}
