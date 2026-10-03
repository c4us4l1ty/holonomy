# Doctrine

Rules this project has learned the hard way. Every one of them exists because
something shipped looking correct and wasn't.

Each rule is written with the failure that produced it, because a rule without
its cause gets argued away the first time it is inconvenient.

---

## 1. A test that asserts something other than the product is worse than no test

**Rule.** Every assertion must be answerable "yes" by the real system. If a test
can pass while the feature is broken, it is not a test.

**Why.** Across M0 and M3, eleven defects produced confident, plausible, wrong
results. The recurring shape:

| the harness did | the defect it hid |
|---|---|
| dispatched to an editor that was not focused | the boundary merge appeared broken |
| read `editor.state.tr` after dispatch | undo captured nothing, silently |
| measured its own styled probe | fitted constants for a box that never shipped |
| compared two coordinate spaces | reported a 72px drift on a correct compensation |
| timed one op per `Instant::now()` | both candidate paths read `0.0001ms` |

Every one was a green run. That is the point: a harness that cannot fail is worse
than no harness, because it converts "unknown" into "fine".

**Test.** Before believing a number, ask what would have to be true for the
measurement to be measuring something else. Where that question has bitten once,
the harness now asserts it explicitly rather than relying on review.

---

## 2. A flaky test is worse than a missing one

**Rule.**

- **Quarantine on first flake.** Mark it, do not re-run until green.
- **Root-cause it or delete it with a written justification.** Never "fixed".
- **No `setTimeout` sleeps** as a fix for a timing assertion. Use an observable
  condition.
- **No weakened assertions.** A looser bound that still passes is a deleted test
  wearing a disguise.

**Why.** The merge tests failed 4 times in 8 runs, a different test each time,
with the message `merge should have grown s0: 129 -> 129`. That message points
squarely at the boundary-merge logic, which was working correctly. The cause was
that two tests called `setCaret` without confirming the editor held DOM focus, so
`Backspace` went to a stale editor.

The dangerous part is not the flake. It is the natural response: seeing a merge
test fail intermittently, the tempting conclusion is that the merge is racy, so
add a sleep, or that 129→129 is close enough, so widen the bound. Both would have
converted a loud, precise diagnostic into silence while the real bug — a focus
assumption nobody checked — stayed unexamined.

Both tests now assert focus *before* the keypress, which turns the failure into
`s1 does not hold DOM focus` and names the cause. Ten consecutive runs pass.

**Corollary.** A failure message must point at the cause, not at a plausible
suspect. When an assertion failed, ask what else could produce that message, and
make the test distinguish.

---

## 3. Measure the thing you will ship, not a proxy for it

**Rule.** A benchmark or calibration harness must copy the real system's
configuration — stylesheet, build flags, platform — rather than approximating
it. Where a proxy is unavoidable, assert the proxy still matches the real thing.

**Why.** `test/calibrate.ts` fitted the height model against a probe element it
styled itself. Later, the scroll surface appended ProseMirror straight into a
slot, so there was no `.section-slice` ancestor at all: text wrapped at 1216px
instead of 736px and sections rendered 844px where calibration had measured
1527px. Every height in the geometry described a document that was not on screen,
and the estimate was 45% out while the compensation logic worked *perfectly on
wrong numbers*.

That is the most dangerous failure mode in this project: a self-consistent system
reasoning correctly about the wrong inputs. Nothing crashes. Everything looks
fine.

The scroll suite now asserts the text column is ~736px, and the calibration
harness copies the app's own stylesheet.

---

## 4. A harness must prove it can detect the failure it exists to detect

**Rule.** Before trusting a measurement, establish that the instrument responds.
Assert a known property, not just that the code ran.

**Why.** The M4 gate timed one operation per clock read. A ~100ns Fenwick lookup
and the ~30ns cost of reading the clock were indistinguishable, so the tree and
the linear scan reported identical times at 64 *and* at 8192 sections. It looked
like a clean result — the tree was never more than a couple of operations, after
all — and the instrumentation was simply too coarse to see the difference.

Three more in the same harness: the floor constant compared nanoseconds against
milliseconds and rejected all 500 results as unmeasurable, including an 8.69µs
scan; the closure indexed its input inside the timed region, so a modulo cost
more than the operation measured; and the sample count made the O(n) baseline
1.05e10 operations, which hung rather than reporting.

