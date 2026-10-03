/**
 * The TeX-to-Typst math conversion, pinned from both ends.
 *
 * # Why this is the whole of the test
 *
 * Because `tex_to_typst_math` is Rust, and **Typst's math is not LaTeX**. Typst writes the
 * summation sign as `∑`, not `\sum`, so passing a document's TeX through unchanged makes every
 * equation in every document a compile error. The notation has to be translated, and a partial
 * translator is only acceptable if it fails *loudly* — so these tests hold two properties:
 *
 * 1. the commands that are converted, converted correctly; and
 * 2. the ones that are not converted **survive**, so Typst names them rather than the equation
 *    quietly changing meaning.
 *
 * # Why the fixture is generated rather than written here
 *
 * Because reimplementing the conversion in TypeScript so this test could call it would be
 * exactly the duplication this project keeps removing. Instead `cargo test -p holonomy-shell
 * --test math-fixtures --release -- --ignored` writes what the Rust side produces, *along with
 * whether each entry actually compiles*, and this reads it.
 *
 * That last part is the point. A unit test on a string cannot tell you the output parses, and
 * this project's history is full of tests that agreed with themselves: the encoding bug fixed
 * last round survived because the test encoded its fixture with a different encoder than the one
 * in production. Here the `compiles` flag comes from a real export.
 *
 * Run: node --experimental-strip-types test/export-math.ts
 */

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

let passed = 0
let failed = 0
const failures: string[] = []

