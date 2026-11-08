use gpui::{Hsla, Rgba, rgb, rgba};

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Rgba,
    pub panel: Rgba,
    pub card: Rgba,
    pub card_hover: Rgba,
    pub raised: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub good: Rgba,
    pub warn: Rgba,
    pub bad: Rgba,
    pub meter_lo: Rgba,
    pub meter_mid: Rgba,
    pub meter_hi: Rgba,
    pub track: Rgba,
    pub overlay: Rgba,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: rgb(0x0f1012),
            panel: rgb(0x151619),
            card: rgb(0x1b1c20),
            card_hover: rgb(0x212328),
            raised: rgb(0x282a30),
            border: rgb(0x2a2c32),
            text: rgb(0xeceef2),
            muted: rgb(0x9a9ea8),
            faint: rgb(0x5d616b),
            accent: rgb(0x7c8cff),
            accent_text: rgb(0x0f1012),
            good: rgb(0x3ccf8e),
            warn: rgb(0xf2b544),
            bad: rgb(0xf0605d),
            meter_lo: rgb(0x3ccf8e),
            meter_mid: rgb(0xd6d45a),
            meter_hi: rgb(0xf0605d),
            track: rgb(0x0b0c0e),
            overlay: rgba(0x00000099),
        }
    }

    pub fn light() -> Self {
        Self {
            bg: rgb(0xf4f5f7),
            panel: rgb(0xeceef1),
            card: rgb(0xffffff),
            card_hover: rgb(0xf7f8fa),
            raised: rgb(0xe6e8ec),
            border: rgb(0xdcdfe4),
            text: rgb(0x16181c),
            muted: rgb(0x60646d),
            faint: rgb(0xa2a6ae),
            accent: rgb(0x4e5ff0),
            accent_text: rgb(0xffffff),
            good: rgb(0x1fa36b),
            warn: rgb(0xc98a12),
            bad: rgb(0xd8423f),
            meter_lo: rgb(0x2bb47a),
            meter_mid: rgb(0xc2b02c),
            meter_hi: rgb(0xd8423f),
            track: rgb(0xe4e6ea),
            overlay: rgba(0x00000055),
        }
    }
}

pub fn with_alpha(c: Rgba, a: f32) -> Hsla {
    let mut h: Hsla = c.into();
    h.a = a;
    h
}

pub fn ui_font() -> &'static str {
    if cfg!(target_os = "macos") { ".SystemUIFont" } else { "Segoe UI" }
}
