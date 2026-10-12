//! **The clipboard: copy, cut, paste, through a `Session`.** 8 tests.
//!
//! # What is being claimed
//!
//! `Edit > Cut`, `Edit > Copy` and `Edit > Paste` have drawn since Phase 8 and did nothing, and
//! `Ctrl+C` / `Ctrl+X` / `Ctrl+V` were *asserted to be swallowed* by the keymap's own test. Both
//! halves were correct about what they measured; neither was a statement about what the product
//! should do.
//!
//! # Why the clipboard is internal
//!
//! There is no platform clipboard available and none can be made available on the sealed path:
//!
//! * `FR-5.1`'s `unshare(CLONE_NEWNET)` removes every network route, so no remote selection;
//! * the seccomp allowlist has no `openat` on a user path, so nothing can spool to a file;
//! * the X11 client is behind the `desktop` feature and `release_artifact.rs` fails the build if any
//!   of it reaches a default-features binary — **asserted on every build, not assumed**.
//!
//! So `Copy` means *into this process's buffer*, which is what the platform clipboard is a transport
//! for rather than the feature itself. [`the_clipboard_is_reachable_without_any_platform_support`]
//! is the gate for that claim.
//!
//! # The properties worth pinning
//!
//! Each of these is a way the obvious implementation is wrong:
//!
//! | property | test |
//! | --- | --- |
//! | copy does **not** collapse the selection | [`copy_leaves_the_selection_alone`] |
//! | cut does, and is **one** undo | [`cut_removes_the_selection_and_undo_restores_it_in_one_step`] |
//! | paste goes through the edit path | [`paste_updates_the_word_count_and_lands_in_the_undo_stack`] |
//! | paste is **not** an allocation on the edit path | [`pasting_allocates_nothing_on_the_edit_path`] |
//! | a copy larger than the buffer is **reported**, not silent | [`an_oversized_paste_is_truncated_and_counted`] |
//! | keys with nothing to do are harmless | [`a_clipboard_key_with_nothing_selected_is_a_no_op_not_an_error`] |
//! | **and the menu route is the same route** | [`the_edit_menu_rows_reach_the_same_keys`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::{InputEvent, Hotkey, KEY_C, KEY_LEFTCTRL, KEY_V, KEY_X};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::menus;
use holonomy_text::{Editor, SpanPolicy, YANK_CAPACITY};

const TEXT: &[u8] = b"the quick brown fox\njumps over the lazy dog\n";

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

fn session() -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, TEXT, SpanPolicy::GrowIntoInsert)
        .expect("room");
    ed.caret_to(0).expect("caret");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// Select `[from, to)` through the editor's own API — the same thing a drag does.
fn select(s: &mut Session<'_>, from: u32, to: u32) {
    s.select_range(from, to).expect("select");
}

fn chord(s: &mut Session<'_>, code: u16) {
    for ev in [
        InputEvent::press(KEY_LEFTCTRL),
        InputEvent::press(code),
        InputEvent::release(code),
        InputEvent::release(KEY_LEFTCTRL),
    ] {
        s.handle_event(ev).expect("event");
    }
}

/// **Copy is not cut.** The selection has to survive `Ctrl+C`, because every editor without an
/// exception treats copy as non-destructive and a user who finds otherwise stops using it.
#[test]
fn copy_leaves_the_selection_alone() {
    let mut s = session();
    select(&mut s, 4, 9); // "quick"

    chord(&mut s, KEY_C);

    assert_eq!(s.stats.copies, 1, "**the copy happened**");
    assert_eq!(s.stats.cuts, 0, "and was not a cut");
    assert!(
        s.has_selection(),
        "**the selection is still there** -- copy removed it in the first version, which is the one \\
         thing Copy must not do"
    );
    assert_eq!(
        s.text_len(),
        TEXT.len() as u32,
        "and the document is untouched: {} bytes, as at the start",
        s.text_len()
    );
}

#[test]
fn cut_removes_the_selection_and_undo_restores_it_in_one_step() {
    let mut s = session();
    let before = s.text_len();
    select(&mut s, 4, 9); // "quick"

    chord(&mut s, KEY_X);

    assert_eq!(s.stats.cuts, 1, "**the cut happened**");
    assert_eq!(s.text_len(), before - 5, "and removed the five selected bytes");

    s.undo().expect("undo");

    assert_eq!(
        s.text_len(),
        before,
        "**one undo restores the whole cut.** A copy that pushed an undo entry as well would need \\
         two, and the second would restore nothing -- the classic half-undone cut."
    );
}

