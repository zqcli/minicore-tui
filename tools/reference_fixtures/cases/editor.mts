// Editor cell fixtures: real RailEditor (a Pi 0.84.4 CustomEditor subclass)
// driven headlessly, plus native paste-threshold black-box captures.
//
// The paste threshold (11 lines / 1001 chars -> marker; 10 lines / 1000 chars
// -> inlined) is Pi 0.84.4's native `Editor.handlePaste` behavior; we drive
// the real method and record the resulting buffer text plus rendered cells.

import {
	importReferenceModule,
	importReferencePackage,
	PI_TUI_PACKAGE,
} from "../lib/ctx.mts";
import { writeCase, writeCaseAnnotated } from "../lib/emit.mts";
import { editorTheme, fakeTui, referenceKeybindings } from "../lib/ctx.mts";

const { RailEditor } = await importReferenceModule("components/editor/index.ts");
const { CombinedAutocompleteProvider, SelectList, visibleWidth } = await importReferencePackage(PI_TUI_PACKAGE);

const appTheme = { fg(_name: string, value: string) { return value; } };

type EditorWithInternals = {
	getText(): string;
	setText(text: string): void;
	handlePaste(text: string): void;
	handleInput(data: string): void;
	render(width: number): string[];
	setAutocompleteProvider(provider: unknown): void;
	setCursor(line: number, col: number): void;
	[key: string]: unknown;
};

function newEditor(rows: number, cols: number): EditorWithInternals {
	const tui = fakeTui(rows, cols);
	const keybindings = referenceKeybindings();
	const editor = new (RailEditor as unknown as new (...args: unknown[]) => EditorWithInternals)(
		tui,
		editorTheme(),
		keybindings,
		appTheme,
	);
	// The native editor only emits CURSOR_MARKER (and the reverse-video cursor
	// cell) while focused; the headless harness focuses it explicitly.
	return Object.assign(editor, { focused: true });
}

function renderCase(group: string, name: string, cols: number, rows: number, editor: EditorWithInternals): void {
	const rendered = editor.render(cols);
	writeCase(group, name, { cols, rows }, rendered);
}

