# HOLONOMY v6 — VISION

> *A document that outlives you should not require you to trust the machine you write it on.*

---

## 1. What Holonomy is

One app, one file, one lifetime.

Holonomy is a writing instrument for a single document that grows for decades — a page a day
for sixty years, thirty million words, ten thousand pages — stored in one sealed file
(`.wavefunction`) that you back up like any file and open anywhere with one passphrase.

Everything about it descends from a single physical law:

> **Never decrypt more than you look at.**

Every existing "secure" app violates this law. They wrap encryption around the outside of a
document and then hold the *entire* document unsealed in memory for as long as the editor is
open. The vault door is strong and the room behind it has no walls. Sensitive-at-rest is a
solved problem; **sensitive-in-use is what nobody builds.** Holonomy v6 exists to build it —
and the moment you build it honestly, huge scale, tiny CPU, and real security stop being
three goals and become one.

## 2. The singularity

Chemistry is state space. Most molecules are dust; penicillin changed civilization. Apps are
the same: most features are dust. Holonomy has one idea that re-weights everything under it.
It is stated above, and it is enforced as geometry, not as policy:

- The document on disk is **never "the text"**. It is an append-only world-line of small,
  independently sealed segments. There is no bulk-decrypt operation in the codebase — not
  hidden under a feature, not reachable by accident.
- The editor keeps awake **only the segments that overlap the screen** — a handful of pages
  out of a lifetime. When you scroll, pages trade places. When you stop, the app idles at
  zero work.
- What is awake can be wiped in milliseconds. What stays sealed cannot be taken.

Security stops being a wrapper and becomes the shape of the object. Scale stops being a
budget and becomes the topology of the data. That is the singularity this version is built
around: one invariant, load-bearing, measured, non-negotiable.

## 3. What this instrument is *for*

A sixty-year document is not a file you edit; it is a place you live. The requirements below
are constitutional. Any design that violates one is wrong regardless of how good it looks.

### I. Eyes-only plaintext
The amount of readable document held in memory is bounded by the viewport, not by the
document. One page or thirty million words — the same handful of segments is awake.
Protected pages are wiped the moment they leave the screen's neighborhood. A tripwire canary
rides the same allocator paths as real plaintext; if its ghost ever survives a wipe, the
instrument *admits it* — proof, not promise.

### II. One file, one lifetime
The `.wavefunction` is the document's entire history. Open it on any machine with your
passphrase and continue mid-sentence. There is no account, no server, no sync protocol, no
cloud dependency of ours. **Backup = copy the file.** Import to a new computer = copy the
file back. Because the file only ever grows by appending, incremental backup tools cost
almost nothing.

### III. History is the default state
The world-line never overwrites. Every day of writing, every version, is preserved as sealed
segments. Undo spans decades. A "version" is free — it is a bookmark into the log, not a
copy of the document. Sixty years of history is the format's natural condition, not a
feature that costs you anything.

### IV. Butter at any size
Ten pages and ten-thousand pages must feel identical per keystroke. Editing cost is
proportional to what changed, layout cost to what is visible, save cost to what moved.
Memory is constant in document size. Frame work is zero when nothing is on screen changes.
These are release gates measured by test hardware, not adjectives.

### V. Password-only, hardware-optional
The passphrase is the whole key. The post-quantum envelope (Argon2id → ML-KEM-1024 + X25519,
inherited from the audited v5.1.0 design) protects everything users without extra hardware
will ever need. A USB stick may exist later as an *optional extra deadbolt*, but it is never
required, never load-bearing, and removes nothing if absent.

### VI. Calm surface, familiar control
The page is quiet — generous margins, no chrome competing with the words. The toolbar is
where hands already know to look — the Google-Docs-shaped strip. One geometry engine feeds
both the paint and the caret, so what you see and what you edit can never disagree.

### VII. Proven, not promised
Every protection ships with the test that would fail if it regressed, and every document
specifying a protection survives an audit that tries to kill it before implementation begins.
Version 6 inherits the discipline that made v5.1.0 trustworthy and the UI specification that
v3 never had. Decorative security is worse than absent security: absent admits itself.

### VIII. Pure Rust
One language, top to bottom: core, crypto, document engine, and interface (GPUI, GPU-drawn,
macOS/Windows/Linux). Small, pinned, auditable dependency set. No fork-patches hiding in the
build. A user building from source should be able to say what every crate is for.

## 4. Constitutive budgets

These are the numbers the singularity pays with. They are targets enforced as gates in the
roadmap; a build that misses them is not v6.

| Law                              | Bound                                                            |
| -------------------------------- | ---------------------------------------------------------------- |
| Document size                    | ≥ 30,000,000 words, ≥ 10,000 pages, 60-year growth               |
| Open                             | Cold open of a maximum-size document in seconds, not minutes     |
| Resident plaintext               | Bounded by viewport neighborhood, independent of document length |
| Character echo                   | Under one display refresh, at any document offset, at any size   |
| Idle work                        | Zero repaint, zero background scanning when nothing changes      |
| Save                             | Touches appended segments only; atomic; survives power loss      |
| Wipe                             | Everything readable, wiped in milliseconds and *proven* gone     |
| Portability                      | One passphrase, one file, any machine, no account                |

## 5. What we refuse

Refusals are architecture too. Each of these has already gone wrong in a previous life of
this project, or in the industry's.

- **Refuse bulk decryption.** Any edit, export, search, or render that requires the whole
  document in plaintext is a design defect no matter what it enables.
- **Refuse mandatory hardware.** A security model that cannot fly with a password alone
  excludes most of humanity. Optional hardening: yes. Required: never.
- **Refuse UI without a spec.** The UI is specified and audited before code, with painted
  geometry and edited geometry forced through one source. v3 died of a caret that painted in
  one unit and clicked in another. That exact failure class is designed out.
- **Refuse impossible budgets.** Every millisecond and byte we claim is decomposed from
  measurements on target hardware, as v3 and v5.1 learned to do the hard way. Estimates are
  labeled estimates; gates are measurements.
- **Refuse decorative defenses.** No anti-debug theater performed at zero cost unless it
  binds to a real boundary. If a protection cannot name the attack it stops and the test
  that proves it, it does not ship — the v5.0-OMEGA audit ended seven such claims, and that
  list stays on the wall.
- **Refuse silence about weakness.** Anything known-broken, half-built, or ungated is written
  down in the audit trail where a user can read it, the way v5.1 documented its own gaps.

## 6. The line this version draws

Holonomy v6 is finished when a person can buy a plain laptop, receive a sixty-year
`.wavefunction` file, type one passphrase, and write — with the machine holding no more of
their life in the open than the page in front of her.

Nothing else is v6. Everything else is later.
