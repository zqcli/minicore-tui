// Stage 7 xterm.js driver: a long-running browser session driven over a
// JSON-lines stdin protocol. The page embed a real xterm.js terminal that
// connects to the harness's WebSocket bridge; all PTY bytes that reach the
// browser are the real raw bytes written by the TUI/Agent.
//
// Input handling:
//  - plain text is typed through page.keyboard so real key events hit the
//    emulator, and xterm.js forwards them (onData) over the WebSocket to the
//    PTY;
//  - special keys (Tab) go through xterm's custom key handler -> onData so
//    browser focus navigation cannot steal them.
//
// Screenshots are element screenshots of the rendered terminal; no synthetic
// cell grid is drawn anywhere.

import { chromium } from "playwright-core";
import * as readline from "node:readline";
import * as fs from "node:fs";

let browser = null;
let page = null;
const pageLogs = [];
const wsTrace = [];
const OP_TIMEOUT_MS = 60_000;

function reply(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

function pageCode() {
  return {
    open: async (req) => {
      const { url, viewport } = req;
      browser = await chromium.launch({
        channel: "msedge",
        headless: true,
        args: [
          "--disable-gpu-sandbox",
          "--no-sandbox",
          "--force-color-profile=srgb",
          "--hide-scrollbars",
          "--disable-backgrounding-occluded-windows",
          "--disable-background-timer-throttling",
          "--disable-renderer-backgrounding",
          "--disable-features=CalculateNativeWinOcclusion,IntensiveWakeUpThrottling",
        ],
      });
      page = await browser.newPage({
        viewport: { width: viewport?.width ?? 1280, height: viewport?.height ?? 800 },
        deviceScaleFactor: 1,
      });
      page.on("websocket", (ws) => {
        pageLogs.push(`[websocket:connect] ${ws.url()}`);
        ws.on("framereceived", (ev) => {
          const data = ev.payload;
          const len = typeof data === "string" ? data.length : (data?.byteLength ?? 0);
          wsTrace.push({ dir: "recv", len });
        });
        ws.on("framesent", (ev) => {
          const data = ev.payload;
          const len = typeof data === "string" ? data.length : (data?.byteLength ?? 0);
          wsTrace.push({ dir: "sent", len });
        });
      });
      page.on("console", (msg) => {
        pageLogs.push(`[console:${msg.type()}] ${msg.text()}`);
      });
      page.on("pageerror", (err) => {
        pageLogs.push(`[pageerror] ${String(err?.stack ?? err)}`);
      });
      page.on("requestfailed", (req) => {
        pageLogs.push(`[requestfailed] ${req.method()} ${req.url()} ${String(req.failure()?.errorText)}`);
      });
      await page.goto(url, { waitUntil: "load", timeout: 60_000 });
      await page.waitForFunction(() => window.__stage7Ready === true, null, {
        timeout: 60_000,
      });
      await page.bringToFront();
      return {};
    },
    wsTrace: async () => ({ trace: [...wsTrace] }),
    termEval: async (req) => ({ value: await page.evaluate(req.expr) }),
    resize: async (req) => {
      const { cols, rows } = req;
      await page.evaluate(
        ({ cols, rows }) => window.__stage7Resize(cols, rows),
        { cols, rows },
      );
      return {};
    },
    type: async (req) => {
      const { text } = req;
      await page.evaluate(() => window.__stage7Term.focus());
      await page.keyboard.type(text, { delay: 8 });
      return {};
    },
    press: async (req) => {
      const { key } = req;
      await page.evaluate(() => window.__stage7Term.focus());
      await page.keyboard.press(key);
      return {};
    },
    click: async (req) => {
      const { col, row } = req;
      const point = await page.evaluate(
        ({ col, row }) => window.__stage7CellPoint(col, row),
        { col, row },
      );
      await page.mouse.click(point.x, point.y);
      return {};
    },
    screenshot: async (req) => {
      const path = req.path;
      await page.evaluate(() => window.__stage7Flush());
      await page.waitForTimeout(120);
      const locator = page.locator(".terminal");
      await locator.scrollIntoViewIfNeeded();
      const buffer = await locator.screenshot();
      fs.writeFileSync(path, buffer);
      return { bytes: buffer.length };
    },
    flush: async (req, ) => {
      const stats = await page.evaluate(() => window.__stage7Flush());
      return { stats };
    },
    screenText: async (req, ) => {
      // The exact DOM rows the element screenshot captures, post-flush.
      return { text: await page.locator(".xterm-rows").innerText() };
    },
    buffer: async (req, ) => {
      const text = await page.evaluate(() => window.__stage7Buffer());
      return { text };
    },
    cursor: async (req, ) => {
      const value = await page.evaluate(() => {
        const term = window.__stage7Term;
        const c = term.buffer.active.cursorX;
        const r = term.buffer.active.cursorY;
        return { col: c, row: r };
      });
      return value;
    },
    close: async (req, ) => {
      if (browser) await browser.close();
      pageLogs.length = 0;
      wsTrace.length = 0;
      browser = null;
      return {};
    },
  };
}

async function dispatch(req) {
  const code = pageCode();
  const handler = code?.[req.op] ?? (req.op === "open" ? code.open : undefined);
  if (!handler) return { error: `unknown op ${req.op}` };
  const result = await Promise.race([
    handler(req),
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error(`op ${req.op} timed out after ${OP_TIMEOUT_MS}ms`)), OP_TIMEOUT_MS),
    ),
  ]);
  return result ?? {};
}

async function main() {
  const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  rl.on("line", async (line) => {
    rl.pause();
    let req;
    try {
      req = JSON.parse(line);
    } catch {
      reply({ ok: false, error: "bad json" });
      rl.resume();
      return;
    }
    try {
      const out = await dispatch(req);
      reply({ ok: true, ...out });
    } catch (error) {
      reply({ ok: false, error: String(error?.message ?? error) });
    }
    rl.resume();
  });
  rl.on("close", async () => {
    if (browser) await browser.close();
    process.exit(0);
  });
}

main();