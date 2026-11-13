//! The Machine tab: the machine at the top, who uses it on the left, the agent sessions on the right.
use crate::glide::Glide;
use crate::icons;
use crate::seg::Seg;
use crate::state;
use crate::ui::*;
use crate::ui::{blue, green, red}; // over gpui's colors of the same names
use asterism_core::Report;
use asterism_core::attribute::{ContainerView, GroupView, Owner, OwnerKind, ProcRow, SessionView};
use asterism_core::host::Pressure;
use asterism_core::i18n;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "snapshot")]
use std::time::Instant;
use tokio::sync::watch;

/// How long a session's details take to grow or shrink.
const UNFOLD: Duration = Duration::from_millis(220);

/// From this much CPU (percent of one core) a command works; under it, it runs but waits.
const WORKING_CPU: f32 = 1.0;
/// How often the marks of what works are redrawn. Each time redraws the window: at 24 a second they cost a tenth of a
/// core, which is too much for a window that says who uses the machine.
const LIVE_FPS: f32 = 10.;
/// The columns every row of a session's details ends with: since when, memory, CPU. With `TREND_W` kept free after
/// them, the numbers stand right under the card's own.
const SINCE_W: f32 = 52.;
const MEM_W: f32 = 64.;
const CPU_W: f32 = 70.;
const TREND_W: f32 = 12.;
/// The height of a row's first line, which a command's mark and numbers share when its text takes two.
const ROW_H: f32 = 20.;
/// How far what a command started stands in: under its text, past its mark.
const UNDER: f32 = 20.;

/// `UNFOLD`, or slower for snapshots (`MDB_UNFOLD_MS`) so frames can be saved mid-way.
fn unfold() -> Duration {
    #[cfg(feature = "snapshot")]
    if let Some(ms) = std::env::var("MDB_UNFOLD_MS").ok().and_then(|v| v.parse().ok()) {
        return Duration::from_millis(ms);
    }
    UNFOLD
}

pub struct MachineView {
    report: Option<Arc<Report>>,
    /// The report before: each number shows if it went up or down since.
    prev: Option<Arc<Report>>,
    /// App icons by bundle path; None while loading or when there is none.
    icons: HashMap<String, Option<Arc<Image>>>,
    expanded: HashSet<String>,
    /// Sessions whose details are still shrinking after a click closed them.
    closing: HashSet<String>,
    /// Clicks per session: each one starts a new animation.
    clicks: HashMap<String, u32>,
    by_cpu: bool,
    owners_scroll: Glide,
    sort_seg: Seg,
    sessions_scroll: Glide,
    /// Each column's search: the apps on the left, the sessions on the right.
    apps_search: Entity<InputState>,
    sessions_search: Entity<InputState>,
    _updates: Task<()>,
    _searches: [Subscription; 2],
    #[cfg(feature = "snapshot")]
    snapshot_taken: bool,
}

