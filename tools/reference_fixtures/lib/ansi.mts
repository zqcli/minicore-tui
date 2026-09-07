// ANSI row -> normalized cell grid parser for the Rail reference capture.
//
// The fixed Rail/Pi renderers emit terminal rows as ANSI strings. This parser
// is test-only tooling that turns those rows into a deterministic per-cell
// model (char, fg, bg, style flags) that the Rust parity tests consume. It
// must stay faithful to what a terminal (and ratatui's Buffer) would see:
//   - SGR fg/bg are 24-bit RGB (the renderers only use 38;2 / 48;2, but
//     256-color and the basic palette are handled defensively);
//   - wide graphemes occupy two screen columns (the token carries `w:2` and
//     owns the next screen column, mirroring ratatui's wide-char buffer);
//   - CURSOR_MARKER (`\x1b_pi:c\x07`) records the cursor screen column;
//   - OSC 133 prompt zones and Rail click-zone markers are zero-width and are
//     stripped, but OSC 133 presence is recorded per row;
//   - any unexpected CSI control fails the parse (fixtures would be invalid).

import { CURSOR_MARKER, importReferencePackage, PI_TUI_PACKAGE } from "./ctx.mts";

const { visibleWidth } = await importReferencePackage(PI_TUI_PACKAGE);

export type Rgb = [number, number, number];
export type StyleFlags = number;

export const S_BOLD = 1;
export const S_DIM = 2;
export const S_ITALIC = 4;
export const S_UNDERLINE = 8;
export const S_STRIKE = 16;
export const S_REVERSE = 32;

export type Cell = {
	c: string;
	fg?: Rgb;
	bg?: Rgb;
	s?: StyleFlags;
	w?: 2;
};

export type ParsedRow = {
	cells: Cell[];
	cursorCol?: number;
	osczone: boolean;
};

const RAIL_CLICK_MARKER_OSC = /\x1b_pi-rail-click:start:\d+\x07|\x1b_pi-rail-click:end:\d+\x07/gu;
const OSC_SEQ = /\x1b\]([^\x07\x1b]*)(?:\x07|\x1b\\)/u;

type SgrState = {
	fg?: Rgb;
	bg?: Rgb;
	s: StyleFlags;
};

function clamp255(v: number): number {
	return Math.max(0, Math.min(255, v));
}

function parseAnsiColor(params: number[], offset: number): { kind: "fg" | "bg"; rgb: Rgb } | undefined {
	const kind = params[offset] === 38 ? ("fg" as const) : ("bg" as const);
	const sep = params[offset + 1];
	if (sep === 2 && params.length >= offset + 5) {
		return {
			kind,
			rgb: [params[offset + 2], params[offset + 3], params[offset + 4]].map(clamp255) as Rgb,
		};
	}
	if (sep === 5 && params.length >= offset + 3) {
		return { kind, rgb: xterm256(params[offset + 2]!) };
	}
	return undefined;
}

function xterm256(index: number): Rgb {
	const i = Math.max(0, Math.min(255, Math.round(index)));
	if (i < 16) {
		const bright = [
			[0, 0, 0],
			[128, 0, 0],
			[0, 128, 0],
			[128, 128, 0],
			[0, 0, 128],
			[128, 0, 128],
			[0, 128, 128],
			[192, 192, 192],
			[128, 128, 128],
			[255, 0, 0],
			[0, 255, 0],
			[255, 255, 0],
			[0, 0, 255],
			[255, 0, 255],
			[0, 255, 255],
			[255, 255, 255],
		];
		return bright[i]! as Rgb;
	}
	if (i >= 232) {
		const v = 8 + (i - 232) * 10;
		return [v, v, v];
	}
	const n = i - 16;
	const cube = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff] as const;
	return [cube[Math.floor(n / 36)]!, cube[Math.floor((n % 36) / 6)]!, cube[n % 6]!] as unknown as Rgb;
}

function basicColor(index: number, bright: boolean): Rgb {
	const brightVga = [
		[128, 128, 128],
		[255, 0, 0],
		[0, 255, 0],
		[255, 255, 0],
		[0, 0, 255],
		[255, 0, 255],
		[0, 255, 255],
		[255, 255, 255],
	] as const;
	const darkVga = [
		[0, 0, 0],
		[128, 0, 0],
		[0, 128, 0],
		[128, 128, 0],
		[0, 0, 128],
		[128, 0, 128],
		[0, 128, 128],
		[192, 192, 192],
	] as const;
	const table = bright ? brightVga : darkVga;
	return table[index]! as unknown as Rgb;
}

function applySgr(state: SgrState, params: number[]): void {
	for (let i = 0; i < params.length; i++) {
		const p = params[i]!;
		if (p === 0) {
			state.fg = undefined;
			state.bg = undefined;
			state.s = 0;
		} else if (p === 39) state.fg = undefined;
		else if (p === 49) state.bg = undefined;
		else if (p === 1) state.s |= S_BOLD;
		else if (p === 2) state.s |= S_DIM;
		else if (p === 3) state.s |= S_ITALIC;
		else if (p === 4) state.s |= S_UNDERLINE;
		else if (p === 9) state.s |= S_STRIKE;
		else if (p === 7) state.s |= S_REVERSE;
		else if (p === 22) state.s &= ~(S_BOLD | S_DIM);
		else if (p === 23) state.s &= ~S_ITALIC;
		else if (p === 24) state.s &= ~S_UNDERLINE;
		else if (p === 29) state.s &= ~S_STRIKE;
		else if (p === 27) state.s &= ~S_REVERSE;
		else if (p === 38 || p === 48) {
			const parsed = parseAnsiColor(params, i);
			if (parsed) {
				if (parsed.kind === "fg") state.fg = parsed.rgb;
				else state.bg = parsed.rgb;
				i += 4;
			}
		} else if (p >= 30 && p <= 37) state.fg = basicColor(p - 30, false);
		else if (p >= 90 && p <= 97) state.fg = basicColor(p - 90, true);
		else if (p >= 40 && p <= 47) state.bg = basicColor(p - 40, false);
		else if (p >= 100 && p <= 107) state.bg = basicColor(p - 100, true);
	}
}

