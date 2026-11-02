//! Document library for the native desktop. Geometry is shared by painting and hit testing.
use crate::{DamageRect, Label, Layout, Node, Rect, RoundRect, Style, SurfaceTree};

const WHITE: u32 = 0xFFFF_FFFF;
const INK: u32 = 0xFF20_2124;
const DIM: u32 = 0xFF5F_6368;
const BLUE: u32 = 0xFF1A73_E8;
const PALE: u32 = 0xFFF1_F3F8;
const BORDER: u32 = 0xFFDA_DCE0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// Titles in vault order; document contents never enter the library renderer.
    pub titles: Vec<String>,
    pub query: String,
    pub page: usize,
    pub hover: Hit,
    pub notice: Option<String>,
}
impl View {
    pub fn new(titles: Vec<String>) -> Self {
        Self {
            titles,
            query: String::new(),
            page: 0,
            hover: Hit::None,
            notice: None,
        }
    }
    pub fn matches(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.titles
            .iter()
            .enumerate()
            .filter_map(|(i, title)| title.to_lowercase().contains(&query).then_some(i))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    None,
    New,
    Sample,
    Open,
    Lock,
    Document(usize),
    Previous,
    Next,
}

/// Library layout. Only complete cards are exposed as hit targets.
pub struct Geometry {
    pub new: DamageRect,
    pub sample: DamageRect,
    pub open: DamageRect,
    pub lock: DamageRect,
    pub search: DamageRect,
    pub previous: DamageRect,
    pub next: DamageRect,
    pub cards: Vec<(usize, DamageRect)>,
    pub page: usize,
    pub pages: usize,
    pub content_x: u32,
    pub list_y: u32,
}

pub fn geometry(l: &Layout, view: &View) -> Geometry {
    let margin = if l.width >= 760 {
        ((l.width - 720) / 2).min(160)
    } else {
        24.min(l.width / 8)
    };
    let room = l.width.saturating_sub(margin * 2);
    let template_w = 170.min(room.saturating_sub(8));
    let template_h = (if l.height < 600 || (l.width < 700 && l.height < 650) {
        90
    } else {
        150
    })
    .min(l.height.saturating_sub(130));
    let new = DamageRect::new(margin, 126.min(l.height), template_w, template_h);
    let sample_x = margin.saturating_add(template_w + 24);
    let sample = if sample_x + template_w <= l.width.saturating_sub(margin) {
        DamageRect::new(sample_x, new.y, template_w, template_h)
    } else {
        DamageRect::EMPTY
    };
    let open_w = if room >= 300 { 132 } else { 80 };
    let open = if room >= 170 && l.height >= 52 {
        DamageRect::new(l.width.saturating_sub(margin + open_w), 14, open_w, 40)
    } else {
        DamageRect::EMPTY
    };
    let lock_w = if room >= 300 { 80 } else { 64 };
    let lock = if !open.is_empty() {
        DamageRect::new(open.x.saturating_sub(lock_w + 10), 14, lock_w, 40)
    } else {
        DamageRect::EMPTY
    };
    let search_x = margin.saturating_add(180);
    let search_end = if lock.is_empty() { open.x } else { lock.x };
    let search_w = search_end.saturating_sub(search_x + 20).min(420);
    let header_search = search_w >= 110 && l.height >= 58;
    let fallback_search =
        !header_search && room >= 80 && new.bottom().saturating_add(70) <= l.height;
    let search = if header_search {
        DamageRect::new(search_x, 14, search_w, 42)
    } else if fallback_search {
        DamageRect::new(margin, new.bottom() + 34, room, 32)
    } else {
        DamageRect::EMPTY
    };
    let list_y = new
        .bottom()
        .saturating_add(if fallback_search { 104 } else { 62 });
    let card_w = 190.min(room);
    let gap = 20;
    let cols = ((room + gap) / (card_w + gap).max(1)).max(1) as usize;
    let available = l.height.saturating_sub(list_y + 55);
    let rows = if available < 188 {
        0
    } else {
        (1 + (available - 188) / 208) as usize
    };
    let capacity = cols.saturating_mul(rows);
    let matches = view.matches();
    // Zero capacity means no cards fit; the query still works after a resize.
    let pages = if capacity == 0 {
        1
    } else {
        matches.len().max(1).div_ceil(capacity)
    };
    let page = view.page.min(pages - 1);
    let mut cards = Vec::new();
    if capacity > 0 && card_w > 0 {
        for (slot, &index) in matches
            .iter()
            .skip(page * capacity)
            .take(capacity)
            .enumerate()
        {
            let x = margin + (slot % cols) as u32 * (card_w + gap);
            let y = list_y + (slot / cols) as u32 * 208;
            cards.push((index, DamageRect::new(x, y, card_w, 188)));
        }
    }
    let py = l.height.saturating_sub(42);
    let previous = if pages > 1 && page > 0 {
        DamageRect::new(margin, py, 96.min(room), 32)
    } else {
        DamageRect::EMPTY
    };
    let next = if pages > 1 && page + 1 < pages {
        DamageRect::new(
            margin.saturating_add(110).min(l.width),
            py,
            96.min(room.saturating_sub(110)),
            32,
        )
    } else {
        DamageRect::EMPTY
    };
    Geometry {
        new,
        sample,
        open,
        lock,
        search,
        previous,
        next,
        cards,
        page,
        pages,
        content_x: margin,
        list_y,
    }
}

pub fn hit(l: &Layout, view: &View, x: i32, y: i32) -> Hit {
    let g = geometry(l, view);
    for (r, h) in [
        (g.new, Hit::New),
        (g.sample, Hit::Sample),
        (g.open, Hit::Open),
        (g.lock, Hit::Lock),
        (g.previous, Hit::Previous),
        (g.next, Hit::Next),
    ] {
        if r.contains(x, y) {
            return h;
        }
    }
    for (i, r) in g.cards {
        if r.contains(x, y) {
            return Hit::Document(i);
        }
    }
    Hit::None
}

fn rect(t: &mut SurfaceTree, r: DamageRect, c: u32, radius: u32) {
    if !r.is_empty() {
        t.before
            .push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(
                r.x as i32, r.y as i32, r.width, r.height, radius, c,
            ))));
    }
}
fn label(
    t: &mut SurfaceTree,
    l: &Layout,
    x: u32,
    y: u32,
    text: &str,
    bold: bool,
    colour: u32,
    max_width: u32,
) {
    if x >= l.width || y.saturating_add(l.cell_h) > l.height {
        return;
    }
    let max = (max_width.min(l.width - x) / l.cell_w.max(1)) as usize;
    let s: String = text.chars().take(max).collect();
    t.before.push(SurfaceTree::leaf(Node::Label(Label::new(
        x as i32,
        y as i32,
        &s,
        if bold { Style::BOLD } else { Style::REGULAR },
        0,
        colour,
    ))));
}
fn inner(r: DamageRect, n: u32) -> DamageRect {
    DamageRect::new(
        r.x.saturating_add(n),
        r.y.saturating_add(n),
        r.width.saturating_sub(n * 2),
        r.height.saturating_sub(n * 2),
    )
}

