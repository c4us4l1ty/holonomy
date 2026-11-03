//! WordCraft's light/dark design tokens, copied from `ui-egui/src/theme.rs`.
//! Modified for Holonomy: packed ARGB replaces egui::Color32. Values are unchanged.
//! MIT; see `third_party/wordcraft/LICENSE-MIT`.

/// WordCraft's app colour and its darker ink for text on light backgrounds.
pub const APP_COLOR: u32 = 0xFF3B_5BDB;
pub const APP_INK: u32 = 0xFF2B_47B5;

/// The subset of WordCraft's `Tokens` used by the native desktop chrome.
/// Unlike the old approximation, controls, tabs, panels and status have their own planes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub dark: bool,
    pub title_bar: u32,
    pub ribbon: u32,
    pub tab_strip: u32,
    pub canvas: u32,
    pub panel: u32,
    pub border: u32,
    pub border_strong: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_disabled: u32,
    pub icon: u32,
    pub hover: u32,
    pub pressed: u32,
    pub checked: u32,
    pub accent: u32,
    pub accent_text: u32,
    pub on_accent: u32,
    pub input: u32,
    pub input_border: u32,
    pub group_label: u32,
    pub ruler: u32,
    pub ruler_margin: u32,
    pub ruler_tick: u32,
    pub status_bar: u32,
    pub menu: u32,
}

impl Tokens {
    pub const fn light() -> Self {
        Self {
            dark: false,
            title_bar: 0xFFF0F0F0,
            ribbon: 0xFFF9F9F9,
            tab_strip: 0xFFF0F0F0,
            canvas: 0xFFE6E6E6,
            panel: 0xFFFAFAFA,
            border: 0xFFE1E1E1,
            border_strong: 0xFFC8C8C8,
            text: 0xFF242424,
            text_dim: 0xFF616161,
            text_disabled: 0xFFA8A8A8,
            icon: 0xFF424242,
            hover: 0xFFE6E6E6,
            pressed: 0xFFD6D6D6,
            checked: 0xFFDDE3F8,
            accent: APP_COLOR,
            accent_text: APP_INK,
            on_accent: 0xFFFFFFFF,
            input: 0xFFFFFFFF,
            input_border: 0xFF8A8A8A,
            group_label: 0xFF707070,
            ruler: 0xFFFFFFFF,
            ruler_margin: 0xFFD4D4D4,
            ruler_tick: 0xFF707070,
            status_bar: 0xFFF0F0F0,
            menu: 0xFFFFFFFF,
        }
    }

    pub const fn dark() -> Self {
        Self {
            dark: true,
            title_bar: 0xFF1F1F1F,
            ribbon: 0xFF292929,
            tab_strip: 0xFF1F1F1F,
            canvas: 0xFF141414,
            panel: 0xFF262626,
            border: 0xFF3A3A3A,
            border_strong: 0xFF555555,
            text: 0xFFEEEEEE,
            text_dim: 0xFFC8C8C8,
            text_disabled: 0xFF6A6A6A,
            icon: 0xFFDDDDDD,
            hover: 0xFF3D3D3D,
            pressed: 0xFF4A4A4A,
            checked: 0xFF2E3A66,
            accent: 0xFF7A93F0,
            accent_text: 0xFF9CB0F5,
            on_accent: 0xFF11162E,
            input: 0xFF1E1E1E,
            input_border: 0xFF7A7A7A,
            group_label: 0xFFA0A0A0,
            ruler: 0xFF454545,
            ruler_margin: 0xFF2E2E2E,
            ruler_tick: 0xFFB4B4B4,
            status_bar: 0xFF1F1F1F,
            menu: 0xFF2B2B2B,
        }
    }

    pub const fn get(dark: bool) -> Self {
        if dark { Self::dark() } else { Self::light() }
    }
}

/// Compatibility names used by the original Holonomy ribbon API.
macro_rules! compatibility {
    ($module:ident, $constructor:ident) => {
        pub mod $module {
            const T: super::Tokens = super::Tokens::$constructor();
            pub const TITLE: u32 = T.title_bar;
            pub const RIBBON: u32 = T.ribbon;
            pub const CANVAS: u32 = T.canvas;
            pub const BORDER: u32 = T.border;
            pub const TEXT: u32 = T.text;
            pub const DIM: u32 = T.text_dim;
            pub const DISABLED: u32 = T.text_disabled;
            pub const HOVER: u32 = T.hover;
            pub const PRESSED: u32 = T.pressed;
            pub const CHECKED: u32 = T.checked;
            pub const ACCENT: u32 = T.accent;
            pub const ON_ACCENT: u32 = T.on_accent;
            pub const INPUT: u32 = T.input;
            pub const INPUT_BORDER: u32 = T.input_border;
            pub const GROUP: u32 = T.group_label;
            pub const RULER: u32 = T.ruler;
            pub const RULER_MARGIN: u32 = T.ruler_margin;
        }
    };
}
compatibility!(light, light);
compatibility!(dark, dark);