impl MachineView {
    pub fn new(mut rx: watch::Receiver<Option<Arc<Report>>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let updates = cx.spawn(async move |this, cx| {
            while rx.changed().await.is_ok() {
                let report = rx.borrow_and_update().clone();
                let updated = this.update(cx, |m, cx| {
                    m.prev = m.report.take();
                    m.report = report;
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        let apps_search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("view.search_apps").to_string()));
        let sessions_search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("view.search_sessions").to_string()));
        // Snapshot builds can start with words in the searches (MDB_APPS_SEARCH, MDB_SESSIONS_SEARCH).
        #[cfg(feature = "snapshot")]
        for (input, var) in [(&apps_search, "MDB_APPS_SEARCH"), (&sessions_search, "MDB_SESSIONS_SEARCH")] {
            if let Ok(v) = std::env::var(var) {
                input.update(cx, |i, cx| i.set_value(v, window, cx));
            }
        }
        let typed = |_: &mut Self, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<Self>| {
            if let InputEvent::Change = e {
                cx.notify();
            }
        };
        let searches = [cx.subscribe(&apps_search, typed), cx.subscribe(&sessions_search, typed)];
        Self {
            report: None,
            prev: None,
            icons: HashMap::new(),
            expanded: HashSet::new(),
            closing: HashSet::new(),
            clicks: HashMap::new(),
            by_cpu: false,
            owners_scroll: Glide::default(),
            sort_seg: Seg::default(),
            sessions_scroll: Glide::default(),
            apps_search,
            sessions_search,
            _updates: updates,
            _searches: searches,
            #[cfg(feature = "snapshot")]
            snapshot_taken: false,
        }
    }

    #[cfg(feature = "snapshot")]
    pub fn ready(&self) -> bool {
        self.report.is_some()
    }

    /// The searches keep the placeholders they were made with: they get this language's.
    pub fn relabel(&self, window: &mut Window, cx: &mut App) {
        for (input, key) in [(&self.apps_search, "view.search_apps"), (&self.sessions_search, "view.search_sessions")] {
            input.update(cx, |i, cx| i.set_placeholder(t!(key).to_string(), window, cx));
        }
    }

    fn toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        *self.clicks.entry(id.to_string()).or_default() += 1;
        if self.expanded.remove(id) {
            // Keep drawing the details while they shrink, then drop them.
            self.closing.insert(id.to_string());
            let id = id.to_string();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(unfold()).await;
                this.update(cx, |m, cx| {
                    if !m.expanded.contains(&id) && m.closing.remove(&id) {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        } else {
            self.closing.remove(id);
            self.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    /// Starts reading the icon of every app on screen that has none yet, away from the window's thread.
    fn load_icons(&mut self, r: &Report, cx: &mut Context<Self>) {
        for b in r.owners.iter().filter_map(|o| o.bundle.clone()) {
            if self.icons.contains_key(&b) {
                continue;
            }
            self.icons.insert(b.clone(), None);
            cx.spawn(async move |this, cx| {
                let path = b.clone();
                let png = cx.background_executor().spawn(async move { icons::app_icon_png(&path) }).await;
                this.update(cx, |m, cx| {
                    m.icons.insert(b, png.map(|p| Arc::new(Image::from_bytes(ImageFormat::Png, p))));
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn header(&self, r: &Report, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // `id` stays English so the element keeps its id in every language.
        let toggle = |id: &'static str, label: String, on: bool, cpu_sort: bool| {
            div()
                .id(id)
                .px(px(10.))
                .py(px(3.))
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .text_color(if on { text() } else { muted() })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.by_cpu = cpu_sort;
                    cx.notify();
                }))
                .into_any_element()
        };
        let sort = self.sort_seg.render(
            usize::from(self.by_cpu),
            vec![
                toggle("RAM", t!("view.sort_ram").into_owned(), !self.by_cpu, false),
                toggle("CPU", t!("view.cpu").into_owned(), self.by_cpu, true),
            ],
            window,
            cx,
        );
        let updated = ago(now_ms().saturating_sub(r.at) / 1000);
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(10.))
            .border_b_1()
            .border_color(line())
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(t!("view.title").into_owned()))
            .child(div().text_size(px(12.)).text_color(faint()).child(t!("view.updated", ago = updated).into_owned()))
            .child(div().flex_1())
            .child(div().text_size(px(12.)).text_color(faint()).child(t!("view.sort_by").into_owned()))
            .child(sort)
    }

    fn machine(&self, r: &Report) -> Div {
        let m = &r.host.memory;
        let tile = |icon: Option<IconName>, label: &str| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .flex_1()
                .min_w_0()
                .p(px(12.))
                .rounded(px(10.))
                .bg(panel())
                .border_1()
                .border_color(line())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .children(icon.map(|i| icons::line(i, faint(), 12.)))
                        .child(caption(label.to_uppercase())),
                )
        };
        let p = self.prev.as_ref().map(|p| &p.host);
        let with_trend = |big: Div, t: Trend, key: &str| div().flex().items_center().gap(px(6.)).child(big).child(trend_mark(t, key, r.at));
        let big = |s: String| div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(text()).child(s);
        let sub = |s: String| div().text_size(px(12.)).text_color(muted()).truncate().child(s);

        let (pressure, pressure_color) = match m.pressure {
            Some(Pressure::Normal) => (t!("view.pressure.normal"), green()),
            Some(Pressure::Warning) => (t!("view.pressure.warning"), amber()),
            Some(Pressure::Critical) => (t!("view.pressure.critical"), red()),
            None => (t!("view.pressure.unknown"), grey()),
        };
        let used = m.used as f32 / m.total.max(1) as f32;
        let cores = r.host.ncpu.max(1) as f32 * 100.0;
        let mut row = div()
            .flex()
            .gap(px(10.))
            .px(px(16.))
            .py(px(12.))
            .child(
                tile(Some(IconName::MemoryStick), &t!("view.memory_pressure"))
                    .child(
                        div().flex().items_center().gap(px(8.)).child(dot(pressure_color).size(px(10.))).child(big(pressure.into_owned())),
                    )
                    .child(sub(t!("view.compressed", size = m.compressed.map_or("—".into(), bytes)).into_owned())),
            )
            .child(
                tile(Some(IconName::MemoryStick), &t!("view.memory"))
                    .child(with_trend(
                        big(t!("view.of", used = bytes(m.used), total = bytes(m.total)).into_owned()),
                        mem_trend(m.used, p.map(|h| h.memory.used)),
                        "t-mem",
                    ))
                    .child(bar(
                        used,
                        if used > 0.9 {
                            red()
                        } else if used > 0.75 {
                            amber()
                        } else {
                            blue()
                        },
                    )),
            )
            .child(
                tile(Some(IconName::MemoryStick), &t!("view.swap"))
                    .child(with_trend(
                        big(t!("view.of", used = bytes(m.swap_used), total = bytes(m.swap_total)).into_owned()),
                        mem_trend(m.swap_used, p.map(|h| h.memory.swap_used)),
                        "t-swap",
                    ))
                    .child(sub(match (m.swap_in, m.swap_out) {
                        (Some(i), Some(o)) => t!("view.swap_io", swap_in = rate(i), swap_out = rate(o)).into_owned(),
                        _ => "—".into(),
                    })),
            )
            .child(
                tile(Some(IconName::Cpu), &t!("view.cpu"))
                    .child(with_trend(
                        big(t!("view.cpu_of_cores", cpu = cpu(r.host.cpu), n = r.host.ncpu).into_owned()),
                        cpu_trend(r.host.cpu, p.map(|h| h.cpu)),
                        "t-cpu",
                    ))
                    .child(bar(r.host.cpu / cores, if r.host.cpu / cores > 0.85 { red() } else { blue() })),
            );
        if let Some(d) = &r.docker {
            let docker = match &d.vm {
                Some(vm) => tile(None, &t!("view.docker_vm")).child(big(bytes(vm.mem))).child(sub(t!(
                    "view.vm_split",
                    containers = bytes(d.containers_mem),
                    other = bytes(vm.other_mem)
                )
                .into_owned())),
                None => tile(None, "Docker")
                    .child(big(bytes(d.containers_mem)))
                    .child(sub(i18n::count("view.containers_n", d.containers as u64))),
            };
            row = row.child(docker);
        }
        row
    }

    fn owners(&self, r: &Report, cx: &App) -> impl IntoElement {
        let mut owners: Vec<&Owner> = r.owners.iter().collect();
        if self.by_cpu {
            owners.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        }
        let top = |o: &Owner| if self.by_cpu { o.cpu } else { o.mem as f32 };
        // The bars keep the scale of the whole list while it is searched.
        let max = owners.first().map_or(1.0, |o| top(o).max(1.0));
        // "System" stays English in the data (it keys the trend and the id): only its label is translated.
        let label = |o: &Owner| if o.kind == OwnerKind::System { t!("core.system").into_owned() } else { o.name.clone() };
        let typed = self.apps_search.read(cx).value();
        let q = state::norm(typed.trim());
        owners.retain(|o| state::norm(&label(o)).contains(&q));
        let kind_color = |k: OwnerKind| match k {
            OwnerKind::Session => blue(),
            OwnerKind::Docker => cyan(),
            OwnerKind::App => violet(),
            OwnerKind::System | OwnerKind::Other => grey(),
        };
        let none = owners.is_empty().then(|| no_match(t!("view.no_app_match", query = typed.trim()).into_owned()));
        let list = div()
            .id("owners")
            .flex()
            .flex_col()
            .gap(px(2.))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.owners_scroll.handle)
            .px(px(16.))
            .pb(px(16.))
            .children(none)
            .children(owners.into_iter().take(30).map(|o| {
                let before = self.prev.as_ref().and_then(|p| p.owners.iter().find(|x| x.kind == o.kind && x.name == o.name));
                let icon = o.bundle.as_ref().and_then(|b| self.icons.get(b).cloned().flatten());
                let key = format!("o-{}", o.name);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .py(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(match icon {
                                Some(i) => img(i).flex_none().size(px(16.)).into_any_element(),
                                None => {
                                    div().w(px(16.)).flex_none().flex().justify_center().child(dot(kind_color(o.kind))).into_any_element()
                                }
                            })
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).text_color(text()).child(label(o)))
                            .child(
                                metric(IconName::MemoryStick, bytes(o.mem), mem_trend(o.mem, before.map(|b| b.mem)), &key, r.at).w(px(92.)),
                            )
                            .child(metric(IconName::Cpu, cpu(o.cpu), cpu_trend(o.cpu, before.map(|b| b.cpu)), &key, r.at).w(px(70.))),
                    )
                    .child(bar(top(o) / max, kind_color(o.kind)))
            }))
            .child(self.owners_scroll.driver());
        div()
            .flex()
            .flex_col()
            .w(px(380.))
            .flex_none()
            .h_full()
            .child(column_head(caption(t!("view.who_uses_it").into_owned()), &self.apps_search, 170.).px(px(16.)))
            .child(list)
    }

