//! The Map tab: sessions as bubbles gathered by project, linked by who started or forked whom, by the messages
//! they send, and to their subagents. Positions are kept between launches so the map looks the same each time.
use crate::glide;
use crate::layout::{Body, Link, Sim};
use crate::state::{Filters, is_session, project_colors, visible, working};
use crate::ui::*;
use crate::ui::{blue, green}; // over gpui's colors of the same names
use gpui_kit::prelude::*;
use gpui_kit::*;
use mdb_core::graph::{EdgeKind, Graph, MsgKind, Node, NodeKind};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Titles {
    All,
    Open,
    Off,
}

/// The selection changed: read it from `selected`.
pub enum MapEvent {
    Select,
}

/// Messages between two sessions, drawn as one arc.
struct Pair {
    a: usize,
    b: usize,
    n: u32,
    reports: u32,
    last: u64,
}

struct Comet {
    from: usize,
    to: usize,
    started: Instant,
    color: Rgba,
}

const COMET: Duration = Duration::from_millis(1300);
const RIPPLE: Duration = Duration::from_millis(1000);

pub struct MapView {
    graph: Option<Arc<Graph>>,
    filters: Filters,
    colors: HashMap<String, Rgba>,
    sim: Sim,
    /// Body index → graph node index, and back by id.
    node_of: Vec<usize>,
    body_of: HashMap<String, usize>,
    radius: Vec<f32>,
    links: Vec<(usize, usize, EdgeKind)>,
    pairs: Vec<Pair>,
    subagents: Vec<u32>,
    quiet: Vec<bool>,
    signature: u64,
    saved: HashMap<String, (f32, f32)>,
    first_layout: bool,
    k: f32,
    tx: f32,
    ty: f32,
    fit_pending: bool,
    /// A wheel zoom on its way: the zoom it goes to, the point that stays put, the last frame.
    zoom: Option<(f32, (f32, f32), Instant)>,
    canvas: Rc<Cell<Bounds<Pixels>>>,
    hover: Option<usize>,
    mouse: Point<Pixels>,
    drag: Option<(Point<Pixels>, f32, f32, bool)>,
    pub selected: Option<String>,
    pub titles: Titles,
    pub active: bool,
    comets: Vec<Comet>,
    seen: HashSet<String>,
    _breath: Task<()>,
}

impl EventEmitter<MapEvent> for MapView {}

fn data_dir() -> PathBuf {
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/my-dev-business")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map_or(home.clone(), PathBuf::from).join("my-dev-business")
    } else {
        home.join(".config/my-dev-business")
    }
}

fn radius_of(n: &Node, deg: u32) -> f32 {
    match n.kind {
        NodeKind::Agent => 3.4,
        NodeKind::Remote | NodeKind::Ghost => 8.0,
        _ => 7.0 + ((n.turns as f32 * 0.6 + deg as f32 * 2.0).sqrt() * 1.6).min(14.0),
    }
}

/// The same small offset for the same id: new bubbles start near their anchor without being random.
fn jitter(id: &str) -> (f32, f32) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    let v = h.finish();
    (((v & 0xffff) as f32 / 65535.0 - 0.5) * 60.0, (((v >> 16) & 0xffff) as f32 / 65535.0 - 0.5) * 60.0)
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba { r: a.r * t + b.r * (1.0 - t), g: a.g * t + b.g * (1.0 - t), b: a.b * t + b.b * (1.0 - t), a: 1.0 }
}

fn alpha(c: Rgba, a: f32) -> Rgba {
    Rgba { a: c.a * a, ..c }
}

