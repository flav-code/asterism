//! Shared look: the dark palette, number formats and small building blocks.
use gpui_kit::prelude::*;
use gpui_kit::*;
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
        g if g >= 10.0 => format!("{g:.0} GB"),
        g if g >= 1.0 => format!("{g:.1} GB"),
        _ => format!("{:.0} MB", b as f64 / 1e6),
    }
}

pub fn cpu(p: f32) -> String {
    format!("{p:.0}%")
}

pub fn rate(per_s: f64) -> String {
    if per_s < 1e6 { format!("{:.0} KB/s", per_s / 1e3) } else { format!("{:.1} MB/s", per_s / 1e6) }
}

pub fn ago(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
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