    fn session(&self, s: &SessionView, cx: &mut Context<Self>) -> Stateful<Div> {
        let r_at = self.report.as_ref().map_or(0, |r| r.at);
        let open = self.expanded.contains(&s.id);
        let working = s.status.as_deref() == Some("busy");
        let id = s.id.clone();
        let no_limit = s.groups.iter().flat_map(|g| &g.containers).filter(|c| c.unlimited).count();
        let mut card = div()
            .id(SharedString::from(s.id.clone()))
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(12.))
            .rounded(px(10.))
            .bg(panel())
            .border_1()
            .border_color(if open { rgba(0x38bdf866) } else { line() })
            .cursor_pointer()
            .hover(|st| st.bg(panel_hi()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(&id, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(if working { pulse(dot(amber()), format!("busy-{}", s.id)) } else { dot(green()).into_any_element() })
                    .child(div().flex_1().min_w_0().truncate().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(s.title.clone()))
                    .child(chip(s.project.clone(), muted()))
                    .child({
                        let before = self.prev.as_ref().and_then(|p| p.sessions.iter().find(|x| x.id == s.id));
                        let key = format!("s-{}", s.id);
                        div()
                            .flex()
                            .child(
                                metric(
                                    IconName::MemoryStick,
                                    bytes(s.mem + s.docker_mem),
                                    mem_trend(s.mem + s.docker_mem, before.map(|b| b.mem + b.docker_mem)),
                                    &key,
                                    r_at,
                                )
                                .w(px(92.)),
                            )
                            .child(
                                metric(
                                    IconName::Cpu,
                                    cpu(s.cpu + s.docker_cpu),
                                    cpu_trend(s.cpu + s.docker_cpu, before.map(|b| b.cpu + b.docker_cpu)),
                                    &key,
                                    r_at,
                                )
                                .w(px(70.)),
                            )
                    }),
            );
        if let Some(a) = &s.activity {
            let since = (a.since > 0).then(|| now_ms().saturating_sub(a.since) / 1000);
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(16.))
                    .child(
                        div()
                            .flex_none()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(amber())
                            .child(crate::state::tool_name(&a.tool).into_owned()),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(muted()).child(a.detail.clone()))
                    .child(figures(since, a.mem.map(|m| (m, a.cpu.unwrap_or(0.0))))),
            );
        }
        let docker_line = s.groups.iter().map(|g| format!("{} {}", g.kind, g.name)).collect::<Vec<_>>().join(", ");
        if !s.builds.is_empty() || !s.groups.is_empty() {
            card = card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .pl(px(16.))
                    .when(!s.builds.is_empty(), |d| d.child(chip(i18n::count("view.builds_n", s.builds.len() as u64), amber())))
                    .when(!s.groups.is_empty(), |d| d.child(chip(docker_line, cyan())))
                    .when(no_limit > 0, |d| d.child(chip(t!("view.no_limit").into_owned(), red()))),
            );
        }
        let clicks = self.clicks.get(&s.id).copied().unwrap_or(0);
        if open && clicks == 0 {
            card = card.child(self.details(s)); // opened without a click (a snapshot): no animation
        } else if open || self.closing.contains(&s.id) {
            // Grow (or shrink) to about the details' height, fading in (or out); once open, nothing limits it.
            let height = px(details_height(s));
            let animation = Animation::new(unfold()).with_easing(ease_in_out);
            card = card.child(div().overflow_hidden().child(self.details(s)).with_animation(
                SharedString::from(format!("{}-{clicks}", s.id)),
                animation,
                move |el, t| {
                    let shown = if open { t } else { 1.0 - t };
                    if open && t >= 1.0 { el } else { el.max_h(height * shown).opacity(shown) }
                },
            ));
        }
        card
    }

    /// What the session runs, each thing once: its commands (with what each started, containers included), the
    /// containers found by their folder, then the agent itself and what else it keeps open.
    fn details(&self, s: &SessionView) -> Div {
        let (blocks, alone) = blocks(s);
        let build = |pid: u32| s.builds.iter().any(|b| b.pid == Some(pid));
        let (commands, others): (Vec<&Block>, Vec<&Block>) = blocks.iter().partition(|b| b.is_command());
        let section = |title: String| caption(title).pt(px(4.));
        let mut d = div().flex().flex_col().gap(px(4.)).mt(px(6.)).pt(px(6.)).border_t_1().border_color(line());
        if !commands.is_empty() {
            d = d
                .child(section(t!("view.commands_n", n = commands.len()).into_owned()))
                .children(commands.iter().map(|b| command(b, &build)));
        }
        if !alone.is_empty() {
            let n: usize = alone.iter().map(|g| g.containers.len()).sum();
            d = d
                .child(section(t!("view.containers_caption", n = n).into_owned()))
                .children(alone.iter().flat_map(|g| group_rows(g, 0., Some(why(g)))));
        }
        if !others.is_empty() {
            d = d.child(section(t!("view.others_n", n = others.len()).into_owned())).children(others.iter().map(|b| other(b, &build)));
        }
        if s.more > 0 {
            d = d.child(div().text_size(px(11.)).text_color(faint()).child(i18n::count("view.more_processes", s.more as u64)));
        }
        d
    }

    fn sessions(&self, r: &Report, cx: &mut Context<Self>) -> impl IntoElement {
        let typed = self.sessions_search.read(cx).value();
        let q = state::norm(typed.trim());
        let mut sessions: Vec<&SessionView> = r.sessions.iter().filter(|s| session_matches(s, &q)).collect();
        if self.by_cpu {
            sessions.sort_by(|a, b| (b.cpu + b.docker_cpu).total_cmp(&(a.cpu + a.docker_cpu)));
        }
        let unattached: Vec<&GroupView> = r.unattached.iter().filter(|g| group_matches(g, &q)).collect();
        let none = (sessions.is_empty() && unattached.is_empty() && !q.is_empty())
            .then(|| no_match(t!("view.no_session_match", query = typed.trim()).into_owned()));
        let head = column_head(caption(t!("view.sessions_caption", n = sessions.len()).into_owned()), &self.sessions_search, 220.);
        let list = div()
            .id("sessions")
            .flex()
            .flex_col()
            .gap(px(8.))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.sessions_scroll.handle)
            .pb(px(16.))
            .children(none)
            .children(sessions.into_iter().map(|s| self.session(s, cx)))
            .when(!unattached.is_empty(), |d| {
                d.child(div().pt(px(12.)).child(caption(t!("view.unattached").into_owned())))
                    .children(unattached.into_iter().map(|g| unattached_card().children(group_rows(g, 0., Some(why(g))))))
            })
            .child(self.sessions_scroll.driver());
        div().flex().flex_col().flex_1().min_w_0().h_full().pr(px(16.)).child(head).child(list)
    }
}