function stripAnsiForHitTesting(text: string): string {
	return text
		.replaceAll("\x1b_pi:c\x07", "")
		.replace(/\x1b\[[0-?]*[ -/]*[@-~]/gu, "");
}

function renderClickCase(
	name: string,
	cols: number,
	rows: number,
	text: string,
	target: string,
): void {
	const editor = newEditor(rows, cols);
	editor.setText(text);
	const before = editor.render(cols);
	const row = before.findIndex((line) => stripAnsiForHitTesting(line).includes(target));
	if (row < 0) throw new Error(`click target ${target} is not visible in ${name}`);
	const plain = stripAnsiForHitTesting(before[row]!);
	const targetIndex = plain.indexOf(target);
	const column = visibleWidth(plain.slice(0, targetIndex));
	if (!(editor as any).moveCursorToMousePosition(row, column)) {
		throw new Error(`native editor rejected click in ${name}`);
	}
	renderCase("editor-click", name, cols, rows, editor);
}

export function generateEditorCases(): void {
	const term = (cols: number, rows: number) => ({ cols, rows });

	// 80x24 grid.
	{
		const editor = newEditor(24, 80);
		editor.setText("");
		renderCase("editor", "empty", 80, 24, editor);
	}
	{
		const editor = newEditor(24, 80);
		editor.setText("hello world");
		renderCase("editor", "one-line", 80, 24, editor);
	}
	{
		const editor = newEditor(24, 80);
		editor.setText("line one\nline two\nline three\nline four");
		renderCase("editor", "four-lines", 80, 24, editor);
	}
	{
		const editor = newEditor(24, 80);
		editor.setText(Array.from({ length: 13 }, (_, i) => `line ${i + 1}`).join("\n"));
		renderCase("editor", "thirteen-lines", 80, 24, editor);
	}
	{
		// Cursor sits on a middle logical row; the fit window keeps it visible.
		const editor = newEditor(24, 80);
		editor.setText(Array.from({ length: 13 }, (_, i) => `line ${i + 1}`).join("\n"));
		(editor as any).state.cursorLine = 6;
		(editor as any).setCursorCol(3);
		renderCase("editor", "cursor-mid-13line", 80, 24, editor);
	}
	{
		// Long unwrapped line: native soft wrap.
		const editor = newEditor(24, 80);
		editor.setText("the quick brown fox jumps over the lazy dog, ".repeat(5));
		renderCase("editor", "long-soft-wrap", 80, 24, editor);
	}
	{
		const editor = newEditor(24, 40);
		editor.setText("你好，世界！这是中文内容测试。");
		renderCase("editor", "cjk", 40, 24, editor);
	}
	{
		const editor = newEditor(24, 40);
		editor.setText("mix 🚀 rocket ✨ sparkles and 中文");
		renderCase("editor", "cjk-emoji", 40, 24, editor);
	}
	{
		// Short content is centered vertically in the min-height surface.
		const editor = newEditor(24, 80);
		editor.setText("centered");
		renderCase("editor", "one-line-centered", 80, 24, editor);
	}

	// 120x40 and 60x16 grids.
	{
		const editor = newEditor(40, 120);
		editor.setText(Array.from({ length: 20 }, (_, i) => `wide line ${i + 1}`).join("\n"));
		renderCase("editor", "wide-120x40", 120, 40, editor);
	}
	{
		const editor = newEditor(16, 60);
		editor.setText("narrow\nterminal\nworks\nhere");
		renderCase("editor", "narrow-60x16", 60, 16, editor);
	}
}

export function generateEditorClickCases(): void {
	renderClickCase(
		"ascii-middle",
		80,
		24,
		"abcdefghijklmnopqrstuvwxyz 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ",
		"m",
	);
	renderClickCase(
		"wrapped-uppercase",
		40,
		24,
		"abcdefghijklmnopqrstuvwxyz 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ",
		"M",
	);
	renderClickCase("wide-grapheme", 40, 24, "alpha 中文内容 omega", "内");
}

export function generatePasteCases(): void {
	// Native black-box paste thresholds.
	const lines = (n: number) => Array.from({ length: n }, (_, i) => `pasted line ${i + 1}`).join("\n");

	const editor = newEditor(24, 80);
	editor.setText("");
	editor.handlePaste(lines(10));
	const buf10 = editor.getText();
	const rendered10 = editor.render(80);
	writeCase("paste", "ten-lines-inline", { cols: 80, rows: 24 }, rendered10);

	editor.setText("");
	editor.handlePaste(lines(11));
	const buf11 = editor.getText();
	const rendered11 = editor.render(80);
	writeCase("paste", "eleven-lines-marker", { cols: 80, rows: 24 }, rendered11);

	editor.setText("");
	editor.handlePaste("x".repeat(1000));
	const buf1000 = editor.getText();
	const rendered1000 = editor.render(80);
	writeCase("paste", "thousand-chars-inline", { cols: 80, rows: 24 }, rendered1000);

	editor.setText("");
	editor.handlePaste("y".repeat(1001));
	const buf1001 = editor.getText();
	const rendered1001 = editor.render(80);
	writeCase("paste", "thousand-one-chars-marker", { cols: 80, rows: 24 }, rendered1001);

	// A single-line paste inserts atomically without a marker.
	editor.setText("");
	editor.handlePaste("/tmp/build.sh");
	writeCase("paste", "single-line-inline", { cols: 80, rows: 24 }, editor.render(80));

	// Marker rendered with cursor after it.
	editor.setText("");
	editor.handlePaste(lines(11));
	(editor as any).state.cursorCol = editor.getText().length;
	writeCase("paste", "eleven-lines-marker-cursor", { cols: 80, rows: 24 }, editor.render(80));

	// Text-only oracle (no cells): record exact native buffer decisions.
	writeCase("paste", "thresholds", { cols: 80, rows: 24 }, [
		`10 lines  -> ${JSON.stringify(buf10)}`,
		`11 lines  -> ${JSON.stringify(buf11)}`,
		`1000 chars-> ${JSON.stringify(buf1000).slice(0, 40)}`,
		`1001 chars-> ${JSON.stringify(buf1001)}`,
	], "text");
}

export function generateSlashCases(): void {
	// Fixed-commit behavior (rail-editor-autocomplete.ts): Enter on an open
	// slash autocomplete keeps a `skill:` command in the editor instead of
	// submitting (Rail's own test pins this exact contract).
	const keybindings = referenceKeybindings();
	const railEditor = new (RailEditor as unknown as new (...args: unknown[]) => EditorWithInternals)(
		fakeTui(24, 80),
		editorTheme(),
		keybindings,
		appTheme,
	);
	Object.assign(railEditor, { focused: true });
	let submitted: string | undefined;
	let changed: string | undefined;
	railEditor.setAutocompleteProvider(new CombinedAutocompleteProvider([
		{ name: "skill:web-access", description: "Search the web" },
	], process.cwd()));
	railEditor.setText("/ski");
	(railEditor as any).onSubmit = (text: string) => { submitted = text; };
	(railEditor as any).onChange = (text: string) => { changed = text; };
	Object.assign(railEditor, {
		autocompleteState: {},
		autocompletePrefix: "/ski",
		autocompleteList: { getSelectedItem: () => ({ value: "skill:web-access", label: "skill:web-access" }) },
	});
	railEditor.handleInput("\r");
	const textAfter = railEditor.getText();
	const rowsAfter = railEditor.render(80);
	writeCaseAnnotated("slash", "enter-keeps-skill-command", { cols: 80, rows: 24 }, [
		`submitted=${submitted === undefined ? "undefined" : JSON.stringify(submitted)}`,
		`text=${JSON.stringify(textAfter)}`,
		`changed=${JSON.stringify(changed)}`,
		`autocompleteState=${JSON.stringify((railEditor as any).autocompleteState)}`,
	], rowsAfter);

	// Control: Enter without an open slash list submits the text.
	{
		const plain = newEditor(24, 80);
		let plainSubmitted: string | undefined;
		(plain as any).onSubmit = (text: string) => { plainSubmitted = text; };
		plain.setText("/read");
		(plain as any).handleInput("\r");
		const rows = plain.render(80);
		writeCaseAnnotated("slash", "enter-no-list-submits", { cols: 80, rows: 24 }, [
			`submitted=${plainSubmitted === undefined ? "undefined" : JSON.stringify(plainSubmitted)}`,
			`text=${JSON.stringify(plain.getText())}`,
		], rows);
	}

	// The popup is also rendered through the real RailEditor and Pi SelectList;
	// only the provider request is synthetic so this remains deterministic.
	{
		const popup = newEditor(24, 80);
		popup.setText("");
		const items = ["new", "resume", "sessions", "model", "reasoning", "theme", "clear"].map((value) => ({
			value,
			label: value,
		}));
		const editor = popup as any;
		editor.autocompleteState = "regular";
		editor.autocompletePrefix = "/";
		editor.autocompleteList = new SelectList(
			items,
			5,
			editor.theme.selectList,
			{ minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 },
		);
		renderCase("slash", "popup-rows", 80, 24, popup);
	}
}