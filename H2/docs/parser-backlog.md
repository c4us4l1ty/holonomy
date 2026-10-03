# The LaTeX math backlog, after v1

Where the translator stops, and what it would take to go further.

# The one property, and why it is the only one that matters here

**An equation Holonomy cannot render becomes an error that names the problem, never a
different equation.**

This is what the 42 fixtures hold, and it is a property about *failure*, not about coverage.
A converter that quietly dropped an unrecognised command would produce a document that
exports successfully and is wrong, and nothing downstream — not the PDF, not the reader, not
the user — could tell. So the six inputs that do not compile are not bugs to be scheduled;
they are the six places where the property is exercised, and `app/test/export-math.ts` asserts
the failing set is *exactly* the list in `KNOWN_GAPS`, so a fix and a regression both turn the
suite red.

The six, and why each is refused rather than rendered:

| Input | Why it has no meaning |
| --- | --- |
| `\frac{1}{` | no second argument |
| `\sqrt{` | no radicand |
| a trailing `\` | not a command |
| a lone `\{` | not a pair |
| `\acme{x}` | no such command; Typst is asked and says so |
| an unknown operator name | no such operator |

Everything else compiles: 36 of 42, including the four constructs this backlog was originally
written to excuse — `\int` with limits, `\left(...\right)` in its round and brace forms, and a
literal brace in math. Those were closed in the previous round and each of them had been
recorded as "a Typst limitation" first, which turned out to be wrong in all four cases. Two
were bugs in this translator — a space inserted before a subscript, and a `\,` that emitted a
literal backslash and did nothing — and both *compiled*. **A thing that compiles is not the
same as a thing that is right**, and three separate defects in this translator passed a
compile-only check.

# Why a backlog rather than a work queue

Because none of this is blocking, and a work queue implies a date. The decision that matters
is which of these are *ever* worth doing, and the honest answer for most of them is "only as
part of replacing the translator".

# What replacing the translator means

`tex_to_typst_math` is a single-pass string rewriter. It reads a command name, reads its
arguments, and emits Typst markup. That is sufficient for the constructs in the fixture set and
insufficient for everything in the next section, because these need to know where a *group*
ends before they can emit anything:

- **Matrices** (`\begin{matrix}`, `\pmatrix`, `bmatrix`) — needs array/column structure, and
  Typst's `matrix()` takes cells positionally, so the rewriter has to count `&` and emit one
  cell per column.
- **Cases** (`\begin{cases}`, `align`, `aligned`) — the same, plus per-row alignment points,
  and Typst's `cases()` is a function rather than markup.
- **Any `\begin{...}` environment** — needs a dispatch on the environment name and an arity
  for each. Today an unrecognised `\begin` produces a Typst error naming an unknown variable,
  which is the right failure but not a useful message.

None of these are string substitutions. Each is a small parser over the argument list, and the
honest description of the work is "a real (small) TeX math parser with a Typst backend", not
"add some cases".

# The backlog, in the order it would actually be built

1. **`\begin{pmatrix}` / `bmatrix` / `vmatrix`** — the most common by a wide margin in
   mathematics. Needs column counting only; no alignment points.
2. **`cases` and `align`** — one alignment point per row, which is the same counting plus one
   extra split. `align` is common in derivations.
3. **A group-aware argument reader** — the enabling change for 1 and 2, and the only item here
   that is genuinely a piece of infrastructure. Everything else is easier once `\begin` can see
   where its environment ends.
4. **Macros** (`\newcommand`, `\def`) — out of scope until there is an expansion pass. Not
   even listed as a goal: a word processor that expands user macros needs a TeX interpreter,
   not a translator, and half of one is a bad thing to ship.
5. **`\label`/`\ref`** — not a translation problem at all. Needs cross-references through the
   document model, so it belongs with the section manifest rather than with the maths.

# What is deliberately *not* on this list

- **Every command Typst spells differently.** The translator emits Unicode rather than Typst
  identifiers — `\int` becomes `integral`, `\alpha` becomes `α` — because a TeX-to-Unicode
  table is a fact about TeX that does not change between releases, and Typst's identifier list
  does. Adding a command is then a table entry, not a code change, and it is a five-line job.
  The list is in `translate.rs` and is deliberately not exhaustive.
- **Anything that would make an equation *mean* something different.** A limit that moves from
  the baseline to the side, a `\left` that grows the wrong pair, a brace that becomes a
  parenthesis: each of these compiles, exports, and is wrong. Every one of them is worse than
  an error. If a future change makes one of them easier, that is a reason to be more careful,
  not less.

# How this is kept honest

- `crates/holonomy-shell/tests/math-fixtures.rs` — `KNOWN_GAPS` and the `#[ignore]`d generator
  that compiles every input for real.
- `app/test/export-math.ts` — asserts the failing set is exactly `KNOWN_GAPS`, so this
  document and the two lists cannot drift apart without a red test.
- `app/test/fixtures/tex-to-typst.json` — the generated evidence, with each entry's real
  `compiles` flag and Typst's own error text.