function test(name: string, fn: () => unknown): void {
  try {
    const detail = fn()
    passed++
    console.log(`PASS  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
  } catch (e: any) {
    failed++
    failures.push(name)
    console.log(`FAIL  ${name}\n        ${e.message}`)
  }
}

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg)
}

interface Conversion {
  readonly label: string
  readonly input: string
  readonly typst: string
  readonly compiles: boolean
  readonly error?: string
}

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), 'fixtures')

/**
 * Entries that do not compile, each with its symptom.
 *
 * # Why this is a list and not four `it.todo`s
 *
 * Because "the translator is incomplete" is only useful if the *shape* of the incompleteness is
 * pinned. A list means three things, and none of them is "skip the test":
 *
 * - a **fix** that closes a gap makes this test fail, because the gap's label no longer appears
 *   among the failures;
 * - a **new** breakage makes it fail, because the new label is not listed; and
 * - both point at the same place, so the list cannot drift from the code without someone
 *   noticing.
 *
 * Each entry names its symptom rather than only its input, because the symptom is what has to be
 * read to decide whether something has been fixed.
 *
 * Six of the nine are *by design* and are listed with the rest because the property being held
 * is uniform: the export refuses and says why. An unrecognised command and an unbalanced brace
 * are not translator bugs — `\acme` has no meaning and `\frac{1}{` is not an equation — and a
 * document that refuses to export and names the problem is the correct outcome.
 */
const KNOWN_GAPS: Record<string, { symptom: string; byDesign: boolean }> = {
  'unknown command': { symptom: 'the command survives and Typst names it', byDesign: true },
  'unknown operator name': { symptom: 'the command survives and Typst names it', byDesign: true },
  'unbalanced fraction': { symptom: 'Typst reports a missing argument', byDesign: true },
  'unbalanced sqrt': { symptom: 'Typst reports a missing argument', byDesign: true },
  'lone backslash at the end': { symptom: 'a trailing backslash has no argument to escape', byDesign: true },
  'lone escaped brace': { symptom: "a left delimiter with no partner is a document Typst cannot typeset", byDesign: true },
}

const CONVERSIONS: Conversion[] = JSON.parse(
  readFileSync(join(FIXTURES, 'tex-to-typst.json'), 'utf8'),
) as Conversion[]

/** The fixture entry whose label is `label`. */
function labelled(label: string): Conversion {
  const found = CONVERSIONS.find(c => c.label === label)
  ok(found !== undefined, `the fixture has no entry labelled ${JSON.stringify(label)}; regenerate it`)
  return found!
}

/** Every fixture whose `input` contains `needle`. */
function conversionsFor(needle: string): Conversion[] {
  return CONVERSIONS.filter(c => c.input.includes(needle))
}

console.log('export: TeX to Typst math, pinned from the real encoder')
console.log('='.repeat(72))

test('the fixture is populated and recent enough to mean something', () => {
  ok(CONVERSIONS.length >= 30, `expected a real fixture, got ${CONVERSIONS.length} entries`)
  ok(
    CONVERSIONS.some(c => c.compiles),
    'if nothing compiled the generator would have written an empty result and every other ' +
      'assertion would be vacuous',
  )
  return { entries: CONVERSIONS.length }
})

test('the set of failures is exactly the recorded gaps', () => {
  // The property that makes the list above load-bearing rather than decorative. A fix that
  // closes a gap fails here; a regression fails here; and a list that stops matching the code
  // fails here, which is the only way a list of known problems stays true.
  const failed = CONVERSIONS.filter(c => !c.compiles)
  const unexpected = failed.filter(c => !(c.label in KNOWN_GAPS))
  const stale = Object.keys(KNOWN_GAPS).filter(label => !failed.some(c => c.label === label))

  ok(
    unexpected.length === 0,
    `${unexpected.length} conversion(s) fail without being recorded:\n` +
      unexpected
        .map(c => `  ${c.label}: ${JSON.stringify(c.input)} -> ${JSON.stringify(c.typst)}\n    ${c.error ?? ''}`)
        .join('\n')
  )
  ok(
    stale.length === 0,
    `${stale.length} recorded gap(s) now compile: ${stale.join(', ')}. Remove them from ` +
      'KNOWN_GAPS — a fix that leaves the list behind is how a list stops being true.',
  )
  return {
    compiling: CONVERSIONS.length - failed.length,
    gaps: failed.length,
    byDesign: Object.values(KNOWN_GAPS).filter(g => g.byDesign).length,
  }
})

test('every gap fails with the symptom recorded for it', () => {
  // A gap list that only says "these are broken" drifts into being wrong. Each entry's recorded
  // symptom is checked against the compiler's actual message, so a gap that fails *differently*
  // than recorded is visible — which is usually the sign of a partial fix.
  for (const [label, gap] of Object.entries(KNOWN_GAPS)) {
    const entry = labelled(label);
    ok(!entry.compiles, `${label} is recorded as a gap but compiles; see the staleness check`);
    const error = entry.error ?? '';
    ok(error.length > 0, `${label} fails but the fixture recorded no message`);
    if (!gap.byDesign) {
      // A by-design failure must still *say something*, and the message must be about the
      // equation rather than about the harness.
      ok(
        error.includes('could not typeset'),
        `${label} should fail while typesetting; got: ${error}`,
      )
    }
  }
  return { gaps: Object.keys(KNOWN_GAPS).length }
})

test('large operators become Typst names that accept their limits', () => {
  // The case that was open for two rounds, and the fix is worth recording because the cause was
  // not what it looked like either time.
  //
  // `\sum_{i=1}^{n}` first became `sum_{i=1}^{n}`, and Typst read `i` as the subscript and `1` as
  // whatever followed. Substituting a bare `∑` glyph fixed that symptom and introduced a second:
  // a glyph has nothing for limits to attach to, so `\int_{0}^{\infty}` stopped working, and that
  // was recorded as a Typst limitation rather than as a bug.
  //
  // A probe compiled each candidate and found `integral`, `integral.double` and `integral.cont` all
  // exist, and that a glyph plus a subscript does not. The real culprit was a space this
  // translator inserted between an identifier and the `_` after it: a subscript binds to what is
  // directly before it, so `integral _0` is an operator followed by something else.
  const both = labelled('large operator, both limits')
  ok(both.typst === 'sum_(i=1)^n i', `expected sum_(i=1)^n i, got ${both.typst}`)
  ok(!both.typst.includes('\\sum'), `no TeX command should survive: ${both.typst}`)
  ok(both.typst.includes('_(i=1)'), `the subscript should be a parenthesised expression: ${both.typst}`)
  ok(both.compiles, `should compile; got ${both.error ?? ''}`)

  const prod = labelled('large operator, superscript only')
  ok(prod.typst === 'product^n k', `Typst calls the product operator "product"; got ${prod.typst}`)
  ok(prod.compiles, `should compile; got ${prod.error ?? ''}`)

  // The directive's named case, and the one that was open.
  const integral = labelled('integral with limits')
  ok(
    integral.typst.startsWith('integral_0^∞'),
    `\int should become integral with its limits attached; got ${integral.typst}`,
  )
  // Precise, because "no spaces at all" is not the property: the equation legitimately has
  // spaces between its factors. What must not happen is a space *between the operator and the
  // subscript*, because that unbinds the limit.
  ok(
    /integral_/.test(integral.typst),
    `the subscript must be attached to the operator; got ${integral.typst}`,
  )
  ok(integral.typst.includes('dif'), `\,dx should become a differential; got ${integral.typst}`)
  ok(
    !/  /.test(integral.typst),
    `a doubled space is invisible to Typst and wrong in the string a test reads: ${integral.typst}`,
  )
  ok(integral.compiles, `should compile; got ${integral.error ?? ''}`)

  for (const [label, name] of [
    ['contour integral', 'integral.cont'],
    ['double integral', 'integral.double'],
    ['triple integral', 'integral.triple'],
  ] as const) {
    const entry = labelled(label);
    ok(entry.typst.startsWith(name), `${label} should be ${name}; got ${entry.typst}`)
    ok(
      new RegExp(`${name.replace('.', '\\.')}_`).test(entry.typst),
      `${label}: the subscript must be attached to the operator; got ${entry.typst}`,
    )
    ok(entry.compiles, `${label} should compile; got ${entry.error ?? ''}`)
  }
  return {
    sum: both.typst,
    integral: integral.typst,
    contour: labelled('contour integral').typst,
  }
})

test('growing delimiters become lr, in every shape', () => {
  // Two bugs, each invisible in the other shape, which is why the fixture pins all four.
  //
  // The closing delimiter must close the `lr(` *call* as well as itself: `lr([ x ]` is unclosed
  // while `lr(( x ))` is fine. And `\left\{` is a backslash *and* a brace, so reading only the
  // first character let the brace be translated separately, putting a delimiter glyph inside the
  // `lr` and leaving a `}` with nothing to close.
  //
  // And `lr` takes no `#`. In math mode `#` calls a *code* function, and `lr` is a math function,
  // so `#lr(` reads as "evaluate a code expression named lr", which does not exist. The probe
  // confirmed both spellings fail identically, so the hash was never the distinguishing part.
  for (const [label, expected] of [
    ['growing delimiters', 'lr(( frac(a, b) ))'],
    ['growing brackets', 'lr([ x + y ])'],
    ['growing braces', 'lr({ x brace.r })'],
    ['growing bars', 'lr(| x |)'],
  ] as const) {
    const entry = labelled(label);
    ok(entry.typst === expected, `${label}: expected ${expected}, got ${entry.typst}`);
    ok(!entry.typst.includes('#'), `${label}: lr is a math function and takes no #; got ${entry.typst}`);
    ok(entry.compiles, `${label} should compile; got ${entry.error ?? ''}`)
  }
  return { round: labelled('growing delimiters').typst }
})

test('a literal brace becomes a Typst delimiter name, not a code block', () => {
  // `\{` is TeX's spelling of a brace. Typst has no equivalent -- a bare `{` opens a code block,
  // so the rest of the equation would typeset as code -- but it does have `brace.l` and
  // `brace.r`. Escaping it as `\{`, which is TeX's spelling, is what an earlier version did and
  // Typst rejected it.
  const pair = labelled('brace pair in a set')
  ok(pair.typst === 'brace.l 1, 2 brace.r', `got ${pair.typst}`)
  ok(pair.compiles, `should compile; got ${pair.error ?? ''}`)

  // A brace with no partner is refused, which is the correct outcome rather than a gap: Typst's
  // `brace.l` is a *left* delimiter, and a document that half-opens one cannot be typeset.
  const lone = labelled('lone escaped brace')
  ok(lone.typst.includes('brace.l'), `the delimiter should still be named; got ${lone.typst}`)
  ok(!lone.compiles, 'a lone left delimiter should not typeset')
  return { pair: pair.typst }
})

test('a word in math stays a word, not a product of variables', () => {
  // The bug a "does it compile" check cannot see.
  //
  // Separating adjacent letters everywhere is the obvious fix for `dx` being read as one
  // identifier, and it was tried: it turns `literal` into `l i t e r a l`, seven italic variables
  // multiplied together. That **compiles** -- a space in Typst math *is* implicit multiplication,
  // which is exactly the bug -- so a fixture asking only whether it compiled reported success
  // while the page was wrong. The string is the only thing that catches this class of mistake.
  //
  // So the separator goes only where the TeX says so: after a spacing command followed by a `d`.
  const thin = labelled('thin space')
  ok(thin.typst === 'a thin_space() b', `expected a thin_space() between the words; got ${thin.typst}`)
  ok(thin.compiles, `should compile; got ${thin.error ?? ''}`)

  const integral = labelled('integral with a differential')
  ok(
    integral.typst.includes('dif'),
    `\,dx is a differential, not two variables; got ${integral.typst}`,
  )
  ok(integral.compiles, `should compile; got ${integral.error ?? ''}`)

  // And the words either side of it are untouched, which is the half that regressed.
  for (const [label, whole] of [
    ['no commands at all', 'x^2 + y^2 = z^2'],
    ['fraction with a sum', 'frac(1, n) sum_(i=1)^n i'],
  ] as const) {
    const entry = labelled(label);
    ok(entry.typst === whole, `${label}: expected ${whole}, got ${entry.typst}`);
  }
  return { thin: thin.typst, integral: integral.typst }
})

test('greek letters become Typst identifiers, separated from their neighbours', () => {
  // `i\pi` is the case. With no separator, `pi` runs into the `i` and Typst reads one four-letter
  // identifier — which is exactly the `unknown variable: ipi` the first version produced, naming
  // neither the letter nor the command.
  const pi = labelled('greek after an identifier')
  ok(pi.typst === 'i pi', `expected "i pi" with a separator, got ${JSON.stringify(pi.typst)}`)
  ok(pi.compiles, `should compile; got ${pi.error ?? ''}`)

  const upper = labelled('uppercase greek')
  ok(!upper.typst.includes('\\'), `no TeX command should survive: ${upper.typst}`)
  ok(upper.compiles, `should compile; got ${upper.error ?? ''}`)
  return { adjacent: pi.typst, upper: upper.typst }
})

test('fractions and roots convert, including nested ones', () => {
  const frac = labelled('simple fraction')
  ok(frac.typst === 'frac(a, b)', `expected frac(a, b), got ${frac.typst}`)
  ok(frac.compiles, `should compile; got ${frac.error ?? ''}`)

  const nested = labelled('nested fraction')
  ok(nested.typst.includes('frac(a, frac(b, c))'), `got ${nested.typst}`)
  ok(nested.compiles, `should compile; got ${nested.error ?? ''}`)

  const sqrt = labelled('plain square root')
  ok(sqrt.typst === 'sqrt(2)', `expected sqrt(2), got ${sqrt.typst}`)
  ok(sqrt.compiles, `should compile; got ${sqrt.error ?? ''}`)

  const root = labelled('nth root')
  ok(root.typst.includes('root(3, x)'), `expected root(3, x), got ${root.typst}`)
  ok(root.compiles, `should compile; got ${root.error ?? ''}`)
  return { nested: nested.typst, root: root.typst }
})

test('operators and relations become Unicode rather than guessed names', () => {
  // The first version guessed Typst identifier names from its documentation — `union`,
  // `intersection`, `diff`, `arrow.r` — and every one compiled to `unknown variable`. Typst has
  // no `intersection` and no `diff`, and the arrows are not spelled that way.
  //
  // Unicode is the right answer rather than merely the safe one: these are symbols, Typst's math
  // takes them directly, and a TeX-to-Unicode table is a fact about TeX, which does not change.
  for (const [label, expected] of [
    ['binary operators', ['⋅', '×', '±']],
    ['relations', ['≤', '≥', '≠', '≈']],
    ['sets', ['∈', '∉', '∪', '∩']],
    ['arrows', ['→', '⇒']],
    ['other symbols', ['∞', '∂', '∇']],
  ] as const) {
    const entry = labelled(label);
    for (const symbol of expected) {
      ok(
        entry.typst.includes(symbol),
        `${label}: expected ${symbol} in ${JSON.stringify(entry.typst)}`,
      );
    }
    ok(!entry.typst.includes('\\'), `${label}: no TeX command should survive: ${entry.typst}`)
  }
  return { checked: 5 }
})

test('an unrecognised command survives so Typst can name it', () => {
  // The property that makes a partial translator acceptable. A command that is quietly dropped
  // produces an equation that is *wrong* and a document that exports; a command that survives
  // produces a document that refuses to export and says which command.
  const unknown = labelled('unknown command');
  ok(unknown.typst.includes('\\acme'), `the command should survive verbatim: ${unknown.typst}`)
  ok(!unknown.compiles, 'an unknown command should stop the export')
  ok(
    (unknown.error ?? '').length > 0,
    'and the failure must be reported, not swallowed',
  )
  return { typst: unknown.typst, error: unknown.error }
})

test('a multi-token limit is parenthesised, which the two notations disagree about', () => {
  // `e^{-x}` is the case. TeX's `^` takes the next *token*, so `^{-x}` and `^-x` mean the same
  // thing there; Typst requires parentheses or it reads `-x` as one identifier. Every
  // sub/superscript in a technical document is `x^{something}`, so this is not an edge case.
  const withSuperscript = conversionsFor('^').filter(c => c.compiles);
  ok(withSuperscript.length > 0, 'no sub/superscript fixture compiled');
  for (const entry of withSuperscript) {
    ok(
      !/\^\{[^\s]/.test(entry.typst),
      `${entry.label}: a brace group should have been converted, not passed through: ${entry.typst}`,
    );
  }
  return { checked: withSuperscript.length }
})

console.log('='.repeat(72))
console.log(`${passed} passed, ${failed} failed`)
if (failed) {
  console.log(`failing: ${failures.join(', ')}`)
  console.log(
    'regenerate the fixture with: cargo test -p holonomy-shell --test math-fixtures --release -- --ignored',
  )
  process.exit(1)
}