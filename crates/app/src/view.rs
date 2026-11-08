//! The Machine tab: the machine at the top, who uses it on the left, the agent sessions on the right.
use crate::glide::Glide;
use crate::icons;
use crate::ui::*;
use crate::ui::{blue, green, red}; // over gpui's colors of the same names
use asterism_core::Report;
use asterism_core::attribute::{ContainerView, GroupView, Owner, OwnerKind, SessionView};
use asterism_core::host::Pressure;
use asterism_core::i18n::count;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How long a session's details take to grow or shrink.
const UNFOLD: Duration = Duration::from_millis(220);

/// `UNFOLD`, or slower for snapshots (`MDB_UNFOLD_MS`) so frames can be saved mid-way.
fn unfold() -> Duration {
    #[cfg(feature = "snapshot")]
    if let Some(ms) = std::env::var("MDB_UNFOLD_MS").ok().and_then(|v| v.parse().ok()) {
        return Duration::from_millis(ms);
    }
    UNFOLD
}

pub struct Machine {
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
    sort_seg: crate::seg::Seg,
    sessions_scroll: Glide,
    _updates: Task<()>,
    #[cfg(feature = "snapshot")]
    snapshot_taken: bool,
}

impl Machine {
    pub fn new(mut rx: watch::Receiver<Option<Arc<Report>>>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        Self {
            report: None,
            prev: None,
            icons: HashMap::new(),
            expanded: HashSet::new(),
            closing: HashSet::new(),
            clicks: HashMap::new(),
            by_cpu: false,
            owners_scroll: Glide::default(),
            sort_seg: crate::seg::Seg::default(),
            sessions_scroll: Glide::default(),
            _updates: updates,
            #[cfg(feature = "snapshot")]
            snapshot_taken: false,
        }
    }

    #[cfg(feature = "snapshot")]
    pub fn ready(&self) -> bool {
        self.report.is_some()
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
                None => {
                    tile(None, "Docker").child(big(bytes(d.containers_mem))).child(sub(count("view.containers_n", d.containers as u64)))
                }
            };
            row = row.child(docker);
        }
        row
    }

    fn owners(&self, r: &Report) -> impl IntoElement {
        let mut owners: Vec<&Owner> = r.owners.iter().collect();
        if self.by_cpu {
            owners.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        }
        let top = |o: &Owner| if self.by_cpu { o.cpu } else { o.mem as f32 };
        let max = owners.first().map_or(1.0, |o| top(o).max(1.0));
        let kind_color = |k: OwnerKind| match k {
            OwnerKind::Session => blue(),
            OwnerKind::Docker => cyan(),
            OwnerKind::App => violet(),
            OwnerKind::System | OwnerKind::Other => grey(),
        };
        div()
            .id("owners")
            .flex()
            .flex_col()
            .gap(px(2.))
            .w(px(380.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.owners_scroll.handle)
            .px(px(16.))
            .pb(px(16.))
            .child(div().pb(px(8.)).child(caption(t!("view.who_uses_it").into_owned())))
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
                            // "System" stays English in the data (it keys the trend and the id): only its label is translated.
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(text())
                                    .child(if o.kind == OwnerKind::System { t!("core.system").into_owned() } else { o.name.clone() }),
                            )
                            .child(
                                metric(IconName::MemoryStick, bytes(o.mem), mem_trend(o.mem, before.map(|b| b.mem)), &key, r.at).w(px(92.)),
                            )
                            .child(metric(IconName::Cpu, cpu(o.cpu), cpu_trend(o.cpu, before.map(|b| b.cpu)), &key, r.at).w(px(70.))),
                    )
                    .child(bar(top(o) / max, kind_color(o.kind)))
            }))
            .child(self.owners_scroll.driver())
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
                    .child(dot(if working { amber() } else { green() }))
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
            let cost = a.mem.map(|m| format!("{} · {}", bytes(m), cpu(a.cpu.unwrap_or(0.0))));
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
                    .children(cost.map(|c| num(c).text_color(amber())))
                    .when(a.since > 0, |d| {
                        d.child(div().text_size(px(11.)).text_color(faint()).child(ago(now_ms().saturating_sub(a.since) / 1000)))
                    }),
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
                    .when(!s.builds.is_empty(), |d| d.child(chip(count("view.builds_n", s.builds.len() as u64), amber())))
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

    fn details(&self, s: &SessionView) -> Div {
        let mut d = div().flex().flex_col().gap(px(4.)).mt(px(6.)).pt(px(8.)).border_t_1().border_color(line());
        for b in &s.builds {
            let what = b.container.as_ref().map_or(String::new(), |c| format!("{} · ", t!("view.in_container", name = c)));
            d = d.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(chip(t!("view.build").into_owned(), amber()))
                    .child(div().flex_1().min_w_0().truncate().font_family(MONO).text_size(px(12.)).child(format!("{what}{}", b.cmd)))
                    .child(num(bytes(b.mem)))
                    .child(num(cpu(b.cpu)).text_color(muted())),
            );
        }
        for g in &s.groups {
            d = d.child(group(g));
        }
        for p in &s.procs {
            d = d.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .pl(px(12. * p.depth.min(8) as f32))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(if p.heavy {
                                amber()
                            } else if p.detached {
                                violet()
                            } else {
                                muted()
                            })
                            .child(p.cmd.clone()),
                    )
                    .child(num(bytes(p.mem)).w(px(64.)).flex().justify_end())
                    .child(num(cpu(p.cpu)).w(px(56.)).flex().justify_end().text_color(muted())),
            );
        }
        if s.more > 0 {
            d = d.child(div().text_size(px(11.)).text_color(faint()).child(count("view.more_processes", s.more as u64)));
        }
        d
    }

    fn sessions(&self, r: &Report, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sessions: Vec<&SessionView> = r.sessions.iter().collect();
        if self.by_cpu {
            sessions.sort_by(|a, b| (b.cpu + b.docker_cpu).total_cmp(&(a.cpu + a.docker_cpu)));
        }
        div()
            .id("sessions")
            .flex()
            .flex_col()
            .gap(px(8.))
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.sessions_scroll.handle)
            .pr(px(16.))
            .pb(px(16.))
            .child(caption(t!("view.sessions_caption", n = r.sessions.len()).into_owned()))
            .children(sessions.into_iter().map(|s| self.session(s, cx)))
            .when(!r.unattached.is_empty(), |d| {
                d.child(div().pt(px(12.)).child(caption(t!("view.unattached").into_owned()))).children(
                    r.unattached
                        .iter()
                        .map(|g| div().p(px(12.)).rounded(px(10.)).bg(panel()).border_1().border_color(line()).child(group(g))),
                )
            })
            .child(self.sessions_scroll.driver())
    }
}