#[cfg(feature = "snapshot")]
impl MachineView {
    /// `MDB_SNAPSHOT=<file.png>`: once the first report is drawn, save the frame and quit.
    /// `MDB_SNAPSHOT_OPEN=1` opens the biggest session first, `MDB_SNAPSHOT_OPEN=all` every session.
    /// `MDB_SNAPSHOT_FRAMES=<folder>`: click the biggest session open and save frames while it unfolds.
    fn snapshot(&mut self, r: &Report, window: &mut Window, cx: &mut Context<Self>) {
        if self.snapshot_taken {
            return;
        }
        if let Ok(dir) = std::env::var("MDB_SNAPSHOT_FRAMES")
            && let Some(s) = r.sessions.first()
        {
            self.snapshot_taken = true;
            self.toggle(&s.id.clone(), cx);
            frames(window, dir, Instant::now(), 0);
            return;
        }
        let Ok(path) = std::env::var("MDB_SNAPSHOT") else { return };
        if let Ok(which) = std::env::var("MDB_SNAPSHOT_OPEN")
            && self.expanded.is_empty()
            && !r.sessions.is_empty()
        {
            self.expanded.extend(r.sessions.iter().take(if which == "all" { usize::MAX } else { 1 }).map(|s| s.id.clone()));
            window.refresh();
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

/// Saves `<dir>/frame-<ms>.png` at 0, ¼, ½, ¾ and past the end of the unfolding, then quits.
#[cfg(feature = "snapshot")]
fn frames(window: &mut Window, dir: String, start: Instant, next: usize) {
    const MARKS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.2];
    window.on_next_frame(move |window, cx| {
        let mut next = next;
        let ms = start.elapsed().as_millis();
        if ms as f32 >= MARKS[next] * unfold().as_millis() as f32 {
            if let Ok(image) = window.render_to_image() {
                image.save(format!("{dir}/frame-{ms:03}.png")).ok();
            }
            next += 1;
        }
        if next < MARKS.len() { frames(window, dir, start, next) } else { cx.quit() }
    });
}

/// A column's title with its search at the right end; the list under it scrolls, this stays.
fn column_head(title: Div, search: &Entity<InputState>, width: f32) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .pb(px(8.))
        .child(div().flex_1().min_w_0().child(title))
        .child(div().w(px(width)).flex_none().child(Input::new(search)))
}

/// What a search with no answer says, where the list would be.
fn no_match(said: String) -> Div {
    div().py(px(12.)).text_size(px(12.)).text_color(muted()).child(said)
}

/// Whether a session shows this text (already through `norm`): in its title, its project or its containers' line.
fn session_matches(s: &SessionView, q: &str) -> bool {
    let has = |t: &str| state::norm(t).contains(q);
    has(&s.title) || has(&s.project) || s.groups.iter().any(|g| has(&g.name) || has(&g.kind))
}

/// Whether a group of containers no session owns shows this text: its name or one of its containers'.
fn group_matches(g: &GroupView, q: &str) -> bool {
    let has = |t: &str| state::norm(t).contains(q);
    has(&g.name) || has(&g.kind) || g.containers.iter().any(|c| has(&c.name))
}

/// A card for containers no session owns.
fn unattached_card() -> Div {
    div().flex().flex_col().gap(px(4.)).p(px(12.)).rounded(px(10.)).bg(panel()).border_1().border_color(line())
}

/// One thing a session started and everything under it: the agent itself, a command it runs, a server it keeps open.
struct Block<'s> {
    root: &'s ProcRow,
    under: Vec<&'s ProcRow>,
    /// The containers one of its docker commands started.
    groups: Vec<&'s GroupView>,
}

