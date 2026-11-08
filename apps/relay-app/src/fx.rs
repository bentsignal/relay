//! Effects panel: parametric EQ with a drawn response curve, plus gate,
//! compressor, limiter and noise suppression.

use crate::app::{Param, RelayApp, Target};
use crate::theme::with_alpha;
use crate::widgets::*;
use gpui::{
    Context, Div, FontWeight, MouseButton, MouseDownEvent, PathBuilder, ScrollDelta, ScrollWheelEvent, SharedString,
    canvas, div, point, prelude::*, px, relative,
};
use relay_dsp::{BiquadCoeffs, EqBand, FilterKind};
use relay_proto::Effects;

const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
const G_RANGE: f32 = 18.0;
const PLOT_W: f32 = 520.0;
const PLOT_H: f32 = 150.0;

fn freq_to_x(f: f32) -> f32 {
    (f.max(F_MIN) / F_MIN).ln() / (F_MAX / F_MIN).ln()
}

fn x_to_freq(x: f32) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(x.clamp(0.0, 1.0))
}

fn gain_to_y(g: f32) -> f32 {
    0.5 - g / (2.0 * G_RANGE)
}

fn band_gain_display(b: &EqBand) -> f32 {
    match b.kind {
        FilterKind::Peak | FilterKind::LowShelf | FilterKind::HighShelf => b.gain_db,
        _ => 0.0,
    }
}

struct Range {
    min: f32,
    max: f32,
}

fn range(p: Param) -> Range {
    match p {
        Param::GateThreshold => Range { min: -80.0, max: 0.0 },
        Param::CompThreshold => Range { min: -60.0, max: 0.0 },
        Param::CompRatio => Range { min: 1.0, max: 20.0 },
        Param::CompMakeup => Range { min: 0.0, max: 24.0 },
        Param::LimiterCeiling => Range { min: -12.0, max: 0.0 },
        _ => Range { min: 0.0, max: 1.0 },
    }
}

fn get(fx: &Effects, p: Param) -> f32 {
    match p {
        Param::GateThreshold => fx.gate.threshold_db,
        Param::CompThreshold => fx.compressor.threshold_db,
        Param::CompRatio => fx.compressor.ratio,
        Param::CompMakeup => fx.compressor.makeup_db,
        Param::LimiterCeiling => fx.limiter.ceiling_db,
        _ => 0.0,
    }
}

fn set(fx: &mut Effects, p: Param, v: f32) {
    let r = range(p);
    let v = v.clamp(r.min, r.max);
    match p {
        Param::GateThreshold => fx.gate.threshold_db = v,
        Param::CompThreshold => fx.compressor.threshold_db = v,
        Param::CompRatio => fx.compressor.ratio = v,
        Param::CompMakeup => fx.compressor.makeup_db = v,
        Param::LimiterCeiling => fx.limiter.ceiling_db = v,
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
pub fn drag_param(
    app: &mut RelayApp,
    target: Target,
    param: Param,
    a: f32,
    b: f32,
    dx: f32,
    dy: f32,
    scale: (f32, f32),
) {
    let Some(mut fx) = app.effects_of(target) else { return };
    match param {
        Param::EqBand(i) => {
            if let Some(band) = fx.eq.get_mut(i) {
                band.freq = x_to_freq(a + dx / scale.0).round();
                if matches!(band.kind, FilterKind::Peak | FilterKind::LowShelf | FilterKind::HighShelf) {
                    band.gain_db = ((b - dy / scale.1 * 2.0 * G_RANGE) * 10.0).round() / 10.0;
                    band.gain_db = band.gain_db.clamp(-G_RANGE, G_RANGE);
                }
                band.enabled = true;
                fx.eq_enabled = true;
            }
        }
        Param::Gain | Param::EqQ(_) => return,
        p => {
            let r = range(p);
            set(&mut fx, p, a + dx / scale.0 * (r.max - r.min));
        }
    }
    app.set_effects(target, fx);
}

fn slider(
    app: &mut RelayApp,
    target: Target,
    param: Param,
    label: &'static str,
    unit: &'static str,
    enabled: bool,
    cx: &mut Context<RelayApp>,
) -> Div {
    let t = app.theme;
    let fx = app.effects_of(target).unwrap_or_default();
    let v = get(&fx, param);
    let r = range(param);
    let frac = ((v - r.min) / (r.max - r.min)).clamp(0.0, 1.0);
    let value: SharedString =
        if param == Param::CompRatio { format!("{v:.1}:1").into() } else { format!("{v:.1} {unit}").into() };
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .opacity(if enabled { 1.0 } else { 0.45 })
        .child(div().w(px(70.0)).text_size(px(11.0)).text_color(t.muted).child(label))
        .child(
            div()
                .id(SharedString::from(format!("sl-{label}")))
                .relative()
                .flex_1()
                .h(px(16.0))
                .cursor_ew_resize()
                .child(div().absolute().top(px(6.0)).left_0().right_0().h(px(4.0)).rounded(px(2.0)).bg(t.track))
                .child(
                    div().absolute().top(px(6.0)).left_0().w(relative(frac)).h(px(4.0)).rounded(px(2.0)).bg(t.accent),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .left(relative(frac))
                        .ml(px(-6.0))
                        .size(px(12.0))
                        .rounded_full()
                        .bg(t.text),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        let fx = this.effects_of(target).unwrap_or_default();
                        this.start_drag(target, param, ev.position, get(&fx, param), 0.0, (160.0, 1.0));
                        cx.notify();
                    }),
                ),
        )
        .child(div().w(px(64.0)).text_size(px(11.0)).text_color(t.text).flex().justify_end().child(value))
}

