/**
 * Compile-time proof that a caller cannot omit the block count.
 *
 * # Why this is not in the browser suite
 *
 * `metrics.blocks` being required is a *type* property. A browser runtime cannot
 * observe it, and two attempts to assert it from `test/scroll.ts` failed in
 * instructive ways: a regex over `registry.ts` missed because the declaration is
 * split across a doc comment and a type literal, and reading the module over HTTP
 * missed because esbuild erases interfaces entirely. Neither could ever have
 * worked.
 *
 * So this asks the actual question of the actual compiler: write a file that omits
 * `blocks`, compile it against the real `tsconfig.json`, and require the compiler
 * to reject it.
 *
 * # Why that is worth doing rather than trusting `npm run typecheck`
 *
 * `npm run typecheck` does not fail today, because every call site was fixed when
 * the field was made required — five of them, all found by the compiler at that
 * time. But "the code currently compiles" is not the same claim as "the type is
 * required". Making a field optional again, or adding a new call site, would pass
 * the first check and fail the second. This test is what makes the requirement
 * itself the thing under test.
 *
 * Run: node --experimental-strip-types test/blocks-required.ts
 */

import { execFileSync } from 'node:child_process'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const appDir = join(here, '..')

let passed = 0
let failed = 0

function ok(cond: unknown, msg: string): void {
  if (!cond) {
    failed++
    console.log(`FAIL  ${msg}`)
  } else {
    passed++
    console.log(`PASS  ${msg}`)
  }
}

/**
 * Compile a snippet against the project's real configuration.
 *
 * Returns the compiler's diagnostics. Never throws on a compile error — a failure
 * to compile is the result under test in one direction and the pass condition in
 * the other, so the error text is data.
 */
function compile(snippet: string): { ok: boolean; output: string } {
  const dir = mkdtempSync(join(tmpdir(), 'holonomy-blocks-'))
  try {
    const file = join(dir, 'probe.ts')
    // An absolute import of the real type, so this tests the actual declaration
    // rather than a copy of it that could drift.
    writeFileSync(
      file,
      `import type { SectionRecord } from ${JSON.stringify(join(appDir, 'src/core/registry.ts'))}\n\n${snippet}`,
    )
    const output = execFileSync(
      'npx',
      [
        'tsc',
        '--noEmit',
        '--strict',
        '--target',
        'es2022',
        '--module',
        'esnext',
        '--moduleResolution',
        'bundler',
        '--skipLibCheck',
        file,
      ],
      { cwd: appDir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
    )
    return { ok: true, output }
  } catch (e: any) {
    // tsc exits non-zero on diagnostics, which is the expected path here.
    return { ok: false, output: `${e.stdout ?? ''}${e.stderr ?? ''}` }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

// --- the check -------------------------------------------------------------

console.log('block count is required at compile time')
console.log('========================================================')

{
  // The negative case. A section with no block count must not compile.
  const r = compile(`
    export const bad: SectionRecord = {
      id: 's0',
      json: { type: 'doc', content: [] },
      metrics: { words: 100, marks: 0, chars: 600 },
      loaded: true,
      dirty: false,
    }
  `)
  ok(!r.ok, 'a SectionRecord without `blocks` must not compile')
  ok(
    /blocks/.test(r.output),
    `the diagnostic should name the missing property, got:\n${r.output.slice(0, 400)}`,
  )
}

{
  // The positive case, so the check above is not passing for an unrelated reason.
  const r = compile(`
    export const good: SectionRecord = {
      id: 's0',
      json: { type: 'doc', content: [] },
      metrics: { words: 100, marks: 0, chars: 600, blocks: 3 },
      loaded: true,
      dirty: false,
    }
  `)
  ok(r.ok, `a SectionRecord with a block count must compile, got:\n${r.output.slice(0, 400)}`)
}

{
  // The whole point, stated as the error a caller would actually see.
  const r = compile(`
    export function estimate(m: SectionRecord['metrics']): number {
      return (m as any).blocks ?? 1
    }
  `)
  ok(r.ok, 'reading a required field must not itself be a type error')
}

console.log('========================================================')
console.log(`${passed} passed, ${failed} failed`)
process.exit(failed === 0 ? 0 : 1)