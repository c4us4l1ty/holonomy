//! The frame as a picture — `docs/TO-APP.md` **M1**.
//!
//! A renderer draws what a frame hands it, so the frame has to hand over something that is
//! *self-consistent as a picture* before any pixel code exists. This harness rasterises
//! [`holonomy_eye::Picture`] into a grid of one-unit cells — the crudest possible renderer, and the
//! only one that needs no GPU — and asks the four questions a real renderer would be broken by:
//!
//! 1. **Every cluster owns its cells, once.** No cell is claimed by two clusters, every cluster
//!    claims at least one cell, and no cell is claimed outside the wrap width.
//! 2. **The caret lands on a cluster it owns** — or on a line end that offers one — for every
//!    position a click can produce on the visible lines.
//! 3. **A selection covers exactly the clusters between its ends**: a cluster is painted whole if
//!    any of its bytes is selected, and not at all if none is.
//! 4. **Line boxes tile the picture**: they start at zero, each begins where the last ended, and
//!    their total is the height the layout reports.
//!
//! The numbers are printed as `HOLONOMY_PICTURE`, and the counts asserted, so a green run cannot be
//! a run that drew nothing. The shaper is the deterministic fake the other shell tests use: one
//! logical unit per cluster, two for `m` and `中`, so every advance is an integral number of cells
//! and the raster is exact rather than rounded.

use holonomy_eye::{Fixed, Picture, PictureCluster, Point, Shaper, Style};
use holonomy_shell::Editor;
use holonomy_text::{Appended, ResidentSet, SegmentId, SegmentSink, SegmentSource, Typing};

/// One logical unit per cluster, two for `m` and `中` — as in `editing.rs`.
#[derive(Debug, Default)]
struct Fake;

impl Shaper for Fake {
    fn advance(&self, cluster: &str, _style: Style) -> Fixed {
        match cluster.chars().next() {
            Some('m') | Some('中') => Fixed::units(2),
            _ => Fixed::ONE,
        }
    }

    fn line_height(&self, _style: Style) -> Fixed {
        Fixed::units(20)
    }

    fn ascent(&self, _style: Style) -> Fixed {
        Fixed::units(15)
    }
}

/// A sink that mints distinct segment ids and stores nothing.
#[derive(Debug, Default)]
struct Recorder {
    next: u64,
}