function splitSgrParams(raw: string): number[] {
	return raw
		.split(/[;:]/)
		.filter((part) => part !== "")
		.map((part) => Number.parseInt(part, 10));
}

function cellStyle(state: SgrState): Pick<Cell, "fg" | "bg" | "s"> {
	const out: Pick<Cell, "fg" | "bg" | "s"> = {};
	if (state.fg) out.fg = state.fg;
	if (state.bg) out.bg = state.bg;
	if (state.s) out.s = state.s;
	return out;
}

const segmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
function firstGrapheme(text: string): string {
	for (const seg of segmenter.segment(text)) return seg.segment;
	return "";
}

/**
 * Parse one ANSI terminal row into screen cells. Wide graphemes are one cell
 * carrying `w: 2` (they own the following screen column). Returns the cursor
 * screen column and whether an OSC133 prompt zone opened this row.
 */
export function parseAnsiRow(row: string): ParsedRow {
	const state: SgrState = { s: 0 };
	const cells: Cell[] = [];
	let screenCols = 0;
	let cursorCol: number | undefined;
	let osczone = false;

	const cleaned = row.replace(RAIL_CLICK_MARKER_OSC, "");
	let i = 0;
	while (i < cleaned.length) {
		const ch = cleaned[i]!;

		if (ch === "\x1b") {
			if (cleaned.startsWith(CURSOR_MARKER, i)) {
				cursorCol = screenCols;
				i += CURSOR_MARKER.length;
				continue;
			}
			if (cleaned[i + 1] === "]") {
				const oscMatch = OSC_SEQ.exec(cleaned.slice(i));
				if (!oscMatch) throw new Error(`unparseable OSC at col ${screenCols}: ${JSON.stringify(cleaned.slice(i, i + 12))}`);
				if (oscMatch[1].startsWith("133")) osczone = true;
				i += oscMatch[0].length;
				continue;
			}
			if (cleaned[i + 1] === "[") {
				let j = i + 2;
				while (j < cleaned.length && !(cleaned[j]! >= "@" && cleaned[j]! <= "~")) j++;
				if (j >= cleaned.length) throw new Error(`unterminated CSI at col ${screenCols}`);
				const csi = cleaned.slice(i + 2, j);
				const finalByte = cleaned[j]!;
				if (finalByte === "m") {
					applySgr(state, splitSgrParams(csi));
				} else {
					throw new Error(`unexpected CSI '${finalByte}' in captured row: ${JSON.stringify(csi)}`);
				}
				i = j + 1;
				continue;
			}
			throw new Error(`unexpected ESC at col ${screenCols}: ${JSON.stringify(cleaned.slice(i, i + 12))}`);
		}

		const rest = cleaned.slice(i);
		const grapheme = firstGrapheme(rest);
		if (grapheme === "") throw new Error(`failed to segment at col ${screenCols}`);
		const width = visibleWidth(grapheme);
		if (width === 2) {
			cells.push({ c: grapheme, w: 2, ...cellStyle(state) });
			screenCols += 2;
		} else if (width === 1) {
			cells.push({ c: grapheme, ...cellStyle(state) });
			screenCols += 1;
		} else if (width === 0) {
			const last = cells[cells.length - 1];
			if (last && last.w !== 2) last.c += grapheme;
			// Zero-width marks at the very start or inside a wide char are dropped.
		} else {
			throw new Error(`unexpected grapheme width ${width} for ${JSON.stringify(grapheme)}`);
		}
		i += grapheme.length;
	}

	return { cells, cursorCol, osczone };
}

/**
 * Run-length encode cells into the compact fixture token format:
 *   - a bare string token = a run of default-style single-column chars;
 *   - an object token = `{c, fg?, bg?, s?, w?}`; `w: 2` marks a wide trailing
 *     char that owns the next screen column.
 */
export function encodeRow(cells: Cell[]): unknown[] {
	const tokens: unknown[] = [];
	let run: { c: string; fg?: Rgb; bg?: Rgb; s?: StyleFlags } | undefined;

	const sameStyle = (a: Cell, b: Cell) =>
		a.w === undefined
		&& a.fg?.join(",") === b.fg?.join(",")
		&& a.bg?.join(",") === b.bg?.join(",")
		&& (a.s ?? 0) === (b.s ?? 0);

	const flush = () => {
		if (!run) return;
		if (!run.fg && !run.bg && !run.s) {
			tokens.push(run.c);
		} else {
			const token: Record<string, unknown> = { c: run.c };
			if (run.fg) token.fg = run.fg;
			if (run.bg) token.bg = run.bg;
			if (run.s) token.s = run.s;
			tokens.push(token);
		}
		run = undefined;
	};

	for (const cell of cells) {
		if (cell.w === 2) {
			flush();
			const token: Record<string, unknown> = { c: cell.c, w: 2 };
			if (cell.fg) token.fg = cell.fg;
			if (cell.bg) token.bg = cell.bg;
			if (cell.s) token.s = cell.s;
			tokens.push(token);
			continue;
		}
		if (run && sameStyle(cell, { c: cell.c, fg: run.fg, bg: run.bg, s: run.s } as Cell)) {
			run.c += cell.c;
			continue;
		}
		flush();
		run = { c: cell.c, fg: cell.fg, bg: cell.bg, s: cell.s };
	}
	flush();
	return tokens;
}