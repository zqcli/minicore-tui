// Word/paragraph selection oracle: invokes the REAL pinned pi-tui
// `TuiAltScreen.getWordSelection` / `getLineSelection` (Pi 0.84.4) on every
// display cell of each sample line and records the resulting ranges. No part
// of the algorithm is re-typed here: segmented words, the `/` `-` joiner set,
// `canJoin` expansion, and display widths all come from the fixed module.
// `visibleWidth` from the same fixed package enumerates the cells.
//
// Output: tests/fixtures/word_oracle.json (schema minicore-word-oracle-v1),
// consumed by tests/word_oracle.rs which compares the Rust model cell-by-cell.

import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import {
	PINNED_PI_VERSION,
	PINNED_RAIL_COMMIT,
	importReferencePackage,
	PI_TUI_PACKAGE,
	setupDeterministicEnv,
} from "../lib/ctx.mts";

setupDeterministicEnv();

const { TuiAltScreen, visibleWidth } = await importReferencePackage(PI_TUI_PACKAGE);
const getWordSelection = TuiAltScreen.prototype.getWordSelection;
const getLineSelection = TuiAltScreen.prototype.getLineSelection;

// A minimal fake owning the terminal shape the native methods read: the real
// `getSelectionSourceLine` uses `this.previousScreen[point.row]` when no
// scroll view is supplied, so one screen row is all the state required.
function fakeScreen(line: string) {
	return {
		previousScreen: [line],
		currentLayout: undefined,
		terminal: { rows: 1, columns: visibleWidth(line) },
		getSelectionSourceLine: TuiAltScreen.prototype.getSelectionSourceLine,
	};
}

export const WORD_ORACLE_OUT = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	"..",
	"..",
	"..",
	"tests",
	"fixtures",
	"word_oracle.json",
);

// Preserved 230-cell coverage plus decomposed combining-letter, emoji ZWJ,
// and longer CJK lines the reviewer asked for.
const LINES = [
	"https://example.com/x",
	"see https://example.com/x done",
	"foo/bar",
	"foo-bar",
	"foo_bar",
	"snake_case_123",
	"a/b-c",
	"one two  three",
	"中文测试",
	"hello中文world",
	"👨‍👩‍👧family",
	"price: $1,234.50",
	"café résumé",
	"foo:bar",
	"foo.bar",
	"don’t stop",
	"x-y-z",
	"/// leading slashes",
	"trailing-dash-",
	"a/b//c",
	// decomposed base + combining mark: Pi's visibleWidth keeps the cluster
	// one cell wide, and the mark stays inside the word
	"e\u0301clair",
	"cafe\u0301 开\u0301",
	// emoji ZWJ family and standalone emoji, with surrounding prose
	"\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} family",
	"ok 🚀 done",
	// real dictionary CJK segmentation (spec 7.3/14.3): unambiguous and
	// ambiguous Chinese phrases, Chinese-English-number mixing, and Japanese
	// Kanji/Kana where the pinned Intl dictionary supports it
	"真的中文段落包含标点",
	"混合hello世界test",
	"中华人民共和国",
	"计算机科学",
	"南京市长江大桥",
	"我们正在开发终端应用",
	"北京大学",
	"人工智能助手",
	"中文mixed123混合",
	"我2004年在北京大学学习",
	"你好世界abc",
	"日本語のテキストです",
	"日本語漢字かな混じり",
	"上海浦东新区",
	"人工知能エージェント",
];

export function generateWordOracleCases(): void {
	const cases: Array<{ line: string; col: number; start: number; end: number; width: number; kind: string }> = [];
	for (const line of LINES) {
		const width = visibleWidth(line);
		const screen = fakeScreen(line);
		if (width < 1) continue;
		for (let col = 0; col < width; col++) {
			const point = { row: 0, col };
			const word = getWordSelection.call(screen, point);
			if (word) {
				cases.push({ line, col, start: word.start.col, end: word.end.col, width, kind: "word" });
			}
			const lineSel = getLineSelection.call(screen, point);
			cases.push({
				line,
				col,
				start: lineSel.start.col,
				end: lineSel.end.col,
				width,
				kind: "line",
			});
		}
	}

	const doc = {
		schema: "minicore-word-oracle-v1",
		rail_commit: PINNED_RAIL_COMMIT,
		pi: PINNED_PI_VERSION,
		source:
			"node_modules/@earendil-works/pi-tui/dist/index.js " +
			"(TuiAltScreen.prototype.getWordSelection / getLineSelection / visibleWidth, real code)",
		tz: "UTC",
		node: process.version,
		generator: "tools/reference_fixtures/cases/word_oracle.mts",
		generated_at: new Date().toISOString(),
		cases,
	};
	fs.mkdirSync(path.dirname(WORD_ORACLE_OUT), { recursive: true });
	fs.writeFileSync(WORD_ORACLE_OUT, JSON.stringify(doc, null, 2) + "\n");
	console.log(
		`[generate] word-oracle: ${cases.filter((c) => c.kind === "word").length} word + ` +
			`${cases.filter((c) => c.kind === "line").length} line cases -> ${WORD_ORACLE_OUT}`,
	);
}