//! Diagnostic: search and replace a real encrypted 30-million-word world-line while measuring
//! incremental heap usage. This is a sparse-match case, not a proof for arbitrary match density.
#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use holonomy_line::{LineHeader, LineManifest, LineSlab, WorldLine};
use holonomy_seal::{
    create_capsule_for, generate_container_id, KdfParamsRecord, MasterKeys, ParamsClass,
};
use holonomy_text::{
    Appended, ReplacementBuilder, ResidentSet, SegmentId, SegmentSink, SegmentSource, Slab, Spine,
    Typing,
};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
struct Counting;
// SAFETY: allocation and deallocation are forwarded to System; atomics only account for sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies a valid allocation layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller supplies the original pointer and layout.
        unsafe { System.dealloc(ptr, layout) };
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Shared {
    line: Rc<RefCell<WorldLine>>,
    loads: u64,
}
impl SegmentSource for Shared {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        self.loads += 1;
        self.line
            .borrow()
            .open_segment(segment.get())
            .map(|bytes| bytes.to_vec())
            .map_err(|error| error.to_string())
    }
}
impl SegmentSink for Shared {
    fn append_segment(&mut self, _plaintext: &[u8]) -> Result<Appended, String> {
        Err("individual append is not used by this test".to_owned())
    }
    fn atomic_replace(
        &mut self,
        chunks: &[&[u8]],
        build: &mut ReplacementBuilder<'_>,
    ) -> Result<(), String> {
        let mut line = self.line.borrow_mut();
        let first = line.segment_count();
        let ids: Vec<_> = (0..chunks.len())
            .map(|i| SegmentId::new(first + i as u64))
            .collect();
        let (slabs, styles) = build(&ids)?;
        if !styles.is_empty() {
            return Err("fixture unexpectedly styled".to_owned());
        }
        let manifest = LineManifest {
            slabs: slabs
                .iter()
                .map(|s| LineSlab {
                    segment: s.segment().get(),
                    start: s.start(),
                    len: s.len(),
                })
                .collect(),
            styles: Vec::new(),
            links: Vec::new(),
        };
        line.append_segments_with_full_manifest(chunks, &manifest)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[test]
#[ignore = "180 MiB encrypted fixture; run the dedicated release-mode CI measurement"]
fn encrypted_30m_word_search_and_replace_heap_measurement() {
    const SEGMENTS: usize = 2748;
    const LENGTH: usize = 65_536;
    const LIMIT: usize = 4 * 1024 * 1024;
    let id = generate_container_id().unwrap();
    let name: String = id.iter().map(|b| format!("{b:02x}")).collect();
    let path =
        Temp(std::env::temp_dir().join(format!("holonomy-search-memory-{name}.wavefunction")));
    let salt = [0x71; 64];
    let params = KdfParamsRecord {
        m_kib: 8192,
        t: 1,
        p: 1,
    };
    let keys =
        MasterKeys::derive(b"encrypted-search-fixture", &salt, None, ParamsClass::Test).unwrap();
    let (capsule, dem) = create_capsule_for(keys.unwrap_key(), None).unwrap();
    let header = LineHeader::new(params, salt, capsule).unwrap();
    let mut line = WorldLine::create(&path.0, header, dem.duplicate()).unwrap();

    let mut plain = vec![0u8; LENGTH];
    for (i, b) in plain.iter_mut().enumerate() {
        *b = b"word0 "[i % 6];
    }
    let mut middle = plain.clone();
    middle[1024..1032].copy_from_slice(b"holonomy");
    let mut end = plain.clone();
    end[LENGTH - 4..].copy_from_slice(b"holo");
    let mut start = plain.clone();
    start[..4].copy_from_slice(b"nomy");
    for batch in (0..SEGMENTS).step_by(128) {
        let parts: Vec<&[u8]> = (batch..(batch + 128).min(SEGMENTS))
            .map(|index| match index {
                100 | 1800 => middle.as_slice(),
                500 | 1500 | 2500 => end.as_slice(),
                501 | 1501 | 2501 => start.as_slice(),
                _ => plain.as_slice(),
            })
            .collect();
        line.append_segments(&parts).unwrap();
    }
    drop((plain, middle, end, start));
    assert!(SEGMENTS * LENGTH / 6 > 30_000_000);
    drop(line);
    let line = Rc::new(RefCell::new(WorldLine::open(&path.0, dem).unwrap()));
    let slabs: Vec<_> = line
        .borrow()
        .document_slabs()
        .iter()
        .map(|s| Slab::new(SegmentId::new(s.segment), s.start, s.len))
        .collect();
    let original = Spine::from_slabs(slabs);
    let sink = Shared {
        line: Rc::clone(&line),
        loads: 0,
    };
    let source = Shared {
        line: Rc::clone(&line),
        loads: 0,
    };
    let mut resident = ResidentSet::new(source, LIMIT, 1024).unwrap();
    for segment in 0..3 {
        resident.fault_in(SegmentId::new(segment)).unwrap();
    }
    resident.source_mut().loads = 0;
    let resident_allocated = resident.allocated_bytes();
    const TYPING_CAPACITY: usize = 4096;
    let mut typing = Typing::resume(sink, original.clone(), TYPING_CAPACITY);

    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    let hits = typing.find_all(b"holonomy", resident.source_mut()).unwrap();
    let find_peak =
        PEAK.load(Ordering::SeqCst).saturating_sub(baseline) + resident_allocated + TYPING_CAPACITY;
    assert_eq!(hits.len(), 5);
    assert_eq!(resident.source_mut().loads, SEGMENTS as u64);
    assert!(find_peak <= LIMIT, "find peak {find_peak} exceeds {LIMIT}");
    drop(hits);

    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    assert_eq!(
        typing
            .replace_all(b"holonomy", b"HOLONOMY_V6", resident.source_mut())
            .unwrap(),
        5
    );
    let replace_peak =
        PEAK.load(Ordering::SeqCst).saturating_sub(baseline) + resident_allocated + TYPING_CAPACITY;
    assert!(
        replace_peak <= LIMIT,
        "replace peak {replace_peak} exceeds {LIMIT}"
    );
    assert_eq!(resident.allocated_bytes(), resident_allocated);
    assert!(typing.undo().unwrap());
    assert!(typing.spine().ptr_eq(&original));
    println!("HOLONOMY_ENCRYPTED_SEARCH segments={SEGMENTS} bytes={} matches=5 resident={resident_allocated} find_peak={find_peak} replace_peak={replace_peak} bound={LIMIT}", SEGMENTS * LENGTH);
}
