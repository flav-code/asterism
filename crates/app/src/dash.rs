//! The Usage tab: what the tokens of every session would cost on the API, per day or hour, the last two hours,
//! when you work, by model, the top sessions and tools, and records. Read from `UsageView`; nothing is sent anywhere.
use crate::glide::Glide;
use crate::seg::Seg;
use crate::settings::{self, Settings};
use crate::ui::blue; // over gpui's color of the same name
use crate::ui::*;
use asterism_core::graph::{DayRow, UsageView};
use asterism_core::i18n;
use asterism_core::usage::Totals;
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use gpui_kit::prelude::*;
use gpui_kit::*;
use rust_i18n::t;
use std::borrow::Cow;
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum Range {
    Today,
    /// The current hour and the 23 before it.
    Last24h,
    Days(i64),
    All,
}

/// The ranges in the order of their buttons.
const RANGES: [Range; 6] = [Range::Today, Range::Last24h, Range::Days(7), Range::Days(30), Range::Days(90), Range::All];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Metric {
    Usd,
    Out,
    All,
    Replies,
}

impl Metric {
    fn name(self) -> String {
        match self {
            Metric::Usd => t!("dash.metric.usd"),
            Metric::Out => t!("dash.metric.out"),
            Metric::All => t!("dash.metric.all"),
            Metric::Replies => t!("dash.metric.replies"),
        }
        .into_owned()
    }
    fn of(self, t: &Totals) -> f64 {
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

/// 1.2K, 34M, 5.6B (1,2 k, 34 M, 5,6 Md in French).
pub fn short(x: f64) -> String {
    let a = x.abs();
    let (v, unit) = match a {
        a if a >= 1e9 => (x / 1e9, "dash.billions"),
        a if a >= 1e6 => (x / 1e6, "dash.millions"),
        a if a >= 1e3 => (x / 1e3, "dash.thousands"),
        _ => return format!("{}", x.round()),
    };
    let n = if v.abs() >= 10.0 { format!("{v:.0}") } else { i18n::decimal(v, 1) };
    t!(unit, n = n).into_owned()
}

/// $1,234 · $56 · $0.91, still in US dollars in every language (1 234 $ in French).
pub fn usd(x: f64) -> String {
    let n = if x >= 1000.0 {
        let s = format!("{}", x.round() as u64);
        let sep = t!("dash.thousands_sep");
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push_str(&sep);
            }
            out.push(c);
        }
        out
    } else if x >= 10.0 {
        format!("{x:.0}")
    } else {
        i18n::decimal(x, 2)
    };
    t!("dash.usd", n = n).into_owned()
}

/// A family's name on screen: "Other" is a word, the others are model names.
fn fam_label(f: &str) -> String {
    if f == "Other" { t!("dash.other_model").into_owned() } else { f.to_string() }
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
    /// Replies of models with no known price, and those models: worth nothing in $, so the column says so.
    no_price: f64,
    no_price_models: Vec<String>,
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
    sessions: Vec<SessionRow>,
    models: Vec<ModelRow>,
    /// "When you work": a row per day when the range is a week or less (`heat_by_day`), else per weekday.
    heat: Vec<HeatRow>,
    heat_by_day: bool,
    /// The last 120 minutes.
    live: Vec<Minute>,
    facts: [Fact; 6],
}

/// A session of the "top sessions" list, summed over the range.
struct SessionRow {
    id: String,
    title: String,
    project: String,
    /// Output tokens.
    output: f64,
    usd: f64,
    /// All tokens: a session with none is not listed.
    tokens: f64,
    /// What its subagents spent, in $.
    agents_usd: f64,
}

/// A model of the list, summed over the range.
struct ModelRow {
    name: String,
    /// Its family, for the color.
    fam: &'static str,
    usd: f64,
    /// Output tokens.
    output: f64,
    /// False for a model this version has no price for: its `usd` is 0 and says nothing.
    priced: bool,
}

/// The models of these rows that have no known price, by their shown names, each once.
fn no_price_models<'a>(rows: impl Iterator<Item = &'a DayRow>) -> Vec<String> {
    let mut names: Vec<String> = rows.filter(|r| r.t.np > 0).map(|r| pretty_model(&r.model)).collect();
    names.sort();
    names.dedup();
    names
}

/// One hour of a row of "When you work".
#[derive(Clone, Copy, Default)]
struct HeatCell {
    replies: f64,
    usd: f64,
}

/// A row of "When you work": one day, or one weekday over the range.
struct HeatRow {
    label: String,
    cells: [HeatCell; 24],
}

/// The grid's left edge, after its row labels: a day ("Thu 12") is wider than a weekday.
fn heat_left(by_day: bool) -> f32 {
    if by_day { 48.0 } else { 34.0 }
}

/// A row's height: a single row (today) is taller, so it does not look lost in its card.
fn heat_row_h(rows: usize) -> f32 {
    if rows == 1 { 2.0 * CELL_H } else { CELL_H }
}

/// One minute of the live chart.
#[derive(Clone, Copy)]
struct Minute {
    /// When it starts, in ms.
    at: u64,
    /// Output tokens.
    output: f64,
    replies: f64,
    usd: f64,
}

