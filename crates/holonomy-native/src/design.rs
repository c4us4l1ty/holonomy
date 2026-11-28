//! Quiet MD-inspired neutral palette and Vega geometry, adapted from s1dashu/quiet-md
//! a4b89029c29d5e0d4aaba660ee7930b9d0c4942f (MIT, copyright 2026 s1dashu).
//! No storage/network code is imported. See LICENSES/quiet-md-MIT.txt and THIRD_PARTY.md.

pub const CONTROL_HEIGHT: f32 = 36.;
pub const TOOL_HEIGHT: f32 = 28.;
pub const FORM_WIDTH: f32 = 440.;
pub const DOCUMENT_WIDTH: f32 = 860.;

#[derive(Clone, Copy)]
pub struct Palette {
    pub page: u32,
    pub rail: u32,
    pub ink: u32,
    pub muted: u32,
    pub border: u32,
    pub selected: u32,
    pub primary: u32,
    pub on_primary: u32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                page: 0x1e1e1e,
                rail: 0x252827,
                ink: 0xfafafa,
                muted: 0xa3a3a3,
                border: 0x333333,
                selected: 0x343735,
                primary: 0xe5e5e5,
                on_primary: 0x171717,
            }
        } else {
            Self {
                page: 0xffffff,
                rail: 0xf5f5f5,
                ink: 0x171717,
                muted: 0x737373,
                border: 0xe5e5e5,
                selected: 0xebebeb,
                primary: 0x171717,
                on_primary: 0xfafafa,
            }
        }
    }
}

/// Narrow windows keep the document usable instead of shrinking it behind a fixed rail.
pub fn sidebar_width(width: f32) -> f32 {
    if width >= 1000. {
        256.
    } else if width >= 760. {
        184.
    } else {
        0.
    }
}

/// An explicit rail toggle on compact windows must work, while retaining writing space.
pub fn visible_sidebar_width(width: f32, hidden: bool, forced: bool) -> f32 {
    if hidden {
        0.
    } else if forced && sidebar_width(width) == 0. {
        (width * 0.4).clamp(0., 224.)
    } else {
        sidebar_width(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_sidebar_can_be_explicitly_opened_and_closed() {
        assert_eq!(visible_sidebar_width(480., false, false), 0.);
        assert_eq!(visible_sidebar_width(480., false, true), 192.);
        assert_eq!(visible_sidebar_width(1200., true, true), 0.);
        assert_eq!(visible_sidebar_width(1200., false, false), 256.);
    }

    #[test]
    fn narrow_layout_prioritizes_the_writing_surface() {
        assert_eq!(sidebar_width(480.), 0.);
        assert_eq!(sidebar_width(800.), 184.);
        assert_eq!(sidebar_width(1200.), 256.);
        assert!(CONTROL_HEIGHT > TOOL_HEIGHT);
        assert!(FORM_WIDTH < DOCUMENT_WIDTH);
        for dark in [false, true] {
            let palette = Palette::new(dark);
            assert_ne!(palette.ink, palette.page);
            assert_ne!(palette.primary, palette.on_primary);
        }
    }
}