#[cfg(feature = "snapshot")]
impl Machine {
    /// `MDB_SNAPSHOT=<file.png>`: once the first report is drawn, save the frame and quit.
    /// `MDB_SNAPSHOT_OPEN=1` opens the biggest session first.
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
            frames(window, dir, std::time::Instant::now(), 0);
            return;
        }
        let Ok(path) = std::env::var("MDB_SNAPSHOT") else { return };
        if std::env::var("MDB_SNAPSHOT_OPEN").is_ok()
            && self.expanded.is_empty()
            && let Some(s) = r.sessions.first()
        {
            self.expanded.insert(s.id.clone());
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
fn frames(window: &mut Window, dir: String, start: std::time::Instant, next: usize) {
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

/// About how tall a session's details are, so the unfolding animation grows at the right pace.
fn details_height(s: &SessionView) -> f32 {
    let rows = s.builds.len() + s.procs.len() + s.groups.iter().map(|g| g.containers.len() + 1).sum::<usize>() + usize::from(s.more > 0);
    18.0 + rows as f32 * 22.0
}

fn group(g: &GroupView) -> Div {
    let why = match (g.why, g.shared) {
        (Some("client"), _) => t!("view.why.client").into_owned(),
        (Some(_), _) => g.dir.as_ref().map_or(t!("view.why.folder"), |d| t!("view.why.mounts", dir = d)).into_owned(),
        (None, n) if n > 0 => count("view.why.shared", n as u64),
        _ => t!("view.why.none").into_owned(),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(chip(g.kind.clone(), cyan()))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(g.name.clone()))
                .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(faint()).child(why))
                .child(num(bytes(g.mem)).w(px(64.)).flex().justify_end())
                .child(num(cpu(g.cpu)).w(px(56.)).flex().justify_end().text_color(muted())),
        )
        .children(g.containers.iter().map(container))
}

fn container(c: &ContainerView) -> Div {
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
        .pl(px(12.))
        .child(div().flex_none().w(px(170.)).truncate().font_family(MONO).text_size(px(12.)).child(c.name.clone()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(MONO)
                .text_size(px(12.))
                .text_color(if c.heavy { amber() } else { muted() })
                .child(c.cmd.clone()),
        )
        .child(limits)
        .child(num(bytes(c.mem)).w(px(64.)).flex().justify_end())
        .child(num(cpu(c.cpu)).w(px(56.)).flex().justify_end().text_color(muted()))
}

impl Render for Machine {
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
            .child(div().flex().flex_1().min_h_0().child(self.owners(&r)).child(self.sessions(&r, cx)))
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
