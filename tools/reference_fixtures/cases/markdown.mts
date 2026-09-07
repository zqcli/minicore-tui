// Native Markdown cells: Pi 0.84.4's real Markdown renderer with its real dark
// theme. This is the content oracle that user/assistant/thinking surfaces wrap;
// it locks inline tokens, blocks, wrapping and CJK rendering with genuine Pi
// colors. Rail does not replace this renderer — the parity target inherits it.

import { importReferenceModule, importReferencePackage, PI_TUI_PACKAGE } from "../lib/ctx.mts";
import { writeCase } from "../lib/emit.mts";

const { Markdown } = await importReferencePackage(PI_TUI_PACKAGE);
const { getMarkdownTheme } = await importReferenceModule(
	"node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js",
);

const mdTheme = getMarkdownTheme();

function render(text: string, width: number, padX = 0, padY = 0): string[] {
	const md = new (Markdown as unknown as new (...args: unknown[]) => { render(w: number): string[] })(
		text,
		padX,
		padY,
		mdTheme,
	);
	return md.render(width);
}

export function generateMarkdownCases(): void {
	const cases: Array<[string, string, number]> = [
		["plain", "plain text line", 40],
		["bold-italic", "**bold** and *italic* and ***both***", 60],
		["inline-code", "run `cargo test -- --nocapture` now", 60],
		["link", "see [docs](https://example.com/doc) please", 60],
		["heading", "# Title\n\nParagraph below.", 60],
		["list", "- first\n- second\n  1. numbered\n  2. two", 60],
		["codeblock", "```rust\nfn main() {}\n```\n", 60],
		["quote", "> quoted line\n> continued", 60],
		["hr", "text\n\n---\n\nmore", 60],
		["long-wrap", "word ".repeat(30), 40],
		["cjk", "这是中文段落，包含。和，标点。", 40],
		["cjk-markdown", "**加粗** 和 `代码` 混合", 40],
		["emoji", "result 🚀 (positive) ✨ done", 40],
		["codeblock-ansi", "```bash\n$ cargo build --release\n```\n", 60],
		["strikethrough", "~~removed~~ kept", 60],
		["nested-list", "1. one\n2. two\n   - sub a\n   - sub b\n3. three", 60],
	];
	for (const [name, text, width] of cases) {
		writeCase("markdown", name, { cols: width, rows: 24 }, render(text, width));
	}

	// Padding semantics: the surface pass uses padX/padY from the native
	// container; record both padded and unpadded renders for one case.
	{
		const text = "padded markdown";
		writeCase("markdown", "padding-x1", { cols: 40, rows: 24 }, render(text, 40, 1, 0));
		writeCase("markdown", "padding-y1", { cols: 40, rows: 24 }, render(text, 40, 0, 1));
	}
}