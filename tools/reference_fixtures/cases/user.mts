// User message cells: real Rail installUserMessageRail + real Pi
// UserMessageComponent, and the real UserMessageTimestampRegistry FIFO.

import { importReferenceModule, importReferencePackage, PI_CODING_AGENT_PACKAGE } from "../lib/ctx.mts";
import { writeCase } from "../lib/emit.mts";

const { UserMessageComponent } = await importReferencePackage(PI_CODING_AGENT_PACKAGE);
const { installUserMessageRail, uninstallUserMessageRail } = await importReferenceModule(
	"components/messages/user-message.ts",
);
const { UserMessageTimestampRegistry, formatUserMessageTimestamp } = await importReferenceModule(
	"components/messages/user-message-timestamps.ts",
);

function ctxWithBranch(entries: unknown[]): unknown {
	return {
		ui: { theme: undefined },
		sessionManager: { getBranch: () => entries },
	};
}

function userEntry(text: string, timestamp: number): unknown {
	return {
		type: "message",
		timestamp,
		message: { role: "user", content: [{ type: "text", text }] },
	};
}

export function generateUserCases(): void {
	const timestamps: string[] = [];
	// Spec example: en-US 12-hour, local zone forced to UTC.
	timestamps.push(`2026-09-05T14:05:00Z -> ${formatUserMessageTimestamp(new Date("2026-09-05T14:05:00Z").getTime())}`);
	timestamps.push(`2026-01-02T15:04:00Z -> ${formatUserMessageTimestamp(new Date("2026-01-02T15:04:00Z").getTime())}`);
	timestamps.push(`2026-01-03T09:12:00Z -> ${formatUserMessageTimestamp(new Date("2026-01-03T09:12:00Z").getTime())}`);
	timestamps.push(`2026-12-31T23:59:00Z -> ${formatUserMessageTimestamp(new Date("2026-12-31T23:59:00Z").getTime())}`);
	writeCase("user", "timestamp-format", { cols: 80, rows: 24 }, timestamps, "text");

	// Registry FIFO: duplicate text gets distinct per-occurrence timestamps.
	const registry = new UserMessageTimestampRegistry();
	const t1 = new Date("2026-09-05T08:00:00Z").getTime();
	const t2 = new Date("2026-09-05T09:30:00Z").getTime();
	registry.remember({ role: "user", content: "same text" }, { timestamp: t1 });
	registry.remember({ role: "user", content: "same text" }, { timestamp: t2 });
	const compA = {};
	const compB = {};
	const compC = {};
	const oracle: string[] = [
		`first occurrence -> ${formatUserMessageTimestamp(registry.timestampFor(compA, "same text"))}`,
		`second occurrence -> ${formatUserMessageTimestamp(registry.timestampFor(compB, "same text"))}`,
		// Unknown member without history: falls back to now() only in live mode.
		`unknown text -> ${typeof registry.timestampFor(compC, "other text")}`,
	];

	// Real user surface: markdown + rail + timestamp line, 80 wide.
	try {
		const text = "Fix the build, please.";
		installUserMessageRail(ctxWithBranch([userEntry(text, new Date("2026-09-05T14:05:00Z").getTime())]) as never);
		const component = new UserMessageComponent(text);
		const rows = component.render(80);
		writeCase("user", "surface-basic", { cols: 80, rows: 24 }, rows);
		uninstallUserMessageRail();

		const text2 = "**bold** prompt with `code` and a list:\n- item one\n- item two";
		installUserMessageRail(ctxWithBranch([userEntry(text2, new Date("2026-01-03T09:12:00Z").getTime())]) as never);
		const component2 = new UserMessageComponent(text2);
		writeCase("user", "surface-markdown", { cols: 80, rows: 24 }, component2.render(80));
		uninstallUserMessageRail();

		// Duplicate text across two messages keeps distinct timestamps.
		const entryA = userEntry("retry please", new Date("2026-09-05T11:00:00Z").getTime());
		const entryB = userEntry("retry please", new Date("2026-09-05T11:05:00Z").getTime());
		installUserMessageRail(ctxWithBranch([entryA, entryB]) as never);
		const a = new UserMessageComponent("retry please");
		const b = new UserMessageComponent("retry please");
		writeCase("user", "surface-duplicate-text", { cols: 80, rows: 24 }, [
			...a.render(80),
			...b.render(80),
		]);
		uninstallUserMessageRail();
	} catch (error) {
		writeCase("user", "surface-error", { cols: 80, rows: 24 }, [`install/render failed: ${(error as Error).message}`]);
	}

	oracle.push(`surface case completed with Rail patch active=true`);
	writeCase("user", "registry", { cols: 80, rows: 24 }, oracle, "text");
}