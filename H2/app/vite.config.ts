import { copyFileSync, mkdirSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { defineConfig, type Plugin } from 'vite'

const root = dirname(fileURLToPath(import.meta.url))

/**
 * Ship the entry module alongside the bundle.
 *
 * `core/verify.ts` fetches `/src/main.ts` and makes three **source-level** claims about the
 * shipped code: that no height constant is declared in the frontend, that the block count is not
 * derived from character counts, and that `estimateHeight` still exists. Those are the claims
 * that cannot be made by observing a running window, which is the whole point of reading the
 * file.
 *
 * Vite bundles `src/main.ts` into `assets/index-<hash>.js` and drops the original, so in the
 * production bundle `fetch('/src/main.ts')` returns the SPA fallback — `index.html` — and
 * `estimateHeight` is not in it. The verification then reports
 *
 *     could not locate estimateHeight in /src/main.ts; a renamed function would make
 *     every check below vacuous, which is worse than no check
 *
 * which is the guard working exactly as designed and failing for a reason that has nothing to
 * do with the product. It was 44/45 in the production smoke and green in dev, because the dev
 * server serves the real file.
 *
 * So the bundle carries the file the checks read. It is 60KB of source in an artefact nobody
 * loads over HTTP, and that is cheaper than a verification that can only run in development.
 */
function shipSourceEntry(): Plugin {
  return {
    name: 'holonomy:ship-source-entry',
    apply: 'build',
    closeBundle() {
      const out = resolve(root, 'dist/src/main.ts')
      mkdirSync(dirname(out), { recursive: true })
      copyFileSync(resolve(root, 'src/main.ts'), out)
    },
  }
}

export default defineConfig({
  plugins: [shipSourceEntry()],
  server: { port: 5184, strictPort: true },
  build: { target: 'es2022', outDir: 'dist', emptyOutDir: true },
})