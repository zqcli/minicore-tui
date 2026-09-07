// Footer cells: the fixed reference's real renderSimpleFooter + real
// collectFooterLiveState + real footer store (turn duration, selection notice),
// plus the real formatNum / formatCost / formatUserMessageTimestamp-adjacent"
// number oracles.

import { importReferenceModule } from "../lib/ctx.mts";
import { writeCase, writeCaseAnnotated } from "../lib/emit.mts";
import { withFixedNow } from "../lib/ctx.mts";

const {
	renderFooter,
	setTurnStartTime,
	setTurnEndTime,
	showFooterSelectionNotice,
	clearFooterSelectionNotice,
} = await importReferenceModule("components/footer/footer.ts");
const { collectFooterLiveState, formatCost, formatNum } = await importReferenceModule(
	"components/footer/footer-session-snapshot.ts",
);
const { RAIL_FOOTER_STYLE, FOOTER_LAYOUT } = await importReferenceModule("config/index.ts");

function fakeContext(over: Record<string, unknown> = {}): any {
	const ctx: any = {
		cwd: "/Users/zzq/work/project",
		model: { id: "gpt-4o-20241120" },
		modelRegistry: { isUsingOAuth: () => false },
		isIdle: () => true,
		hasPendingMessages: () => false,
		getContextUsage: () => undefined,
		sessionManager: { getCwd: () => undefined },
		...over,
	};
	return ctx;
}

function fakePi(over: Record<string, unknown> = {}): any {
	const pi: any = {
		getThinkingLevel: () => "high",
		getActiveTools: () => [],
		getAllTools: () => [],
		...over,
	};
	return pi;
}

function footerData(branch?: string | null): any {
	return {
		getGitBranch: () => branch,
		getExtensionStatuses: () => new Map<string, string>(),
	};
}

function render(width: number, ctx: any, pi: any, stats: Record<string, number>, branch?: string | null): string[] {
	const style = RAIL_FOOTER_STYLE;
	return renderFooter(
		width,
		ctx,
		pi,
		footerData(branch),
		{ inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0, ...stats },
		style,
	);
}

export function generateFooterCases(): void {
	// Number/format oracles (real functions).
	const numbers: string[] = [];
	for (const v of [0, 999, 999.8, 1000, 42_000, 999_999, 1_000_000, 12_345_000, -5, Number.NaN]) {
		numbers.push(`formatNum(${v})=${formatNum(v)}`);
	}
	for (const v of [0, 0.004, 0.0049, 0.01, 0.05, 0.5, 0.999, 1, 1.5, 12345, -1, Number.NaN]) {
		numbers.push(`formatCost(${v})=${formatCost(v)}`);
	}
	writeCase("footer", "numbers", { cols: 80, rows: 24 }, numbers, "text");

	// Ready state (idle) with realistic usage.
	{
		const ctx = fakeContext({ isIdle: () => true });
		const pi = fakePi();
		const rows = render(80, ctx, pi, { inputTokens: 4234, outputTokens: 67_100, cacheReadTokens: 7_900_000, cacheWriteTokens: 0, cost: 0.02 });
		writeCase("footer", "ready", { cols: 80, rows: 24 }, rows);
	}

	// Working state (not idle).
	{
		const ctx = fakeContext({ isIdle: () => false });
		const rows = render(80, ctx, fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0 });
		writeCase("footer", "working", { cols: 80, rows: 24 }, rows);
	}

	// Queued (+ pending).
	{
		const ctx = fakeContext({ isIdle: () => false, hasPendingMessages: () => true });
		const rows = render(80, ctx, fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0 });
		writeCase("footer", "queued", { cols: 80, rows: 24 }, rows);
	}

	// Selection copied notice (real footer store, fixed clock).
	withFixedNow(1_700_000_000_000, () => {
		clearFooterSelectionNotice();
		showFooterSelectionNotice(undefined, 60_000);
		const rows = render(80, fakeContext(), fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0 });
		writeCase("footer", "selection-copied", { cols: 80, rows: 24 }, rows);
		clearFooterSelectionNotice();
	});

	// Duration boundaries (fixed clock): 0m, 59m, 1h1m.
	const durationRows: string[] = [];
	const makeDuration = (label: string, elapsedMs: number) => {
		withFixedNow(1_700_000_000_000, () => {
			setTurnStartTime(1_700_000_000_000 - elapsedMs);
			setTurnEndTime();
			const rows = renderFooter(
				80,
				fakeContext({ isIdle: () => false }),
				fakePi(),
				footerData(),
				{ inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0 },
				RAIL_FOOTER_STYLE,
			);
			durationRows.push(`${label} -> ${JSON.stringify(rows[0]!)}`);
		});
	};
	makeDuration("0m", 0);
	makeDuration("59m", 59 * 60_000);
	makeDuration("1h1m", 61 * 60_000);
	writeCase("footer", "duration", { cols: 80, rows: 24 }, durationRows, "text");

	// Context thresholds: 69.99 (lilac), 70 (amber), null (ctx ?).
	for (const percent of [69.99, 70, null] as const) {
		const ctx = fakeContext({ getContextUsage: () => (percent === null ? undefined : { percent }) });
		const rows = render(80, ctx, fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0 });
		writeCase("footer", `context-${percent === null ? "unknown" : `${percent}`.replace(".", "-")}`, { cols: 80, rows: 24 }, rows);
	}

	// Cost variants + subscription label.
	{
		const rows1 = render(80, fakeContext(), fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0.0049 });
		writeCase("footer", "cost-0dot0049", { cols: 80, rows: 24 }, rows1);
		const rows2 = render(80, fakeContext(), fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 1.5 });
		writeCase("footer", "cost-1dot5", { cols: 80, rows: 24 }, rows2);
		const ctx = fakeContext({ modelRegistry: { isUsingOAuth: () => true } });
		const rows3 = render(80, ctx, fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0 });
		writeCase("footer", "subscription-no-cost", { cols: 80, rows: 24 }, rows3);
	}

	// Model short truncation and branch, via real collectFooterLiveState.
	{
		const rows: string[] = [];
		for (const id of [
			"claude-3-5-sonnet-20241022",
			"gemini-2.5-pro-preview",
			"gpt-4o-20241120",
			"deepseek-chat",
		]) {
			const ctx = fakeContext({ model: { id }, isIdle: () => true });
			const state = collectFooterLiveState(ctx, fakePi(), footerData("dev"));
			rows.push(`model=${id} -> modelShort=${JSON.stringify(state.modelShort)}`);
		}
		const wide = render(80, fakeContext({ model: { id: "claude-3-5-sonnet-20241022" } }), fakePi(), { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, cost: 0 }, "dev");
		writeCaseAnnotated("footer", "model-short", { cols: 80, rows: 24 }, rows, wide);
	}

	// Narrow-screen fitAligned behavior: right group kept, left truncated.
	{
		const ctx = fakeContext({ isIdle: () => true });
		const pi = fakePi();
		for (const width of [40, 60, 100]) {
			const rows = render(width, ctx, pi, { inputTokens: 4234, outputTokens: 67_100, cacheReadTokens: 7_900_000, cacheWriteTokens: 0, cost: 0.02 });
			writeCase("footer", `narrow-${width}`, { cols: width, rows: 24 }, rows);
		}
	}

	// Layout constants pinned.
	writeCase("footer", "layout", { cols: 120, rows: 40 }, [
		`FOOTER_LAYOUT=${JSON.stringify(FOOTER_LAYOUT)}`,
	], "text");
}