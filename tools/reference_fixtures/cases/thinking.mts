// Thinking collapse cells: the fixed reference's real AssistantThinkingRailBlock
// (reached through the real renderAssistantMessageRail) plus the real
// collapseHint oracle. Child markdown is a stub renderer, exactly as in Rail's
// own assistant-message tests — the collapse/rail behavior under test is the
// reference's.

import { importReferenceModule } from "../lib/ctx.mts";
import { writeCase, writeCaseAnnotated } from "../lib/emit.mts";
import { mockTheme } from "../lib/ctx.mts";

const { renderAssistantMessageRail } = await importReferenceModule(
	"components/messages/assistant-message-rail.ts",
);
const { railThinkingSurface } = await importReferenceModule("rail/rail-surface.ts");
const { collapseHint } = await importReferenceModule("rail/rail-section.ts");

class Spacer {
	render(): string[] {
		return [""];
	}
	invalidate(): void {}
}

class MultiLineMarkdown {
	constructor(readonly text: string) {}
	render(): string[] {
		return this.text.split("\n");
	}
	invalidate(): void {}
}

type Host = {
	contentContainer: { children: unknown[]; clear(): void; addChild(child: unknown): void };
	hideThinkingBlock: boolean;
	hiddenThinkingLabel: string;
	lastMessage?: unknown;
	hasToolCalls: boolean;
	[key: string]: unknown;
};

function hostWithThinking(text: string, hidden = false, content?: unknown[]): { host: Host; block: () => any } {
	const thinking = new MultiLineMarkdown(text);
	const children: any[] = [new Spacer(), thinking];
	const host: Host = {
		contentContainer: {
			children,
			clear() {
				this.children = [];
			},
			addChild(child: unknown) {
				this.children.push(child);
			},
		},
		hideThinkingBlock: hidden,
		hiddenThinkingLabel: "Thinking...",
		lastMessage: undefined,
		hasToolCalls: false,
	};
	renderAssistantMessageRail(
		host,
		{ content: content ?? [{ type: "thinking", thinking: text }] },
		mockTheme() as never,
		railThinkingSurface,
	);
	const block = () => (host.contentContainer.children[1] as any);
	return { host, block };
}

export function generateThinkingCases(): void {
	// Hint oracle (real function, Pi-dark muted/dim styling).
	const hints: string[] = [
		`collapseHint(n=1)=${JSON.stringify(collapseHint(mockTheme(), 1))}`,
		`collapseHint(n=3)=${JSON.stringify(collapseHint(mockTheme(), 3))}`,
		`collapseHint(n=7)=${JSON.stringify(collapseHint(mockTheme(), 7))}`,
		`collapseHint(n=0)=${JSON.stringify(collapseHint(mockTheme(), 0))}`,
	];
	writeCase("thinking", "hint", { cols: 80, rows: 24 }, hints, "text");

	// 2 and 3 raw lines: below/at the 3-line threshold -> full, no hint.
	for (const n of [2, 3]) {
		const text = Array.from({ length: n }, (_, i) => `thought ${i + 1}`).join("\n");
		const { block } = hostWithThinking(text);
		const rows = block().render(80);
		writeCase("thinking", `lines-${n}-full`, { cols: 80, rows: 24 }, rows);
	}

	// 4 and 10 raw lines: over the threshold -> collapsed (first 3 rows + hint),
	// then expanded on setExpanded(true).
	for (const n of [4, 10]) {
		const text = Array.from({ length: n }, (_, i) => `thought ${i + 1}`).join("\n");
		const { block } = hostWithThinking(text);
		block().render(80);
		const collapsedRows = block().render(80);
		writeCaseAnnotated("thinking", `lines-${n}-collapsed`, { cols: 80, rows: 24 }, [
			`expanded=${block().expanded}`,
			`hidden=${n - 3}`,
		], collapsedRows);
		block().setExpanded(true);
		const expandedRows = block().render(80);
		writeCaseAnnotated("thinking", `lines-${n}-expanded`, { cols: 80, rows: 24 }, [
			`expanded=${block().expanded}`,
		], expandedRows);
	}

	// Manual expansion persists across a streaming update (line 5 -> line 6),
	// mirroring Rail's own regression test.
	{
		const { host, block } = hostWithThinking("l1\nl2\nl3\nl4\nl5");
		block().render(80);
		block().setExpanded(true);
		host.contentContainer.children = [new Spacer(), new MultiLineMarkdown("l1\nl2\nl3\nl4\nl5\nl6")];
		renderAssistantMessageRail(
			host,
			{ content: [{ type: "thinking", thinking: "l1\nl2\nl3\nl4\nl5\nl6" }] },
			mockTheme() as never,
			railThinkingSurface,
		);
		const rows = block().render(80);
		writeCaseAnnotated("thinking", "manual-expand-kept", { cols: 80, rows: 24 }, [
			`expanded=${block().expanded}`,
			`hasHint=${rows.some((r: string) => /earlier lines/.test(r))}`,
		], rows);
	}

	// Long single logical line that wraps: hidden count uses raw logical lines,
	// not rendered/soft-wrapped rows.
	{
		const text = Array.from({ length: 5 }, () => "x".repeat(200)).join("\n");
		const { block } = hostWithThinking(text);
		block().render(80);
		const rows = block().render(80);
		writeCase("thinking", "long-wrap-lines-5", { cols: 80, rows: 24 }, rows);
	}

	// Consecutive thinking parts merge into one run (real nativeAssistantRailBlocks).
	{
		const twoPartText = "merge part one\n\nmerge part two";
		const { block } = hostWithThinking(twoPartText, false, [
			{ type: "thinking", thinking: "merge part one" },
			{ type: "thinking", thinking: "merge part two" },
		]);
		const rows = block().render(80);
		writeCase("thinking", "merged-parts", { cols: 80, rows: 24 }, rows);
	}
}