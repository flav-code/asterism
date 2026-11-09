//! The side panel of the map and the timeline: "Right now" when nothing is selected, else the selected session or
//! subagent: what it is, what it does now, who started it, what it started, its messages and its subagents.
use crate::glide::Glide;
use crate::state::{Filters, is_session, visible, working};
use crate::ui::*;
use crate::ui::{blue, green, red}; // over gpui's colors of the same names
use asterism_core::graph::{EdgeKind, Graph, MsgKind, Node, NodeKind};
use asterism_core::i18n;
use gpui_kit::prelude::*;
use gpui_kit::*;
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const WIDTH: f32 = 380.0;
/// How long a message body takes to unfold, and the panel to switch between two sessions.
const UNFOLD: Duration = Duration::from_millis(220);
const SWITCH: Duration = Duration::from_millis(180);

pub enum PanelEvent {
    Select,
}

pub struct Panel {
    graph: Option<Arc<Graph>>,
    index: HashMap<String, usize>,
    /// Messages in the range, for "Most talkative".
    in_range: Vec<usize>,
    range_h: Option<f64>,
    pub selected: Option<String>,
    /// Message bodies unfolded, still folding, and clicks on each (each click starts an animation).
    open: HashSet<String>,
    closing: HashSet<String>,
    clicks: HashMap<String, u32>,
    /// Each new selection fades the panel in.
    switches: u32,
    scroll: Glide,
    started: Instant,
    pub active: bool,
    _tick: Task<()>,
}

impl EventEmitter<PanelEvent> for Panel {}