/// A record over all time: what it is, its number, a line under it.
struct Fact {
    label: String,
    value: String,
    sub: String,
}

/// One day over all time, for the records.
#[derive(Default)]
struct DayTotal {
    usd: f64,
    replies: f64,
    /// Output tokens.
    output: f64,
}

fn compute(u: &UsageView, range: Range, metric: Metric) -> Dash {
    let now = Local::now();
    let today = now.date_naive();
    // An hourly range has a column per hour, keyed "YYYY-MM-DDTHH", and counts the hours' rows instead of the days'.
    let hours: Vec<(String, String)> = match range {
        Range::Today => (0..=now.hour()).map(|h| (format!("{}T{h:02}", day_key(today)), format!("{h:02}h"))).collect(),
        Range::Last24h => (0..24)
            .rev()
            .map(|ago| now - chrono::Duration::hours(ago))
            .map(|t| (t.format("%Y-%m-%dT%H").to_string(), format!("{} {:02}h", t.format(&i18n::names(&t, "%a %-d")), t.hour())))
            .collect(),
        _ => Vec::new(),
    };
    let hourly = !hours.is_empty();
    let start = match range {
        Range::Today => today,
        Range::Last24h => parse_day(&hours[0].0).unwrap_or(today),
        Range::Days(n) => today - chrono::Duration::days(n - 1),
        Range::All => Local.timestamp_millis_opt(u.first as i64).single().map_or(today, |t| t.date_naive()),
    };
    // A day's key comes before the keys of its hours: "today" takes all of them.
    let start_key = if range == Range::Last24h { hours[0].0.clone() } else { day_key(start) };
    let days: Vec<_> = if hourly { &u.hours } else { &u.days }.iter().filter(|d| d.d >= start_key).collect();
    let sum = |f: &dyn Fn(&&DayRow) -> f64| days.iter().map(f).sum::<f64>();
    let tot_usd = sum(&|d| d.t.usd);
    let (i, o, cr, cw) = (sum(&|d| d.t.i as f64), sum(&|d| d.t.o as f64), sum(&|d| d.t.cr as f64), sum(&|d| d.t.cw as f64));
    let all = i + o + cr + cw;

    // Columns: hours of an hourly range, or days of the range.
    let fam_index = |f: &str| FAMS.iter().position(|x| *x == f).unwrap_or(4);
    let col =
        |key: String, label: String, long: String| Col { key, label, long, fams: [0.0; 5], no_price: 0.0, no_price_models: Vec::new() };
    let mut cols: Vec<Col> = if hourly {
        hours.into_iter().map(|(key, long)| col(key.clone(), format!("{}h", &key[11..]), long)).collect()
    } else {
        let mut cols = Vec::new();
        let mut d = start;
        while d <= today {
            cols.push(col(
                day_key(d),
                d.format(&i18n::names(&d, "%-d %b")).to_string(),
                d.format(&i18n::names(&d, "%a %-d %b")).to_string(),
            ));
            d += chrono::Duration::days(1);
        }
        cols
    };
    // The hour that comes twice when clocks go back is one column.
    let mut index = BTreeMap::new();
    for (c, col) in cols.iter().enumerate() {
        index.entry(col.key.clone()).or_insert(c);
    }
    for r in &days {
        if let Some(&c) = index.get(&r.d) {
            cols[c].fams[fam_index(r.fam)] += metric.of(&r.t);
            cols[c].no_price += r.t.np as f64;
        }
    }
    for (key, &c) in &index {
        cols[c].no_price_models = no_price_models(days.iter().copied().filter(|r| r.d == *key));
    }
    let legend = FAMS.iter().copied().filter(|f| days.iter().any(|d| d.fam == *f)).collect();

    let mut sessions: Vec<SessionRow> = u
        .sessions
        .iter()
        .map(|s| {
            let mut v = [0.0; 4];
            for (_, x) in if hourly { &s.hours } else { &s.days }.range(start_key.clone()..) {
                for k in 0..4 {
                    v[k] += x[k];
                }
            }
            // In the order of `SessionUsage::days`.
            let [output, usd, tokens, agents_usd] = v;
            SessionRow { id: s.id.clone(), title: s.title.clone(), project: s.project.clone(), output, usd, tokens, agents_usd }
        })
        .filter(|s| s.tokens > 0.0)
        .collect();
    sessions.sort_by(|a, b| b.usd.total_cmp(&a.usd));
    let session_count = sessions.len();
    sessions.truncate(12);

    let mut by_model: HashMap<&str, ModelRow> = HashMap::new();
    for d in &days {
        let model = d.model.as_str();
        let row = by_model.entry(model).or_insert_with(|| ModelRow {
            name: pretty_model(model),
            fam: d.fam,
            usd: 0.0,
            output: 0.0,
            priced: true,
        });
        row.usd += d.t.usd;
        row.output += d.t.o as f64;
        row.priced &= d.t.np == 0;
    }
    let mut models: Vec<ModelRow> = by_model.into_values().collect();
    // The models with no price first: nothing says how they compare, and they are what needs an update.
    models.sort_by(|a, b| a.priced.cmp(&b.priced).then(b.usd.total_cmp(&a.usd)).then(b.output.total_cmp(&a.output)));

    // A week or less shows each of its days: added up by weekday, most rows of "today" would stay empty.
    let heat_by_day = matches!(range, Range::Today | Range::Last24h | Range::Days(..=7));
    let mut heat: Vec<HeatRow> = if heat_by_day {
        let days = (today - start).num_days().max(0);
        (0..=days)
            .map(|i| {
                let d = start + chrono::Duration::days(i);
                let label = if d == today { t!("dash.today").into_owned() } else { d.format(&i18n::names(&d, "%a %-d")).to_string() };
                HeatRow { label, cells: [HeatCell::default(); 24] }
            })
            .collect()
    } else {
        (0..7).map(|wd| HeatRow { label: weekday(wd), cells: [HeatCell::default(); 24] }).collect()
    };
    for h in u.hours.iter().filter(|h| h.d >= start_key) {
        if let (Some(d), Ok(hr)) = (parse_day(&h.d), h.d[11..].parse::<usize>()) {
            let row = if heat_by_day { (d - start).num_days() } else { i64::from(d.weekday().num_days_from_monday()) };
            if let Some(row) = usize::try_from(row).ok().and_then(|r| heat.get_mut(r)) {
                let cell = &mut row.cells[hr.min(23)];
                cell.replies += h.t.n as f64;
                cell.usd += h.t.usd;
            }
        }
    }

    let minute = now.timestamp_millis() as u64 / 60_000 * 60_000;
    let by_minute: HashMap<u64, &Totals> = u.minutes.iter().map(|(m, t)| (*m, t)).collect();
    let live = (0..120)
        .map(|i| {
            let t = minute - (119 - i) * 60_000;
            by_minute.get(&t).map_or(Minute { at: t, output: 0.0, replies: 0.0, usd: 0.0 }, |x| Minute {
                at: t,
                output: x.o as f64,
                replies: x.n as f64,
                usd: x.usd,
            })
        })
        .collect();

    // Records, over all time.
    let mut per_day: BTreeMap<&str, DayTotal> = BTreeMap::new();
    for d in &u.days {
        let day = per_day.entry(d.d.as_str()).or_default();
        day.usd += d.t.usd;
        day.replies += d.t.n as f64;
        day.output += d.t.o as f64;
    }
    let best = per_day.iter().max_by(|(_, a), (_, b)| a.usd.total_cmp(&b.usd));
    let mut streak = 0;
    let mut d = today;
    loop {
        let n = per_day.get(day_key(d).as_str()).map_or(0.0, |day| day.replies);
        if n > 0.0 {
            streak += 1;
        } else if !(streak == 0 && d == today) {
            break;
        }
        d -= chrono::Duration::days(1);
    }
    let (all_usd, all_o) = per_day.values().fold((0.0, 0.0), |(usd, output), day| (usd + day.usd, output + day.output));
    let since = ((now.timestamp_millis() as f64 - u.first as f64) / 86_400_000.0).round().max(1.0);
    let first = Local
        .timestamp_millis_opt(u.first as i64)
        .single()
        .map(|t| t.format(&i18n::names(&t, "%-d %b %Y")).to_string())
        .unwrap_or_default();
    let long_day = |k: &str| parse_day(k).map(|d| d.format(&i18n::names(&d, "%-d %b")).to_string()).unwrap_or_default();
    let fact = |label: Cow<str>, value: String, sub: String| Fact { label: label.into_owned(), value, sub };
    let facts = [
        fact(
            t!("dash.fact.busiest"),
            best.map_or("—".into(), |(_, day)| usd(day.usd)),
            best.map_or(String::new(), |(key, day)| format!("{} · {}", long_day(key), i18n::count("dash.replies_n", day.replies as u64))),
        ),
        fact(t!("dash.fact.streak"), i18n::count("dash.days_n", streak), t!("dash.fact.streak_sub").into_owned()),
        fact(t!("dash.fact.since"), i18n::count("dash.days_n", since as u64), t!("dash.fact.since_sub", date = first).into_owned()),
        fact(t!("dash.fact.average"), usd(all_usd / since), t!("dash.fact.average_sub", n = short(all_o / since)).into_owned()),
        fact(
            t!("dash.fact.longest"),
            u.biggest.as_ref().map_or("—".into(), |b| t!("dash.fact.tokens", n = short(b.o as f64)).into_owned()),
            u.biggest.as_ref().map_or(String::new(), |b| {
                let day = Local
                    .timestamp_millis_opt(b.ts as i64)
                    .single()
                    .map(|t| t.format(&i18n::names(&t, "%-d %b")).to_string())
                    .unwrap_or_default();
                format!("{} · {day}", pretty_model(&b.model))
            }),
        ),
        fact(t!("dash.fact.all_time"), usd(all_usd), t!("dash.fact.all_time_sub", n = short(all_o)).into_owned()),
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
        heat_by_day,
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
    shown: HashMap<String, Shown>,
    frame: u64,
    dt: f32,
    last: Option<Instant>,
    moving: bool,
}

/// A value on screen, and the last frame that asked for it.
struct Shown {
    value: f64,
    frame: u64,
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
        let e = self.shown.entry(key).or_insert(Shown { value: from, frame });
        let mut v = to + (e.value - to) * k;
        if crate::glide::off() || (v - to).abs() <= 1e-3 * to.abs().max(e.value.abs()) + 1e-9 {
            v = to;
        } else {
            self.moving = true;
        }
        *e = Shown { value: v, frame };
        v
    }

    fn end(&mut self) {
        let frame = self.frame;
        self.shown.retain(|_, shown| shown.frame == frame);
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
    Cell { row: usize, hour: usize },
}

/// The data, range, metric, minute and language a `Dash` was worked out for.
/// What a computed `Dash` was made from: computed again only when one of these changes.
#[derive(PartialEq)]
struct DashKey {
    /// The usage's address: a new one is new data.
    usage: usize,
    range: Range,
    metric: Metric,
    /// The live chart moves each minute.
    minute: u64,
    language: String,
}

pub struct DashView {
    usage: Option<Arc<UsageView>>,
    range: Range,
    metric: Metric,
    cache: Option<(DashKey, Rc<Dash>)>,
    scroll: Glide,
    bars: Rc<Cell<Bounds<Pixels>>>,
    live: Rc<Cell<Bounds<Pixels>>>,
    heat: Rc<Cell<Bounds<Pixels>>>,
    hover: Option<Hover>,
    mouse: Point<Pixels>,
    pub active: bool,
    anim: Anim,
    range_seg: Seg,
    metric_seg: Seg,
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
                "24h" => Range::Last24h,
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
            bars: Rc::default(),
            live: Rc::default(),
            heat: Rc::default(),
            hover: None,
            mouse: Point::default(),
            active: false,
            anim: Anim::default(),
            range_seg: Seg::default(),
            metric_seg: Seg::default(),
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
        let key = DashKey {
            usage: Arc::as_ptr(&u) as usize,
            range: self.range,
            metric: self.metric,
            minute: now_ms() / 60_000,
            language: asterism_core::i18n::current(),
        };
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
            let Band { left: x0, step } = band(f32::from(b.size.width), d.cols.len());
            let i = ((f32::from(p.x - b.origin.x) - x0) / step).floor();
            return (i >= 0.0 && (i as usize) < d.cols.len()).then_some(Hover::Col(i as usize));
        }
        if l.contains(&p) && f32::from(p.y - l.origin.y) >= 50.0 {
            let w = f32::from(l.size.width) - 14.0;
            let i = ((f32::from(p.x - l.origin.x) - 4.0) / w * 119.0).round().clamp(0.0, 119.0);
            return Some(Hover::Minute(i as usize));
        }
        if h.contains(&p) {
            let left = heat_left(d.heat_by_day);
            let cw = (f32::from(h.size.width) - left) / 24.0;
            let (x, y) = (f32::from(p.x - h.origin.x) - left, f32::from(p.y - h.origin.y) - 16.0);
            let (hr, row) = ((x / cw).floor(), (y / heat_row_h(d.heat.len())).floor());
            if (0.0..24.0).contains(&hr) && (0.0..d.heat.len() as f32).contains(&row) {
                return Some(Hover::Cell { row: row as usize, hour: hr as usize });
            }
        }
        None
    }
}

