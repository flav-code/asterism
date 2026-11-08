//! The Usage tab: what the tokens of every session would cost on the API, per day or hour, the last two hours,
//! when you work, by model, the top sessions and tools, and records. Read from `UsageView`; nothing is sent anywhere.
use crate::glide::Glide;
use crate::settings::{self, Settings};
use crate::ui::blue; // over gpui's color of the same name
use crate::ui::*;
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mdb_core::graph::UsageView;
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FAMS: [&str; 5] = ["Opus", "Fable", "Sonnet", "Haiku", "Other"];

/// One fixed color per model family (dark-surface categorical slots, validated together).
fn fam_color(f: &str) -> Rgba {
    rgb(match f {
        "Opus" => 0x3987e5,
        "Fable" => 0xd95926,
        "Sonnet" => 0x199e70,
        "Haiku" => 0xc98500,
        _ => 0x8b8c9c,
    })
}

/// Sequential blue for the heatmap: more replies, lighter.
const HEAT: [u32; 5] = [0x1b3358, 0x24508c, 0x3474c4, 0x6aa2e6, 0xb4d4fa];
const CHART_H: f32 = 230.0;
const CELL_H: f32 = 22.0;

#[derive(Clone, Copy, PartialEq)]
enum Range {
    Today,
    Days(i64),
    All,
}

#[derive(Clone, Copy, PartialEq)]
enum Metric {
    Usd,
    Out,
    All,
    Replies,
}

impl Metric {
    fn name(self) -> &'static str {
        match self {
            Metric::Usd => "API value",
            Metric::Out => "Output tokens",
            Metric::All => "All tokens",
            Metric::Replies => "Replies",
        }
    }
    fn of(self, t: &mdb_core::usage::Totals) -> f64 {
        match self {
            Metric::Usd => t.usd,
            Metric::Out => t.o as f64,
            Metric::All => (t.i + t.o + t.cr + t.cw) as f64,
            Metric::Replies => t.n as f64,
        }
    }
    fn fmt(self, x: f64) -> String {
        if self == Metric::Usd { usd(x) } else { short(x) }
    }
}

/// 1.2K, 34M, 5.6B.
pub fn short(x: f64) -> String {
    let a = x.abs();
    let (v, unit) = match a {
        a if a >= 1e9 => (x / 1e9, "B"),
        a if a >= 1e6 => (x / 1e6, "M"),
        a if a >= 1e3 => (x / 1e3, "K"),
        _ => return format!("{}", x.round()),
    };
    if v.abs() >= 10.0 { format!("{v:.0}{unit}") } else { format!("{v:.1}{unit}") }
}

/// $1,234 · $56 · $0.91
pub fn usd(x: f64) -> String {
    if x >= 1000.0 {
        let s = format!("{}", x.round() as u64);
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        format!("${out}")
    } else if x >= 10.0 {
        format!("${x:.0}")
    } else {
        format!("${x:.2}")
    }
}

/// "claude-opus-5-5" → "Opus 5.5".
fn pretty_model(m: &str) -> String {
    let m = m.strip_prefix("claude-").unwrap_or(m);
    let parts: Vec<&str> = m.split('-').collect();
    let digits = |s: &&str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let n = parts.iter().rev().take_while(|p| digits(p)).count().min(2);
    let name = parts[..parts.len() - n].join("-");
    let version = parts[parts.len() - n..].join(".");
    let mut name: Vec<char> = name.chars().collect();
    if let Some(c) = name.first_mut() {
        *c = c.to_ascii_uppercase();
    }
    let name: String = name.into_iter().collect();
    if version.is_empty() { name } else { format!("{name} {version}") }
}