impl Panel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Working sessions' dots beat.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let alive = this.update(cx, |p, cx| {
                    let beating = p.graph.as_ref().is_some_and(|g| g.nodes.iter().any(|n| is_session(n) && working(n)));
                    if p.active && beating {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Panel {
            graph: None,
            index: HashMap::new(),
            in_range: Vec::new(),
            range_h: None,
            selected: None,
            open: HashSet::new(),
            closing: HashSet::new(),
            clicks: HashMap::new(),
            switches: 0,
            scroll: Glide::default(),
            started: Instant::now(),
            active: true,
            _tick: tick,
        }
    }

    pub fn set_graph(&mut self, g: Arc<Graph>, filters: &Filters, cx: &mut Context<Self>) {
        self.in_range = visible(&g, filters, now_ms()).messages;
        self.range_h = filters.range_h;
        self.index = g.nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
        self.graph = Some(g);
        cx.notify();
    }

    /// Shows another session (or the overview), from a click elsewhere.
    pub fn show(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if id != self.selected {
            self.selected = id;
            self.switches += 1;
            self.scroll.handle.set_offset(point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    fn go(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.show(id, cx);
        cx.emit(PanelEvent::Select);
    }

    fn toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        *self.clicks.entry(id.to_string()).or_default() += 1;
        if self.open.remove(id) {
            self.closing.insert(id.to_string());
            let id = id.to_string();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(UNFOLD).await;
                this.update(cx, |p, cx| {
                    if !p.open.contains(&id) && p.closing.remove(&id) {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        } else {
            self.closing.remove(id);
            self.open.insert(id.to_string());
        }
        cx.notify();
    }

    fn title(&self, g: &Graph, id: &str) -> String {
        self.index.get(id).map_or_else(|| id.chars().take(12).collect(), |&i| g.nodes[i].title.clone())
    }

    /// A status dot: green and glowing when open, amber and beating when working, grey otherwise.
    fn dot(&self, n: &Node, size: f32) -> Div {
        let (color, glow) = if working(n) || (n.kind == NodeKind::Agent && n.live) {
            (amber(), true)
        } else if n.live {
            (green(), true)
        } else {
            (faint(), false)
        };
        let beat = if working(n) {
            0.55 + 0.45 * (0.5 + 0.5 * (self.started.elapsed().as_secs_f32() / 1.2 * std::f32::consts::TAU).cos())
        } else {
            1.0
        };
        div().flex_none().size(px(size)).mt(px(4.0)).rounded_full().bg(alpha(color, beat)).when(glow, |d| {
            d.shadow(vec![BoxShadow {
                color: alpha(color, 0.8 * beat).into(),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(10.0),
                spread_radius: px(0.0),
                inset: false,
            }])
        })
    }

    fn overview(&self, g: &Graph, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut live: Vec<&Node> = g.nodes.iter().filter(|n| is_session(n) && n.live).collect();
        live.sort_by(|a, b| working(b).cmp(&working(a)).then(b.last_at.cmp(&a.last_at)));
        let busy = live.iter().filter(|n| working(n)).count();
        let agents = g.nodes.iter().filter(|n| n.kind == NodeKind::Agent && n.live).count();
        let big = |n: usize, label: &str, color: Rgba| {
            div()
                .flex_1()
                .p(px(10.0))
                .rounded(px(10.0))
                .bg(rgba(0xffffff08))
                .border_1()
                .border_color(line())
                .child(div().text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(n.to_string()))
                .child(div().text_size(px(11.0)).text_color(muted()).child(label.to_string()))
        };
        let now = now_ms();
        let mut talk: HashMap<&str, usize> = HashMap::new();
        for &m in &self.in_range {
            *talk.entry(g.messages[m].from.as_str()).or_default() += 1;
        }
        let mut talk: Vec<(&str, usize)> = talk.into_iter().collect();
        talk.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        talk.truncate(6);
        let range = match self.range_h {
            None => t!("panel.overview.all_time").to_string(),
            Some(h) if h < 48.0 => format!("{h} h"),
            Some(h) => i18n::count("panel.overview.days", (h / 24.0) as u64), // the ranges are whole days
        };
        let open = live
            .iter()
            .map(|n| {
                let when = if n.status_at > 0 { n.status_at } else { n.last_at };
                let sum = match &n.now {
                    Some(m) => div()
                        .flex()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_none()
                                .font_family(MONO)
                                .text_size(px(11.0))
                                .text_color(if working(n) { amber() } else { faint() })
                                .child(crate::state::tool_name(&m.tool).into_owned()),
                        )
                        .child(div().min_w_0().child(clip(&m.detail, 90))),
                    None => div().child(n.project.clone()),
                };
                // Where it runs: an icon, and the machine's name when it is not this computer.
                let o = crate::state::origin(n);
                let right = div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .children(o.icon().map(|icon| crate::icons::line(icon, faint(), 11.0)))
                    .when(o == crate::state::Origin::Machine, |d| d.child(clip(&n.machine, 14)))
                    .child(ago(now.saturating_sub(when) / 1000));
                item(&n.id, self.dot(n, 10.0), n.title.clone(), right, Some(sum.into_any_element()), cx)
            })
            .collect::<Vec<_>>();
        let talkers = talk
            .iter()
            .map(|(id, c)| item(id, arrow("↗", blue()), self.title(g, id), i18n::count("panel.overview.sent", *c as u64), None, cx))
            .collect::<Vec<_>>();
        vec![
            pane()
                .child(h4(&t!("panel.overview.title")))
                .child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(big(live.len(), &i18n::count("panel.overview.open_count", live.len() as u64), text()))
                        .child(big(busy, &t!("panel.overview.working"), amber()))
                        .child(big(agents, &i18n::count("panel.overview.agents_count", agents as u64), text())),
                )
                .into_any_element(),
            pane()
                .child(h4(&t!("panel.overview.open")))
                .children(open)
                .when(live.is_empty(), |p| p.child(empty(&t!("panel.overview.none_open"))))
                .into_any_element(),
            pane()
                .child(h4(&t!("panel.overview.talkative", range = range)))
                .children(talkers)
                .when(talk.is_empty(), |p| p.child(empty(&t!("panel.overview.no_talk"))))
                .into_any_element(),
        ]
    }

    fn detail(&self, g: &Graph, n: &Node, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let now = now_ms();
        let node = |id: &str| self.index.get(id).map(|&i| &g.nodes[i]);
        let mut msgs: Vec<usize> = (0..g.messages.len()).filter(|&m| g.messages[m].from == n.id || g.messages[m].to == n.id).collect();
        msgs.sort_by_key(|&m| std::cmp::Reverse(g.messages[m].ts));
        msgs.truncate(120);
        let out = |k: EdgeKind| -> Vec<&Node> {
            let mut v: Vec<&Node> = g.edges.iter().filter(|e| e.from == n.id && e.kind == k).filter_map(|e| node(&e.to)).collect();
            v.sort_by_key(|x| std::cmp::Reverse(x.created_at));
            v
        };
        let parents: Vec<(EdgeKind, &Node)> = g
            .edges
            .iter()
            .filter(|e| e.to == n.id && matches!(e.kind, EdgeKind::Spawn | EdgeKind::Fork))
            .filter_map(|e| Some((e.kind, node(&e.from)?)))
            .collect();
        let agents = out(EdgeKind::Agent);
        let mut kids = out(EdgeKind::Spawn);
        kids.extend(out(EdgeKind::Fork));
        kids.sort_by_key(|x| std::cmp::Reverse(x.created_at));

        let kind = match n.kind {
            NodeKind::Agent => n.agent_type.clone().unwrap_or_else(|| t!("panel.kind.subagent").into()),
            NodeKind::Cli => t!("panel.kind.terminal").into(),
            NodeKind::Desktop => t!("panel.kind.desktop").into(),
            NodeKind::Remote => t!("panel.kind.remote").into(),
            NodeKind::Ghost => t!("panel.kind.gone").into(),
        };
        let chips = [
            Some(kind),
            Some(n.project.clone()),
            n.profile.clone(),
            n.model.as_ref().map(|m| m.trim_start_matches("claude-").to_string()),
            n.branch.clone(),
            n.scheduled.then(|| t!("panel.chip.scheduled").to_string()),
            n.archived.then(|| t!("panel.chip.archived").to_string()),
        ];
        let mut kv: Vec<(String, AnyElement)> = vec![
            (t!("panel.field.started").to_string(), text_el(if n.created_at > 0 { when(n.created_at) } else { "—".into() })),
            (
                t!("panel.field.last_active").to_string(),
                text_el(if n.last_at > 0 {
                    format!("{} · {}", when(n.last_at), t!("panel.ago", time = ago(now.saturating_sub(n.last_at) / 1000)))
                } else {
                    "—".into()
                }),
            ),
        ];
        if n.turns > 0 {
            kv.push((t!("panel.field.turns").to_string(), text_el(n.turns.to_string())));
        }
        if let Some(t) = n.tok {
            let (total, output, cost) = (crate::dash::short(t.t as f64), crate::dash::short(t.o as f64), crate::dash::usd(t.usd));
            let value = t!("panel.tokens_value", total = total, output = output, cost = cost).to_string();
            kv.push((t!("panel.field.tokens").to_string(), text_el(value)));
        }
        if let Some(pid) = n.pid {
            kv.push((t!("panel.field.process").to_string(), text_el(format!("pid {pid}"))));
        }
        if let Some(p) = n.parent.clone() {
            let label = clip(&self.title(g, &p), 40);
            kv.push((
                t!("panel.field.parent").to_string(),
                div()
                    .id("parent")
                    .text_color(blue())
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |v, _, _, cx| v.go(Some(p.clone()), cx)))
                    .into_any_element(),
            ));
        }
        if !n.prs.is_empty() {
            let links = n.prs.iter().rev().take(8).rev().enumerate().map(|(i, p)| {
                let url = p.url.clone();
                let repo = p.repo.rsplit('/').next().unwrap_or_default().to_string();
                div()
                    .id(SharedString::from(format!("pr-{i}")))
                    .text_color(blue())
                    .cursor_pointer()
                    .child(format!("{repo}#{}", p.n))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            });
            kv.push((
                t!("panel.field.pull_requests").to_string(),
                div().flex().flex_wrap().gap(px(8.0)).children(links).into_any_element(),
            ));
        }
        let now_card = n.now.as_ref().map(|m| {
            let busy = working(n);
            div()
                .mt(px(12.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(10.0))
                .bg(if busy { rgba(0xfbbf2412) } else { rgba(0xffffff08) })
                .border_1()
                .border_color(if busy { rgba(0xfbbf2433) } else { line() })
                .text_size(px(12.0))
                .child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .items_center()
                        .child(div().font_family(MONO).text_size(px(11.0)).text_color(if busy { amber() } else { muted() }).child(format!(
                            "{} · {}",
                            if busy { t!("panel.now_tool") } else { t!("panel.last_tool") },
                            crate::state::tool_name(&m.tool)
                        )))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(faint())
                                .child(t!("panel.ago", time = ago(now.saturating_sub(m.ts) / 1000)).to_string()),
                        ),
                )
                .child(div().mt(px(4.0)).child(m.detail.clone()))
        });
        let head = pane()
            .child(
                div()
                    .id("back")
                    .mb(px(10.0))
                    .text_size(px(12.0))
                    .text_color(muted())
                    .cursor_pointer()
                    .hover(|s| s.text_color(text()))
                    .child(t!("panel.back").to_string())
                    .on_click(cx.listener(|v, _, _, cx| v.go(None, cx))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(self.dot(n, 10.0))
                    .child(div().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).line_height(px(21.0)).child(n.title.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.0))
                    .mt(px(10.0))
                    // Where it runs comes first, with its icon.
                    .children(crate::state::origin(n).icon().map(|icon| {
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(panel_hi())
                            .text_size(px(11.))
                            .text_color(muted())
                            .child(crate::icons::line(icon, muted(), 11.0))
                            .child(crate::state::origin_label(n))
                    }))
                    .children(chips.into_iter().flatten().map(|c| chip(c, muted()).bg(panel_hi()))),
            )
            .child(div().flex().flex_col().gap(px(6.0)).mt(px(12.0)).text_size(px(12.0)).children(kv.into_iter().map(|(k, v)| {
                // Wide enough for the French "Dernière activité".
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(div().w(px(112.0)).flex_none().text_color(faint()).child(k))
                    .child(div().flex_1().min_w_0().child(v))
            })))
            .children(now_card);
        let link = |s: &Node, extra: Option<String>, cx: &mut Context<Self>| {
            item(&s.id, self.dot(s, 8.0), s.title.clone(), when(s.created_at), extra.map(|e| div().child(e).into_any_element()), cx)
        };
        let mut panes = vec![head.into_any_element()];
        if let Some((k, _)) = parents.first() {
            let title = if *k == EdgeKind::Fork { t!("panel.forked_from") } else { t!("panel.started_by") };
            let rows: Vec<AnyElement> = parents.iter().map(|(_, p)| link(p, None, cx)).collect();
            panes.push(pane().child(h4(&title)).children(rows).into_any_element());
        }
        if !kids.is_empty() {
            let rows: Vec<AnyElement> = kids.iter().map(|k| link(k, None, cx)).collect();
            panes.push(pane().child(h4(&t!("panel.kids", n = kids.len()))).children(rows).into_any_element());
        }
        let messages: Vec<AnyElement> = msgs.iter().map(|&m| self.message(g, n, m, cx)).collect();
        panes.push(
            pane()
                .child(h4(&t!("panel.messages", n = msgs.len())))
                .children(messages)
                .when(msgs.is_empty(), |p| p.child(empty(&t!("panel.no_messages"))))
                .into_any_element(),
        );
        if !agents.is_empty() {
            let rows: Vec<AnyElement> = agents.iter().map(|a| link(a, a.agent_type.clone(), cx)).collect();
            panes.push(pane().child(h4(&t!("panel.subagents", n = agents.len()))).children(rows).into_any_element());
        }
        panes
    }

    /// One message to or from `n`: a click unfolds its whole text, a click on the other session selects it.
    fn message(&self, g: &Graph, n: &Node, m: usize, cx: &mut Context<Self>) -> AnyElement {
        let msg = &g.messages[m];
        let outbound = msg.from == n.id;
        let other = if outbound { msg.to.clone() } else { msg.from.clone() };
        let failed = msg.ok == Some(false);
        let color = if failed {
            red()
        } else if msg.kind == MsgKind::Report {
            green()
        } else {
            blue()
        };
        let summary = if msg.summary.is_empty() { clip(&msg.text, 120) } else { msg.summary.clone() };
        let id = msg.id.clone();
        let (open, clicks) = (self.open.contains(&id), self.clicks.get(&id).copied().unwrap_or(0));
        let body_text = if msg.note.is_empty() { msg.text.clone() } else { format!("{}\n\n— {}", msg.text, msg.note) };
        // About the body's height, so it unfolds at the right pace: ~48 characters a line at 11 px.
        let lines: usize = body_text.lines().map(|l| l.chars().count() / 48 + 1).sum();
        let height = px(18.0 + lines as f32 * 16.5);
        let body = (open || self.closing.contains(&id)).then(|| {
            div()
                .overflow_hidden()
                .child(
                    div()
                        .mt(px(4.0))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(8.0))
                        .bg(rgb(0x0b0c12))
                        .text_color(rgb(0xb9bac8))
                        .font_family(MONO)
                        .text_size(px(11.0))
                        .line_height(px(16.5))
                        .child(body_text),
                )
                .with_animation(
                    SharedString::from(format!("{id}-{clicks}")),
                    Animation::new(UNFOLD).with_easing(ease_in_out),
                    move |el, t| {
                        let shown = if open { t } else { 1.0 - t };
                        if open && t >= 1.0 { el } else { el.max_h(height * shown).opacity(shown) }
                    },
                )
        });
        let go_to = other.clone();
        let toggle_id = id.clone();
        div()
            .id(SharedString::from(format!("msg-{id}")))
            .flex()
            .flex_col()
            .px(px(8.0))
            .py(px(7.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(rgba(0xffffff0a)))
            .on_click(cx.listener(move |v, _, _, cx| v.toggle(&toggle_id, cx)))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(arrow(if outbound { "↗" } else { "↙" }, color))
                    .child(
                        div()
                            .id(SharedString::from(format!("who-{id}")))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.title(g, &other))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                cx.stop_propagation();
                                v.go(Some(go_to.clone()), cx);
                            })),
                    )
                    .child(div().flex_none().font_family(MONO).text_size(px(11.0)).text_color(faint()).child(when(msg.ts))),
            )
            .child(
                div()
                    .pl(px(26.0))
                    .text_size(px(12.0))
                    .text_color(muted())
                    .child(
                        div()
                            .flex()
                            .gap(px(4.0))
                            .when(msg.kind == MsgKind::Report, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .text_color(green())
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(t!("panel.msg.report").to_string()),
                                )
                            })
                            .when(failed, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .text_color(red())
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(t!("panel.msg.failed").to_string()),
                                )
                            })
                            .child(div().flex_1().min_w_0().child(summary)),
                    )
                    .children(body),
            )
            .into_any_element()
    }
}

