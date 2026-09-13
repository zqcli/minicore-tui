// Oracle: execute the released Pi implementation, never a rewritten formula.
// Usage: node tools/scrollbar_oracle.mjs <pi-coding-agent package root> > fixture.json
import { readFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { stripVTControlCharacters } from 'node:util';
const root = resolve(process.argv[2]);
const tui = join(root, 'node_modules/@earendil-works/pi-tui');
for (const dir of [root, tui]) {
  if (JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8')).version !== '0.85.1') throw new Error('Expected Pi 0.85.1');
}
const { ScrollView } = await import(pathToFileURL(join(tui, 'dist/components/scroll-view.js')));
const { getScrollbarGeometry, renderLayoutFrame } = await import(pathToFileURL(join(tui, 'dist/layout.js')));
const { TuiAltScreen } = await import(pathToFileURL(join(tui, 'dist/tui-alt-screen.js')));
const realSet = globalThis.setTimeout, realClear = globalThis.clearTimeout;
let now = 0;
const timers = new Map();
globalThis.setTimeout = (callback, ms) => { const token = { unref() {} }; timers.set(token, { callback, due: now + ms }); return token; };
globalThis.clearTimeout = token => timers.delete(token);
function advance(ms) {
  now += ms;
  for (const [token, timer] of timers) if (timer.due <= now) { timers.delete(token); timer.callback(); }
}
const child = { render: () => [], invalidate() {} };
const geometry = [];
try {
  for (const height of [1, 2, 3, 5, 10, 20, 37, 50]) {
    for (const total of [0, height, height + 1, height * 2, 1000, 1000000]) {
      const maximum = Math.max(0, total - height);
      for (const offset of [0, Math.floor(maximum / 2), maximum]) {
        const view = new ScrollView(child, { follow: 'end', scrollbar: 'auto' });
        view.updateLayout(total, height, () => {});
        view.scrollTo(offset);
        const rect = { x: 2, y: 3, width: 80, height };
        const box = { rect, clip: rect, scrollView: view, children: [{ rect: { height: total } }] };
        const g = getScrollbarGeometry(box, true);
        const item = { height, total, offset, geometry: g ?? null, drag: [] };
        if (g) {
          for (const pointer of [0, 3, 3 + Math.floor(height / 2), 3 + height + 5]) {
            for (const grab of [0, Math.floor(g.thumbHeight / 2), g.thumbHeight - 1]) {
              TuiAltScreen.prototype.scrollScrollbarToPointer.call(null, view, g, pointer, grab);
              item.drag.push({ pointer, grab, offset: view.scrollTop });
            }
          }
        }
        geometry.push(item);
        view.hideTransientScrollbar();
      }
    }
  }
  const view = new ScrollView(child, { follow: 'end', scrollbar: 'auto' });
  const visibility = [];
  const step = (action, value) => {
    if (action === 'layout') view.updateLayout(value[0], value[1], () => {});
    if (action === 'by') view.scrollBy(value);
    if (action === 'active') view.setScrollbarActive(value);
    if (action === 'wait') advance(value);
    visibility.push({ action, value, now, visible: view.isScrollbarVisible, active: view.isScrollbarActive, offset: view.scrollTop, following: view.isFollowingEnd });
  };
  step('layout', [100, 20]); step('by', -1); step('wait', 999); step('wait', 1);
  step('active', true); step('wait', 5000); step('active', false); step('wait', 999); step('wait', 1);
  step('by', -5); step('by', -1000); step('by', -1); step('by', 1000); step('wait', 1000);
  step('by', 1); step('layout', [120, 20]); step('by', -1); step('layout', [10, 20]);
  const hashes = Object.fromEntries(['dist/layout.js', 'dist/components/scroll-view.js', 'dist/tui-alt-screen.js'].map(file => [file, createHash('sha256').update(readFileSync(join(tui, file))).digest('hex')]));
  const renderings = [], themeHashes = {};
  for (const theme of ['dark', 'light']) {
    const path = `dist/modes/interactive/theme/${theme}.json`;
    const bytes = readFileSync(join(root, path));
    themeHashes[path] = createHash('sha256').update(bytes).digest('hex');
    const data = JSON.parse(bytes);
    const rgb = key => {
      let color = data.colors[key];
      while (!color.startsWith('#')) color = data.vars[color];
      return [1, 3, 5].map(start => parseInt(color.slice(start, start + 2), 16));
    };
    const colors = { track: rgb('scrollbarTrack'), thumb: rgb('scrollbarThumb') };
    const style = color => text => `\x1b[38;2;${color.join(';')}m${text}\x1b[39m`;
    for (const active of [false, true]) for (const offset of [0, 10, 80]) {
      const view = new ScrollView({ render: () => Array(100).fill(''), invalidate() {} }, {
        scrollbar: 'auto', scrollbarTrackStyle: style(colors.track), scrollbarThumbStyle: style(colors.thumb),
      });
      renderLayoutFrame(view, 80, 20, () => {});
      view.scrollTo(offset);
      view.setScrollbarActive(true);
      if (!active) view.setScrollbarActive(false);
      const frame = renderLayoutFrame(view, 80, 20, () => {});
      renderings.push({ theme, active, offset, colors, rows: frame.lines.map(stripVTControlCharacters) });
      view.hideTransientScrollbar();
    }
  }
  console.log(JSON.stringify({ version: '0.85.1', revision: 'd981de1229ef899957bbe968bc8dcda02a21f477', hashes, themeHashes, geometry, visibility, renderings }, null, 2));
} finally {
  globalThis.setTimeout = realSet; globalThis.clearTimeout = realClear;
}