/// The bars chart's band scale (d3 `scaleBand`, no outer padding).
struct Band {
    /// The first column's left edge.
    left: f32,
    /// From one column's left edge to the next's.
    step: f32,
}

/// The band scale of `n` columns over a chart `w` wide.
fn band(w: f32, n: usize) -> Band {
    let (l, r) = (48.0, 6.0);
    let inner = if n > 60 { 0.12 } else { 0.22 };
    Band { left: l, step: (w - l - r) / (n as f32 - inner).max(1.0) }
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
/// A row of a list: its name, its number, and a bar for its share. No line between rows: so close to the bar, a
/// line read as a relief.
fn list_row(label: AnyElement, value: AnyElement, share: f64, color: Rgba) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .py(px(8.0))
        .text_size(px(12.0))
        .child(div().flex().items_center().gap(px(12.0)).child(div().flex_1().min_w_0().overflow_hidden().child(label)).child(value))
        .child(
            div()
                .h(px(6.0))
                .w_full()
                .rounded_full()
                .bg(bg())
                .child(div().h_full().rounded_full().w(relative((share.max(0.005)) as f32)).bg(color)),
        )
}

/// The mark of replies whose model has no known price: a hollow ring, unlike any family's swatch.
fn no_price_ring(size: f32) -> Div {
    div().size(px(size)).flex_none().rounded_full().border_1().border_color(rgb(0xe4e5ee))
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

/// The short name of a weekday, Monday first, in this language.
fn weekday(wd: usize) -> String {
    t!("date.days_from_sunday").split(' ').nth((wd + 1) % 7).unwrap_or("").to_string()
}

impl Render for DashView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(d) = self.dash() else {
            return div().id("usage").size_full().p(px(20.0)).text_color(muted()).child(t!("dash.loading").into_owned());
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
        // A metric or a range has its own scale and its own bars: switching shows the new scale at once and lets the
        // bars rise, where sliding from dollars to replies made the axis crawl up for a second.
        let view = format!("{:?}{:?}", self.metric, self.range);
        let scale_top = *nice_ticks(target_max, 4.0).last().unwrap_or(&1.0);
        let bars_top = a.val(format!("btop{view}"), scale_top, scale_top);
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
            d.cols.iter().map(|c| std::array::from_fn(|f| a.val(format!("b{:?}{}{f}", self.metric, c.key), c.fams[f], 0.0))).collect();
        let live_max_target = d.live.iter().map(|m| m.output).fold(0.0, f64::max).max(1.0);
        let live_max = a.val("lmax".into(), live_max_target, live_max_target);
        let live_now: Vec<f64> = d.live.iter().map(|m| a.val(format!("m{}", m.at), m.output, 0.0)).collect();
        let heat_max = d.heat.iter().flat_map(|r| &r.cells).map(|c| c.replies).fold(0.0, f64::max).max(1.0);
        let heat_now: Vec<f64> = (0..d.heat.len() * 24)
            .map(|i| {
                let n = d.heat[i / 24].cells[i % 24].replies;
                let level = if n > 0.0 { 1.0 + ((n / heat_max).sqrt() * 5.0).floor().min(4.0) } else { 0.0 };
                // Days and weekdays are different grids: one does not fade into the other.
                a.val(format!("h{}{i}", if d.heat_by_day { 'd' } else { 'w' }), level, 0.0)
            })
            .collect();
        let model_share: Vec<f64> = d.models.iter().map(|m| a.val(format!("lm{}", m.name), m.usd / d.tot_usd.max(1e-9), 0.0)).collect();
        let top_session = d.sessions.first().map_or(1.0, |s| s.usd.max(1e-9));
        let session_share: Vec<f64> = d.sessions.iter().map(|s| a.val(format!("ls{}", s.id), s.usd / top_session, 0.0)).collect();
        let first_tool = u.tools.first().map_or(1.0, |(_, n)| (*n).max(1) as f64);
        let tool_share: Vec<f64> = u.tools.iter().map(|(name, n)| a.val(format!("lt{name}"), *n as f64 / first_tool, 0.0)).collect();
        a.end();
        crate::glide::trace("bars", bars_now.iter().flatten().sum::<f64>() as f32);
        crate::glide::trace("heat", heat_now.iter().sum::<f64>() as f32);
        if a.moving {
            crate::glide::again(window, cx);
        }
        let button = |id: String, label: String, on: bool| {
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
        // `label` stays English: it names the button and the saved setting; `text` is what shows.
        let ranges = [
            (Range::Today, "Today", t!("dash.range.today")),
            (Range::Last24h, "24h", t!("dash.range.24h")),
            (Range::Days(7), "7d", t!("dash.range.7d")),
            (Range::Days(30), "30d", t!("dash.range.30d")),
            (Range::Days(90), "90d", t!("dash.range.90d")),
            (Range::All, "All", t!("dash.range.all")),
        ]
        .into_iter()
        .map(|(r, label, text)| {
            button(format!("u-range-{label}"), text.into_owned(), self.range == r)
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.range = r;
                    settings::change(cx, |s| s.usage_range = label.to_lowercase());
                    cx.notify();
                }))
                .into_any_element()
        })
        .collect();
        let metrics = [
            (Metric::Usd, "Value $", t!("dash.metric_button.usd")),
            (Metric::Out, "Output", t!("dash.metric_button.out")),
            (Metric::All, "All tokens", t!("dash.metric_button.all")),
            (Metric::Replies, "Replies", t!("dash.metric_button.replies")),
        ]
        .into_iter()
        .map(|(m, label, text)| {
            button(format!("u-metric-{label}"), text.into_owned(), self.metric == m)
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
        let range_index = RANGES.iter().position(|r| *r == self.range).unwrap_or(3);
        let metric_index = [Metric::Usd, Metric::Out, Metric::All, Metric::Replies].iter().position(|m| *m == self.metric).unwrap_or(0);
        // Wraps when the title and both controls don't fit on one line (French is longer).
        let head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.0))
            .mb(px(14.0))
            .child(div().mr_auto().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(t!("dash.title").into_owned()))
            .child(self.range_seg.render(range_index, ranges, window, cx))
            .child(self.metric_seg.render(metric_index, metrics, window, cx));

        let pct = |x: f64| t!("number.percent", n = if x.is_finite() { (x * 100.0).round() } else { 0.0 }).into_owned();
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
            .child(tile(&t!("dash.tile.value"), usd(t[0]), t!("dash.tile.value_sub").into_owned()))
            .child(tile(
                &t!("dash.tile.processed"),
                short(t[1]),
                t!("dash.tile.processed_sub", pct = pct(t[7] / t[1].max(1.0))).into_owned(),
            ))
            .child(tile(&t!("dash.tile.output"), short(t[2]), t!("dash.tile.output_sub").into_owned()))
            .child(tile(&t!("dash.tile.cache"), pct(t[3]), t!("dash.tile.cache_sub", usd = usd(t[6])).into_owned()))
            .child(tile(&t!("dash.tile.replies"), short(t[4]), i18n::count("dash.sessions_n", d.session_count as u64)))
            .child(tile(&t!("dash.tile.subagents"), pct(t[5]), t!("dash.tile.subagents_sub").into_owned()));

        // The bars chart.
        let metric = self.metric;
        let hover = self.hover;
        let bars_d = d.clone();
        let hourly = matches!(self.range, Range::Today | Range::Last24h);
        let today_word = if hourly { t!("dash.now") } else { t!("dash.today") }.into_owned();
        // A date label needs about this much room from "today" (wider in French), and from the next date.
        let near = 56.0 + 3.2 * today_word.chars().count().saturating_sub(5) as f32;
        let label_gap = d.cols.iter().map(|c| c.label.chars().count()).max().unwrap_or(0) as f32 * 6.3 + 8.0;
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
                let Band { left: x0, step } = band(w, d.cols.len());
                let plot = w - 48.0 - 6.0;
                let every = d.cols.len().div_ceil(10).max((label_gap / step).ceil() as usize).max(1);
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
                    if metric == Metric::Usd && c.no_price > 0.0 {
                        let r = (bw / 2.0).clamp(2.0, 3.5);
                        let center = o + point(px(x + bw / 2.0), px((y(acc) - 4.0 - r).max(top + r)));
                        window.paint_quad(quad(
                            Bounds::new(center - point(px(r), px(r)), size(px(r * 2.0), px(r * 2.0))),
                            px(r),
                            transparent_black(),
                            px(1.5),
                            rgb(0xe4e5ee),
                            BorderStyle::Solid,
                        ));
                    }
                    let last = i + 1 == d.cols.len();
                    // A date too close to "today" (the last column) would run into it.
                    let near_last = !last && (d.cols.len() - 1 - i) as f32 * step < near;
                    if (i % every == 0 && !near_last) || last {
                        let (label, color) = if last { (today_word.to_string(), text()) } else { (c.label.clone(), faint()) };
                        // Centred under its column; the last one ends at the chart's edge at the latest ("aujourd'hui" is long).
                        let (at, align) = if last {
                            ((x + bw / 2.0 + label.chars().count() as f32 * 6.3 / 2.0).min(w), 1.0)
                        } else {
                            (x + bw / 2.0, 0.5)
                        };
                        paint_text(
                            window,
                            cx,
                            label.into(),
                            (o.x + px(at), o.y + px(h - 9.0)),
                            align,
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
                let last10 = d.live[110..].iter().map(|m| m.output).sum::<f64>() / 10.0;
                let last_hour = d.live[60..].iter().map(|m| m.usd).sum::<f64>();
                paint_text(
                    window,
                    cx,
                    t!("dash.live_rate", n = short(last10)).into_owned().into(),
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
                    t!("dash.live_sub", usd = usd(last_hour)).into_owned().into(),
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
                let (ago, now) = (t!("dash.two_hours_ago").into_owned(), t!("dash.now").into_owned());
                paint_text(window, cx, ago.into(), (o.x + px(l), o.y + px(h - 7.0)), 0.0, 10.5, faint(), FontWeight::NORMAL, true);
                paint_text(window, cx, now.into(), (o.x + px(w - r), o.y + px(h - 7.0)), 1.0, 10.5, text(), FontWeight::NORMAL, true);
                if let Some(Hover::Minute(i)) = hover {
                    window.paint_quad(fill(Bounds::new(o + point(px(x(i)), px(t)), size(px(1.0), px(h - bm - t))), rgba(0xffffff40)));
                }
            },
        )
        .w_full()
        .h(px(CHART_H));

        // When you work.
        let heat_d = d.clone();
        let (heat_l, heat_h) = (heat_left(d.heat_by_day), heat_row_h(d.heat.len()));
        let heat_rows_h = d.heat.len() as f32 * heat_h;
        let heat = canvas(
            {
                let cell = self.heat.clone();
                move |b, _, _| cell.set(b)
            },
            move |b, _, window, cx| {
                let o = b.origin;
                let w = f32::from(b.size.width);
                let (l, t) = (heat_l, 16.0);
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
                for (wd, row) in heat_d.heat.iter().enumerate() {
                    paint_text(
                        window,
                        cx,
                        row.label.clone().into(),
                        (o.x, o.y + px(t + wd as f32 * heat_h + heat_h / 2.0)),
                        0.0,
                        10.5,
                        faint(),
                        FontWeight::NORMAL,
                        true,
                    );
                    for hr in 0..24 {
                        let color = heat_color(heat_now[wd * 24 + hr] as f32);
                        let cell = Bounds::new(
                            o + point(px(l + hr as f32 * cw + 1.0), px(t + wd as f32 * heat_h + 1.0)),
                            size(px(cw - 2.0), px(heat_h - 2.0)),
                        );
                        let ring = if hover == Some(Hover::Cell { row: wd, hour: hr }) { rgba(0xffffffb0) } else { rgba(0) };
                        window.paint_quad(quad(cell, px(2.0), color, px(1.0), ring, BorderStyle::Solid));
                    }
                }
                let (ly, lx) = (t + heat_rows_h + 18.0, w - 5.0 * 16.0 - 40.0);
                paint_text(
                    window,
                    cx,
                    t!("dash.fewer").into_owned().into(),
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
                    t!("dash.more").into_owned().into(),
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
        .h(px(16.0 + heat_rows_h + 30.0));

        let legend = div().flex().gap(px(12.0)).children(d.legend.iter().map(|f| {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .text_color(muted())
                .child(div().size(px(10.0)).rounded(px(2.0)).bg(fam_color(f)))
                .child(fam_label(f))
        }));
        // In dollars, a model with no price adds nothing to its day's bar: a ring over the day stands for it.
        let no_price_shown = self.metric == Metric::Usd && d.cols.iter().any(|c| c.no_price > 0.0);
        let legend = legend.when(no_price_shown, |l| {
            l.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .text_color(muted())
                    .child(no_price_ring(10.0))
                    .child(t!("dash.no_price").into_owned()),
            )
        });
        let chart_title =
            if hourly { t!("dash.per_hour", metric = self.metric.name()) } else { t!("dash.per_day", metric = self.metric.name()) };
        let models = d.models.iter().enumerate().map(|(i, m)| {
            let out = t!("dash.out", n = short(m.output)).into_owned();
            // A model with no price says so, where "$0.00" would read as free.
            let worth = if m.priced {
                value(usd(m.usd), out)
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .flex_none()
                    .child(no_price_ring(8.0))
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(amber()).child(t!("dash.no_price").into_owned()))
                    .child(div().text_size(px(11.0)).text_color(faint()).child(out))
                    .into_any_element()
            };
            list_row(div().child(m.name.clone()).into_any_element(), worth, model_share[i], fam_color(m.fam))
        });
        let sessions = d.sessions.iter().enumerate().map(|(i, s)| {
            let id = s.id.clone();
            let label = div()
                .id(SharedString::from(format!("u-s-{i}")))
                .flex()
                .items_center()
                .gap(px(6.0))
                .min_w_0()
                .child(div().text_color(blue()).cursor_pointer().overflow_hidden().child(clip(&s.title, 60)))
                .child(chip(s.project.clone(), muted()).bg(panel()).border_1().border_color(line()).font_family(MONO).text_size(px(10.0)))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(DashEvent::Open(id.clone()))))
                .into_any_element();
            let out = t!("dash.out", n = short(s.output)).into_owned();
            let sub = if s.agents_usd > 0.0 { format!("{out} · {}", t!("dash.subagents_usd", usd = usd(s.agents_usd))) } else { out };
            list_row(label, value(usd(s.usd), sub), session_share[i], rgb(0x3987e5))
        });
        let tools = u.tools.iter().enumerate().map(|(i, (t, n))| {
            list_row(div().child(t.clone()).into_any_element(), value(short(*n as f64), String::new()), tool_share[i], rgb(0x3987e5))
        });
        let empty = || div().text_size(px(12.0)).text_color(faint()).child(t!("dash.empty").into_owned());
        let facts = div().flex().gap(px(14.0)).children(d.facts.iter().map(|Fact { label: k, value: v, sub: s }| {
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
            let models = u.unpriced.iter().map(|(m, n)| format!("{m} ({n})")).collect::<Vec<_>>().join(", ");
            format!(" {}", t!("dash.not_priced", models = models))
        };
        let heat_sub = match (self.range, d.heat_by_day) {
            (Range::Today, _) => t!("dash.card.heat_sub_today"),
            (_, true) => t!("dash.card.heat_sub_days"),
            _ => t!("dash.card.heat_sub"),
        }
        .into_owned();
        let row = || div().flex().gap(px(12.0)).mb(px(12.0));
        let content = div()
            .px(px(20.0))
            .pt(px(16.0))
            .pb(px(32.0))
            .child(head)
            .child(tiles)
            .child(
                row()
                    .child(span(card(chart_title.into_owned(), Some(legend.into_any_element())).child(bars), 8.0))
                    .child(span(card(t!("dash.card.live").into_owned(), None).child(live), 4.0)),
            )
            .child(
                row().child(span(card(t!("dash.card.heat").into_owned(), Some(small(heat_sub))).child(heat), 6.0)).child(span(
                    card(t!("dash.card.models").into_owned(), Some(small(t!("dash.card.models_sub").into_owned())))
                        .children(models)
                        .when(d.models.is_empty(), |c| c.child(empty())),
                    6.0,
                )),
            )
            .child(
                row()
                    .child(span(
                        card(t!("dash.card.sessions").into_owned(), Some(small(t!("dash.card.sessions_sub").into_owned())))
                            .children(sessions)
                            .when(d.sessions.is_empty(), |c| c.child(empty())),
                        8.0,
                    ))
                    .child(span(
                        card(t!("dash.card.tools").into_owned(), Some(small(t!("dash.card.all_time").into_owned()))).children(tools),
                        4.0,
                    )),
            )
            .child(row().child(span(
                card(t!("dash.card.records").into_owned(), Some(small(t!("dash.card.all_time").into_owned()))).child(facts),
                12.0,
            )))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(faint())
                    .line_height(px(17.0))
                    .child(t!("dash.footer", unpriced = unpriced).into_owned()),
            );

        let tip = self.hover.map(|h| {
            let card = match h {
                Hover::Col(i) => {
                    let c = &d.cols[i];
                    let total: f64 = c.fams.iter().sum();
                    tip_card()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} · {}", c.long, metric.fmt(total))))
                        .children(FAMS.iter().zip(&c.fams).filter(|(_, v)| **v > 0.0).map(|(f, v)| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(px(12.0))
                                .child(div().size(px(8.0)).rounded(px(2.0)).bg(fam_color(f)))
                                .child(fam_label(f))
                                .child(div().text_color(muted()).child(metric.fmt(*v)))
                        }))
                        .when(c.no_price > 0.0, |card| {
                            let said = t!(
                                "dash.no_price_on",
                                models = c.no_price_models.join(", "),
                                replies = i18n::count("dash.replies_n", c.no_price as u64)
                            );
                            card.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .text_size(px(12.0))
                                    .child(no_price_ring(8.0))
                                    .child(div().text_color(amber()).child(said.into_owned())),
                            )
                        })
                }
                Hover::Minute(i) => {
                    let Minute { at: t, output: o, replies: n, usd: v } = d.live[i];
                    tip_card().child(div().font_weight(FontWeight::SEMIBOLD).child(when(t))).child(
                        div().text_size(px(12.0)).text_color(muted()).child(format!(
                            "{} · {} · {}",
                            t!("dash.output_n", n = short(o)),
                            i18n::count("dash.replies_n", n as u64),
                            usd(v)
                        )),
                    )
                }
                Hover::Cell { row, hour: hr } => {
                    let HeatCell { replies: n, usd: v } = d.heat[row].cells[hr];
                    tip_card().child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} {hr:02}:00", d.heat[row].label))).child(
                        div().text_size(px(12.0)).text_color(muted()).child(format!(
                            "{} · {}",
                            i18n::count("dash.replies_n", n as u64),
                            usd(v)
                        )),
                    )
                }
            };
            tip_at(self.mouse, card.text_size(px(13.0)).text_color(text()))
        });
        let _ = window;
        let dash = d.clone();
        div()
            .id("usage")
            .size_full()
            .relative()
            .bg(bg())
            .child(
                div()
                    .id("usage-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.handle)
                    .child(content)
                    .child(self.scroll.driver()),
            )
            .children(tip)
            .on_mouse_move(cx.listener(move |v, e: &MouseMoveEvent, _, cx| {
                v.mouse = e.position;
                v.hover = v.hit(e.position, &dash);
                cx.notify();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Metric, Range, compute, nice_ticks, pretty_model, short, usd};
    use asterism_core::graph::{DayRow, NodeKind, SessionUsage, UsageView};
    use asterism_core::usage::Totals;
    use chrono::Local;
    use std::collections::BTreeMap;

    #[test]
    fn the_last_24_hours_count_hours_not_days() {
        let now = Local::now();
        let hour = |ago: i64| (now - chrono::Duration::hours(ago)).format("%Y-%m-%dT%H").to_string();
        let row = |d: String, usd: f64| DayRow {
            d,
            model: "claude-opus-5-5".into(),
            fam: "Opus",
            agent: false,
            t: Totals { o: 10, n: 1, usd, ..Totals::default() },
            saved: 0.0,
        };
        let session = SessionUsage {
            id: "s".into(),
            title: "s".into(),
            project: "p".into(),
            kind: NodeKind::Cli,
            days: BTreeMap::new(),
            hours: BTreeMap::from([(hour(30), [10.0, 4.0, 10.0, 0.0]), (hour(2), [10.0, 2.0, 10.0, 0.0])]),
        };
        let u = UsageView {
            first: (now - chrono::Duration::hours(30)).timestamp_millis() as u64,
            hours: vec![row(hour(30), 4.0), row(hour(2), 2.0), row(hour(0), 1.0)],
            sessions: vec![session],
            ..UsageView::default()
        };
        let d = compute(&u, Range::Last24h, Metric::Usd);
        // a column per hour, the reply of 30 hours ago left out everywhere
        assert_eq!(d.cols.len(), 24);
        assert_eq!((d.tot_usd, d.cols.iter().map(|c| c.fams.iter().sum::<f64>()).sum::<f64>()), (3.0, 3.0));
        assert_eq!(d.sessions.iter().map(|s| s.usd).collect::<Vec<_>>(), [2.0]);
        assert_eq!(d.models.iter().map(|m| m.usd).collect::<Vec<_>>(), [3.0]);
    }

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
        // The only test here that reads text, so switching the language races with nothing.
        asterism_core::i18n::set("fr");
        let (fr_usd, fr_big, fr_short) = (usd(9.92), usd(29555.4), short(6.6e6));
        asterism_core::i18n::set("en");
        assert_eq!((fr_usd.as_str(), fr_big.as_str(), fr_short.as_str()), ("9,92\u{a0}$", "29\u{a0}555\u{a0}$", "6,6\u{a0}M"));
    }
}