impl Block<'_> {
    /// A Bash call of the agent: core shows those as "$ <command>".
    fn is_command(&self) -> bool {
        self.root.cmd.starts_with("$ ")
    }

    /// What it uses now, CPU then memory, with what it started and its containers.
    fn load(&self) -> (f32, u64) {
        self.groups.iter().fold((self.root.tree_cpu, self.root.tree_mem), |(cpu, mem), g| (cpu + g.cpu, mem + g.mem))
    }
}

/// A session's processes as blocks, in the order they come (the agent first), then the containers no docker command
/// of a block started.
fn blocks(s: &SessionView) -> (Vec<Block<'_>>, Vec<&GroupView>) {
    let mut blocks: Vec<Block> = Vec::new();
    let mut detached = false;
    for p in &s.procs {
        if p.depth == 0 {
            detached = p.detached;
        }
        // The agent, each process it started and each detached one open a block; what those started goes under them.
        match blocks.last_mut() {
            Some(b) if p.depth > 1 || (p.depth == 1 && detached) => b.under.push(p),
            _ => blocks.push(Block { root: p, under: Vec::new(), groups: Vec::new() }),
        }
    }
    let mut alone = Vec::new();
    for g in &s.groups {
        let runs = |b: &&mut Block| g.client.is_some_and(|pid| b.root.pid == pid || b.under.iter().any(|p| p.pid == pid));
        match blocks.iter_mut().find(runs) {
            Some(b) => b.groups.push(g),
            None => alone.push(g),
        }
    }
    (blocks, alone)
}

