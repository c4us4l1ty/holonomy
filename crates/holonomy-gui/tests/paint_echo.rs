//! Block 6 Gate 1 & `L-4`: software keystroke-to-paint echo latency (`App::press` +
//! `PixelSurface` OpenType rasterization) on a real sealed 30-million-word `.wavefunction`.
//!
//! Run in release CI (`persist` job):
//! `cargo test --release -p holonomy-gui --test paint_echo -- --ignored --nocapture`

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use holonomy_eye::{FontSet, View};
use holonomy_gui::canvas::PixelSurface;
use holonomy_gui::store::{self, Document};
use holonomy_gui::{App, Key, Keymap, Modifiers};
use holonomy_seal::generate_container_id;
use holonomy_shell::{Editor, Intent, Move, QUIET_MD_THEME};
use holonomy_text::{ResidentSet, SegmentSink, StyleMap, Typing};

struct TempDoc(PathBuf);

impl Drop for TempDoc {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn fast_temp_path(label: &str) -> TempDoc {
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let id = generate_container_id().expect("entropy");
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    TempDoc(dir.join(format!("holonomy-echo-{label}-{hex}.wavefunction")))
}

fn percentile_us(sorted: &[u128], pct: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * pct) / 100;
    sorted[idx]
}

#[test]
#[ignore = "30M-word pixel keystroke-to-paint latency benchmark; run in release CI `persist` job"]
fn software_paint_echo_latency_at_30m_words_head_and_tail() {
    let path = fast_temp_path("paint-echo-30m");
    let passphrase = b"paint-echo-benchmark-passphrase";
    let document = Document::create(&path.0, passphrase).expect("creates 30M-word container");
    let chunk = "alpha ".repeat(10_922); // 65,532 B (10,922 words)
    const LEAVES: usize = 2_800; // 30,581,600 words (~183.5 MiB)

    {
        let mut sink = document.sink();
        let mut writer = sink.begin_stream_publish().expect("stream publish");
        let mut slabs = Vec::with_capacity(LEAVES);
        for _ in 0..LEAVES {
            let seg = writer.write_chunk(chunk.as_bytes()).expect("write chunk");
            slabs.push(holonomy_text::Slab::new(seg, 0, chunk.len() as u32));
        }
        writer
            .finish(&slabs, &StyleMap::empty())
            .expect("commit 30M-word manifest");
    }
    drop(document);

    let reopened = Document::open(&path.0, passphrase).expect("cold reopen 30M-word document");
    let spine = reopened.spine().expect("spine");
    assert!(spine.len() > 180_000_000);

    let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded OpenType fonts");
    let surface = PixelSurface::new(fonts.clone(), View::IDENTITY, 1_280, 800);
    let (lw, lh) = surface.logical_editor_extent();
    let residency = ResidentSet::new(reopened.source(), store::BUDGET, store::SLACK)
        .expect("bounded residency");
    let mut editor = Editor::new(
        Typing::resume(reopened.sink(), spine, store::CAPACITY),
        residency,
        fonts,
        lw,
        lh,
    );
    editor.set_view(View::IDENTITY);
    let mut app = App::new(editor, Keymap::new(lh), surface);
    assert!(app.draw().expect("initial paint"));

    let mut head_us = Vec::with_capacity(50);
    for ch in "Keystroke-to-pixel echo benchmark at head of 30M doc."
        .chars()
        .take(50)
    {
        let t0 = Instant::now();
        app.press(Key::Character(ch), Modifiers::none())
            .expect("type at head");
        assert!(app.draw().expect("paint at head"));
        head_us.push(t0.elapsed().as_micros());
    }
    assert!(
        app.residency().resident_bytes() <= store::BUDGET,
        "head keystroke echo blew ResidentSet budget"
    );

    // Jump to the far end of the 183.5 MiB (30M-word) document and measure tail echo latency.
    app.editor()
        .apply(Intent::Move {
            to: Move::DocumentEnd,
            extend: false,
        })
        .expect("jump to end of 30M words");
    assert!(app.draw().expect("paint at 30M-word tail"));

    let mut tail_us = Vec::with_capacity(50);
    for ch in "And symmetric tail keystroke echo past 30M words!!."
        .chars()
        .take(50)
    {
        let t0 = Instant::now();
        app.press(Key::Character(ch), Modifiers::none())
            .expect("type at tail");
        assert!(app.draw().expect("paint at tail"));
        tail_us.push(t0.elapsed().as_micros());
    }
    assert!(
        app.residency().resident_bytes() <= store::BUDGET,
        "tail keystroke echo blew ResidentSet budget"
    );
    assert!(app.surface().painted_clusters() > 0);

    head_us.sort_unstable();
    tail_us.sort_unstable();
    println!(
        "HOLONOMY_PAINT_ECHO_30M bytes={} head_p50_us={} head_p99_us={} head_max_us={} tail_p50_us={} tail_p99_us={} tail_max_us={} resident_bytes={}",
        app.document_len(),
        percentile_us(&head_us, 50),
        percentile_us(&head_us, 99),
        head_us.last().copied().unwrap_or(0),
        percentile_us(&tail_us, 50),
        percentile_us(&tail_us, 99),
        tail_us.last().copied().unwrap_or(0),
        app.residency().resident_bytes(),
    );
}