impl SegmentSink for Recorder {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let id = self.next;
        self.next += 1;
        Ok(Appended {
            segment: SegmentId::new(id),
            witness_sequence: id + 1,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

/// A world-line that generates its leaves: each is `leaf_bytes` letters and no break.
#[derive(Debug)]
struct Repeated {
    leaf_bytes: usize,
}

impl SegmentSource for Repeated {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        if segment.get() >= 4_096 {
            return Err(format!("no {segment}"));
        }
        Ok(vec![b'a'; self.leaf_bytes])
    }
}

const WIDTH: Fixed = Fixed::units(200);
const VIEWPORT: Fixed = Fixed::units(40);
const BUDGET: usize = 256;
/// Leaves in the generated document — the document is wider than the screen on purpose.
const LEAVES: usize = 40;
/// The fake shaper's line box, in units: the cell grid's row height.
const LINE: i64 = 20;

type Shell = Editor<Recorder, Repeated, Fake>;

fn editor() -> Shell {
    let residency =
        ResidentSet::new(Repeated { leaf_bytes: 64 }, BUDGET, 0).expect("a workable budget");
    Editor::new(
        Typing::new(Recorder::default(), 64),
        residency,
        Fake,
        WIDTH,
        VIEWPORT,
    )
}

/// A shell over a measured document, at the top.
fn measured() -> Shell {
    let mut shell = editor();
    shell
        .type_text(&"a".repeat(64).repeat(LEAVES))
        .expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    shell
}

/// `value` in whole cells, asserting it is a whole number of them — the raster is only exact if
/// the geometry is integral, and the fake shaper makes it so.
fn cells(value: Fixed, what: &str) -> i64 {
    let sixtyfourths = value.sixtyfourths();
    assert_eq!(
        sixtyfourths % 64,
        0,
        "{what} is not a whole cell: {value:?}"
    );
    sixtyfourths / 64
}

/// The picture's clusters painted into a grid of `width × lines` cells, each cell the index of the
/// cluster that claimed it. Panics on a double claim — which is the property, not a test helper.
fn raster(picture: &Picture) -> Vec<Vec<Option<usize>>> {
    let width = cells(picture.width, "the wrap width");
    assert!(width > 0, "a picture with no width");
    let mut grid = vec![vec![None; width as usize]; picture.lines.len()];
    let mut claimed = 0usize;
    for (index, row) in grid.iter_mut().enumerate() {
        for cluster in picture.clusters_on(index) {
            let from = cells(cluster.x, "a cluster's left edge");
            let to = cells(cluster.end(), "a cluster's right edge");
            assert!(from < to, "cluster {} has no width", cluster.byte);
            assert!(
                from >= 0 && to <= width,
                "cluster {} at {from}..{to} is outside the {width}-cell width",
                cluster.byte
            );
            for cell in &mut row[from as usize..to as usize] {
                assert!(
                    cell.is_none(),
                    "line {index}: two clusters claim the same cell"
                );
                *cell = Some(index);
                claimed += 1;
            }
        }
    }
    assert!(
        claimed > 0,
        "the picture claimed no cells — it drew nothing"
    );
    grid
}

/// The cluster on `line` whose bytes contain `byte`, if any.
fn cluster_at(picture: &Picture, line: usize, byte: u64) -> Option<&PictureCluster> {
    picture
        .clusters_on(line)
        .iter()
        .find(|cluster| byte >= cluster.byte && byte < cluster.byte + cluster.len as u64)
}

#[test]
fn every_cluster_owns_its_cells_once() {
    let mut shell = measured();
    let mut pictures = 0u64;
    let mut clusters = 0u64;
    let mut claimed = 0u64;
    for top in [0, 40, 400, 1_200] {
        shell.scroll_to(Fixed::units(top));
        let frame = shell.frame().expect("a frame");
        let picture = frame.picture();
        assert_eq!(
            picture
                .lines
                .iter()
                .map(|line| line.clusters.len())
                .sum::<usize>(),
            picture.clusters.len(),
            "at scroll {top}: the lines' cluster ranges do not partition the cluster list"
        );
        let grid = raster(&picture);
        clusters += picture.clusters.len() as u64;
        claimed += grid
            .iter()
            .flat_map(|row| row.iter())
            .filter(|cell| cell.is_some())
            .count() as u64;
        pictures += 1;
    }
    println!("HOLONOMY_PICTURE pictures={pictures} clusters={clusters} cells={claimed}");
    assert_eq!(pictures, 4, "four scroll positions");
    assert!(clusters > 100, "only {clusters} clusters drawn");
    assert!(
        claimed >= clusters,
        "only {claimed} cells claimed for {clusters} clusters"
    );
}

#[test]
fn line_boxes_tile_the_picture() {
    let mut shell = measured();
    let frame = shell.frame().expect("a frame");
    let picture = frame.picture();
    assert!(
        picture.lines.len() >= 2,
        "a viewport holding one line proves nothing"
    );
    let mut top = Fixed::ZERO;
    for (index, line) in picture.lines.iter().enumerate() {
        assert!(line.height > Fixed::ZERO, "line {index} has no height");
        assert_eq!(
            line.top, top,
            "line {index} does not begin where the last ended"
        );
        assert_eq!(
            cells(line.height, "a line box"),
            LINE,
            "line {index} is not one line box tall"
        );
        top += line.height;
    }
    assert_eq!(
        top, picture.height,
        "the boxes do not sum to the picture's height"
    );
    assert_eq!(
        picture.height,
        frame.layout().height(),
        "the picture and the layout disagree about height"
    );
}

#[test]
fn the_caret_lands_on_a_cluster_it_owns() {
    let mut shell = measured();
    // Every cluster on the first two visible lines, by clicking its left edge.
    let places: Vec<(usize, Fixed, Fixed)> = {
        let frame = shell.frame().expect("a frame");
        let picture = frame.picture();
        let mut places = Vec::new();
        for index in 0..picture.lines.len().min(2) {
            let line = &picture.lines[index];
            for cluster in picture.clusters_on(index) {
                places.push((index, cluster.x, line.top + Fixed::ONE));
            }
        }
        places
    };
    assert!(!places.is_empty(), "nothing to click");
    let mut checked = 0u64;
    for (line_index, x, y) in places {
        let Some(caret) = shell.click(Point::new(x, y)).expect("a click") else {
            continue;
        };
        let frame = shell.frame().expect("a frame");
        let picture = frame.picture();
        let Some(shown) = picture.caret else {
            continue;
        };
        assert_eq!(
            shown.byte, caret.byte,
            "the picture's caret is not the one the click made"
        );
        let line = &picture.lines[shown.line];
        if let Some(cluster) = cluster_at(&picture, shown.line, shown.byte) {
            assert_eq!(
                shown.x, cluster.x,
                "line {}: the caret at byte {} sits at {:?}, not on its cluster's edge {:?}",
                shown.line, shown.byte, shown.x, cluster.x
            );
            assert_eq!(shown.top, line.top, "the caret is off its own line's box");
            assert_eq!(
                shown.height, line.height,
                "the caret is not a line box tall"
            );
        } else {
            assert!(
                line.has_end && shown.byte == line.end_byte,
                "line {line_index}: a caret at byte {} is on no cluster and at no line end",
                shown.byte
            );
            assert_eq!(
                shown.x, line.end_x,
                "line {}: the end caret is not at the line's end",
                shown.line
            );
        }
        checked += 1;
    }
    println!("HOLONOMY_PICTURE_CARETS carets={checked}");
    assert!(checked >= 10, "only {checked} carets checked");
}

#[test]
fn a_selection_covers_exactly_the_clusters_between_its_ends() {
    let mut shell = measured();

    // A caret is not a selection.
    let frame = shell.frame().expect("a frame");
    assert!(
        frame.picture().selection.is_empty(),
        "a caret painted a selection"
    );
    assert_eq!(frame.selection(), None);

    // Click near the start, drag into the second line: a partial selection across a wrap.
    shell
        .click(Point::new(Fixed::units(9), Fixed::ONE))
        .expect("a click");
    shell
        .drag_to(Point::new(Fixed::units(120), Fixed::units(25)))
        .expect("a drag");
    let frame = shell.frame().expect("a frame");
    let (start, end) = frame.selection().expect("a selection");
    let picture = frame.picture();
    assert!(
        !picture.selection.is_empty(),
        "the selection painted nothing"
    );

    let mut painted = 0u64;
    let mut absent = 0u64;
    for (index, line) in picture.lines.iter().enumerate() {
        let spans: Vec<_> = picture.selections_on(index).collect();
        for cluster in picture.clusters_on(index) {
            let bytes = cluster.byte..cluster.byte + cluster.len as u64;
            let selected = bytes.start < end && bytes.end > start;
            let covered = spans
                .iter()
                .any(|span| span.x <= cluster.x && cluster.end() <= span.end);
            let says = if covered { "painted" } else { "unpainted" };
            let wanted = if selected { "painted" } else { "unpainted" };
            assert_eq!(
                covered, selected,
                "line {index}: cluster {}..{} is {says}, selection {start}..{end} says {wanted}",
                bytes.start, bytes.end
            );
            // Whole clusters only: a painted cluster is inside its line's box.
            if covered {
                for span in &spans {
                    assert!(span.end <= line.end_x);
                }
                painted += 1;
            } else {
                absent += 1;
            }
        }
    }
    println!("HOLONOMY_PICTURE_SELECTION painted={painted} unpainted={absent}");
    assert!(
        painted > 0 && absent > 0,
        "a selection that covers everything proves nothing"
    );

    // Selecting everything paints every cluster the window shows.
    shell.select_all();
    let frame = shell.frame().expect("a frame");
    let (start, end) = frame.selection().expect("a selection");
    let picture = frame.picture();
    for (index, _) in picture.lines.iter().enumerate() {
        let span = picture.selection_on(index).expect("a span on every line");
        assert_eq!(
            span.x,
            Fixed::ZERO,
            "line {index}: the selection does not start at the margin"
        );
        for cluster in picture.clusters_on(index) {
            assert!(
                cluster.byte >= start && cluster.byte + cluster.len as u64 <= end,
                "line {index}: a visible cluster outside the select-all range"
            );
            assert!(
                cluster.end() <= span.end,
                "line {index}: a cluster past the span"
            );
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Memory(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

impl SegmentSink for Memory {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let mut segments = self.0.lock().map_err(|_| "poisoned")?;
        let id = segments.len() as u64;
        segments.push(plaintext.to_vec());
        Ok(Appended {
            segment: SegmentId::new(id),
            witness_sequence: id + 1,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

impl SegmentSource for Memory {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        let segments = self.0.lock().map_err(|_| "poisoned")?;
        segments
            .get(usize::try_from(segment.get()).unwrap_or(usize::MAX))
            .cloned()
            .ok_or_else(|| format!("no {segment}"))
    }
}

#[test]
fn mixed_bidi_selection_emits_and_covers_disjoint_visual_spans() {
    let memory_store = Memory::default();
    let residency = ResidentSet::new(memory_store.clone(), BUDGET, 0).expect("budget");
    let mut shell = Editor::new(
        Typing::new(memory_store, 64),
        residency,
        Fake,
        WIDTH,
        VIEWPORT,
    );
    shell.type_text("ab של cd").expect("typed mixed BiDi");
    // Logical bytes `1..5` select `'b'`, `' '`, and `'ש'` (`'ל'` sits visually between `' '` and
    // `'ש'`), splitting the highlight on line 0 into two disjoint visual boxes.
    let b_point = {
        let frame = shell.frame().expect("initial frame");
        frame.layout().paint_point(1).expect("caret at 1")
    };
    let after_shin = {
        let frame = shell.frame().expect("initial frame");
        frame.layout().paint_point(5).expect("caret at 5")
    };
    shell
        .click(Point::new(b_point.x, b_point.top))
        .expect("clicked 1");
    shell
        .drag_to(Point::new(after_shin.x, after_shin.top))
        .expect("dragged to 5");
    let frame = shell.frame().expect("selected frame");
    assert_eq!(frame.selection(), Some((1, 5)));
    let picture = frame.picture();
    let spans: Vec<_> = picture.selections_on(0).collect();
    assert_eq!(spans.len(), 2, "expected two disjoint selection boxes");
    for cluster in picture.clusters_on(0) {
        let bytes = cluster.byte..cluster.byte + cluster.len as u64;
        let selected = bytes.start < 5 && bytes.end > 1;
        let covered = spans
            .iter()
            .any(|span| span.x <= cluster.x && cluster.end() <= span.end);
        assert_eq!(covered, selected, "cluster {bytes:?}");
    }
}