fn pane() -> Div {
    div().p(px(16.0)).border_b_1().border_color(line())
}

fn h4(s: &str) -> Div {
    div().mx(px(10.0)).mb(px(6.0)).text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(faint()).child(s.to_uppercase())
}

fn empty(s: &str) -> Div {
    div().px(px(10.0)).py(px(4.0)).text_size(px(12.0)).text_color(faint()).child(s.to_string())
}

fn arrow(s: &'static str, color: Rgba) -> Div {
    div().w(px(18.0)).flex_none().font_weight(FontWeight::BOLD).text_color(color).child(s)
}

fn text_el(s: String) -> AnyElement {
    div().child(s).into_any_element()
}

/// A clickable row: a mark, a title, a time on the right and an optional line under it. A click selects `id`.
fn item(id: &str, mark: Div, title: String, time: impl IntoElement, sum: Option<AnyElement>, cx: &mut Context<Panel>) -> AnyElement {
    let go = id.to_string();
    div()
        .id(SharedString::from(format!("item-{id}")))
        .flex()
        .flex_col()
        .px(px(8.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xffffff0a)))
        .on_click(cx.listener(move |v, _, _, cx| v.go(Some(go.clone()), cx)))
        .child(
            div()
                .flex()
                .gap(px(8.0))
                .child(div().w(px(18.0)).flex_none().flex().justify_center().child(mark))
                .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().flex_none().font_family(MONO).text_size(px(11.0)).text_color(faint()).child(time)),
        )
        .children(sum.map(|s| div().pl(px(26.0)).text_size(px(12.0)).text_color(muted()).child(s)))
        .into_any_element()
}

impl Render for Panel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(g) = self.graph.clone() else {
            return div().id("panel").size_full();
        };
        let selected = self.selected.as_deref().and_then(|id| self.index.get(id)).map(|&i| &g.nodes[i]);
        let panes = match selected {
            Some(n) => self.detail(&g, n, cx),
            None => self.overview(&g, cx),
        };
        div()
            .id("panel")
            .size_full()
            .bg(panel())
            .text_size(px(13.0))
            .text_color(text())
            .overflow_y_scroll()
            .track_scroll(&self.scroll.handle)
            .child(
                // A new selection fades in and rises a little.
                div().flex().flex_col().children(panes).with_animation(
                    SharedString::from(format!("panel-{}", self.switches)),
                    Animation::new(SWITCH).with_easing(ease_in_out),
                    |el, t| el.opacity(t).mt(px(6.0 * (1.0 - t))),
                ),
            )
            .child(self.scroll.driver())
    }
}