/// About how tall a session's details are, so the unfolding animation grows at the right pace.
fn details_height(s: &SessionView) -> f32 {
    let (blocks, alone) = blocks(s);
    let of_group = |g: &&GroupView| g.containers.len() + usize::from(g.containers.len() > 1);
    let of_block = |b: &Block| if b.is_command() { 2 + b.under.len() } else { 1 } + b.groups.iter().map(of_group).sum::<usize>();
    let rows = blocks.iter().map(of_block).sum::<usize>() + alone.iter().map(of_group).sum::<usize>() + 3 + usize::from(s.more > 0);
    18.0 + rows as f32 * 24.0
}

/// What a row of a session's details ends with, each in its column: since when (seconds), then memory and CPU.
fn figures(since: Option<u64>, load: Option<(u64, f32)>) -> Div {
    let cell = |w: f32| div().w(px(w)).flex_none().flex().justify_end();
    let (mem, cpu_now) = load.unzip();
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(ROW_H))
        .pr(px(TREND_W))
        .child(cell(SINCE_W).text_size(px(11.)).text_color(faint()).children(since.map(ago)))
        .child(cell(MEM_W).children(mem.map(|m| num(bytes(m)))))
        .child(cell(CPU_W).children(cpu_now.map(|c| num(cpu(c)).text_color(muted()))))
}

/// A command on one line: a script typed over several lines would take them all.
fn one_line(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One line of a command, in the fixed-width font, cut with "…" where the row ends.
fn command_line(cmd: &str, color: Rgba) -> Div {
    div().flex_1().min_w_0().truncate().font_family(MONO).text_size(px(12.)).text_color(color).child(one_line(cmd))
}

fn build_chip() -> Div {
    chip(t!("view.build").into_owned(), amber())
}

/// Breathes: what it marks is at work. With reduced motion it stands still, which GPUI sees to.
fn pulse(mark: Div, key: String) -> AnyElement {
    let breath =
        Animation::new(Duration::from_millis(1600)).repeat_synced().with_max_fps(LIVE_FPS).with_easing(pulsating_between(0.35, 1.0));
    mark.with_animation(SharedString::from(key), breath, |el, t| el.opacity(t)).into_any_element()
}

/// A command the agent runs: its own line (two when it is long), what it started under it, and one bar down its
/// left side. It turns and is amber while it works, and stands grey while it waits.
fn command(b: &Block, build: &dyn Fn(u32) -> bool) -> Div {
    let (cpu_all, mem_all) = b.load();
    let working = cpu_all >= WORKING_CPU;
    let mark = if working {
        let turn = Animation::new(Duration::from_millis(1000)).repeat_synced().with_max_fps(LIVE_FPS);
        icons::line(IconName::LoaderCircle, amber(), 12.)
            .with_animation(SharedString::from(format!("turn-{}", b.root.pid)), turn, |icon, t| {
                icon.with_transformation(Transformation::rotate(percentage(t)))
            })
            .into_any_element()
    } else {
        div().size(px(8.)).rounded_full().border_1().border_color(faint()).into_any_element()
    };
    let typed = one_line(b.root.cmd.strip_prefix("$ ").unwrap_or(&b.root.cmd));
    // Named by what the agent said it was for, the command itself under it; else by the command, on two lines if long.
    let title = match &b.root.what {
        Some(what) => div().truncate().text_size(px(13.)).child(what.clone()),
        None => div().line_clamp(2).text_ellipsis().font_family(MONO).text_size(px(12.)).child(typed.clone()),
    };
    let head = div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(div().flex_none().w(px(12.)).h(px(ROW_H)).flex().items_center().justify_center().child(mark))
        .child(title.flex_1().min_w_0().line_height(px(ROW_H)).text_color(text()))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(ROW_H))
                .when(build(b.root.pid), |d| d.child(build_chip()))
                .when(!working, |d| d.child(chip(t!("view.waiting").into_owned(), muted()))),
        )
        .child(figures(Some(b.root.age), Some((mem_all, cpu_all))));
    let under = |cmd: &str, depth: usize| {
        div().flex().items_center().gap(px(8.)).pl(px(UNDER + 12. * depth as f32)).child(command_line(cmd, muted()))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .pl(px(8.))
        .py(px(4.))
        .border_l_2()
        .border_color(if working { amber() } else { grey() })
        .bg(rgba(0xffffff06))
        .child(head)
        .children(b.root.what.is_some().then(|| under(&typed, 0).child(figures(None, None))))
        // The shell a command opens for its parentheses repeats it word for word, and so does the program of a
        // command that is only that: their cost is in the total, and saying them twice is noise.
        .children(b.under.iter().filter(|p| p.cmd != b.root.cmd && one_line(&p.cmd) != typed).map(|p| {
            under(&p.cmd, p.depth.saturating_sub(2).min(8))
                .when(build(p.pid), |d| d.child(build_chip()))
                .child(figures(None, Some((p.mem, p.cpu))))
        }))
        .children(b.groups.iter().flat_map(|g| group_rows(g, UNDER, None)))
}