The self-check now gates on two properties: timing must be proportional to work
(measured 93x for 256x), and a 10000-iteration loop must read as more expensive
than a no-op (measured 80000x).

---

## 5. Report the number that was measured, including when it refutes the design

**Rule.** If a measurement contradicts an architectural decision, say so in the
commit and in the findings. Do not tune the threshold until the existing design
wins.

**Why.** `2000.md` asserts that a prefix-sum tree is *required* for scroll
geometry. Measured: a linear scan over 667 sections costs 0.46µs, which is 0.003%
of one 60Hz frame. The tree is 10x faster and still irrelevant. The linear cost
scales cleanly, so it becomes real somewhere past 100k sections — 300,000 pages.

The tree was kept, for its O(log n) scaling and because a prefix-sum array would
need rebuilding on structural change anyway. But the gate is now on the linear
path staying under 0.5ms, and the commit states plainly that the tree is not
earning its keep on speed and that `manifest.rs`'s existing comment was correct
all along.

Similarly: I believed character-only height estimation was fine on large sections
(~15%). Measured: 41%, and 225% on short dense ones. That produced the
`block_count` column rather than a threshold adjustment.

---

## 6. A read path with no writer is not a feature

**Rule.** If a value is read on a hot path, assert that something writes it.

**Why.** `block_count` was added to the schema, threaded through the manifest,
and read by the geometry on every scroll event. Nothing ever wrote it, so it was
permanently 0, which routes every height estimate into the character-derived
fallback. The 3.8%-accurate path was unreachable and the 225%-error path was the
only one running.

Schema without a write path is worse than no schema: it implies a capability that
does not exist, and every test that exercises the read path passes.

---

## 7. Two code paths must not independently decide the same thing

**Rule.** Where a rule is non-obvious — especially "when is it correct to move
the scroll position" — exactly one function decides it, and a test asserts there
is only one.

**Why.** A section can be measured from two directions: the scroller's own
mount-time pass and a `ResizeObserver` firing because the content reflowed. Both
computed a compensation and both applied it, so every section that mounted shifted
the viewport twice. It presents as stutter, which reads like a tuning problem
rather than a structural one.

The compensation is now decided in one place and funneled through
`applyCompensation`, and a test counts the `scrollTop +=` sites in both files and
fails if either grows a second one.

---

## 8. Prefer deleting duplication over guarding it

**Rule.** When two things must agree, generate one from the other. A parity test
between hand-copied constants is a last resort, and it should be deleted in the
same commit that removes the duplication.

**Why.** The height-model calibration constants were duplicated between
`GeometryCalibration::default` in Rust and three constants in `scroller-app.ts`,
with a test asserting they matched. That test worked. It was also, by its
existence, an admission that the duplication was a mistake someone had papered
over — and it would have failed at the *worst* possible moment, silently comparing
a stale number against a fresh one after a typography change.

The boot payload deletes both the constants and the test.

---

## 9. Distrust an assumption that a passing test appears to confirm

**Rule.** When a test unexpectedly fails, check whether the test's premise is
wrong before assuming the code is. Then check whether the code is wrong. Record
which it was.

**Why.** Repeatedly this cycle:

- A calibration test reported 225% error, which looked like a broken height model.
  The model was accurate to 2.5–4.4%; the *fixture* did not match the model's
  premise.
- A compensation test reported a 72px drift on a correctly compensated case. The
  assertion compared a slot's on-screen top against `scrollTop` — two different
  coordinate spaces.
- A column-width assertion expected 672px and failed against a correctly rendered
  section. The card has no `box-sizing`, so 46rem is the content box and 736px is
  right.

Each looked like a product bug. None was. Left undiagnosed, all three would have
produced a "fix" that made the number pass and the system unchanged.

---

## 10. Deleting an early design is not a setback

**Rule.** Record what was rejected and why, in the repository, at the time.

**Why.** The reasoning behind `taino-edit`, `crdt-richtext`, `CryptPad`, Joplin
and Tiptap Pages existed only in conversation. Deleting the vendored source left
no record of why they had been considered, which means the next person re-runs
the same evaluation. `spikes/REJECTED.md` exists so that does not happen.
---

## 11. A `let` holding a guard is not a scope

**Rule.** When a comment says a lock is released before an expensive operation,
prove it by *type*. Split the function so the locked half returns owned data, and
write a test that destroys the resource between the halves.