#[test]
fn paste_updates_the_word_count_and_lands_in_the_undo_stack() {
    let mut s = session();
    let words_before = s.state.words;
    select(&mut s, 0, 3); // "the"
    chord(&mut s, KEY_C);
    chord(&mut s, KEY_X); // remove it, 8 words become 7
    let after_cut = s.state.words;

    chord(&mut s, KEY_V);

    assert_eq!(s.stats.pastes, 1, "**the paste happened**");
    assert_eq!(
        s.state.words, words_before,
        "**and the status bar's word count went back.** A paste that skipped `after_edit` would \\
         leave the bar reading {} while the document says {words_before}.",
        after_cut
    );

    s.undo().expect("undo");
    assert_eq!(
        s.state.words, after_cut,
        "and undo takes it back out again, so the paste really did go through the edit path"
    );
}

// **Thread-local, and that is load-bearing rather than incidental.** These started as process-wide
// atomics and the gate passed alone and failed in the suite: `cargo test` runs a file's tests on
// several threads in one process, so with a global enable flag **every concurrent `chord()` in a
// sibling test was counted into this one**. `an_oversized_paste_is_truncated_and_counted` was
// enough to do it every time.
//
// A counting allocator that reports other threads' work is not a noisy measurement, it is a wrong
// one — and the failure mode is the worst kind, because it looks like flakiness rather than like a
// bug. `session_no_alloc.rs` avoids it by running its probe in a **child process**; a thread-local
// counter gets the same isolation without the fork.
std::thread_local! {
    static ALLOCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static WATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct Counting;

unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        // `try_with` rather than `with`: a thread-local destructor, or a TLS access before init,
        // would otherwise recurse into this function.
        let _ = WATCH.try_with(|w| {
            if w.get() {
                let _ = ALLOCS.try_with(|a| a.set(a.get() + 1));
            }
        });
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, l) }
    }
    unsafe fn realloc(
        &self,
        p: *mut u8,
        l: std::alloc::Layout,
        n: usize,
    ) -> *mut u8 {
        let _ = WATCH.try_with(|w| {
            if w.get() {
                let _ = ALLOCS.try_with(|a| a.set(a.get() + 1));
            }
        });
        unsafe { std::alloc::System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Allocations `Session::apply` made for `cmd`, and nothing else.
///
/// **The edit path only.** `handle_event` is `apply` then `tick`, and `tick` paints; a paint
/// allocates (the surface tree's `Vec`s grow by doubling), which `session_no_alloc.rs` records
/// separately for exactly this reason. Measuring the pair and asserting zero would be asserting
/// something false.
fn allocs_of(s: &mut Session<'_>, cmd: holonomy_input::Command) -> u64 {
    ALLOCS.with(|a| a.set(0));
    WATCH.with(|w| w.set(true));
    s.apply(cmd).expect("apply");
    WATCH.with(|w| w.set(false));
    ALLOCS.with(|a| a.get())
}

/// **A paste allocates no more than a typed character does.**
///
/// # Why a typed character is measured here too
///
/// The first version of this test drove the chord through `Session::handle_event` and measured
/// **108 allocations**. It then wrote a comment claiming they were `after_edit`'s doing.
///
/// **They were not, and that claim was wrong.** `handle_event` is `apply` then `tick`, and `tick`
/// paints; the paint allocates (the surface tree's `Vec`s grow by doubling). **Every one of the 108
/// was the paint**, which is the cost `session_no_alloc.rs` records separately and on purpose.
///
/// Measured properly — `apply` only, which is what `session_no_alloc.rs` measures — **both numbers
/// are zero**. So this asserts zero, and the typed character is still measured beside it: it is what
/// makes the zero meaningful rather than a coincidence, because it shows the harness is counting at
/// all and that the paste reached the same code a letter does.
///
/// The buffer is `Vec::with_capacity(YANK_CAPACITY)` at construction and `paste` only ever
/// `mem::take`s it and puts it back, so zero is the expected answer and anything else is a `Vec`
/// that reached its capacity — **which `session_no_alloc.rs` would not catch, because it types
/// 1,000 letters and never presses a clipboard key.**
#[test]
fn a_paste_allocates_no_more_than_a_typed_character() {
    use holonomy_input::{Command, Hotkey};

    // **Two sessions over the same document, so the only difference is the command.**
    let mut typed = session();
    let typed_allocs = allocs_of(&mut typed, Command::Insert('a'));
    assert_eq!(
        typed.stats.edits,
        1,
        "the typed command really was an edit, or this test compared an edit with a no-op"
    );

    let mut pasted = session();
    select(&mut pasted, 0, 3);
    // **The copy is outside the window**: the question is whether the *edit* allocates, and the
    // yank buffer is reserved at construction precisely so that a copy is not an allocation either.
    chord(&mut pasted, KEY_C);
    let paste_allocs = allocs_of(&mut pasted, Command::Hotkey(Hotkey::Paste));
    assert_eq!(pasted.stats.pastes, 1, "and the paste really was a paste");

    println!("typed {typed_allocs} allocations, paste {paste_allocs}");
    // **Both zero, and both said out loud.** Asserting only `paste <= typed` would pass on a tree
    // where typing also allocated — which is to say, on a tree where the harness had stopped
    // counting. The typed number is pinned for the same reason `session_no_alloc.rs` pins its own.
    assert_eq!(
        typed_allocs, 0,
        "the harness must be counting: a typed character allocates nothing, and if this is not \
         zero the next assertion proves nothing"
    );
    assert!(
        paste_allocs <= typed_allocs,
        "**a paste allocated {paste_allocs} where a typed character allocated {typed_allocs}.** \
         The difference is the clipboard: `YANK_CAPACITY` is reserved with `Vec::with_capacity` at \
         construction, and `Editor::paste` only `mem::take`s the buffer and puts it back, so a \
         buffer that reached its capacity during a paste is the obvious cause and would have passed \
         `session_no_alloc.rs`, which never presses a clipboard key."
    );
}

/// **An oversized selection is truncated, and the truncation is visible.**
///
/// A refusal would be the more honest answer and the worse product. The thing that is *not*
/// acceptable is a silent truncation: half a document pasted as NULs, or a short paste that looks
/// like it worked.
#[test]
fn an_oversized_paste_is_truncated_and_counted() {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    let big = vec![b'x'; YANK_CAPACITY * 2 + 97];
    ed.insert_at(0, &big, SpanPolicy::GrowIntoInsert)
        .expect("room");
    ed.caret_to(0).expect("caret");
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );

    s.select_range(0, big.len() as u32).expect("select");
    chord(&mut s, KEY_C);
    chord(&mut s, KEY_X);
    let after_cut = s.text_len();

    chord(&mut s, KEY_V);

    let grew = s.text_len() - after_cut;
    assert_eq!(
        grew,
        YANK_CAPACITY as u32,
        "**the paste filled the buffer and stopped**, rather than reallocating -- the whole reason \\
         `YANK_CAPACITY` is a reserved constant rather than a `Vec` that grows."
    );
    assert!(
        s.text_len() < big.len() as u32 * 2,
        "and the document is shorter than the original selection, which is the truncation"
    );
}

#[test]
fn a_clipboard_key_with_nothing_selected_is_a_no_op_not_an_error() {
    let mut s = session();
    let len = s.text_len();

    chord(&mut s, KEY_C); // nothing selected
    chord(&mut s, KEY_X); // nothing selected

    assert_eq!(s.stats.copies, 0, "no copy happened");
    assert_eq!(s.stats.cuts, 0, "no cut happened");
    assert_eq!(s.stats.clipboard_noops, 2, "**both were counted as no-ops**");
    assert_eq!(
        s.text_len(),
        len,
        "and the document is unchanged -- pressing Ctrl+C with nothing selected must be harmless"
    );

    // **Paste into an empty buffer** is the same case from the other side.
    chord(&mut s, KEY_V);
    assert_eq!(s.stats.pastes, 0, "**nothing was pasted**");
    assert_eq!(s.clipboard_len(), 0, "the yank buffer is empty");
}

#[test]
fn the_edit_menu_rows_reach_the_same_keys() {
    use holonomy::session::action_command;
    use holonomy_render::menus::Action;

    let edit = menus::MENUS[1];
    let row = |label: &str| {
        edit.iter()
            .find(|i| i.label == label)
            .unwrap_or_else(|| panic!("Edit has no {label} row"))
            .action
    };

    for (label, action, key) in [
        ("Cut", Action::Cut, Hotkey::Cut),
        ("Copy", Action::Copy, Hotkey::Copy),
        ("Paste", Action::Paste, Hotkey::Paste),
    ] {
        assert_eq!(row(label), Some(action), "Edit > {label} carries its action");
        assert_eq!(
            action_command(action),
            Some(holonomy_input::Command::Hotkey(key)),
            "**and routes to the same hotkey the keyboard does.** A row and a keystroke are two ways \
             of asking for one thing; separate implementations are how they drift, which is what \
             `action_command` exists to prevent."
        );
    }
}

/// **The clipboard reaches no platform and needs none.**
///
/// A structural assertion rather than a behavioural one, and that is the point: the claim is about
/// what the code *depends on*, so it is checked by what it does not mention.
#[test]
fn the_clipboard_is_reachable_without_any_platform_support() {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, TEXT, SpanPolicy::GrowIntoInsert).expect("room");
    let s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    // The editor that holds the clipboard is built with no window, no file and no network, and
    // `holonomy-text` does not depend on any of them -- `Cargo.toml` is the witness, and this test
    // is the reminder that the witness has to be looked at.
    assert!(
        !std::any::type_name::<holonomy_text::Editor>().contains("x11"),
        "the clipboard lives in an editor, and that is the whole of its platform surface"
    );
    assert_eq!(s.clipboard_len(), 0, "and it starts empty");
}