/// Anything else the session keeps open, on one line: the agent itself (its own use), a tool server or a detached
/// process (with all it started, which is not listed).
fn other(b: &Block, build: &dyn Fn(u32) -> bool) -> Div {
    let p = b.root;
    let agent = p.depth == 0 && !p.detached;
    let load = if agent { (p.mem, p.cpu) } else { (p.tree_mem, p.tree_cpu) };
    let builds = build(p.pid) || b.under.iter().any(|u| build(u.pid));
    let row = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(command_line(&p.cmd, muted()))
        .when(p.detached, |d| d.child(chip(t!("view.detached").into_owned(), violet())))
        .when(builds, |d| d.child(build_chip()))
        .child(figures(Some(p.age), Some(load)));
    div().flex().flex_col().gap(px(2.)).child(row).children(b.groups.iter().flat_map(|g| group_rows(g, 12., None)))
}

/// How a group of containers was attached to its session, or why it was not.
fn why(g: &GroupView) -> String {
    match (g.why, g.shared) {
        (Some("client"), _) => t!("view.why.client").into_owned(),
        (Some(_), _) => g.dir.as_ref().map_or(t!("view.why.folder"), |d| t!("view.why.mounts", dir = d)).into_owned(),
        (None, n) if n > 0 => i18n::count("view.why.shared", n as u64),
        _ => t!("view.why.none").into_owned(),
    }
}

/// A group of containers as rows, `indent` in: a container alone is one row; a cluster or a Compose project is its
/// own row, then one per container. `why` is said where nothing else shows what the group belongs to.
fn group_rows(g: &GroupView, indent: f32, why: Option<String>) -> Vec<Div> {
    if let [only] = &g.containers[..]
        && only.name == g.name
    {
        return vec![container(only, indent, why)];
    }
    let head = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(indent))
        .child(chip(g.kind.clone(), cyan()))
        .child(div().flex_none().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(g.name.clone()))
        .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(faint()).child(why.unwrap_or_default()))
        .child(figures(None, Some((g.mem, g.cpu))));
    std::iter::once(head).chain(g.containers.iter().map(|c| container(c, indent + 12., None))).collect()
}

fn container(c: &ContainerView, indent: f32, why: Option<String>) -> Div {
    let limits = if c.unlimited {
        chip(t!("view.no_limit").into_owned(), red())
    } else {
        // Docker's CPU count keeps its own decimals (1.5, 0.25); only the decimal mark changes.
        let n = c.cpus.to_string().replace('.', &t!("number.decimal"));
        let cpus = if c.cpus > 0.0 { t!("view.cpus", n = n) } else { t!("view.any_cpu") };
        let mem = if c.mem_limit > 0 { bytes(c.mem_limit) } else { t!("view.any_ram").into_owned() };
        chip(format!("{cpus} · {mem}"), muted())
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(indent))
        .child(icons::line(IconName::Container, cyan(), 12.))
        .child(div().flex_none().max_w(px(170.)).truncate().font_family(MONO).text_size(px(12.)).child(c.name.clone()))
        .child(command_line(&c.cmd, muted()))
        .children(why.map(|w| div().flex_none().max_w(px(200.)).truncate().text_size(px(11.)).text_color(faint()).child(w)))
        .when(c.heavy, |d| d.child(build_chip()))
        .child(limits)
        .child(figures((c.age > 0).then_some(c.age), Some((c.mem, c.cpu))))
}

impl Render for MachineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().flex().flex_col().bg(bg()).text_color(text()).text_size(px(13.));
        let Some(r) = self.report.clone() else {
            return root.items_center().justify_center().child(div().text_color(muted()).child(t!("view.loading").into_owned()));
        };
        #[cfg(feature = "snapshot")]
        self.snapshot(&r, window, cx);
        #[cfg(not(feature = "snapshot"))]
        let _ = window;
        self.load_icons(&r, cx);
        root.child(self.header(&r, window, cx))
            .child(self.machine(&r))
            .child(div().flex().flex_1().min_h_0().child(self.owners(&r, cx)).child(self.sessions(&r, cx)))
    }
}

/// Whether a number went up, down or stayed about the same since the last report.
#[derive(Clone, Copy, PartialEq)]
enum Trend {
    Up,
    Down,
    Flat,
}

