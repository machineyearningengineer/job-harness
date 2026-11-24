//! Zeron Dark-inspired high-contrast frosted palette for GPUI.
//! Deep 95%+ charcoal glass keeps subtle macOS vibrancy without text bleed-through.
use gpui::{Pixels, Rgba, px, rgba};

/// Standardized architectural corner radius scale (tighter, more sculpted than generic 6/8/12/16px defaults).
#[allow(dead_code)]
pub fn r_xs() -> Pixels {
    px(3.5)
}
pub fn r_sm() -> Pixels {
    px(5.0)
}
pub fn r_md() -> Pixels {
    px(8.0)
}
pub fn r_lg() -> Pixels {
    px(11.0)
}

pub fn window_glass() -> Rgba {
    rgba(0x0b0b0eff)
}
pub fn panel_glass() -> Rgba {
    rgba(0x101015ff)
}
pub fn popover_glass() -> Rgba {
    rgba(0x15151cff)
}
pub fn dock_glass() -> Rgba {
    rgba(0x16161eff)
}
pub fn row_hover() -> Rgba {
    rgba(0xffffff0e)
}
pub fn surface_subtle() -> Rgba {
    rgba(0xffffff08)
}
pub fn surface_active() -> Rgba {
    rgba(0xffffff16)
}
pub fn hairline() -> Rgba {
    rgba(0xffffff12)
}
pub fn hairline_strong() -> Rgba {
    rgba(0xffffff24)
}
pub fn drop_highlight() -> Rgba {
    rgba(0x8b7cf616)
}

pub fn text_primary() -> Rgba {
    rgba(0xe8e8eaff)
}
pub fn text_secondary() -> Rgba {
    rgba(0xa9a9aeff)
}
pub fn text_muted() -> Rgba {
    rgba(0x76767dff)
}

pub fn amber_wash() -> Rgba {
    rgba(0xfacc1512)
}
pub fn amber_accent() -> Rgba {
    rgba(0xfacc15ff)
}
pub fn amber_text() -> Rgba {
    rgba(0xfde047ff)
}

pub fn rose_text() -> Rgba {
    rgba(0xf87171ff)
}
pub fn rose_wash() -> Rgba {
    rgba(0xf8717118)
}

pub fn emerald() -> Rgba {
    rgba(0x34d399ff)
}
pub fn lavender() -> Rgba {
    rgba(0xa78bfaff)
}
pub fn lavender_wash() -> Rgba {
    rgba(0x8b7cf61c)
}
pub fn lavender_border() -> Rgba {
    rgba(0x8b7cf648)
}
pub fn white_solid() -> Rgba {
    rgba(0xf4f4f6ff)
}
pub fn ink_dark() -> Rgba {
    rgba(0x0b0b0eff)
}
