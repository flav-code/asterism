//! Shared look: the dark palette, number formats and small building blocks.
use asterism_core::i18n;
use gpui_kit::prelude::*;
use gpui_kit::*;
use rust_i18n::t;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const MONO: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const MONO: &str = "DejaVu Sans Mono";

pub fn bg() -> Rgba {
    rgb(0x07080d)
}
pub fn panel() -> Rgba {
    rgb(0x11121a)
}
pub fn panel_hi() -> Rgba {
    rgb(0x191a23)
}
pub fn line() -> Rgba {
    rgba(0xffffff12)
}
pub fn text() -> Rgba {
    rgb(0xe6e7f0)
}
pub fn muted() -> Rgba {
    rgb(0x8b8c9c)
}
pub fn faint() -> Rgba {
    rgb(0x84859a)
}
pub fn green() -> Rgba {
    rgb(0x4ade80)
}
pub fn amber() -> Rgba {
    rgb(0xfbbf24)
}
pub fn red() -> Rgba {
    rgb(0xf87171)
}
pub fn blue() -> Rgba {
    rgb(0x38bdf8)
}
pub fn violet() -> Rgba {
    rgb(0xa78bfa)
}
pub fn cyan() -> Rgba {
    rgb(0x22d3ee)
}
pub fn grey() -> Rgba {
    rgb(0x5b5c6b)
}

pub fn bytes(b: u64) -> String {
    let gb = b as f64 / 1e9;
    match gb {
        g if g >= 10.0 => t!("ui.gb", n = i18n::decimal(g, 0)),
        g if g >= 1.0 => t!("ui.gb", n = i18n::decimal(g, 1)),
        _ => t!("ui.mb", n = i18n::decimal(b as f64 / 1e6, 0)),
    }
    .into_owned()
}

pub fn cpu(p: f32) -> String {
    t!("number.percent", n = format!("{p:.0}")).into_owned()
}

pub fn rate(per_s: f64) -> String {
    if per_s < 1e6 { t!("ui.kb_s", n = i18n::decimal(per_s / 1e3, 0)) } else { t!("ui.mb_s", n = i18n::decimal(per_s / 1e6, 1)) }
        .into_owned()
}

pub fn ago(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => t!("ui.days", n = s / 86_400).into_owned(),
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A thin bar, square-ended: `ratio` of the width in `color`.
pub fn bar(ratio: f32, color: Rgba) -> Div {
    div().h(px(4.)).w_full().bg(line()).child(div().h_full().w(relative(ratio.clamp(0.0, 1.0))).bg(color))
}

pub fn caption(s: impl Into<SharedString>) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(faint()).child(s.into())
}

pub fn num(s: impl Into<SharedString>) -> Div {
    div().font_family(MONO).text_size(px(12.)).text_color(text()).child(s.into())
}

pub fn dot(color: Rgba) -> Div {
    div().flex_none().size(px(8.)).rounded_full().bg(color)
}

pub fn chip(s: impl Into<SharedString>, color: Rgba) -> Div {
    div().flex_none().px(px(6.)).py(px(1.)).rounded(px(4.)).bg(panel_hi()).text_size(px(11.)).text_color(color).child(s.into())
}

/// At most `n` characters, with "…" when cut.
pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n - 1).chain(['…']).collect() }
}

pub fn alpha(c: Rgba, a: f32) -> Rgba {
    Rgba { a: c.a * a, ..c }
}

/// Paints one line of text whose box starts `align` of its width left of `x` (0 left, 0.5 centered) and whose
/// middle is at `y`. Returns its width.
#[allow(clippy::too_many_arguments)]
pub fn paint_text(
    window: &mut Window,
    cx: &mut App,
    text: SharedString,
    (x, y): (Pixels, Pixels),
    align: f32,
    size: f32,
    color: Rgba,
    weight: FontWeight,
    mono: bool,
) -> Pixels {
    let mut font = window.text_style().font();
    font.weight = weight;
    if mono {
        font.family = MONO.into();
    }
    let run = TextRun { len: text.len(), font, color: color.into(), background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(text, px(size), &[run], None);
    let (w, lh) = (line.width(), px(size * 1.25));
    line.paint(point(x - w * align, y - lh / 2.0), lh, TextAlign::Left, None, window, cx).ok();
    w
}

/// The box of a hover card; [`tip_at`] places it.
pub fn tip_card() -> Div {
    div()
        .max_w(px(340.0))
        .p(px(10.0))
        .rounded(px(10.0))
        // Opaque: the card can sit over the side panel's text.
        .bg(panel())
        .border_1()
        .border_color(line())
        .flex()
        .flex_col()
        .gap(px(2.0))
}

/// A hover card next to the mouse (a place in the window), drawn over everything else: a card placed in its tab was
/// squeezed to a few pixels next to the side panel. Where it would leave the window, it goes to the other side of
/// the mouse.
pub fn tip_at(mouse: Point<Pixels>, card: Div) -> Deferred {
    // The padding is the gap between the mouse and the card, whichever side the card ends up on.
    deferred(anchored().position(mouse).child(div().p(px(14.0)).child(card)))
}

/// "14:05" today, "6 Nov 14:05" another day.
pub fn when(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_millis_opt(ms as i64).single() else { return String::new() };
    if t.date_naive() == Local::now().date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format(&i18n::names(&t, "%-d %b %H:%M")).to_string()
    }
}