/// Memory moves by a few MB all the time: only a change of 1% and at least 8 MB counts.
fn mem_trend(now: u64, before: Option<u64>) -> Trend {
    let Some(b) = before else { return Trend::Flat };
    let d = now as f64 - b as f64;
    if d.abs() < (b as f64 * 0.01).max(8e6) {
        Trend::Flat
    } else if d > 0.0 {
        Trend::Up
    } else {
        Trend::Down
    }
}

/// CPU jumps from one sample to the next: only a change of 15% and at least 3 points counts.
fn cpu_trend(now: f32, before: Option<f32>) -> Trend {
    let Some(b) = before else { return Trend::Flat };
    let d = now - b;
    if d.abs() < (b * 0.15).max(3.0) {
        Trend::Flat
    } else if d > 0.0 {
        Trend::Up
    } else {
        Trend::Down
    }
}

/// A small triangle: red and up when a number grew, green and down when it shrank. It keeps its room when flat, so
/// the numbers never shift, and fades in with each new report.
fn trend_mark(t: Trend, key: &str, at: u64) -> AnyElement {
    let slot = div().w(px(8.)).h(px(8.)).flex_none();
    if t == Trend::Flat {
        return slot.into_any_element();
    }
    let up = t == Trend::Up;
    let color = if up { red() } else { green() };
    slot.child(
        canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let (x, y, w, h) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
                let mut p = PathBuilder::fill();
                if up {
                    p.move_to(point(x + w / 2.0, y));
                    p.line_to(point(x + w, y + h));
                    p.line_to(point(x, y + h));
                } else {
                    p.move_to(point(x, y));
                    p.line_to(point(x + w, y));
                    p.line_to(point(x + w / 2.0, y + h));
                }
                p.close();
                if let Ok(p) = p.build() {
                    window.paint_path(p, color);
                }
            },
        )
        .size_full(),
    )
    .with_animation(SharedString::from(format!("{key}-{at}")), Animation::new(Duration::from_millis(250)), |el, t| el.opacity(t))
    .into_any_element()
}

/// A number with its icon in front and its trend after, right-aligned in its column.
fn metric(icon: IconName, value: String, t: Trend, key: &str, at: u64) -> Div {
    let key = format!("{key}-{}", if icon == IconName::Cpu { "cpu" } else { "mem" });
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .gap(px(4.))
        .child(icons::line(icon, faint(), 11.))
        .child(num(value).text_color(if icon == IconName::Cpu { muted() } else { text() }))
        .child(trend_mark(t, &key, at))
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` attribute would take the place of `#[test]`.
    use super::blocks;
    use asterism_core::attribute::{GroupView, ProcRow, SessionView};

    fn row(pid: u32, depth: usize, cmd: &str, detached: bool) -> ProcRow {
        ProcRow { pid, depth, cpu: 0., mem: 0, tree_cpu: 0., tree_mem: 0, age: 0, heavy: false, detached, cmd: cmd.into(), what: None }
    }

    fn group(name: &str, client: Option<u32>) -> GroupView {
        GroupView {
            kind: "Container".into(),
            name: name.into(),
            why: None,
            dir: None,
            shared: 0,
            client,
            cpu: 0.,
            mem: 0,
            containers: vec![],
        }
    }

    #[test]
    fn a_session_is_cut_into_what_it_started() {
        let s = SessionView {
            id: "s".into(),
            title: String::new(),
            project: String::new(),
            kind: String::new(),
            status: None,
            pid: 1,
            cpu: 0.,
            mem: 0,
            docker_cpu: 0.,
            docker_mem: 0,
            activity: None,
            builds: vec![],
            procs: vec![
                row(1, 0, "claude", false),
                row(2, 1, "$ ./test.sh", false),
                row(3, 2, "bash ./test.sh", false),
                row(4, 3, "docker run img", false),
                row(5, 1, "npm exec some-server", false),
                row(6, 2, "node some-server", false),
                row(7, 0, "node dev-server.js", true),
                row(8, 1, "esbuild --service", false),
            ],
            more: 0,
            groups: vec![group("by-command", Some(4)), group("by-folder", None), group("command-not-listed", Some(99))],
        };
        let (blocks, alone) = blocks(&s);
        let got: Vec<(u32, Vec<u32>, Vec<&str>, bool)> = blocks
            .iter()
            .map(|b| {
                (b.root.pid, b.under.iter().map(|p| p.pid).collect(), b.groups.iter().map(|g| g.name.as_str()).collect(), b.is_command())
            })
            .collect();
        // the agent alone; its Bash call with all it started and the container of its docker command; a tool server;
        // a detached process, whose own children are not new blocks
        assert_eq!(
            got,
            [
                (1, vec![], vec![], false),
                (2, vec![3, 4], vec!["by-command"], true),
                (5, vec![6], vec![], false),
                (7, vec![8], vec![], false)
            ]
        );
        assert_eq!(alone.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["by-folder", "command-not-listed"]);
    }
}