fn section(
    app: &RelayApp,
    title: &'static str,
    on: bool,
    id: &'static str,
    cx: &mut Context<RelayApp>,
    target: Target,
    toggle_fn: fn(&mut Effects),
) -> Div {
    let t = app.theme;
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(t.text).child(title))
        .child(toggle(id, on, &t).on_click(cx.listener(move |this, _, _, cx| {
            if let Some(mut fx) = this.effects_of(target) {
                toggle_fn(&mut fx);
                this.set_effects(target, fx);
                cx.notify();
            }
        })))
}

pub fn panel(app: &mut RelayApp, target: Target, cx: &mut Context<RelayApp>) -> Div {
    let t = app.theme;
    let Some(fx) = app.effects_of(target) else { return div() };
    let name = match target {
        Target::Strip(id) => app.strip(id).map(|s| s.name.clone()),
        Target::Bus(id) => app.bus(id).map(|b| b.name.clone()),
    }
    .unwrap_or_default();
    let is_strip = matches!(target, Target::Strip(_));

    // EQ plot.
    let bands = fx.eq.clone();
    let eq_on = fx.eq_enabled;
    let curve_color = if eq_on { t.accent } else { t.faint };
    let grid = t.border;
    let plot = canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let w = f32::from(bounds.size.width);
            let h = f32::from(bounds.size.height);
            let ox = bounds.origin.x;
            let oy = bounds.origin.y;
            // Grid: 0 dB line and decades.
            window.paint_quad(gpui::fill(
                gpui::Bounds::new(point(ox, oy + px(h * 0.5)), gpui::size(px(w), px(1.0))),
                grid,
            ));
            for f in [100.0f32, 1000.0, 10000.0] {
                let x = freq_to_x(f) * w;
                window.paint_quad(gpui::fill(
                    gpui::Bounds::new(point(ox + px(x), oy), gpui::size(px(1.0), px(h))),
                    with_alpha(grid, 0.6),
                ));
            }
            let coeffs: Vec<BiquadCoeffs> =
                bands.iter().filter(|b| !b.is_neutral()).map(|b| b.coeffs(48000.0)).collect();
            let mut pb = PathBuilder::stroke(px(2.0));
            let steps = 160;
            for i in 0..=steps {
                let x = i as f32 / steps as f32;
                let f = x_to_freq(x);
                let g: f32 = if eq_on { coeffs.iter().map(|c| c.magnitude_db(f, 48000.0)).sum() } else { 0.0 };
                let y = gain_to_y(g.clamp(-G_RANGE, G_RANGE)) * h;
                let p = point(ox + px(x * w), oy + px(y));
                if i == 0 {
                    pb.move_to(p);
                } else {
                    pb.line_to(p);
                }
            }
            if let Ok(path) = pb.build() {
                window.paint_path(path, curve_color);
            }
        },
    )
    .size_full();

    let handles: Vec<_> = fx
        .eq
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let x = freq_to_x(b.freq);
            let y = gain_to_y(band_gain_display(b));
            let active = b.enabled && eq_on;
            div()
                .id(("eqh", i))
                .absolute()
                .left(relative(x))
                .top(relative(y))
                .ml(px(-7.0))
                .mt(px(-7.0))
                .size(px(14.0))
                .rounded_full()
                .border_2()
                .border_color(if active { t.accent } else { t.faint })
                .bg(t.card)
                .cursor_move()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        let Some(fx) = this.effects_of(target) else { return };
                        let Some(b) = fx.eq.get(i) else { return };
                        if ev.click_count >= 2 {
                            let mut fx = fx.clone();
                            fx.eq[i].enabled = !fx.eq[i].enabled;
                            this.set_effects(target, fx);
                        } else {
                            this.start_drag(
                                target,
                                Param::EqBand(i),
                                ev.position,
                                freq_to_x(b.freq),
                                b.gain_db,
                                (PLOT_W, PLOT_H),
                            );
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                    let dy = match ev.delta {
                        ScrollDelta::Lines(l) => l.y,
                        ScrollDelta::Pixels(p) => f32::from(p.y) / 20.0,
                    };
                    if let Some(mut fx) = this.effects_of(target)
                        && let Some(b) = fx.eq.get_mut(i)
                    {
                        b.q = (b.q * (1.0 + dy * 0.1)).clamp(0.1, 18.0);
                        this.set_effects(target, fx);
                        cx.notify();
                    }
                }))
        })
        .collect();

    let selected_band_text: SharedString = fx
        .eq
        .iter()
        .filter(|b| b.enabled)
        .map(|b| {
            let f = if b.freq >= 1000.0 { format!("{:.1}k", b.freq / 1000.0) } else { format!("{:.0}", b.freq) };
            match b.kind {
                FilterKind::HighPass => format!("HP {f}"),
                FilterKind::LowPass => format!("LP {f}"),
                _ => format!("{f} {:+.1}", b.gain_db),
            }
        })
        .collect::<Vec<_>>()
        .join("  ·  ")
        .into();

    let gate_on = fx.gate.enabled;
    let comp_on = fx.compressor.enabled;
    let lim_on = fx.limiter.enabled;
    let right = div()
        .w(px(330.0))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .when(is_strip, |d| {
            d.child(section(app, "Noise suppression", fx.denoise, "fx-dn", cx, target, |fx| fx.denoise = !fx.denoise))
                .child(section(app, "Gate", gate_on, "fx-gate", cx, target, |fx| fx.gate.enabled = !fx.gate.enabled))
                .child(slider(app, target, Param::GateThreshold, "Threshold", "dB", gate_on, cx))
        })
        .child(section(app, "Compressor", comp_on, "fx-comp", cx, target, |fx| {
            fx.compressor.enabled = !fx.compressor.enabled
        }))
        .child(slider(app, target, Param::CompThreshold, "Threshold", "dB", comp_on, cx))
        .child(slider(app, target, Param::CompRatio, "Ratio", "", comp_on, cx))
        .child(slider(app, target, Param::CompMakeup, "Makeup", "dB", comp_on, cx))
        .child(section(app, "Limiter", lim_on, "fx-lim", cx, target, |fx| fx.limiter.enabled = !fx.limiter.enabled))
        .child(slider(app, target, Param::LimiterCeiling, "Ceiling", "dB", lim_on, cx));

    div()
        .h(px(250.0))
        .flex_shrink_0()
        .border_t_1()
        .border_color(t.border)
        .bg(t.panel)
        .p(px(14.0))
        .flex()
        .gap(px(20.0))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).child(name))
                                .child(section(app, "EQ", eq_on, "fx-eq", cx, target, |fx| {
                                    fx.eq_enabled = !fx.eq_enabled
                                })),
                        )
                        .child(button("fx-close", "Done", &t).on_click(cx.listener(|this, _, _, cx| {
                            this.fx = None;
                            cx.notify();
                        }))),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .rounded(px(8.0))
                        .bg(t.track)
                        .overflow_hidden()
                        .child(plot)
                        .children(handles),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(t.muted)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(selected_band_text),
                ),
        )
        .child(right)
}
