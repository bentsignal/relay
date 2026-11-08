use crate::theme::Theme;
use gpui::{Div, ElementId, FontWeight, Rgba, SharedString, Stateful, div, prelude::*, px};

/// Fader law: position 0..1 <-> -60..+12 dB, with 0 dB near 78%.
pub fn db_to_pos(db: f32) -> f32 {
    if db <= -60.0 { 0.0 } else { ((db + 60.0) / 72.0).clamp(0.0, 1.0).powf(4.0 / 3.0) }
}

pub fn pos_to_db(pos: f32) -> f32 {
    let p = pos.clamp(0.0, 1.0);
    if p <= 0.0005 { -60.0 } else { -60.0 + 72.0 * p.powf(0.75) }
}

/// Meter position for a linear peak value (-60..0 dBFS).
pub fn level_pos(lin: f32) -> f32 {
    let db = relay_dsp::gain_to_db(lin);
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

pub fn level_color(t: &Theme, lin: f32) -> Rgba {
    let db = relay_dsp::gain_to_db(lin);
    if db > -3.0 {
        t.meter_hi
    } else if db > -12.0 {
        t.meter_mid
    } else {
        t.meter_lo
    }
}

pub fn fmt_db(db: f32) -> SharedString {
    if db <= -59.9 {
        "-∞".into()
    } else if db.abs() < 0.05 {
        "0.0".into()
    } else {
        format!("{db:+.1}").into()
    }
}

pub fn text(s: impl Into<SharedString>, t: &Theme) -> Div {
    div().text_color(t.text).child(s.into())
}

pub fn muted(s: impl Into<SharedString>, t: &Theme) -> Div {
    div().text_color(t.muted).child(s.into())
}

pub fn section_title(s: impl Into<SharedString>, t: &Theme) -> Div {
    div().text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(t.faint).child(s.into())
}

pub fn dot(color: Rgba) -> Div {
    div().size(px(8.0)).rounded_full().bg(color).flex_shrink_0()
}

/// A compact rounded button.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, t: &Theme) -> Stateful<Div> {
    let hover = t.card_hover;
    div()
        .id(id)
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .bg(t.raised)
        .text_size(px(12.0))
        .text_color(t.text)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

pub fn primary_button(id: impl Into<ElementId>, label: impl Into<SharedString>, t: &Theme) -> Stateful<Div> {
    let accent = t.accent;
    div()
        .id(id)
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(accent)
        .text_size(px(12.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.accent_text)
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .child(label.into())
}

/// Small toggle chip; lit when `on`.
pub fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool, t: &Theme) -> Stateful<Div> {
    let (bg, fg) = if on { (t.accent, t.accent_text) } else { (t.raised, t.muted) };
    div()
        .id(id)
        .px(px(7.0))
        .py(px(3.0))
        .rounded(px(5.0))
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.0))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .child(label.into())
}

/// Segmented two-way switch.
pub fn segmented(id: &'static str, options: &[&'static str], selected: usize, t: &Theme) -> (Div, Vec<Stateful<Div>>) {
    let items = options
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let on = i == selected;
            div()
                .id((id, i))
                .px(px(12.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .text_size(px(12.0))
                .cursor_pointer()
                .when(on, |d| d.bg(t.card).text_color(t.text).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(t.muted))
                .child(*label)
        })
        .collect();
    (div().flex().gap(px(2.0)).p(px(2.0)).rounded(px(7.0)).bg(t.raised), items)
}

pub fn toggle(id: impl Into<ElementId>, on: bool, t: &Theme) -> Stateful<Div> {
    let knob = div().size(px(14.0)).rounded_full().bg(if on { t.accent_text } else { t.muted });
    div()
        .id(id)
        .w(px(30.0))
        .h(px(18.0))
        .rounded_full()
        .p(px(2.0))
        .flex()
        .when(on, |d| d.justify_end().bg(t.accent))
        .when(!on, |d| d.justify_start().bg(t.raised))
        .cursor_pointer()
        .child(knob)
}
