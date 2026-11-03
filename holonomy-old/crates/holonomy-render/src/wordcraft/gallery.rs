//! The Home tab's Styles gallery, adapted from WordCraft `ui-egui/src/previews.rs`.
//! Upstream's six-tile / three-tile responsive gallery and `AaBbCcDd` specimen are
//! retained. Holonomy previews its six real persisted paragraph styles with atlas
//! faces/colours instead of introducing an incompatible document/style engine.
//! MIT; see `third_party/wordcraft/LICENSE-MIT`.

use crate::menus::Action;
use crate::tree::Style;

pub const STYLE_TILES: usize = 6;
pub const STYLE_TILES_COMPACT: usize = 3;
/// Upstream tiles are 76 points wide at its 25-pixel ribbon pitch.
pub const TILE_W: u32 = 76;
pub const SAMPLE: &str = "AaBbCcDd";

/// Action, label, atlas face and colour. Every tile applies an existing style;
/// there are deliberately no decorative styles that the editor cannot persist.
pub const TILES: &[(Action, &str, Style, u32)] = &[
    (Action::SetStyle(0), "Normal", Style::REGULAR, 0xFF242424),
    (Action::SetStyle(1), "Heading 1", Style::BOLD, 0xFF1A5C75),
    (Action::SetStyle(5), "Heading 2", Style::BOLD, 0xFF1A5C75),
    (Action::SetStyle(3), "Title", Style::BOLD, 0xFF242424),
    (Action::SetStyle(2), "Quote", Style::ITALIC, 0xFF242424),
    (Action::SetStyle(4), "Subtitle", Style::ITALIC, 0xFF595959),
];

pub fn tile_width(cell_h: u32) -> u32 {
    (TILE_W.saturating_mul(cell_h.max(1)) / 25).max(68)
}

pub fn specimen(action: Action) -> Option<(&'static str, Style, u32)> {
    TILES
        .iter()
        .find(|(a, ..)| *a == action)
        .map(|(_, _, face, colour)| (SAMPLE, *face, *colour))
}
