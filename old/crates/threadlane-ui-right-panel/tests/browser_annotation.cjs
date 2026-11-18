// Run with Node's test runner and Playwright (including its WebKit browser):
// NODE_PATH=<directory containing playwright> node --test crates/threadlane-ui-right-panel/tests/browser_annotation.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test, before, after } = require('node:test');
const { webkit } = require('playwright');

// Exercise the exact script injected by BrowserView, not a copy of the picker.
const source = readFileSync(join(__dirname, '../src/browser/scripts.rs'), 'utf8');
const install = source.split('const ANNOTATE_INSTALL: &str = r##"')[1].split('"##;')[0];
let browser;
before(async () => { browser = await webkit.launch(); });
after(async () => { await browser?.close(); });

async function picker(t, policy = '', selectBeforeLoad = false) {
  const page = await browser.newPage();
  t.after(() => page.close());
  await page.setContent(
    `<meta http-equiv="Content-Security-Policy" content="${policy}">` +
    '<button id="target" style="margin:100px;width:150px;height:80px">Target</button>',
  );
  await page.evaluate(() => {
    window.observedCommentEvents = [];
    for (const type of ['keydown', 'input']) {
      window.addEventListener(type, (event) => {
        window.observedCommentEvents.push(event.type);
      }, true);
    }
  });
  if (selectBeforeLoad) {
    // Both operations run in one task, before the iframe's load can fire.
    await page.evaluate(install + `;document.getElementById('target').dispatchEvent(
      new PointerEvent('pointerdown', {bubbles:true, cancelable:true, clientX:150, clientY:130, button:0})
    );`);
  } else {
    await page.evaluate(install);
    await page.locator('#target').click();
  }
  await page.waitForFunction(() => {
    const host = document.querySelector('[data-tlane-annotator]');
    return host && document.activeElement === host;
  });
  const frame = page.frames().find((frame) => frame.parentFrame());
  assert.ok(frame, 'comment iframe exists');
  await frame.waitForFunction(() => document.activeElement?.id === 'c');
  return { page, frame };
}

for (const policy of ['', "script-src 'none'"]) {
  for (const action of ['Enter', 'Attach']) {
    test(`${action} attaches typed comments with policy ${policy || '(none)'}`, async (t) => {
      const { page, frame } = await picker(t, policy);
      // Real keystrokes, not fill(): this checks that selection focused the input.
      await page.keyboard.type('Fix this element');
      assert.equal(await frame.locator('#c').inputValue(), 'Fix this element');
      if (action === 'Enter') await page.keyboard.press('Enter');
      else await frame.locator('#a').click();
      const pick = await page.evaluate(() => window.__tlane_pick);
      assert.equal(pick?.comment, 'Fix this element');
      assert.equal(pick.elements[0].selector, '#target');
      assert.ok(pick.crop.w > 0 && pick.crop.h > 0);
      assert.equal(await page.locator('[data-tlane-annotator]').count(), 0);
      assert.deepEqual(await page.evaluate(() => window.observedCommentEvents), []);
    });
  }
}

test('selection before iframe load still focuses the comment', async (t) => {
  const { page, frame } = await picker(t, "script-src 'none'", true);
  await page.keyboard.type('Early selection');
  assert.equal(await frame.locator('#c').inputValue(), 'Early selection');
  await page.keyboard.press('Enter');
  assert.equal(await page.evaluate(() => window.__tlane_pick?.comment), 'Early selection');
});

test('IME Enter does not attach; Escape cancels without leaking keystrokes', async (t) => {
  const { page, frame } = await picker(t, "script-src 'none'");
  await frame.locator('#c').dispatchEvent('keydown', { key: 'Enter', isComposing: true });
  assert.equal(await page.evaluate(() => window.__tlane_pick), null);
  assert.equal(await page.evaluate(() => window.__tlane_annotating), true);
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => window.__tlane_pick), null);
  assert.equal(await page.evaluate(() => window.__tlane_annotating), false);
  assert.equal(await page.locator('[data-tlane-annotator]').count(), 0);
  assert.deepEqual(await page.evaluate(() => window.observedCommentEvents), []);
});
