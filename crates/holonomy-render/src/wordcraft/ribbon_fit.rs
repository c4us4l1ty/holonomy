//! Ribbon groups that shrink to fit the window, from WordCraft's `ribbon_fit.rs`.
//! Modified for Holonomy: measured integer pixels replace f32 points, and the planner
//! is pure (no egui Context or frame-local storage). The ordering and give-back algorithm
//! are retained, including the upstream regression cases below.
//! MIT; see `third_party/wordcraft/LICENSE-MIT`.

pub const FULL: u8 = 0;
/// Small buttons lose labels; the Styles gallery shows fewer tiles.
pub const COMPACT: u8 = 1;
/// A single measured button opens the complete group.
pub const COLLAPSED: u8 = 2;
const COLLAPSED_W: u32 = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupWidths {
    pub title: &'static str,
    pub widths: [Option<u32>; 3],
}

/// Compact from the right, then collapse from the right. Give room back from the left
/// when a later, wider group (notably Styles) releases more space than was needed.
pub fn plan(groups: &[GroupWidths], avail: u32) -> Vec<u8> {
    let full = |g: &GroupWidths| g.widths[0].unwrap_or(0);
    let width = |g: &GroupWidths, level: u8| match level {
        FULL => full(g),
        COMPACT => g.widths[1].unwrap_or(full(g)),
        _ => g.widths[2].unwrap_or(COLLAPSED_W.min(full(g))),
    };
    let mut levels = vec![FULL; groups.len()];
    let mut total: u32 = groups.iter().map(full).fold(0, u32::saturating_add);
    for target in [COMPACT, COLLAPSED] {
        for i in (0..groups.len()).rev() {
            if total <= avail {
                break;
            }
            let (Some(g), Some(level)) = (groups.get(i), levels.get_mut(i)) else {
                continue;
            };
            total = total
                .saturating_sub(width(g, *level))
                .saturating_add(width(g, target));
            *level = target;
        }
    }
    for (g, level) in groups.iter().zip(levels.iter_mut()) {
        while *level > FULL {
            let wider = *level - 1;
            let next = total
                .saturating_sub(width(g, *level))
                .saturating_add(width(g, wider));
            if next > avail {
                break;
            }
            total = next;
            *level = wider;
        }
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(
        title: &'static str,
        full: u32,
        compact: Option<u32>,
        collapsed: Option<u32>,
    ) -> GroupWidths {
        GroupWidths {
            title,
            widths: [Some(full), compact, collapsed],
        }
    }

    #[test]
    fn everything_full_when_it_fits() {
        let gs = [
            g("A", 100, Some(80), Some(60)),
            g("B", 100, Some(80), Some(60)),
        ];
        assert_eq!(plan(&gs, 200), vec![FULL, FULL]);
    }

    #[test]
    fn rightmost_groups_compact_first_then_collapse() {
        let gs = [
            g("A", 100, Some(80), Some(60)),
            g("B", 100, Some(80), Some(60)),
            g("C", 100, Some(80), Some(60)),
        ];
        assert_eq!(plan(&gs, 285), vec![FULL, FULL, COMPACT]);
        assert_eq!(plan(&gs, 230), vec![COMPACT, COMPACT, COLLAPSED]);
        assert!(plan(&gs, 10).iter().all(|l| *l == COLLAPSED));
    }

    #[test]
    fn groups_that_still_fit_are_given_back() {
        let gs = [
            g("Font", 300, Some(250), Some(60)),
            g("Styles", 500, Some(500), Some(60)),
            g("Voice", 60, Some(60), Some(60)),
            g("Editor", 60, Some(60), Some(60)),
        ];
        assert_eq!(plan(&gs, 800), vec![FULL, COLLAPSED, FULL, FULL]);
    }

    #[test]
    fn trimmed_gallery_keeps_styles_open() {
        let gs = [
            g("Clipboard", 130, Some(70), Some(60)),
            g("Font", 330, Some(290), Some(60)),
            g("Paragraph", 300, Some(280), Some(60)),
            g("Styles", 520, Some(290), Some(60)),
            g("Editing", 80, Some(40), Some(60)),
            g("Voice", 60, Some(60), Some(60)),
        ];
        assert_eq!(plan(&gs, 1250), vec![FULL, FULL, FULL, COMPACT, FULL, FULL]);
        let mut wide = gs.clone();
        wide[3].widths[1] = Some(520);
        assert_eq!(plan(&wide, 1250)[3], COLLAPSED);
    }

    #[test]
    fn same_titles_keep_their_own_levels() {
        let gs = [
            g("New Group", 300, Some(300), Some(60)),
            g("New Group", 100, Some(100), Some(60)),
        ];
        assert_eq!(plan(&gs, 200), vec![COLLAPSED, FULL]);
    }

    #[test]
    fn unmeasured_widths_are_estimated() {
        let gs = [g("A", 200, None, None), g("B", 200, None, None)];
        assert_eq!(plan(&gs, 300), vec![FULL, COLLAPSED]);
    }

    #[test]
    fn integer_pixel_plans_never_increase_width_or_overflow() {
        let gs = [
            g("A", u32::MAX, Some(100), Some(60)),
            g("B", 300, Some(200), Some(60)),
        ];
        for avail in [0, 1, 59, 60, 119, 120, 300, u32::MAX] {
            let levels = plan(&gs, avail);
            assert_eq!(levels.len(), gs.len());
            assert!(levels.iter().all(|l| *l <= COLLAPSED));
        }
    }
}