**Why.** The PDF export stops touching the store after preloading assets, and
`export_pdf`'s doc comment, `HoloWorld`'s doc comment and the Tauri command's doc
comment all said the store lock was released before the 45-second compile. The
command wrote

```rust
let core  = state.lock().unwrap();
let store = core.store.lock().unwrap();
let outcome = export_pdf(&store, ...);
```

and every one of those guards lives to the end of the *enclosing block*, not to
the end of the statement that used it. So a 45-second Typst compile held
`Mutex<DocumentCore>` and the SQLite store, and `get_section`,
`commit_section_edit` and every height sync blocked behind an export.

Three doc comments agreeing with each other and disagreeing with the code is the
part worth remembering: prose is not a scope. The test that keeps it honest
destroys the database between `prepare` and `compile`, so merging them back
together stops compiling rather than quietly reintroducing the freeze.

---

## 12. A test that builds its own subject tests the helper, not the wiring

**Rule.** If the feature is "the application attaches X", at least one test must
attach X the way the application does and go through the real page.

**Why.** `attachImageIngestion` had nineteen passing browser tests, none of which
ran in the application: each attached the handler to a `div` the test created
itself. The feature was never called at boot, and the single expression inside it
called `registry.focused` — a section *id* — as a function, so every paste would
have thrown. With that fixed, the insert itself turned out to be a **silent
no-op**: `chain().focus().insertContent` leaves a section at 2524 nodes with no
image node in it, and the status bar said "inserted 1 image (8×8)".

Nineteen green tests, a feature no user could reach, and a lie in the status bar.
The four tests that caught all three go through the real `#scroller`, with a faked
bridge and a real Tiptap editor that has focus.

---

## 13. Count the things that ran, not the things that returned

**Rule.** A step that produces a number reports the number of units it processed.
"the suite ran" and "seventeen suites ran" are different claims.

**Why.** The pagination fixture estimated 34 paragraphs per page and produced
**182** pages instead of 50. An over-estimate yields a fixture that is longer than
asked for, which still exercises pagination, still passes `>= 50`, and costs 3.6×
the compile time. Only printing the actual page count noticed.

---

## 14. A stylesheet copied into a harness is a second stylesheet

**Rule.** If a test harness renders what the product renders, it *loads* what the product
loads. A copy is two things that can differ, and it will.

**Why.** `calibrate.html` carried the `body` rule marked "copied verbatim from index.html".
When the product moved to an embedded face, the harness kept asking for `system-ui` — and
`npm run test:calibrate` reported the *previous* constants to the last decimal (24.83, 34.89)
for a font no longer in use. Nothing failed. The height model was fitted to a world the
product had left: a confidently wrong answer, which is the only kind that is dangerous.

The fix was one stylesheet both pages link, and a harness that reads its line height and
content width from the page rather than restating them. The deeper rule is the second half
of it: **a harness that re-derives the product's constants from its own copy of the rules
is testing the copy.**

**And the corollary, which cost a day:** `document.fonts.check('15px Inter')` returns
`true` on a page with no `Inter` face at all, because it asks whether the fonts needed for
the text are loaded and the fallback always is. A webfont assertion built on it is
vacuous — it reports the font loaded precisely when nothing was loaded. Enumerate
`document.fonts` and match on `status === 'loaded'` instead.

---

## 15. Registration order is a safety property, not a style preference

**Rule.** When a thing must happen before another thing, say *which line of which file*
orders them, and put a test that fails if the order changes.

**Why.** `tauri-plugin-single-instance` has to be registered before anything in this app
touches the database, and "before" is not obvious: it is before the application's `.setup()`
hook, because Tauri runs plugin setup inside `Builder::build()`. Read the framework, found
`app.rs:2607` and `app.rs:2697`, and the ordering became a fact instead of a habit.

Get it wrong and nothing complains. The second process opens the same `.holo`, two `Store`s
fight over the WAL lock, and the damage depends on which one the user quits first — which is
not a crash anyone can reproduce.

**And the corollary, which is the part that is easy to miss.** A callback can run *before the
app is ready*, because the thing it needs may not exist yet. `open_and_notify` took
`app.state::<Mutex<DocumentCore>>()`, which **panics** on an unregistered type — on a D-Bus
handler thread whose panic surfaces nowhere. The bus name is claimed before the frontend
exists, so a user double-clicking a second document during startup hits exactly that. The
check is `try_state`, and it is not defensive programming; it is the difference between
handing someone a document and handing them a panic.
