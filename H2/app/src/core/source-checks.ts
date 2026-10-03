/**
 * Source-level checks, defined once and run by both hosts.
 *
 * # Why these exist at all
 *
 * Two things about this codebase are true only of the *source text* and cannot be
 * observed from a running program:
 *
 * - `estimateHeight` contains no character-derived fallback.
 * - the height constants are not written in the frontend.
 *
 * Both are properties of code, and the interesting failure is code that *looks* right.
 * A `?? Math.max(1, chars / 620)` reads as a harmless default and is the 225%-error path.
 *
 * So they are checked by reading the file. That is inherently fragile — a check written
 * against text is a check against whatever the text happens to be — and these two have
 * both been wrong at least once:
 *
 * - A whole-file regex for `blocks ??` flagged
 *   `sections[index]?.metrics.blocks ?? null`, a correct lookup guard in the test
 *   surface. An assertion that matches correct code is worse than none, because it
 *   trains you to ignore it.
 * - A whole-file regex for the derivation flagged `estimateHeight`'s own doc comment,
 *   which quotes the removed line in order to explain why it was removed.
 *
 * Both fixes are the same: scope the search to the function body and strip comments.
 *
 * # One definition, two hosts
 *
 * `app/test/scroll.ts` runs in headless Chromium and `app/src/core/verify.ts` runs
 * inside a real Tauri window on webkit2gtk, WKWebView and WebView2. Both need this, and
 * two copies of a source-scanning heuristic is exactly the arrangement where the copy
 * that is not running is the one that rots.
 */

/** The frontend entry point whose source is scanned. */
export const ENTRY_MODULE = '/src/main.ts'

/**
 * The body of `estimateHeight`, with comments stripped.
 *
 * Returns `null` when the function cannot be found, so a caller can report "the check
 * could not run" distinctly from "the check ran and found something". Those are
 * different failures and conflating them is how a renamed function turns into a passing
 * suite.
 */
export function estimateHeightBody(source: string): string | null {
  const start = source.indexOf('function estimateHeight')
  if (start < 0) return null
  const end = source.indexOf('\n}', start)
  if (end < 0) return null
  return stripComments(source.slice(start, end))
}

/** Remove block and line comments. */
export function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '')
}

/**
 * Whether `estimateHeight` derives a block count instead of reading one.
 *
 * Returns the offending fragment rather than a boolean, so a failure message can show
 * what was found. "still derives a block count" without the text is a thing to go and
 * look up.
 */
export function derivesBlockCount(body: string): string | null {
  // `/ 620` is the character-density divisor the removed fallback used. `??` catches a
  // nullish default written any other way.
  if (/\b620\b/.test(body)) return 'the 620-characters-per-block divisor'
  const nullish = body.match(/\?\?[^;]*/)
  if (nullish) return `a \`??\` default: ${nullish[0].trim().slice(0, 60)}`
  const derived = body.match(/Math\.(?:ceil|round)\([^)]*\/[^)]*\)/)
  if (derived) return `a derived count: ${derived[0]}`
  return null
}

/** Whether the frontend declares a height constant of its own. */
export function declaresHeightConstant(source: string): string | null {
  const found = source.match(/\bconst\s+(PX_PER_100_CHARS|PX_PER_BLOCK|SECTION_CHROME_PX|PX_PER_PARAGRAPH)\b/)
  return found ? found[0] : null
}