fn day_key(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn parse_day(k: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(k.get(..10)?, "%Y-%m-%d").ok()
}

/// Ticks for a value axis from 0, about `count` of them, on round steps (d3's `nice`).
fn nice_ticks(max: f64, count: f64) -> Vec<f64> {
    let max = if max > 0.0 { max } else { 1.0 };
    let raw = max / count;
    let mut step = 10f64.powf(raw.log10().floor());
    let err = raw / step;
    step *= if err >= 50f64.sqrt() {
        10.0
    } else if err >= 10f64.sqrt() {
        5.0
    } else if err >= 2f64.sqrt() {
        2.0
    } else {
        1.0
    };
    let top = (max / step).ceil() * step;
    (0..=((top / step).round() as usize)).map(|i| i as f64 * step).collect()
}

/// One column of the bars chart.
struct Col {
    /// The day or hour it stands for: the same column keeps its bar across updates.
    key: String,
    label: String,
    long: String,
    fams: [f64; 5],
}

/// What the tab shows for one range and metric, worked out once per data update.
struct Dash {
    cols: Vec<Col>,
    legend: Vec<&'static str>,
    tot_usd: f64,
    all_tokens: f64,
    tot_o: f64,
    cache_read: f64,
    cache_rate: f64,
    saved: f64,
    replies: f64,
    /// Sessions with tokens in the range.
    session_count: usize,
    agent_share: f64,
    sessions: Vec<(String, String, String, [f64; 4])>,
    models: Vec<(String, &'static str, f64, f64)>,
    heat: [[(f64, f64); 24]; 7],
    /// The last 120 minutes: output tokens, replies, $.
    live: Vec<(u64, f64, f64, f64)>,
    facts: [(String, String, String); 6],
}

fn compute(u: &UsageView, range: Range, metric: Metric) -> Dash {
    let now = Local::now();
    let today = now.date_naive();
    let start = match range {
        Range::Today => today,
        Range::Days(n) => today - chrono::Duration::days(n - 1),
        Range::All => Local.timestamp_millis_opt(u.first as i64).single().map_or(today, |t| t.date_naive()),
    };
    let start_key = day_key(start);
    let days: Vec<_> = u.days.iter().filter(|d| d.d >= start_key).collect();
    let sum = |f: &dyn Fn(&&mdb_core::graph::DayRow) -> f64| days.iter().map(f).sum::<f64>();
    let tot_usd = sum(&|d| d.t.usd);
    let (i, o, cr, cw) = (sum(&|d| d.t.i as f64), sum(&|d| d.t.o as f64), sum(&|d| d.t.cr as f64), sum(&|d| d.t.cw as f64));
    let all = i + o + cr + cw;

    // Columns: hours of today, or days of the range.
    let fam_index = |f: &str| FAMS.iter().position(|x| *x == f).unwrap_or(4);
    let cols: Vec<Col> = if range == Range::Today {
        let mut cols: Vec<Col> = (0..=now.hour())
            .map(|h| Col { key: format!("h{h}"), label: format!("{h:02}h"), long: format!("{h:02}h"), fams: [0.0; 5] })
            .collect();
        let prefix = format!("{}T", day_key(today));
        for h in u.hours.iter().filter(|h| h.h.starts_with(&prefix)) {
            if let Some(c) = h.h[11..].parse::<usize>().ok().and_then(|hr| cols.get_mut(hr)) {
                c.fams[fam_index(h.fam)] += metric.of(&h.t);
            }
        }
        cols
    } else {
        let mut index = BTreeMap::new();
        let mut cols = Vec::new();
        let mut d = start;
        while d <= today {
            index.insert(day_key(d), cols.len());
            cols.push(Col {
                key: day_key(d),
                label: d.format("%-d %b").to_string(),
                long: d.format("%a %-d %b").to_string(),
                fams: [0.0; 5],
            });
            d += chrono::Duration::days(1);
        }
        for r in &days {
            if let Some(&c) = index.get(&r.d) {
                cols[c].fams[fam_index(r.fam)] += metric.of(&r.t);
            }
        }
        cols
    };
    let legend = FAMS.iter().copied().filter(|f| days.iter().any(|d| d.fam == *f)).collect();

    let mut sessions: Vec<(String, String, String, [f64; 4])> = u
        .sessions
        .iter()
        .map(|s| {
            let mut v = [0.0; 4];
            for (_, x) in s.days.range(start_key.clone()..) {
                for k in 0..4 {
                    v[k] += x[k];
                }
            }
            (s.id.clone(), s.title.clone(), s.project.clone(), v)
        })
        .filter(|s| s.3[2] > 0.0)
        .collect();
    sessions.sort_by(|a, b| b.3[1].total_cmp(&a.3[1]));
    let session_count = sessions.len();
    sessions.truncate(12);

    let mut by_model: HashMap<&str, (&'static str, f64, f64)> = HashMap::new();
    for d in &days {
        let e = by_model.entry(d.model.as_str()).or_insert((d.fam, 0.0, 0.0));
        (e.1, e.2) = (e.1 + d.t.usd, e.2 + d.t.o as f64);
    }
    let mut models: Vec<(String, &'static str, f64, f64)> =
        by_model.into_iter().map(|(m, (f, usd, o))| (pretty_model(m), f, usd, o)).collect();
    models.sort_by(|a, b| b.2.total_cmp(&a.2));

    let mut heat = [[(0.0, 0.0); 24]; 7];
    for h in u.hours.iter().filter(|h| h.h.as_str() >= start_key.as_str()) {
        if let (Some(d), Ok(hr)) = (parse_day(&h.h), h.h[11..].parse::<usize>()) {
            let cell = &mut heat[d.weekday().num_days_from_monday() as usize][hr.min(23)];
            (cell.0, cell.1) = (cell.0 + h.t.n as f64, cell.1 + h.t.usd);
        }
    }

    let minute = now.timestamp_millis() as u64 / 60_000 * 60_000;
    let by_minute: HashMap<u64, &mdb_core::usage::Totals> = u.minutes.iter().map(|(m, t)| (*m, t)).collect();
    let live = (0..120)
        .map(|i| {
            let t = minute - (119 - i) * 60_000;
            by_minute.get(&t).map_or((t, 0.0, 0.0, 0.0), |x| (t, x.o as f64, x.n as f64, x.usd))
        })
        .collect();

    // Records, over all time.
    let mut per_day: BTreeMap<&str, (f64, f64, f64)> = BTreeMap::new();
    for d in &u.days {
        let e = per_day.entry(d.d.as_str()).or_default();
        (e.0, e.1, e.2) = (e.0 + d.t.usd, e.1 + d.t.n as f64, e.2 + d.t.o as f64);
    }
    let best = per_day.iter().max_by(|a, b| a.1.0.total_cmp(&b.1.0));
    let mut streak = 0;
    let mut d = today;
    loop {
        let n = per_day.get(day_key(d).as_str()).map_or(0.0, |v| v.1);
        if n > 0.0 {
            streak += 1;
        } else if !(streak == 0 && d == today) {
            break;
        }
        d -= chrono::Duration::days(1);
    }
    let (all_usd, all_o) = per_day.values().fold((0.0, 0.0), |a, v| (a.0 + v.0, a.1 + v.2));
    let since = ((now.timestamp_millis() as f64 - u.first as f64) / 86_400_000.0).round().max(1.0);
    let first = Local.timestamp_millis_opt(u.first as i64).single().map(|t| t.format("%-d %b %Y").to_string()).unwrap_or_default();
    let long_day = |k: &str| parse_day(k).map(|d| d.format("%-d %b").to_string()).unwrap_or_default();
    let facts = [
        (
            "Busiest day".into(),
            best.map_or("—".into(), |b| usd(b.1.0)),
            best.map_or(String::new(), |b| format!("{} · {} replies", long_day(b.0), short(b.1.1))),
        ),
        ("Streak".into(), format!("{streak} day{}", if streak == 1 { "" } else { "s" }), "in a row with Claude at work".into()),
        ("Since".into(), format!("{since} days"), format!("first reply {first}")),
        ("Daily average".into(), usd(all_usd / since), format!("{} output tokens a day", short(all_o / since))),
        (
            "Longest reply".into(),
            u.biggest.as_ref().map_or("—".into(), |b| format!("{} tokens", short(b.o as f64))),
            u.biggest.as_ref().map_or(String::new(), |b| {
                let day = Local.timestamp_millis_opt(b.ts as i64).single().map(|t| t.format("%-d %b").to_string()).unwrap_or_default();
                format!("{} · {day}", pretty_model(&b.model))
            }),
        ),
        ("All time".into(), usd(all_usd), format!("{} output tokens", short(all_o))),
    ];

    Dash {
        cols,
        legend,
        tot_usd,
        all_tokens: all,
        tot_o: o,
        cache_read: cr,
        cache_rate: cr / (i + cr + cw).max(1.0),
        saved: sum(&|d| d.saved),
        replies: sum(&|d| d.t.n as f64),
        agent_share: sum(&|d| if d.agent { d.t.usd } else { 0.0 }) / tot_usd.max(1e-9),
        session_count,
        sessions,
        models,
        heat,
        live,
        facts,
    }
}

/// How fast shown values slide to new data: about 95% of the way in 3 × DATA_TAU.
const DATA_TAU: f32 = 0.12;

/// Numbers on screen that slide to their new value when the data, the range or the metric change, instead of
/// jumping. Each frame asks for every value by key; values nobody asked for are dropped.
#[derive(Default)]
struct Anim {
    shown: HashMap<String, (f64, u64)>,
    frame: u64,
    dt: f32,
    last: Option<Instant>,
    moving: bool,
}

impl Anim {
    fn begin(&mut self) {
        let now = Instant::now();
        self.dt = self.last.map_or(0.0, |l| (now - l).as_secs_f32().min(0.1));
        (self.last, self.moving) = (Some(now), false);
        self.frame += 1;
    }

    /// The value of `key` this frame, on its way to `to`; a new key starts from `from`.
    fn val(&mut self, key: String, to: f64, from: f64) -> f64 {
        let (frame, k) = (self.frame, (-self.dt / DATA_TAU).exp() as f64);
        let e = self.shown.entry(key).or_insert((from, frame));
        let mut v = to + (e.0 - to) * k;
        if crate::glide::off() || (v - to).abs() <= 1e-3 * to.abs().max(e.0.abs()) + 1e-9 {
            v = to;
        } else {
            self.moving = true;
        }
        *e = (v, frame);
        v
    }

    fn end(&mut self) {
        let frame = self.frame;
        self.shown.retain(|_, v| v.1 == frame);
    }
}

pub enum DashEvent {
    /// Open this session on the map.
    Open(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Hover {
    Col(usize),
    Minute(usize),
    Cell(usize, usize),
}

/// The data, range, metric and minute a `Dash` was worked out for.
type DashKey = (usize, Range, Metric, u64);

pub struct DashView {
    usage: Option<Arc<UsageView>>,
    range: Range,
    metric: Metric,
    cache: Option<(DashKey, Rc<Dash>)>,
    scroll: Glide,
    root: Rc<Cell<Bounds<Pixels>>>,
    bars: Rc<Cell<Bounds<Pixels>>>,
    live: Rc<Cell<Bounds<Pixels>>>,
    heat: Rc<Cell<Bounds<Pixels>>>,
    hover: Option<Hover>,
    mouse: Point<Pixels>,
    pub active: bool,
    anim: Anim,
    range_seg: crate::seg::Seg,
    metric_seg: crate::seg::Seg,
    started: Instant,
    _tick: Task<()>,
}

impl EventEmitter<DashEvent> for DashView {}

impl DashView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // The live chart's dot breathes; the minute rolls over.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                if this
                    .update(cx, |d, cx| {
                        if d.active {
                            cx.notify()
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        DashView {
            usage: None,
            range: match cx.global::<Settings>().usage_range.as_str() {
                "today" => Range::Today,
                "7d" => Range::Days(7),
                "90d" => Range::Days(90),
                "all" => Range::All,
                _ => Range::Days(30),
            },
            metric: match cx.global::<Settings>().usage_metric.as_str() {
                "out" => Metric::Out,
                "all" => Metric::All,
                "replies" => Metric::Replies,
                _ => Metric::Usd,
            },
            cache: None,
            scroll: Glide::default(),
            root: Rc::default(),
            bars: Rc::default(),
            live: Rc::default(),
            heat: Rc::default(),
            hover: None,
            mouse: Point::default(),
            active: false,
            anim: Anim::default(),
            range_seg: crate::seg::Seg::default(),
            metric_seg: crate::seg::Seg::default(),
            started: Instant::now(),
            _tick: tick,
        }
    }

    pub fn set_usage(&mut self, u: Arc<UsageView>, cx: &mut Context<Self>) {
        self.usage = Some(u);
        cx.notify();
    }

    fn dash(&mut self) -> Option<Rc<Dash>> {
        let u = self.usage.clone()?;
        let key = (Arc::as_ptr(&u) as usize, self.range, self.metric, now_ms() / 60_000);
        if let Some((k, d)) = &self.cache
            && *k == key
        {
            return Some(d.clone());
        }
        let d = Rc::new(compute(&u, self.range, self.metric));
        self.cache = Some((key, d.clone()));
        Some(d)
    }

    fn hit(&self, p: Point<Pixels>, d: &Dash) -> Option<Hover> {
        let (b, l, h) = (self.bars.get(), self.live.get(), self.heat.get());
        if b.contains(&p) && !d.cols.is_empty() {
            let (x0, step, _) = band(f32::from(b.size.width), d.cols.len());
            let i = ((f32::from(p.x - b.origin.x) - x0) / step).floor();
            return (i >= 0.0 && (i as usize) < d.cols.len()).then_some(Hover::Col(i as usize));
        }
        if l.contains(&p) && f32::from(p.y - l.origin.y) >= 50.0 {
            let w = f32::from(l.size.width) - 14.0;
            let i = ((f32::from(p.x - l.origin.x) - 4.0) / w * 119.0).round().clamp(0.0, 119.0);
            return Some(Hover::Minute(i as usize));
        }
        if h.contains(&p) {
            let cw = (f32::from(h.size.width) - 34.0) / 24.0;
            let (x, y) = (f32::from(p.x - h.origin.x) - 34.0, f32::from(p.y - h.origin.y) - 16.0);
            let (hr, wd) = ((x / cw).floor(), (y / CELL_H).floor());
            if (0.0..24.0).contains(&hr) && (0.0..7.0).contains(&wd) {
                return Some(Hover::Cell(wd as usize, hr as usize));
            }
        }
        None
    }
}

/// The bars chart's band scale over `n` columns: first x, step, bar width (d3 `scaleBand`, no outer padding).
fn band(w: f32, n: usize) -> (f32, f32, f32) {
    let (l, r) = (48.0, 6.0);
    let inner = if n > 60 { 0.12 } else { 0.22 };
    let step = (w - l - r) / (n as f32 - inner).max(1.0);
    (l, step, step * (1.0 - inner))
}

/// A monotone curve through `pts` (d3 `curveMonotoneX`), added to `path` after its first point.
fn monotone(path: &mut PathBuilder, pts: &[Point<Pixels>]) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let (x, y): (Vec<f32>, Vec<f32>) = pts.iter().map(|p| (f32::from(p.x), f32::from(p.y))).unzip();
    let mut t = vec![0.0f32; n];
    for i in 1..n - 1 {
        let (h0, h1) = (x[i] - x[i - 1], x[i + 1] - x[i]);
        let (s0, s1) = ((y[i] - y[i - 1]) / h0, (y[i + 1] - y[i]) / h1);
        let p = (s0 * h1 + s1 * h0) / (h0 + h1);
        t[i] = (s0.signum() + s1.signum()) * s0.abs().min(s1.abs()).min(0.5 * p.abs());
        if s0 == 0.0 || s1 == 0.0 {
            t[i] = 0.0;
        }
    }
    let end = |i: usize, j: usize, tj: f32| {
        let h = x[j] - x[i];
        (3.0 * (y[j] - y[i]) / h - tj) / 2.0
    };
    t[0] = if n > 2 { end(0, 1, t[1]) } else { (y[1] - y[0]) / (x[1] - x[0]) };
    t[n - 1] = if n > 2 { end(n - 2, n - 1, t[n - 2]) } else { t[0] };
    for i in 0..n - 1 {
        let dx = (x[i + 1] - x[i]) / 3.0;
        path.cubic_bezier_to(
            pts[i + 1],
            point(px(x[i] + dx), px(y[i] + dx * t[i])),
            point(px(x[i + 1] - dx), px(y[i + 1] - dx * t[i + 1])),
        );
    }
}

fn card(title: impl Into<SharedString>, side: Option<AnyElement>) -> Div {
    div().flex().flex_col().min_w_0().px(px(16.0)).py(px(14.0)).rounded(px(12.0)).bg(panel_hi()).border_1().border_color(line()).child(
        div()
            .flex()
            .justify_between()
            .items_center()
            .gap(px(10.0))
            .mb(px(12.0))
            .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child(title.into()))
            .children(side),
    )
}

fn small(s: impl Into<SharedString>) -> AnyElement {
    div().text_size(px(11.0)).text_color(faint()).child(s.into()).into_any_element()
}

/// A share of the card's width: `g` of 12 columns.
fn span(d: Div, g: f32) -> Div {
    let mut d = d.flex_basis(relative(0.0));
    d.style().flex_grow = Some(g);
    d
}

/// A row of a list: label, value, and a thin bar with its share.
fn list_row(label: AnyElement, value: AnyElement, share: f64, color: Rgba, first: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .py(px(7.0))
        .when(!first, |d| d.border_t_1().border_color(line()))
        .text_size(px(12.0))
        .child(div().flex().items_center().gap(px(12.0)).child(div().flex_1().min_w_0().overflow_hidden().child(label)).child(value))
        .child(
            div()
                .h(px(4.0))
                .w_full()
                .rounded(px(2.0))
                .bg(bg())
                .child(div().h_full().rounded(px(2.0)).w(relative((share.max(0.005)) as f32)).bg(color)),
        )
}

fn value(main: String, sub: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .flex_none()
        .child(div().font_weight(FontWeight::MEDIUM).child(main))
        .when(!sub.is_empty(), |d| d.child(div().text_size(px(11.0)).text_color(faint()).child(sub)))
        .into_any_element()
}

/// A heatmap cell's color at `level`: 0 is empty, 1 to 5 the steps; in between while it changes.
fn heat_color(level: f32) -> Rgba {
    let steps = [0x161722, HEAT[0], HEAT[1], HEAT[2], HEAT[3], HEAT[4]];
    let l = level.clamp(0.0, 5.0);
    let (i, t) = (l.floor() as usize, l.fract());
    let (a, b) = (rgb(steps[i]), rgb(steps[(i + 1).min(5)]));
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1.0 }
}

/// Captures where an element was laid out, for hit tests.
fn probe(cell: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().size_full()
}

impl Render for DashView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(d) = self.dash() else {
            return div().id("usage").size_full().p(px(20.0)).text_color(muted()).child("Reading your transcripts…");
        };
        let u = self.usage.clone().expect("dash() needs it");
        // What is on screen this frame: every number slides to the data.
        let a = &mut self.anim;
        a.begin();
        let tiles_now: Vec<f64> = [d.tot_usd, d.all_tokens, d.tot_o, d.cache_rate, d.replies, d.agent_share, d.saved, d.cache_read]
            .iter()
            .enumerate()
            .map(|(i, &v)| a.val(format!("t{i}"), v, 0.0))
            .collect();
        let target_max = d.cols.iter().map(|c| c.fams.iter().sum::<f64>()).fold(0.0, f64::max);
        let bars_top =
            a.val("btop".into(), *nice_ticks(target_max, 4.0).last().unwrap_or(&1.0), *nice_ticks(target_max, 4.0).last().unwrap_or(&1.0));
        // Where each column sits, as a share of the plot's width: a column that stays when the range changes
        // slides and widens to its new place.
        let (n, inner) = (d.cols.len() as f64, if d.cols.len() > 60 { 0.12 } else { 0.22 });
        let step_n = 1.0 / (n - inner).max(1.0);
        let bars_pos: Vec<(f64, f64)> = d
            .cols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (x, w) = (i as f64 * step_n, step_n * (1.0 - inner));
                (a.val(format!("bx{}", c.key), x, x), a.val(format!("bw{}", c.key), w, w))
            })
            .collect();
        let bars_now: Vec<[f64; 5]> =
            d.cols.iter().map(|c| std::array::from_fn(|f| a.val(format!("b{}{f}", c.key), c.fams[f], 0.0))).collect();
        let live_max_target = d.live.iter().map(|p| p.1).fold(0.0, f64::max).max(1.0);
        let live_max = a.val("lmax".into(), live_max_target, live_max_target);
        let live_now: Vec<f64> = d.live.iter().map(|p| a.val(format!("m{}", p.0), p.1, 0.0)).collect();
        let heat_max = d.heat.iter().flatten().map(|c| c.0).fold(0.0, f64::max).max(1.0);
        let heat_now: Vec<f64> = (0..7 * 24)
            .map(|i| {
                let n = d.heat[i / 24][i % 24].0;
                let level = if n > 0.0 { 1.0 + ((n / heat_max).sqrt() * 5.0).floor().min(4.0) } else { 0.0 };
                a.val(format!("h{i}"), level, 0.0)
            })
            .collect();
        let model_share: Vec<f64> = d.models.iter().map(|m| a.val(format!("lm{}", m.0), m.2 / d.tot_usd.max(1e-9), 0.0)).collect();
        let top_session = d.sessions.first().map_or(1.0, |s| s.3[1].max(1e-9));
        let session_share: Vec<f64> = d.sessions.iter().map(|s| a.val(format!("ls{}", s.0), s.3[1] / top_session, 0.0)).collect();
        let first_tool = u.tools.first().map_or(1.0, |t| t.1.max(1) as f64);
        let tool_share: Vec<f64> = u.tools.iter().map(|t| a.val(format!("lt{}", t.0), t.1 as f64 / first_tool, 0.0)).collect();
        a.end();
        crate::glide::trace("bars", bars_now.iter().flatten().sum::<f64>() as f32);
        crate::glide::trace("heat", heat_now.iter().sum::<f64>() as f32);
        if a.moving {
            crate::glide::again(window, cx);
        }
        let button = |id: String, label: &'static str, on: bool| {
            div()
                .id(SharedString::from(id))
                .h_full()
                .flex()
                .items_center()
                .px(px(10.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_size(px(12.0))
                .text_color(if on { text() } else { muted() })
                .child(label)
        };
        let ranges =
            [(Range::Today, "Today"), (Range::Days(7), "7d"), (Range::Days(30), "30d"), (Range::Days(90), "90d"), (Range::All, "All")]
                .into_iter()
                .map(|(r, label)| {
                    button(format!("u-range-{label}"), label, self.range == r)
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.range = r;
                            settings::change(cx, |s| s.usage_range = label.to_lowercase());
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();
        let metrics = [(Metric::Usd, "Value $"), (Metric::Out, "Output"), (Metric::All, "All tokens"), (Metric::Replies, "Replies")]
            .into_iter()
            .map(|(m, label)| {
                button(format!("u-metric-{label}"), label, self.metric == m)
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.metric = m;
                        let name = match m {
                            Metric::Usd => "usd",
                            Metric::Out => "out",
                            Metric::All => "all",
                            Metric::Replies => "replies",
                        };
                        settings::change(cx, |s| s.usage_metric = name.into());
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let range_index =
            [Range::Today, Range::Days(7), Range::Days(30), Range::Days(90), Range::All].iter().position(|r| *r == self.range).unwrap_or(2);
        let metric_index = [Metric::Usd, Metric::Out, Metric::All, Metric::Replies].iter().position(|m| *m == self.metric).unwrap_or(0);
        let head = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .mb(px(14.0))
            .child(div().mr_auto().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Token usage"))
            .child(self.range_seg.render(range_index, ranges, window, cx))
            .child(self.metric_seg.render(metric_index, metrics, window, cx));

        let pct = |x: f64| format!("{}%", if x.is_finite() { (x * 100.0).round() } else { 0.0 });
        let tile = |k: &str, v: String, s: String| {
            span(
                div()
                    .min_w_0()
                    .px(px(14.0))
                    .py(px(12.0))
                    .rounded(px(12.0))
                    .bg(panel_hi())
                    .border_1()
                    .border_color(line())
                    .child(div().text_size(px(11.5)).text_color(muted()).child(k.to_string()))
                    .child(div().mt(px(4.0)).text_size(px(26.0)).font_weight(FontWeight::SEMIBOLD).child(v))
                    .child(div().mt(px(2.0)).text_size(px(11.5)).text_color(faint()).child(s)),
                1.0,
            )
        };
        let t = &tiles_now;
        let tiles = div()
            .flex()
            .gap(px(10.0))
            .mb(px(12.0))
            .child(tile("API-equivalent value", usd(t[0]), "at Anthropic API prices, not what you pay".into()))
            .child(tile("Tokens processed", short(t[1]), format!("{} read from cache", pct(t[7] / t[1].max(1.0)))))
            .child(tile("Output tokens", short(t[2]), "written by Claude".into()))
            .child(tile("Cache hit rate", pct(t[3]), format!("saved ≈ {} vs no cache", usd(t[6]))))
            .child(tile("Replies", short(t[4]), format!("{} sessions", d.session_count)))
            .child(tile("Subagents", pct(t[5]), "of the value".into()));

        // The bars chart.
        let metric = self.metric;
        let hover = self.hover;
        let bars_d = d.clone();
        let today_word = if self.range == Range::Today { "now" } else { "today" };
        let bars = canvas(
            {
                let cell = self.bars.clone();
                move |b, _, _| cell.set(b)
            },
            move |b, _, window, cx| {
                let d = &bars_d;
                let o = b.origin;
                let (w, h) = (f32::from(b.size.width), CHART_H);
                let (top, bottom, right) = (10.0, h - 24.0, w - 6.0);
                // The scale slides too: grid lines move with it, and only the target's lines are drawn.
                let ticks = nice_ticks(target_max, 4.0);
                let y = |v: f64| bottom - (v / bars_top.max(1e-9)) as f32 * (bottom - top);
                for &t in &ticks {
                    window.paint_quad(fill(Bounds::new(o + point(px(48.0), px(y(t))), size(px(right - 48.0), px(1.0))), rgba(0xffffff0f)));
                    paint_text(
                        window,
                        cx,
                        metric.fmt(t).into(),
                        (o.x + px(40.0), o.y + px(y(t))),
                        1.0,
                        10.5,
                        faint(),
                        FontWeight::NORMAL,
                        true,
                    );
                }
                let (x0, step, _) = band(w, d.cols.len());
                let plot = w - 48.0 - 6.0;
                let every = d.cols.len().div_ceil(10).max(1);
                for (i, c) in d.cols.iter().enumerate() {
                    let (x, bw) = (x0 + bars_pos[i].0 as f32 * plot, bars_pos[i].1 as f32 * plot);
                    let mut acc = 0.0;
                    let mut first = true;
                    for (f, &v) in FAMS.iter().zip(&bars_now[i]) {
                        if v <= 0.0 {
                            continue;
                        }
                        let (y0, y1) = (y(acc), y(acc + v));
                        // A 2 px gap between stacked parts.
                        let (top_y, bot_y) = (y1 + 1.0, if first { y0 } else { y0 - 1.0 });
                        if bot_y > top_y {
                            window.paint_quad(fill(
                                Bounds::new(o + point(px(x + 0.5), px(top_y)), size(px(bw - 1.0), px(bot_y - top_y))),
                                fam_color(f),
                            ));
                        }
                        acc += v;
                        first = false;
                    }
                    let last = i + 1 == d.cols.len();
                    if i % every == 0 || last {
                        let (label, color) = if last { (today_word.to_string(), text()) } else { (c.label.clone(), faint()) };
                        paint_text(
                            window,
                            cx,
                            label.into(),
                            (o.x + px(x + bw / 2.0), o.y + px(h - 9.0)),
                            0.5,
                            10.5,
                            color,
                            FontWeight::NORMAL,
                            true,
                        );
                    }
                    if hover == Some(Hover::Col(i)) {
                        window.paint_quad(fill(
                            Bounds::new(o + point(px(x - (step - bw) / 2.0), px(top)), size(px(step), px(bottom - top))),
                            rgba(0xffffff0a),
                        ));
                    }
                }
            },
        )
        .w_full()
        .h(px(CHART_H));

        // The last two hours.
        let live_d = d.clone();
        let breath = self.started.elapsed().as_secs_f32();
        let live = canvas(
            {
                let cell = self.live.clone();
                move |b, _, _| cell.set(b)
            },
            move |b, _, window, cx| {
                let d = &live_d;
                let o = b.origin;
                let (w, h) = (f32::from(b.size.width), CHART_H);
                let (l, r, t, bm) = (4.0, 10.0, 50.0, 18.0);
                let last10 = d.live[110..].iter().map(|p| p.1).sum::<f64>() / 10.0;
                let last_hour = d.live[60..].iter().map(|p| p.3).sum::<f64>();
                paint_text(
                    window,
                    cx,
                    format!("{} tok/min", short(last10)).into(),
                    (o.x, o.y + px(15.0)),
                    0.0,
                    20.0,
                    text(),
                    FontWeight::SEMIBOLD,
                    false,
                );
                paint_text(
                    window,
                    cx,
                    format!("output, last 10 min · {} in the last hour", usd(last_hour)).into(),
                    (o.x, o.y + px(35.0)),
                    0.0,
                    10.5,
                    faint(),
                    FontWeight::NORMAL,
                    true,
                );
                let x = |i: usize| l + i as f32 / 119.0 * (w - l - r);
                let y = |v: f64| (h - bm) - (v / live_max) as f32 * (h - bm - t);
                let pts: Vec<Point<Pixels>> = live_now.iter().enumerate().map(|(i, v)| o + point(px(x(i)), px(y(*v)))).collect();
                let mut area = PathBuilder::fill();
                area.move_to(o + point(px(x(0)), px(h - bm)));
                area.line_to(pts[0]);
                monotone(&mut area, &pts);
                area.line_to(o + point(px(x(119)), px(h - bm)));
                area.close();
                if let Ok(p) = area.build() {
                    window.paint_path(p, rgba(0x3987e52e));
                }
                let mut line = PathBuilder::stroke(px(2.0));
                line.move_to(pts[0]);
                monotone(&mut line, &pts);
                if let Ok(p) = line.build() {
                    window.paint_path(p, rgb(0x3987e5));
                }
                // The last minute's dot breathes.
                let a = 0.35 + 0.65 * (0.5 + 0.5 * (breath / 1.6 * std::f32::consts::TAU).cos());
                let c = pts[119];
                window.paint_quad(quad(
                    Bounds::new(c - point(px(4.0), px(4.0)), size(px(8.0), px(8.0))),
                    px(4.0),
                    alpha(rgb(0x3987e5), a),
                    px(0.0),
                    rgba(0),
                    BorderStyle::Solid,
                ));
                paint_text(window, cx, "−2 h".into(), (o.x + px(l), o.y + px(h - 7.0)), 0.0, 10.5, faint(), FontWeight::NORMAL, true);
                paint_text(window, cx, "now".into(), (o.x + px(w - r), o.y + px(h - 7.0)), 1.0, 10.5, text(), FontWeight::NORMAL, true);
                if let Some(Hover::Minute(i)) = hover {
                    window.paint_quad(fill(Bounds::new(o + point(px(x(i)), px(t)), size(px(1.0), px(h - bm - t))), rgba(0xffffff40)));
                }
            },
        )
        .w_full()
        .h(px(CHART_H));

        // When you work.
        let heat = canvas(
            {
                let cell = self.heat.clone();
                move |b, _, _| cell.set(b)
            },
            move |b, _, window, cx| {
                let o = b.origin;
                let w = f32::from(b.size.width);
                let (l, t) = (34.0, 16.0);
                let cw = (w - l) / 24.0;
                for h in [0, 6, 12, 18, 23] {
                    paint_text(
                        window,
                        cx,
                        format!("{h}h").into(),
                        (o.x + px(l + h as f32 * cw + cw / 2.0), o.y + px(7.0)),
                        0.5,
                        10.5,
                        faint(),
                        FontWeight::NORMAL,
                        true,
                    );
                }
                for (wd, name) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].iter().enumerate() {
                    paint_text(
                        window,
                        cx,
                        (*name).into(),
                        (o.x, o.y + px(t + wd as f32 * CELL_H + CELL_H / 2.0)),
                        0.0,
                        10.5,
                        faint(),
                        FontWeight::NORMAL,
                        true,
                    );
                    for hr in 0..24 {
                        let color = heat_color(heat_now[wd * 24 + hr] as f32);
                        let cell = Bounds::new(
                            o + point(px(l + hr as f32 * cw + 1.0), px(t + wd as f32 * CELL_H + 1.0)),
                            size(px(cw - 2.0), px(CELL_H - 2.0)),
                        );
                        let ring = if hover == Some(Hover::Cell(wd, hr)) { rgba(0xffffffb0) } else { rgba(0) };
                        window.paint_quad(quad(cell, px(2.0), color, px(1.0), ring, BorderStyle::Solid));
                    }
                }
                let (ly, lx) = (t + 7.0 * CELL_H + 18.0, w - 5.0 * 16.0 - 40.0);
                paint_text(
                    window,
                    cx,
                    "fewer".into(),
                    (o.x + px(lx - 6.0), o.y + px(ly + 5.5)),
                    1.0,
                    10.5,
                    faint(),
                    FontWeight::NORMAL,
                    true,
                );
                for (i, c) in HEAT.iter().enumerate() {
                    window.paint_quad(quad(
                        Bounds::new(o + point(px(lx + i as f32 * 16.0), px(ly)), size(px(13.0), px(11.0))),
                        px(2.0),
                        rgb(*c),
                        px(0.0),
                        rgba(0),
                        BorderStyle::Solid,
                    ));
                }
                paint_text(
                    window,
                    cx,
                    "more".into(),
                    (o.x + px(lx + 5.0 * 16.0 + 4.0), o.y + px(ly + 5.5)),
                    0.0,
                    10.5,
                    faint(),
                    FontWeight::NORMAL,
                    true,
                );
            },
        )
        .w_full()
        .h(px(16.0 + 7.0 * CELL_H + 30.0));

        let legend = div().flex().gap(px(12.0)).children(d.legend.iter().map(|f| {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .text_color(muted())
                .child(div().size(px(10.0)).rounded(px(2.0)).bg(fam_color(f)))
                .child(f.to_string())
        }));
        let per = if self.range == Range::Today { "hour" } else { "day" };
        let models = d.models.iter().enumerate().map(|(i, (m, f, v, o))| {
            list_row(
                div().child(m.clone()).into_any_element(),
                value(usd(*v), format!("{} out", short(*o))),
                model_share[i],
                fam_color(f),
                i == 0,
            )
        });
        let sessions = d.sessions.iter().enumerate().map(|(i, (id, title, project, v))| {
            let id = id.clone();
            let label = div()
                .id(SharedString::from(format!("u-s-{i}")))
                .flex()
                .items_center()
                .gap(px(6.0))
                .min_w_0()
                .child(div().text_color(blue()).cursor_pointer().overflow_hidden().child(clip(title, 60)))
                .child(chip(project.clone(), muted()).bg(panel()).border_1().border_color(line()).font_family(MONO).text_size(px(10.0)))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(DashEvent::Open(id.clone()))))
                .into_any_element();
            let sub = if v[3] > 0.0 { format!("{} out · {} subagents", short(v[0]), usd(v[3])) } else { format!("{} out", short(v[0])) };
            list_row(label, value(usd(v[1]), sub), session_share[i], rgb(0x3987e5), i == 0)
        });
        let tools = u.tools.iter().enumerate().map(|(i, (t, n))| {
            list_row(
                div().child(t.clone()).into_any_element(),
                value(short(*n as f64), String::new()),
                tool_share[i],
                rgb(0x3987e5),
                i == 0,
            )
        });
        let empty = || div().text_size(px(12.0)).text_color(faint()).child("Nothing in this range.");
        let facts = div().flex().gap(px(14.0)).children(d.facts.iter().map(|(k, v, s)| {
            span(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().text_size(px(11.0)).text_color(muted()).child(k.clone()))
                    .child(div().text_size(px(18.0)).font_weight(FontWeight::SEMIBOLD).child(v.clone()))
                    .child(div().text_size(px(11.0)).text_color(faint()).child(s.clone())),
                1.0,
            )
        }));
        let unpriced = if u.unpriced.is_empty() {
            String::new()
        } else {
            format!(" Not priced: {}.", u.unpriced.iter().map(|(m, n)| format!("{m} ({n})")).collect::<Vec<_>>().join(", "))
        };
        let row = || div().flex().gap(px(12.0)).mb(px(12.0));
        let content = div()
            .px(px(20.0))
            .pt(px(16.0))
            .pb(px(32.0))
            .child(head)
            .child(tiles)
            .child(
                row()
                    .child(span(card(format!("{} per {per}", self.metric.name()), Some(legend.into_any_element())).child(bars), 8.0))
                    .child(span(card("Last 2 hours", None).child(live), 4.0)),
            )
            .child(row().child(span(card("When you work", Some(small("replies per weekday and hour"))).child(heat), 6.0)).child(span(
                card("By model", Some(small("share of the value"))).children(models).when(d.models.is_empty(), |c| c.child(empty())),
                6.0,
            )))
            .child(
                row()
                    .child(span(
                        card("Top sessions", Some(small("click to open on the map")))
                            .children(sessions)
                            .when(d.sessions.is_empty(), |c| c.child(empty())),
                        8.0,
                    ))
                    .child(span(card("Most used tools", Some(small("all time"))).children(tools), 4.0)),
            )
            .child(row().child(span(card("Records", Some(small("all time"))).child(facts), 12.0)))
            .child(div().text_size(px(11.5)).text_color(faint()).line_height(px(17.0)).child(format!(
                "Value = what these tokens would cost on the Anthropic API (public prices of 6 Oct 2026: input, output, cache writes \
                 at 1.25× or 2×, cache reads). A subscription isn't billed this way; it shows how much work went through.{unpriced}"
            )));

        let tip = self.hover.map(|h| {
            let pos = self.mouse - self.root.get().origin;
            match h {
                Hover::Col(i) => {
                    let c = &d.cols[i];
                    let total: f64 = c.fams.iter().sum();
                    tip_card(pos)
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} · {}", c.long, metric.fmt(total))))
                        .children(FAMS.iter().zip(&c.fams).filter(|(_, v)| **v > 0.0).map(|(f, v)| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(px(12.0))
                                .child(div().size(px(8.0)).rounded(px(2.0)).bg(fam_color(f)))
                                .child(f.to_string())
                                .child(div().text_color(muted()).child(metric.fmt(*v)))
                        }))
                }
                Hover::Minute(i) => {
                    let (t, o, n, v) = d.live[i];
                    tip_card(pos).child(div().font_weight(FontWeight::SEMIBOLD).child(when(t))).child(
                        div().text_size(px(12.0)).text_color(muted()).child(format!("{} output · {n} replies · {}", short(o), usd(v))),
                    )
                }
                Hover::Cell(wd, hr) => {
                    let (n, v) = d.heat[wd][hr];
                    let day = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][wd];
                    tip_card(pos)
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{day} {hr:02}:00")))
                        .child(div().text_size(px(12.0)).text_color(muted()).child(format!("{} replies · {}", short(n), usd(v))))
                }
            }
        });
        let _ = window;
        let dash = d.clone();
        div()
            .id("usage")
            .size_full()
            .relative()
            .bg(bg())
            .child(probe(self.root.clone()))
            .child(
                div()
                    .id("usage-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.handle)
                    .child(content)
                    .child(self.scroll.driver()),
            )
            .children(tip.map(|t| t.text_size(px(13.0)).text_color(text())))
            .on_mouse_move(cx.listener(move |v, e: &MouseMoveEvent, _, cx| {
                v.mouse = e.position;
                v.hover = v.hit(e.position, &dash);
                cx.notify();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{nice_ticks, pretty_model, short, usd};

    #[test]
    fn formats() {
        assert_eq!(usd(29555.4), "$29,555");
        assert_eq!(usd(1248.0), "$1,248");
        assert_eq!(usd(9.92), "$9.92");
        assert_eq!(short(86e9), "86B");
        assert_eq!(short(6.6e6), "6.6M");
        assert_eq!(short(950.0), "950");
        assert_eq!(pretty_model("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(pretty_model("claude-fable-5"), "Fable 5");
        assert_eq!(nice_ticks(2226.0, 4.0), vec![0.0, 500.0, 1000.0, 1500.0, 2000.0, 2500.0]);
    }
}