impl MapView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let saved = std::fs::read(data_dir().join("positions.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<HashMap<String, (f32, f32)>>(&b).ok())
            .unwrap_or_default();
        // Working sessions breathe; a gentle 12 frames per second is enough and costs little.
        let breath = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(83)).await;
                let alive = this.update(cx, |m, cx| {
                    if m.active && m.any_working() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        MapView {
            graph: None,
            filters: Filters::default(),
            colors: HashMap::new(),
            sim: Sim { bodies: Vec::new(), links: Vec::new(), alpha: 0.0, ring_r: 360.0 },
            node_of: Vec::new(),
            body_of: HashMap::new(),
            radius: Vec::new(),
            links: Vec::new(),
            pairs: Vec::new(),
            subagents: Vec::new(),
            quiet: Vec::new(),
            signature: 0,
            saved,
            first_layout: true,
            k: 1.0,
            tx: 0.0,
            ty: 0.0,
            fit_pending: true,
            zoom: None,
            canvas: Rc::new(Cell::new(Bounds::default())),
            hover: None,
            mouse: Point::default(),
            drag: None,
            selected: None,
            titles: Titles::All,
            active: true,
            comets: Vec::new(),
            seen: HashSet::new(),
            _breath: breath,
        }
    }

    /// Laid out and framed: nothing moves until the data changes.
    #[cfg(feature = "snapshot")]
    pub fn settled(&self) -> bool {
        self.graph.is_some() && !self.sim.running() && !self.fit_pending
    }

    fn any_working(&self) -> bool {
        let Some(g) = &self.graph else { return false };
        self.node_of.iter().any(|&i| working(&g.nodes[i]) || (g.nodes[i].kind == NodeKind::Agent && g.nodes[i].live))
    }

    pub fn set_graph(&mut self, graph: Arc<Graph>, filters: &Filters, cx: &mut Context<Self>) {
        let first = self.graph.is_none();
        // New messages fly from sender to receiver (not the ones already there when the map opened).
        for m in &graph.messages {
            if self.seen.insert(m.id.clone()) && !first && crate::ui::now_ms().saturating_sub(m.ts) < 60_000 {
                self.queue_comet(&m.from, &m.to, m.kind == MsgKind::Report);
            }
        }
        if filters != &self.filters {
            self.first_layout = true;
            self.fit_pending = true;
        }
        self.filters = filters.clone();
        self.colors = project_colors(&graph);
        self.graph = Some(graph);
        self.relayout();
        cx.notify();
    }

    fn queue_comet(&mut self, from: &str, to: &str, report: bool) {
        if let (Some(&a), Some(&b)) = (self.body_of.get(from), self.body_of.get(to)) {
            self.comets.push(Comet { from: a, to: b, started: Instant::now(), color: if report { green() } else { blue() } });
        }
    }

    fn relayout(&mut self) {
        let Some(g) = self.graph.clone() else { return };
        let v = visible(&g, &self.filters, now_ms());
        let index: HashMap<&str, usize> = g.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        let shown: HashMap<usize, usize> = v.nodes.iter().enumerate().map(|(b, &i)| (i, b)).collect();
        let body = |id: &str| index.get(id).and_then(|i| shown.get(i)).copied();

        let mut deg = vec![0u32; v.nodes.len()];
        let mut pairs: HashMap<(usize, usize), Pair> = HashMap::new();
        for &mi in &v.messages {
            let m = &g.messages[mi];
            let (Some(a), Some(b)) = (body(&m.from), body(&m.to)) else { continue };
            deg[a] += 1;
            deg[b] += 1;
            let key = (a.min(b), a.max(b));
            let p = pairs.entry(key).or_insert(Pair { a: key.0, b: key.1, n: 0, reports: 0, last: 0 });
            p.n += 1;
            p.reports += u32::from(m.kind == MsgKind::Report);
            p.last = p.last.max(m.ts);
        }
        let links: Vec<(usize, usize, EdgeKind)> =
            v.edges.iter().filter_map(|&e| Some((body(&g.edges[e].from)?, body(&g.edges[e].to)?, g.edges[e].kind))).collect();
        let mut linked = vec![false; v.nodes.len()];
        let mut subagents = vec![0u32; v.nodes.len()];
        for &(a, b, kind) in &links {
            linked[a] = true;
            linked[b] = true;
            if kind == EdgeKind::Agent {
                subagents[a] += 1;
            }
        }
        for p in pairs.values() {
            linked[p.a] = true;
            linked[p.b] = true;
        }
        let nodes: Vec<&Node> = v.nodes.iter().map(|&i| &g.nodes[i]).collect();
        let radius: Vec<f32> = nodes.iter().zip(&deg).map(|(n, &d)| radius_of(n, d)).collect();
        let iso: Vec<bool> = nodes.iter().zip(&linked).map(|(n, &l)| is_session(n) && !n.live && !l).collect();
        let many = nodes.iter().filter(|n| is_session(n)).count() > 24;
        self.quiet = nodes.iter().zip(&deg).map(|(n, &d)| n.kind != NodeKind::Agent && many && !n.live && d < 2).collect();

        // Project anchors on a circle, the busiest project nearest the middle.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for n in nodes.iter().filter(|n| is_session(n)) {
            *counts.entry(&n.project).or_default() += 1;
        }
        let mut list: Vec<(&str, usize)> = counts.into_iter().collect();
        list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let big = if list.len() > 1 { 260.0 + list.len() as f32 * 40.0 } else { 0.0 };
        let mut anchors: HashMap<&str, (f32, f32)> = HashMap::new();
        for (i, (p, _)) in list.iter().enumerate() {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 / list.len() as f32 * std::f32::consts::TAU;
            let r = big * if i == 0 { 0.35 } else { 1.0 };
            anchors.insert(p, (a.cos() * r, a.sin() * r));
        }
        let remote = (0.0, big + 260.0);
        self.sim.ring_r = 170.0 + (nodes.iter().zip(&iso).filter(|(n, i)| !**i && n.kind != NodeKind::Agent).count() as f32).sqrt() * 42.0;

        let mut h = std::collections::hash_map::DefaultHasher::new();
        for n in &nodes {
            n.id.hash(&mut h);
        }
        for l in &links {
            (l.0, l.1).hash(&mut h);
        }
        for k in pairs.keys() {
            k.hash(&mut h);
        }
        let signature = h.finish();
        let changed = signature != self.signature;
        self.signature = signature;

        let old: HashMap<String, (f32, f32)> =
            self.body_of.iter().filter_map(|(id, &b)| self.sim.bodies.get(b).map(|bd| (id.clone(), (bd.x, bd.y)))).collect();
        let mut restored = 0;
        let mut bodies = Vec::with_capacity(nodes.len());
        for (b, n) in nodes.iter().enumerate() {
            let anchor = anchors.get(n.project.as_str()).copied().unwrap_or(remote);
            let known = old.get(&n.id).or_else(|| self.saved.get(&n.id)).copied();
            restored += usize::from(known.is_some());
            let (x, y) = known.unwrap_or_else(|| {
                let near = n.parent.as_deref().and_then(|p| old.get(p).or_else(|| self.saved.get(p))).copied().unwrap_or(anchor);
                let (jx, jy) = jitter(&n.id);
                (near.0 + jx, near.1 + jy)
            });
            let agent = n.kind == NodeKind::Agent;
            let pad = if agent {
                1.2
            } else if n.live {
                34.0
            } else if deg[b] >= 2 {
                24.0
            } else {
                10.0
            };
            bodies.push(Body {
                x,
                y,
                vx: 0.0,
                vy: 0.0,
                pinned: false,
                r: radius[b] + pad,
                charge: if agent { -14.0 } else { -200.0 },
                anchor,
                anchor_pull: if agent || iso[b] { 0.0 } else { 0.045 },
                ring: iso[b],
            });
        }
        self.sim.links = links
            .iter()
            .map(|&(s, t, kind)| match kind {
                EdgeKind::Agent => Link { s, t, distance: 22.0 + radius[s], strength: 1.0 },
                _ => Link { s, t, distance: 85.0, strength: 0.45 },
            })
            .chain(pairs.values().map(|p| Link { s: p.a, t: p.b, distance: 130.0, strength: 0.14 }))
            .collect();
        self.sim.bodies = bodies;
        self.node_of = v.nodes.clone();
        self.body_of = nodes.iter().enumerate().map(|(b, n)| (n.id.clone(), b)).collect();
        self.radius = radius;
        self.links = links;
        self.pairs = pairs.into_values().collect();
        self.subagents = subagents;
        if changed || self.first_layout {
            let all_known = !nodes.is_empty() && restored == nodes.len();
            if self.first_layout && all_known {
                self.sim.alpha = 0.0; // the same picture as last time: nothing moves
            } else {
                if self.first_layout {
                    // Bodies with a known place hold still while the new ones find theirs.
                    for (b, n) in nodes.iter().enumerate() {
                        self.sim.bodies[b].pinned = old.contains_key(&n.id) || self.saved.contains_key(&n.id);
                    }
                }
                self.sim.alpha = if self.first_layout { 1.0 } else { 0.35 };
            }
            self.first_layout = nodes.is_empty();
        }
    }

    fn settle(&mut self) {
        for b in &mut self.sim.bodies {
            b.pinned = false;
        }
        let Some(g) = &self.graph else { return };
        for (b, &i) in self.node_of.iter().enumerate() {
            self.saved.insert(g.nodes[i].id.clone(), (self.sim.bodies[b].x, self.sim.bodies[b].y));
        }
        let ids: HashSet<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        self.saved.retain(|id, _| ids.contains(id.as_str()));
        let dir = data_dir();
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(json) = serde_json::to_vec(&self.saved)
        {
            std::fs::write(dir.join("positions.json"), json).ok();
        }
    }

    fn do_fit(&mut self) {
        let size = self.canvas.get().size;
        let (w, h) = (f32::from(size.width), f32::from(size.height));
        if self.sim.bodies.is_empty() || w < 10.0 {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for b in &self.sim.bodies {
            (x0, y0, x1, y1) = (x0.min(b.x), y0.min(b.y), x1.max(b.x), y1.max(b.y));
        }
        // Keep clear of the Titles switch above and the legend below.
        let (top, bottom) = (52.0, 150.0);
        let ah = (h - top - bottom).max(200.0);
        self.k = (0.9 / ((x1 - x0 + 120.0) / w).max((y1 - y0 + 70.0) / ah)).min(1.2);
        self.tx = w / 2.0 - self.k * (x0 + x1) / 2.0;
        self.ty = top + ah / 2.0 - 10.0 - self.k * (y0 + y1) / 2.0;
        self.fit_pending = false;
        self.zoom = None;
    }

    fn to_data(&self, p: Point<Pixels>) -> (f32, f32) {
        let o = self.canvas.get().origin;
        ((f32::from(p.x - o.x) - self.tx) / self.k, (f32::from(p.y - o.y) - self.ty) / self.k)
    }

    /// Zoom to `k`, keeping the canvas point `at` where it is.
    fn zoom_at(&mut self, k: f32, (mx, my): (f32, f32)) {
        (self.tx, self.ty) = (mx - (mx - self.tx) * k / self.k, my - (my - self.ty) * k / self.k);
        self.k = k;
    }

    fn hit(&self, p: Point<Pixels>) -> Option<usize> {
        let (x, y) = self.to_data(p);
        let mut best = None;
        let mut best_d = f32::MAX;
        for (b, body) in self.sim.bodies.iter().enumerate() {
            let d = (body.x - x).hypot(body.y - y);
            let reach = self.radius[b] + 4.0 / self.k;
            if d < reach && d < best_d {
                (best, best_d) = (Some(b), d);
            }
        }
        best
    }

    fn neighbours(&self, b: usize) -> HashSet<usize> {
        let mut set = HashSet::from([b]);
        for &(s, t, _) in &self.links {
            if s == b {
                set.insert(t);
            }
            if t == b {
                set.insert(s);
            }
        }
        for p in &self.pairs {
            if p.a == b {
                set.insert(p.b);
            }
            if p.b == b {
                set.insert(p.a);
            }
        }
        set
    }

    fn color(&self, n: &Node) -> Rgba {
        match n.kind {
            NodeKind::Remote | NodeKind::Ghost => rgb(0xb8b9c6),
            _ => self.colors.get(&n.project).copied().unwrap_or(rgb(0x93c5fd)),
        }
    }
}

/// What one frame draws, in canvas coordinates.
/// Points, the arrow tip, width, color, dashed.
type Line = (Vec<Point<Pixels>>, Option<Point<Pixels>>, f32, Rgba, bool);

struct Scene {
    lines: Vec<Line>,
    arrows: Vec<(Point<Pixels>, Point<Pixels>, Rgba)>,
    circles: Vec<(Point<Pixels>, f32, Rgba, Rgba, f32, bool)>,
    labels: Vec<(Point<Pixels>, SharedString, f32, Rgba, bool, FontWeight)>,
    comets: Vec<(Point<Pixels>, Rgba)>,
    ripples: Vec<(Point<Pixels>, f32, Rgba)>,
}

fn quad_point(a: Point<Pixels>, c: Point<Pixels>, b: Point<Pixels>, t: f32) -> Point<Pixels> {
    let u = 1.0 - t;
    point(a.x * (u * u) + c.x * (2.0 * u * t) + b.x * (t * t), a.y * (u * u) + c.y * (2.0 * u * t) + b.y * (t * t))
}

fn paint(scene: Scene, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let o = bounds.origin;
    for (pts, ctrl, width, color, dashed) in &scene.lines {
        let (a, b) = (o + pts[0], o + pts[1]);
        let mut path = PathBuilder::stroke(px(*width));
        match ctrl {
            Some(c) if !*dashed => {
                path.move_to(a);
                path.curve_to(b, o + *c);
            }
            _ if *dashed => {
                // Dashes along the line: 5 drawn, 4 skipped.
                let len = ((b - a).magnitude() as f32).max(1.0);
                let mut d = 0.0;
                while d < len {
                    let (t0, t1) = (d / len, ((d + 5.0) / len).min(1.0));
                    path.move_to(a + (b - a) * t0);
                    path.line_to(a + (b - a) * t1);
                    d += 9.0;
                }
            }
            _ => {
                path.move_to(a);
                path.line_to(b);
            }
        }
        if let Ok(p) = path.build() {
            window.paint_path(p, *color);
        }
    }
    for (tip, from, color) in &scene.arrows {
        let (tip, from) = (o + *tip, o + *from);
        let d = tip - from;
        let len = (d.magnitude() as f32).max(1e-3);
        let (ux, uy) = (f32::from(d.x) / len, f32::from(d.y) / len);
        let base = point(tip.x - px(ux * 7.0), tip.y - px(uy * 7.0));
        let mut path = PathBuilder::fill();
        path.move_to(tip);
        path.line_to(point(base.x + px(-uy * 3.5), base.y + px(ux * 3.5)));
        path.line_to(point(base.x - px(-uy * 3.5), base.y - px(ux * 3.5)));
        path.close();
        if let Ok(p) = path.build() {
            window.paint_path(p, *color);
        }
    }
    for (c, r, fill, stroke, width, dashed) in &scene.circles {
        let c = o + *c;
        let b = Bounds::new(point(c.x - px(*r), c.y - px(*r)), size(px(r * 2.0), px(r * 2.0)));
        let style = if *dashed { BorderStyle::Dashed } else { BorderStyle::Solid };
        window.paint_quad(quad(b, px(*r), *fill, px(*width), *stroke, style));
    }
    for (c, r, color) in &scene.ripples {
        let c = o + *c;
        let b = Bounds::new(point(c.x - px(*r), c.y - px(*r)), size(px(r * 2.0), px(r * 2.0)));
        window.paint_quad(quad(b, px(*r), transparent_black(), px(2.5), *color, BorderStyle::Solid));
    }
    for (c, color) in &scene.comets {
        let c = o + *c;
        let b = Bounds::new(point(c.x - px(4.5), c.y - px(4.5)), size(px(9.0), px(9.0)));
        window.paint_quad(quad(b, px(4.5), rgb(0xffffff), px(2.0), *color, BorderStyle::Solid));
    }
    for (p, text, font_size, color, center, weight) in scene.labels {
        let font = Font { weight, ..window.text_style().font() };
        let run = |c: Rgba| TextRun {
            len: text.len(),
            font: font.clone(),
            color: c.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let fs = px(font_size);
        let halo = window.text_system().shape_line(text.clone(), fs, &[run(bg())], None);
        let line = window.text_system().shape_line(text.clone(), fs, &[run(color)], None);
        let x = if center { p.x - line.width() / 2.0 } else { p.x };
        let origin = o + point(x, p.y);
        // A halo in the background color keeps titles readable over links.
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            halo.paint(origin + point(px(dx), px(dy)), fs * 1.25, TextAlign::Left, None, window, cx).ok();
        }
        line.paint(origin, fs * 1.25, TextAlign::Left, None, window, cx).ok();
    }
}

impl Render for MapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((to, at, last)) = self.zoom {
            let now = Instant::now();
            // In log space, so zooming in and out feel the same.
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
        glide::trace("zoom", self.k);
        if self.sim.running() {
            self.sim.tick();
            if self.sim.running() {
                window.request_animation_frame();
            } else {
                self.settle();
            }
        }
        if self.fit_pending && (!self.sim.running() || self.sim.alpha < 0.5) {
            self.do_fit();
            if self.fit_pending {
                window.request_animation_frame(); // the canvas has no size before its first frame
            }
        }
        let Some(g) = self.graph.clone() else {
            return div().id("map").size_full().flex().items_center().justify_center().text_color(muted()).child("Reading the sessions…");
        };

        let (k, tx, ty) = (self.k, self.tx, self.ty);
        let at = |b: usize| point(px(tx + self.sim.bodies[b].x * k), px(ty + self.sim.bodies[b].y * k));
        let focus = self.hover.or_else(|| self.selected.as_ref().and_then(|s| self.body_of.get(s).copied()));
        let near = focus.map(|f| self.neighbours(f));
        let dim = |b: usize| near.as_ref().is_some_and(|n| !n.contains(&b));
        let far = k < 0.55 && self.node_of.len() > 150;
        let now = now_ms();
        let t = Instant::now();
        let mut scene = Scene { lines: vec![], arrows: vec![], circles: vec![], labels: vec![], comets: vec![], ripples: vec![] };

        let focus_link = |a: usize, b: usize| focus.is_some_and(|f| f == a || f == b);
        for &(s, tgt, kind) in &self.links {
            let (a, b) = (at(s), at(tgt));
            let n = &g.nodes[self.node_of[s]];
            let faded = |c: Rgba, op: f32| alpha(c, if near.is_some() && !focus_link(s, tgt) { 0.12 } else { op });
            match kind {
                EdgeKind::Agent => {
                    if far && !focus_link(s, tgt) {
                        continue;
                    }
                    scene.lines.push((vec![a, b], None, 0.7, faded(self.color(n), 0.35), false));
                }
                EdgeKind::Spawn | EdgeKind::Fork => {
                    let d = b - a;
                    let len = (d.magnitude() as f32).max(1.0);
                    let back = (self.radius[tgt] + 5.0) * k / len;
                    let end = b - d * back;
                    let color = if kind == EdgeKind::Spawn { rgb(0xa78bfa) } else { rgb(0xf472b6) };
                    scene.lines.push((vec![a, end], None, 1.5, faded(color, 0.75), kind == EdgeKind::Fork));
                    scene.arrows.push((end, a, faded(color, 0.9)));
                }
            }
        }
        for p in &self.pairs {
            let (a, b) = (at(p.a), at(p.b));
            let mid = point((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
            let ctrl = point(mid.x - (b.y - a.y) * 0.2, mid.y + (b.x - a.x) * 0.2);
            let color = if p.reports == p.n { green() } else { blue() };
            let fresh = now.saturating_sub(p.last) < 20 * 60_000;
            let op = if near.is_some() && !focus_link(p.a, p.b) {
                0.12
            } else if fresh {
                0.9
            } else {
                0.6
            };
            scene.lines.push((vec![a, b], Some(ctrl), 1.3 + (p.n as f32).log2() * 1.1, alpha(color, op), false));
        }

        // Bubbles, then their titles: a title never covers another, the most important stays.
        let breath = (t.elapsed().as_secs_f32() + now as f32 / 1000.0 * std::f32::consts::TAU / 1.8).sin() * 0.5 + 0.5;
        let mut order: Vec<usize> = (0..self.node_of.len()).collect();
        let prio = |b: usize| {
            let n = &g.nodes[self.node_of[b]];
            (Some(n.id.as_str()) == self.selected.as_deref()) as u32 * 1_000_000
                + working(n) as u32 * 100_000
                + n.live as u32 * 10_000
                + self.subagents[b]
        };
        order.sort_by_key(|&b| std::cmp::Reverse(prio(b)));
        for &b in order.iter().rev() {
            let n = &g.nodes[self.node_of[b]];
            let agent = n.kind == NodeKind::Agent;
            if far && agent && !near.as_ref().is_some_and(|s| s.contains(&b)) {
                continue;
            }
            let c = at(b);
            let r = self.radius[b] * k;
            let pc = self.color(n);
            let op = if dim(b) {
                0.12
            } else if n.archived && !n.live {
                0.38
            } else {
                1.0
            };
            let (fill, stroke, width, dashed) = match n.kind {
                NodeKind::Remote | NodeKind::Ghost => (rgb(0x0b0c12), rgb(0xb8b9c6), 1.4, true),
                NodeKind::Agent if n.live => (amber(), amber(), 0.0, false),
                NodeKind::Agent => (pc, pc, 0.8, false),
                _ if n.live => (mix(pc, rgb(0x0b0c12), 0.6), mix(pc, rgb(0xffffff), 0.7), 1.4, false),
                _ => (mix(pc, rgb(0x0b0c12), 0.22), pc, 1.4, true),
            };
            let selected = Some(n.id.as_str()) == self.selected.as_deref();
            if working(n) && !dim(b) {
                scene.circles.push((
                    c,
                    r + 9.0 * k.max(0.6) * (1.0 + 0.35 * breath),
                    alpha(amber(), 0.05 + 0.13 * (1.0 - breath)),
                    alpha(amber(), 0.0),
                    0.0,
                    false,
                ));
                scene.circles.push((c, r + 4.5 * k.max(0.6), alpha(amber(), 0.0), alpha(amber(), op), 2.0, true));
            }
            let (stroke, width) = if selected { (blue(), 2.5) } else { (stroke, width) };
            scene.circles.push((c, r.max(1.5), alpha(fill, op), alpha(stroke, op), width, dashed && !selected));
            if n.live && !agent {
                let dr = (r * 0.3).max(3.0);
                let dc = point(c.x + px(r * 0.72), c.y - px(r * 0.72));
                scene.circles.push((dc, dr, alpha(if working(n) { amber() } else { green() }, op), alpha(bg(), op), 2.5, false));
            }
        }

        let mut kept: Vec<Bounds<Pixels>> = Vec::new();
        let cores: Vec<(usize, Bounds<Pixels>)> = (0..self.node_of.len())
            .filter(|&b| g.nodes[self.node_of[b]].kind != NodeKind::Agent)
            .map(|b| {
                let (c, r) = (at(b), self.radius[b] * k);
                (b, Bounds::new(point(c.x - px(r), c.y - px(r)), size(px(r * 2.0), px(r * 2.0))))
            })
            .collect();
        let font_px = 10.5 * k.max(1.0);
        for &b in &order {
            let n = &g.nodes[self.node_of[b]];
            if n.kind == NodeKind::Agent {
                continue;
            }
            let in_focus = near.as_ref().is_some_and(|s| s.contains(&b));
            let wanted = match self.titles {
                Titles::Off => in_focus,
                Titles::Open => n.live || in_focus,
                Titles::All => (!self.quiet[b] || in_focus) && (!far || n.live || in_focus),
            };
            if !wanted || dim(b) {
                continue;
            }
            let title: SharedString = n.title.chars().take(34).collect::<String>().into();
            let width = f32::from(
                window
                    .text_system()
                    .shape_line(
                        title.clone(),
                        px(font_px),
                        &[TextRun {
                            len: title.len(),
                            font: window.text_style().font(),
                            color: Hsla::default(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    )
                    .width(),
            );
            let c = at(b);
            let r = self.radius[b] * k;
            let h = font_px * 1.25;
            let place = |above: bool| {
                let y = if above { c.y - px(r + 3.0 + h) } else { c.y + px(r + 3.0) };
                Bounds::new(point(c.x - px(width / 2.0), y), size(px(width), px(h)))
            };
            let soft = n.live || Some(n.id.as_str()) == self.selected.as_deref();
            let free = |rect: &Bounds<Pixels>, ignore_cores: bool| {
                !kept.iter().any(|k| k.intersects(rect)) && (ignore_cores || !cores.iter().any(|(o, cb)| *o != b && cb.intersects(rect)))
            };
            let spot = [(false, false), (true, false), (false, true), (true, true)]
                .into_iter()
                .filter(|(_, ignore)| !ignore || soft || in_focus)
                .map(|(above, ignore)| (place(above), ignore))
                .find(|(rect, ignore)| free(rect, *ignore));
            let Some((rect, _)) = spot else { continue };
            kept.push(rect);
            let color = if n.live { rgb(0xffffff) } else { rgb(0xcfd0dc) };
            scene.labels.push((
                point(c.x, rect.origin.y),
                title,
                font_px,
                color,
                true,
                if n.live { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM },
            ));
            if in_focus && let Some(m) = n.now.as_ref().filter(|_| working(n)) {
                let line: SharedString = format!("⟳ {} · {}", m.tool, m.detail).chars().take(44).collect::<String>().into();
                scene.labels.push((point(c.x, rect.origin.y + px(h)), line, font_px * 0.95, amber(), true, FontWeight::NORMAL));
            }
            if far && self.subagents[b] > 0 {
                scene.labels.push((
                    point(c.x + px(r + 5.0), c.y - px(h / 2.0)),
                    format!("+{}", self.subagents[b]).into(),
                    font_px * 0.95,
                    muted(),
                    false,
                    FontWeight::SEMIBOLD,
                ));
            }
        }
        // Project names, faint, behind everything: only where no title or bubble is.
        {
            let mut seen = HashSet::new();
            for b in 0..self.node_of.len() {
                let n = &g.nodes[self.node_of[b]];
                if !is_session(n) || !seen.insert(n.project.clone()) || self.sim.bodies[b].ring {
                    continue;
                }
                let (ax, ay) = self.sim.bodies[b].anchor;
                let p = point(px(tx + ax * k), px(ty + ay * k) - px(60.0 * k.max(0.5)));
                let text: SharedString = n.project.to_uppercase().into();
                let rect = Bounds::new(point(p.x - px(text.len() as f32 * 4.5), p.y), size(px(text.len() as f32 * 9.0), px(14.0)));
                if !kept.iter().any(|k| k.intersects(&rect)) && !cores.iter().any(|(_, cb)| cb.intersects(&rect)) {
                    scene.labels.insert(0, (p, text, 11.0 * k.clamp(1.0, 1.4), faint(), true, FontWeight::SEMIBOLD));
                }
            }
        }

        // Comets: a dot flies along the arc, then a ripple at the receiver.
        self.comets.retain(|c| c.started.elapsed() < COMET + RIPPLE);
        for c in &self.comets {
            let (a, b) = (at(c.from), at(c.to));
            let e = c.started.elapsed();
            if e < COMET {
                let t = e.as_secs_f32() / COMET.as_secs_f32();
                let t = t * t * (3.0 - 2.0 * t);
                let mid = point((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                let ctrl = point(mid.x - (b.y - a.y) * 0.2, mid.y + (b.x - a.x) * 0.2);
                scene.comets.push((quad_point(a, ctrl, b, t), c.color));
            } else {
                let t = (e - COMET).as_secs_f32() / RIPPLE.as_secs_f32();
                scene.ripples.push((b, self.radius[c.to] * k + 34.0 * t, alpha(c.color, 1.0 - t)));
            }
        }
        if !self.comets.is_empty() {
            window.request_animation_frame();
        }

        let canvas_bounds = self.canvas.clone();
        let tip = self.hover.map(|b| {
            let n = &g.nodes[self.node_of[b]];
            let state = if working(n) {
                "working"
            } else if n.live {
                "open"
            } else {
                n.status.as_str()
            };
            let ago_s = now.saturating_sub(n.last_at) / 1000;
            let pos = self.mouse - self.canvas.get().origin;
            div()
                .absolute()
                .left(pos.x + px(14.0))
                .top(pos.y + px(14.0))
                .max_w(px(340.0))
                .p(px(10.0))
                .rounded(px(10.0))
                .bg(rgba(0x11121ad0))
                .border_1()
                .border_color(line())
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(n.title.clone()))
                .child(div().text_size(px(12.0)).text_color(muted()).child(format!("{state} · {} · {} ago", n.project, ago(ago_s))))
                .children(n.now.as_ref().map(|m| div().text_size(px(12.0)).child(format!("{} · {}", m.tool, m.detail))))
        });

        let titles_seg = |label: &'static str, mode: Titles| {
            div()
                .id(label)
                .px(px(10.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_size(px(12.0))
                .text_color(if self.titles == mode { text() } else { muted() })
                .when(self.titles == mode, |d| d.bg(panel_hi()))
                .child(label)
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.titles = mode;
                    cx.notify();
                }))
        };

        div()
            .id("map")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(bg())
            .child(
                canvas(move |bounds, _, _| canvas_bounds.set(bounds), move |bounds, _, window, cx| paint(scene, bounds, window, cx))
                    .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|m, e: &MouseDownEvent, _, _| {
                    m.drag = Some((e.position, m.tx, m.ty, false));
                }),
            )
            .on_mouse_move(cx.listener(|m, e: &MouseMoveEvent, _, cx| {
                m.mouse = e.position;
                if let Some((start, tx, ty, moved)) = m.drag.as_mut() {
                    let d = e.position - *start;
                    if *moved || (d.magnitude() as f32) > 3.0 {
                        *moved = true;
                        (m.tx, m.ty) = (*tx + f32::from(d.x), *ty + f32::from(d.y));
                        cx.notify();
                    }
                    return;
                }
                let hover = m.hit(e.position);
                if hover != m.hover {
                    m.hover = hover;
                    cx.notify();
                } else if hover.is_some() {
                    cx.notify(); // the tooltip follows the pointer
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|m, e: &MouseUpEvent, _, cx| {
                    let moved = m.drag.take().is_some_and(|d| d.3);
                    if moved {
                        return;
                    }
                    let id = m.hit(e.position).and_then(|b| m.graph.as_ref().map(|g| g.nodes[m.node_of[b]].id.clone()));
                    m.selected = if id == m.selected { None } else { id };
                    cx.emit(MapEvent::Select);
                    cx.notify();
                }),
            )
            .on_scroll(cx.listener(|m, e: &ScrollEvent, window, cx| {
                let dy = f32::from(e.delta.pixel_delta(window.line_height()).y);
                let o = m.canvas.get().origin;
                let at = (f32::from(e.position.x - o.x), f32::from(e.position.y - o.y));
                let to = (m.zoom.map_or(m.k, |z| z.0) * (dy * 0.003).exp()).clamp(0.12, 5.0);
                if e.delta.precise() || glide::off() {
                    m.zoom = None;
                    m.zoom_at(to, at);
                } else {
                    m.zoom = Some((to, at, Instant::now()));
                }
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .top(px(8.0))
                    .left(px(12.0))
                    .flex()
                    .items_center()
                    .p(px(2.0))
                    .rounded(px(8.0))
                    .bg(panel())
                    .border_1()
                    .border_color(line())
                    .child(div().px(px(8.0)).text_size(px(11.0)).text_color(faint()).child("Titles"))
                    .child(titles_seg("All", Titles::All))
                    .child(titles_seg("Open", Titles::Open))
                    .child(titles_seg("Off", Titles::Off)),
            )
            .child(legend())
            .children(tip)
    }
}

fn legend() -> Div {
    let item = |swatch: Div, label: &'static str| div().flex().items_center().gap(px(7.0)).child(swatch).child(label);
    let ring = |c: Rgba, dashed: bool| div().size(px(10.0)).rounded_full().border_1().border_color(c).when(dashed, |d| d.border_dashed());
    let bar = |c: Rgba| div().w(px(14.0)).h(px(2.0)).bg(c);
    div()
        .absolute()
        .right(px(16.0))
        .bottom(px(16.0))
        .p(px(10.0))
        .rounded(px(12.0))
        .bg(rgba(0x11121ad0))
        .border_1()
        .border_color(line())
        .grid()
        .grid_cols(2)
        .gap_x(px(16.0))
        .gap_y(px(5.0))
        .text_size(px(11.0))
        .text_color(muted())
        .child(item(dot(amber()), "working"))
        .child(item(dot(green()), "open, idle"))
        .child(item(ring(rgb(0x7c9cff), true), "closed"))
        .child(item(ring(rgb(0xb8b9c6), true), "other machine"))
        .child(item(bar(rgb(0xa78bfa)), "started"))
        .child(item(bar(rgb(0xf472b6)), "forked"))
        .child(item(bar(blue()), "message"))
        .child(item(bar(green()), "report back"))
        .child(item(dot(rgb(0x7c9cff)).size(px(5.0)), "subagent"))
}
