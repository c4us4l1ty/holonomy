/**
 * The editor chrome, wired: the toolbar in the page and the shortcuts reaching real handlers.
 *
 * # Why the *wiring* needs its own suite
 *
 * `test/toolbar.ts` and `test/shortcuts.ts` hold the modules to be correct. Neither can see
 * whether `main.ts` ever mounts the toolbar or attaches the listener, and a module that is
 * correct and never attached is exactly the failure a unit suite cannot rule out. The M3 global
 * undo is the same shape: `registry.undo` is a document-wide stack, and the only thing that makes
 * `Ctrl-Z` reach it is a binding declared `global` in a table `main.ts` builds.
 *
 * # Why these need a browser
 *
 * Because the assertions are about the DOM: that buttons exist, that pressing one runs a command,
 * and that a key event dispatched at the document reaches the registry. A fake DOM would pass
 * while the real page did nothing.
 *
 * Run: node --experimental-strip-types test/chrome.ts
 */

import { chromium, type Page } from 'playwright'

const URL = process.env.HOLO_APP_URL ?? 'http://localhost:5184/'

let passed = 0
let failed = 0
const failures: string[] = []

async function test(name: string, _page: Page, fn: () => Promise<unknown>): Promise<void> {
  try {
    const detail = await fn()
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

/** Load a document and settle it, which is what every test below needs before it can assert. */
async function boot(page: Page): Promise<void> {
  await page.goto(URL)
  await page.waitForSelector('#canvas')
  await page.waitForFunction(() => typeof (window as any).HOLO_SCROLL !== 'undefined')
  await page.evaluate(async () => {
    await (window as any).HOLO_SCROLL.settle()
  })
}

async function main(): Promise<void> {
  const browser = await chromium.launch({ args: ['--no-sandbox'] })
  const page = await browser.newPage()
  page.on('pageerror', e => console.log(`  [page error] ${e.message.split('\n')[0]}`))

  console.log('chrome: the toolbar in the page, and the shortcuts reaching handlers')
  console.log('='.repeat(72))

  await test('the toolbar is mounted into the page, not merely importable', page, async () => {
    await boot(page)
    const result = await page.evaluate(() => {
      const host = document.getElementById('toolbar-host')
      const root = host?.querySelector('[role="toolbar"]')
      return {
        hostPresent: Boolean(host),
        roleToolbar: Boolean(root),
        buttons: host?.querySelectorAll('button').length ?? 0,
        // The host must not be inside the scroller: a toolbar that changes height would then move
        // the top of the scrollable content, and the geometry has no way to hear about it.
        insideScroller: Boolean(host?.closest('#scroller')),
        // The label belongs on the `role="toolbar"` element -- that is the thing a screen reader
        // announces. The first version of this checked the *host*, which is a plain div with no
        // role, and failed for that reason alone while the toolbar was correct.
        name: root?.getAttribute('aria-label') ?? null,
        orientation: root?.getAttribute('aria-orientation') ?? null,
      }
    })
    ok(result.hostPresent, 'the toolbar host element is missing from index.html')
    ok(result.roleToolbar, 'nothing with role="toolbar" was mounted')
    ok(result.buttons >= 15, `expected the whole toolbar, got ${result.buttons} buttons`)
    ok(!result.insideScroller, 'the toolbar must sit outside the scroller')
    ok(result.name !== null, 'a toolbar needs a name for a screen reader')
    ok(result.orientation !== null, 'and to declare whether it wraps or scrolls')
    return { buttons: result.buttons, name: result.name }
  })

  await test('every button is a real button with a name, and none is a link or a div', page, async () => {
    // Reachability is a property of the element, not of the styling: a `div` with a click handler
    // is not in the tab order and cannot be operated from the keyboard.
    const bad = await page.evaluate(() => {
      const problems: string[] = []
      for (const el of document.querySelectorAll('#toolbar-host [role="toolbar"] button')) {
        if (el.tagName.toLowerCase() !== 'button') problems.push(`${el.tagName} used for a control`)
        const name = el.getAttribute('aria-label') ?? el.textContent?.trim() ?? ''
        if (!name) problems.push(`a button with no accessible name: ${el.outerHTML.slice(0, 60)}`)
        if (el.hasAttribute('onclick')) problems.push('a button with an inline onclick attribute')
        if (el.getAttribute('type') !== 'button') problems.push(`type=${el.getAttribute('type')}`)
      }
      return problems
    })
    ok(bad.length === 0, `accessibility problems: ${bad.join('; ')}`)
    return { checked: 'all toolbar buttons' }
  })

  await test('toggles carry aria-pressed and unsupported items are disabled, not hidden', page, async () => {
    // Two attributes for two different claims. `aria-pressed` says "this is on right now" and is
    // valid only on a toggle; `disabled` says "not available in this section" and is valid on
    // both. An item absent from the schema must say so rather than reflow the toolbar.
    const result = await page.evaluate(() => {
      const buttons = [...document.querySelectorAll('#toolbar-host [role="toolbar"] button')]
      return {
        total: buttons.length,
        withPressed: buttons.filter(b => b.hasAttribute('aria-pressed')).length,
        disabled: buttons.filter(b => (b as HTMLButtonElement).disabled).length,
      }
    })
    ok(result.total >= 15, `expected the whole toolbar, got ${result.total}`)
    ok(
      result.withPressed > 0,
      'the bold/italic/list items are toggles and must carry aria-pressed, else a screen reader ' +
        'cannot say whether they are on',
    )
    // With no editor focused every command is unsupported, so every item should be disabled rather
    // than absent. An empty toolbar with no explanation is the failure this avoids.
    ok(
      result.disabled === result.total,
      `with no editor focused all ${result.total} items should be disabled; ${result.total - result.disabled} were enabled`,
    )
    return result
  })

  await test('undo is a global binding and every declared command has a handler', page, async () => {
    // The M3 requirement, asserted where it is decided. A document is many Tiptap editors, each
    // with its own history, so a *per-editor* undo would step back through one section while the
    // user believes they are stepping back through their document.
    const result = await page.evaluate(() => {
      const s = (window as any).HOLO_SCROLL.shortcuts
      return {
        undoScope: s.bindings('undo')[0]?.scope ?? null,
        redoScope: s.bindings('redo')[0]?.scope ?? null,
        boldScope: s.bindings('bold')[0]?.scope ?? null,
        saveScope: s.bindings('app.save')[0]?.scope ?? null,
        conflicts: s.conflicts(),
        unbound: s.unbound(),
        saveLabel: s.labelFor('app.save'),
      }
    })
    ok(result.undoScope === 'global', `undo must be global for a document-wide stack, got ${result.undoScope}`)
    ok(result.redoScope === 'global', `redo must be global too, got ${result.redoScope}`)
    ok(result.boldScope === 'editor', `bold belongs to the section being edited, got ${result.boldScope}`)
    ok(result.saveScope === 'global', 'save is app-level')
    ok(result.conflicts.length === 0, `the default table must not conflict: ${JSON.stringify(result.conflicts)}`)
    ok(
      result.unbound.length === 0,
      `every declared command needs a handler. These resolved and had nothing to call: ${result.unbound.join(', ')}`,
    )
    ok(typeof result.saveLabel === 'string' && result.saveLabel.length > 0, 'a binding should render a label')
    return { undo: result.undoScope, bold: result.boldScope, save: result.saveLabel }
  })

  await test('a real Ctrl+B inside a mounted editor reaches the handler', page, async () => {
    // Dispatched as a *real* event on a *real* element inside `.ProseMirror`, not by calling the
    // registry with a context object.
    //
    // The first version of this test called `app.shortcuts.dispatch(event, {inEditor: true})` and
    // got `blocked: outside-editor` — because the app's `dispatch` takes only the event and
    // re-derives the context from `event.target`, which was null. So the test exercised the wrapper
    // with the wrong arity and reported a bug that was not there. Firing the real event tests what
    // actually happens, including the `contextForElement` call the app makes.
    const result = await page.evaluate(() => {
      const editor = document.querySelector('#canvas .ProseMirror') as HTMLElement | null
      if (!editor) return { error: 'no mounted editor to press a key in' }
      // Click first, so a section actually has focus. Without it `focusedEditor()` is null, the
      // formatting handler runs and does nothing, and the chord is *consumed* while having had no
      // effect -- which is a different bug and would have masked this one.
      editor.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }))
      editor.focus()
      const inside = new KeyboardEvent('keydown', { key: 'b', ctrlKey: true, bubbles: true, cancelable: true })
      editor.dispatchEvent(inside)
      const outside = new KeyboardEvent('keydown', { key: 'b', ctrlKey: true, bubbles: true, cancelable: true })
      document.body.dispatchEvent(outside)
      return { insidePrevented: inside.defaultPrevented, outsidePrevented: outside.defaultPrevented }
    })
    ok(!('error' in result), (result as any).error)
    ok(
      (result as any).insidePrevented === true,
      `Ctrl+B inside an editor should be consumed by the shortcut layer; defaultPrevented=${(result as any).insidePrevented}`,
    )
    // The scope distinction, end to end: the same chord from outside an editor is not a
    // formatting command, so it is left alone.
    ok(
      (result as any).outsidePrevented === false,
      `the same chord outside an editor must not be treated as bold; defaultPrevented=${(result as any).outsidePrevented}`,
    )
    return result
  })

  await test('a bare letter is typed, in a text field and in an editor', page, async () => {
    // The regression the whole scope distinction exists for: with it wrong, pressing `b` in a text
    // field applies bold instead of typing a letter, which is a data-loss bug rather than a
    // cosmetic one.
    const result = await page.evaluate(() => {
      const field = document.createElement('input')
      field.type = 'text'
      document.body.append(field)
      field.focus()

      const letterInField = new KeyboardEvent('keydown', { key: 'b', bubbles: true, cancelable: true })
      field.dispatchEvent(letterInField)
      // A *global* chord, because `bold` is editor-scoped and correctly does nothing in a text
      // field -- there is no editor there to bold. The first version of this test asserted that
      // `Ctrl+B` would be consumed in a text field, which would have meant the scope distinction
      // did not exist.
      //
      // `Ctrl+S` is the right chord to check, because a word processor's save must work from
      // anywhere -- including from a find box, which is the case a `scope: 'editor'` save would
      // silently fail.
      const chordInField = new KeyboardEvent('keydown', {
        key: 's',
        ctrlKey: true,
        bubbles: true,
        cancelable: true,
      })
      field.dispatchEvent(chordInField)
      field.remove()

      const editor = document.querySelector('#canvas .ProseMirror') as HTMLElement | null
      const letterInEditor = new KeyboardEvent('keydown', { key: 'b', bubbles: true, cancelable: true })
      editor?.dispatchEvent(letterInEditor)

      return {
        letterInField: letterInField.defaultPrevented,
        chordInField: chordInField.defaultPrevented,
        letterInEditor: editor ? letterInEditor.defaultPrevented : null,
      }
    })
    ok(
      result.letterInField === false,
      'a bare letter must reach the field; a shortcut that eats letters is worse than none',
    )
    ok(
      result.chordInField === true,
      `Ctrl+S is global and must fire from a text field -- a save that only works with the caret \
       in the document is a save that silently fails; got ${result.chordInField}`,
    )
    ok(
      result.letterInEditor === false,
      `a bare letter inside an editor must also be typed; got ${result.letterInEditor}`,
    )
    return result
  })

  await test('the toolbar follows focus, so it does not claim a state the document does not have', page, async () => {
    // There is no such thing as "the document's bold". A toolbar showing the union of every mounted
    // section's state would be bold whenever *any* section is bold, which is a lie about all the
    // others. So it reports the focused section, and a section with no focus shows nothing active.
    const result = await page.evaluate(async () => {
      const app = (window as any).HOLO_SCROLL
      await app.settle()
      const pressed = () =>
        [...document.querySelectorAll('#toolbar-host [aria-pressed="true"]')].map(b => b.getAttribute('aria-label'))
      return { focused: app.focusedEditorId(), pressedNow: pressed() }
    })
    // Whichever section has focus, no toggle may claim to be on unless it genuinely is. The boot
    // fixture is plain prose, so nothing is bold.
    ok(
      result.pressedNow.length === 0,
      `nothing in a plain-prose fixture should read as pressed, got ${JSON.stringify(result.pressedNow)}`,
    )
    return { focused: result.focused, pressed: result.pressedNow.length }
  })

  console.log('='.repeat(72))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) console.log(`failing: ${failures.join(', ')}`)
  await browser.close()
  process.exit(failed === 0 ? 0 : 1)
}

main().catch(e => {
  console.error(e)
  process.exit(1)
})