pub fn tree(l: &Layout, view: &View) -> SurfaceTree {
    let g = geometry(l, view);
    let mut t = SurfaceTree::group();
    t.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(
        0, 0, l.width, l.height, WHITE,
    ))));
    rect(
        &mut t,
        DamageRect::new(
            0,
            72.min(l.height),
            l.width,
            g.new
                .bottom()
                .saturating_add(45)
                .min(l.height)
                .saturating_sub(72),
        ),
        PALE,
        0,
    );
    rect(
        &mut t,
        DamageRect::new(
            g.content_x,
            19,
            30.min(l.width.saturating_sub(g.content_x)),
            36.min(l.height.saturating_sub(19)),
        ),
        BLUE,
        5,
    );
    label(&mut t, l, g.content_x + 8, 26, "H", true, WHITE, 20);
    let title_end = if !g.lock.is_empty() {
        g.lock.x
    } else if !g.open.is_empty() {
        g.open.x
    } else {
        l.width
    };
    label(
        &mut t,
        l,
        g.content_x + 42,
        26,
        "Holonomy",
        true,
        INK,
        title_end.saturating_sub(g.content_x + 50).min(150),
    );
    if !g.search.is_empty() {
        rect(&mut t, g.search, PALE, 10);
        let available = g.search.width.saturating_sub(24);
        let max_chars = (available / l.cell_w.max(1)) as usize;
        let text = if view.query.is_empty() {
            "Type to search documents".to_string()
        } else {
            let skip = view.query.chars().count().saturating_sub(max_chars);
            view.query.chars().skip(skip).collect()
        };
        label(
            &mut t,
            l,
            g.search.x + 12,
            g.search.y + (g.search.height.saturating_sub(l.cell_h)) / 2,
            &text,
            false,
            DIM,
            available,
        );
    }
    rect(
        &mut t,
        g.open,
        if view.hover == Hit::Open {
            0xFFD3_E3FD
        } else {
            PALE
        },
        10,
    );
    if !g.open.is_empty() {
        label(
            &mut t,
            l,
            g.open.x + 12,
            g.open.y + 11,
            "Open...",
            true,
            BLUE,
            g.open.width.saturating_sub(20),
        );
    }
    rect(
        &mut t,
        g.lock,
        if view.hover == Hit::Lock {
            0xFFD3_E3FD
        } else {
            PALE
        },
        10,
    );
    if !g.lock.is_empty() {
        label(
            &mut t,
            l,
            g.lock.x + 12,
            g.lock.y + 11,
            "Lock",
            true,
            BLUE,
            g.lock.width.saturating_sub(20),
        );
    }
    label(
        &mut t,
        l,
        g.content_x,
        92,
        "Start a new document",
        true,
        INK,
        l.width.saturating_sub(g.content_x),
    );
    for (r, h, name) in [
        (g.new, Hit::New, "Blank document"),
        (g.sample, Hit::Sample, "Studio handbook"),
    ] {
        if r.is_empty() {
            continue;
        }
        rect(&mut t, r, if view.hover == h { BLUE } else { BORDER }, 8);
        rect(&mut t, inner(r, 1), WHITE, 7);
        if h == Hit::New && r.width >= 46 && r.height >= 46 {
            let cx = r.x + r.width / 2;
            let cy = r.y + r.height / 2;
            for (x, y, w, h, colour) in [
                (cx - 3, cy - 23, 6, 23, BLUE),
                (cx - 3, cy, 6, 23, 0xFF34_A853),
                (cx - 23, cy - 3, 23, 6, 0xFFEA_4335),
                (cx, cy - 3, 23, 6, 0xFFFB_BC04),
            ] {
                t.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(
                    x as i32, y as i32, w, h, colour,
                ))));
            }
        } else if r.width >= 90 && r.height >= 65 {
            let page = DamageRect::new(
                r.x + r.width / 2 - 35,
                r.y + 12,
                70,
                r.height.saturating_sub(24),
            );
            rect(&mut t, page, PALE, 2);
            for i in 0..5 {
                let y = page.y + 12 + i * 10;
                if y + 2 <= page.bottom() {
                    rect(
                        &mut t,
                        DamageRect::new(page.x + 10, y, 48 - (i % 2) * 12, 2),
                        BORDER,
                        0,
                    );
                }
            }
        }
        label(&mut t, l, r.x, r.bottom() + 10, name, true, INK, r.width);
    }
    let heading_y = g.list_y.saturating_sub(32);
    label(
        &mut t,
        l,
        g.content_x,
        heading_y,
        "Your documents",
        true,
        INK,
        l.width.saturating_sub(g.content_x),
    );
    if g.cards.is_empty() {
        label(
            &mut t,
            l,
            g.content_x,
            g.list_y,
            if view.titles.is_empty() {
                "No saved documents yet."
            } else if view.matches().is_empty() {
                "No documents match your search."
            } else {
                "Make the window taller to see documents."
            },
            false,
            DIM,
            l.width.saturating_sub(g.content_x),
        );
    }
    for &(i, r) in &g.cards {
        rect(
            &mut t,
            r,
            if view.hover == Hit::Document(i) {
                BLUE
            } else {
                BORDER
            },
            8,
        );
        rect(&mut t, inner(r, 1), WHITE, 7);
        let preview = DamageRect::new(r.x + 1, r.y + 1, r.width.saturating_sub(2), 129);
        rect(&mut t, preview, 0xFFF8_F9FC, 7);
        if r.width >= 90 {
            let px = r.x + (r.width - 72) / 2;
            rect(&mut t, DamageRect::new(px, r.y + 14, 72, 108), WHITE, 2);
            for n in 0..6 {
                rect(
                    &mut t,
                    DamageRect::new(px + 11, r.y + 31 + n * 12, 48 - (n % 3) * 8, 2),
                    BORDER,
                    0,
                );
            }
        }
        label(
            &mut t,
            l,
            r.x + 12,
            r.y + 139,
            &view.titles[i],
            true,
            INK,
            r.width.saturating_sub(24),
        );
        label(
            &mut t,
            l,
            r.x + 12,
            r.y + 163,
            "Encrypted document",
            false,
            DIM,
            r.width.saturating_sub(24),
        );
    }
    for (r, h, text) in [
        (g.previous, Hit::Previous, "Previous"),
        (g.next, Hit::Next, "Next"),
    ] {
        if r.is_empty() {
            continue;
        }
        rect(
            &mut t,
            r,
            if view.hover == h { 0xFFD3_E3FD } else { PALE },
            8,
        );
        label(
            &mut t,
            l,
            r.x + 10,
            r.y + 6,
            text,
            true,
            BLUE,
            r.width.saturating_sub(20),
        );
    }
    if let Some(notice) = &view.notice {
        let x = if g.pages > 1 {
            g.content_x.saturating_add(220)
        } else {
            g.content_x
        };
        label(
            &mut t,
            l,
            x,
            l.height.saturating_sub(32),
            notice,
            false,
            DIM,
            l.width.saturating_sub(x + g.content_x),
        );
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChromeMetrics;
    #[test]
    fn search_and_hit_use_original_vault_index() {
        let l = Layout::new(&ChromeMetrics::for_size(1200, 800));
        let mut v = View::new(vec!["First".into(), "Roadmap".into(), "Notes".into()]);
        v.query = "ROAD".into();
        let g = geometry(&l, &v);
        assert_eq!(g.cards.len(), 1);
        let (i, r) = g.cards[0];
        assert_eq!(i, 1);
        assert_eq!(
            hit(&l, &v, r.x as i32 + 1, r.y as i32 + 1),
            Hit::Document(1)
        );
    }
    #[test]
    fn all_matching_documents_are_reachable_by_pages() {
        let l = Layout::new(&ChromeMetrics::for_size(800, 600));
        let mut v = View::new((0..40).map(|i| format!("Doc {i}")).collect());
        let pages = geometry(&l, &v).pages;
        assert!(pages > 1);
        let mut found = Vec::new();
        for page in 0..pages {
            v.page = page;
            let g = geometry(&l, &v);
            found.extend(g.cards.iter().map(|(i, _)| *i));
            if page + 1 < pages {
                assert_eq!(
                    hit(&l, &v, g.next.x as i32 + 1, g.next.y as i32 + 1),
                    Hit::Next
                );
            }
        }
        assert_eq!(found, (0..40).collect::<Vec<_>>());
    }
    #[test]
    fn controls_match_their_drawn_geometry() {
        let l = Layout::new(&ChromeMetrics::for_size(1200, 800));
        let v = View::new(vec!["Notes".into()]);
        let g = geometry(&l, &v);
        for (r, expected) in [
            (g.new, Hit::New),
            (g.sample, Hit::Sample),
            (g.open, Hit::Open),
            (g.lock, Hit::Lock),
        ] {
            assert!(!r.is_empty());
            assert_eq!(hit(&l, &v, r.x as i32 + 1, r.y as i32 + 1), expected);
        }
    }
    #[test]
    fn library_tree_never_embeds_document_runs() {
        fn contains_doc_run(tree: &SurfaceTree) -> bool {
            matches!(tree.node, Some(Node::DocText(_)))
                || tree.before.iter().any(contains_doc_run)
                || tree.after.iter().any(contains_doc_run)
        }
        let l = Layout::new(&ChromeMetrics::for_size(1200, 800));
        let v = View::new(vec!["Notes".into()]);
        assert!(!contains_doc_run(&tree(&l, &v)));
    }
    #[test]
    fn compact_library_keeps_search_and_cards_available() {
        let l = Layout::new(&ChromeMetrics::for_size(320, 600));
        let v = View::new(vec!["Notes".into()]);
        let g = geometry(&l, &v);
        assert!(!g.search.is_empty());
        assert!(!g.lock.is_empty());
        assert_eq!(g.cards.len(), 1);
        assert!(g.search.bottom() < g.list_y);
    }
    #[test]
    fn every_visible_hit_target_stays_inside_the_window() {
        let v = View::new((0..20).map(|i| format!("Doc {i}")).collect());
        for w in (1..=1000).step_by(37) {
            for h in (1..=900).step_by(43) {
                let l = Layout::new(&ChromeMetrics::for_size(w, h));
                let g = geometry(&l, &v);
                for r in [
                    g.new, g.sample, g.open, g.lock, g.search, g.previous, g.next,
                ]
                .into_iter()
                .chain(g.cards.iter().map(|(_, r)| *r))
                {
                    if !r.is_empty() {
                        assert!(r.right() <= w && r.bottom() <= h, "{w}x{h}: {r:?}");
                    }
                }
            }
        }
    }
    #[test]
    fn pages_and_small_windows_are_safe() {
        let mut v = View::new((0..40).map(|i| format!("Doc {i}")).collect());
        for (w, h) in [(1, 1), (320, 240), (800, 600), (1600, 900)] {
            let l = Layout::new(&ChromeMetrics::for_size(w, h));
            v.page = usize::MAX;
            let g = geometry(&l, &v);
            assert!(g.page < g.pages);
            for (_, r) in &g.cards {
                assert!(r.right() <= w && r.bottom() <= h);
            }
            let _ = tree(&l, &v);
        }
    }
}
