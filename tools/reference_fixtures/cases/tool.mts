// Tool execution rail cells: the fixed reference's real renderer pipeline
// (execution-rail.ts -> execution-collapse.ts -> rail-surface.ts) invoked with
// the same stub-component shape Rail's own tests use. This exercises the
// three-line simple collapse, the auto-collapse estimate, write default
// collapse, per-state background/rail, `$ cmd` / path:range detail and hidden
// counts.

import {
	importReferenceModule,
	importReferencePackage,
	PI_CODING_AGENT_PACKAGE,
} from "../lib/ctx.mts";
import { writeCase, writeCaseAnnotated } from "../lib/emit.mts";
import { mockTheme } from "../lib/ctx.mts";

const { ToolExecutionComponent } = await importReferencePackage(PI_CODING_AGENT_PACKAGE);
const { renderExecutionRail } = await importReferenceModule("components/executions/execution-rail.ts");
const {
	collapsedSimpleLine,
	collapsedSimpleContentRows,
	collapsedSimpleRows,
	executionHiddenLineCount,
	simpleCollapseHint,
} = await importReferenceModule("components/executions/execution-collapse.ts");
const { collapseHint, markRailSectionManuallyToggled } = await importReferenceModule("rail/rail-section.ts");

type ToolStub = {
	expanded: boolean;
	isPartial: boolean;
	result?: { isError?: boolean } | undefined;
	args?: Record<string, unknown>;
	toolCallId?: string;
	toolName: string;
	hasRendererDefinition?(): boolean;
	getTextOutput?(): string;
	getOutput?(): string;
	outputLines?: string[];
	getCommand?(): string;
	setExpanded?(expanded: boolean): void;
	contentContainer?: { render(width: number): string[] };
};

function toolStub(over: Partial<ToolStub>): ToolStub {
	return {
		expanded: true,
		isPartial: false,
		result: {},
		toolCallId: "call-1",
		toolName: "bash",
		hasRendererDefinition: () => true,
		setExpanded(expanded: boolean) { this.expanded = expanded; },
		...over,
	};
}

function bashStub(command: string, outputLines: number): ToolStub {
	const lines = Array.from({ length: outputLines }, (_, i) => `output line ${i + 1}`);
	return toolStub({
		toolName: "bash",
		args: { command },
		hasRendererDefinition: () => false,
		getCommand: () => command,
		outputLines: lines,
		getOutput: () => lines.join("\n"),
		contentContainer: {
			render: () => ["bash", `$ ${command}`, ...lines],
		},
	});
}

const store = { active: true, theme: mockTheme() };

function modelTool(name: string, args: Record<string, unknown>, outputLines: string[], isError = false): ToolStub {
	const component = new ToolExecutionComponent(
		name,
		`model-${name}`,
		args,
		{},
		undefined,
		{ requestRender() {} },
		"/project",
	);
	component.markExecutionStarted();
	component.setArgsComplete();
	component.updateResult({
		content: outputLines.map((text) => ({ type: "text", text })),
		isError,
	}, false);
	return component as unknown as ToolStub;
}

function nativeOriginal(width: number): string[] {
	return [`native title line width=${width}`];
}

export function generateToolCases(): void {
	// Pure accounting / formatting oracles (real functions).
	const hints: string[] = [
		`collapseHint(theme, 5)=${JSON.stringify(collapseHint(mockTheme(), 5))}`,
		`collapseHint(theme, 0)=${JSON.stringify(collapseHint(mockTheme(), 0))}`,
		`collapseHint(undefined, 5)=${JSON.stringify(collapseHint(undefined, 5))}`,
		`simpleCollapseHint(theme, 68)=${JSON.stringify(simpleCollapseHint(mockTheme(), 68))}`,
		`collapsedSimpleLine('a\\nb\\tc\\td')=${JSON.stringify(collapsedSimpleLine("a\nb\tc\td"))}`,
		`collapsedSimpleLine('multi  space')=${JSON.stringify(collapsedSimpleLine("multi  space"))}`,
		`hidden(write content 2+out1)=${executionHiddenLineCount(toolStub({ toolName: "write", args: { content: "a\nb" }, getTextOutput: () => "created" }), "toolExecution")}`,
		`hidden(edit old2/new1+out1)=${executionHiddenLineCount(toolStub({ toolName: "edit", args: { oldText: "l1\nl2", newText: "n1" }, getTextOutput: () => "ok" }), "toolExecution")}`,
		`hidden(generic args)=${executionHiddenLineCount(toolStub({ args: { scope: "x", limit: 3 } }), "toolExecution")}`,
		`simpleRows=${JSON.stringify(collapsedSimpleContentRows("write", "src/a.ts", 42, mockTheme()))}`,
		`simpleRowsOuter=${JSON.stringify(collapsedSimpleRows("bash", "$ cargo test", 20, mockTheme()))}`,
	];
	writeCase("tool", "accounting", { cols: 120, rows: 40 }, hints, "text");

	// Per-tool-state surfaces with a real native content row wrapped by the
	// real renderer (expanded path).
	const states: Array<[string, ToolStub]> = [
		["pending", toolStub({ isPartial: true, toolName: "bash", args: { command: "cargo check" } })],
		["success", toolStub({ result: { isError: false }, toolName: "read", args: { path: "src/main.rs" } })],
		["error", toolStub({ result: { isError: true }, toolName: "bash", args: { command: "npm run check" } })],
		// Fixed reference quirk: no cancelled surface in the renderer's state
		// mapping; a cancelled (never-completed) tool stays isPartial -> pending.
		["cancelled-as-pending", toolStub({ isPartial: true, toolName: "write", args: { path: "/tmp/x" } })],
	];
	for (const [name, component] of states) {
		const rows = renderExecutionRail(component, 80, nativeOriginal, store);
		writeCase("tool", `state-${name}`, { cols: 80, rows: 24 }, rows);
	}

	// Three-line simple collapsed layout. The real auto-collapse decision runs
	// first (write forced folded); the other simple cases are explicitly
	// collapsed as a user would have (real manual-toggle marker) to pin the
	// exact three-line cells.
	const simpleCases: Array<[string, ToolStub]> = [
		["write-forced-collapse", toolStub({ toolName: "write", args: { path: "src/app.rs" }, getTextOutput: () => "created", hasRendererDefinition: () => false, contentContainer: { render: () => [] } })],
		["reference-bash-mode-command", (() => { const c = bashStub("cargo test --all-targets", 0); c.expanded = false; markRailSectionManuallyToggled(c); return c; })()],
		["read-range", (() => { const c = toolStub({ toolName: "read", args: { path: "/project/src/main.rs", offset: 10, limit: 5 }, hasRendererDefinition: () => false, contentContainer: { render: () => [] } }); c.expanded = false; markRailSectionManuallyToggled(c); return c; })()],
		["generic-tool", (() => { const c = toolStub({ toolName: "apply_patch", args: { patch: "---a\n+++b\n@@ -1 +1 @@\n-x\n+y" }, hasRendererDefinition: () => false, contentContainer: { render: () => [] } }); c.expanded = false; markRailSectionManuallyToggled(c); return c; })()],
	];
	for (const [name, component] of simpleCases) {
		const rows = renderExecutionRail(component, 80, nativeOriginal, store);
		writeCase("tool", `simple-${name}`, { cols: 80, rows: 24 }, rows);
	}

	// These are the legacy BashExecutionComponent-shaped fixtures. A getCommand
	// method intentionally selects Rail's user !bash surface, so they are
	// reference-only and not model ToolExecutionComponent acceptance fixtures.
	for (const n of [18, 19, 20, 21, 68]) {
		const component = bashStub("make", n);
		const rows = renderExecutionRail(component, 80, nativeOriginal, store);
		writeCaseAnnotated("tool", `reference-bash-mode-output-${n}`, { cols: 80, rows: 24 }, [
			"source-only bashExecution fixture; not a model ToolExecutionComponent oracle",
			`expanded=${component.expanded}`,
			`hidden=${executionHiddenLineCount(component, "bashExecution")}`,
		], rows);
	}

	// Expandability: an expanded card must reach all provided rows (no fixed
	// 30/40-line truncation in the surface path).
	{
		const component = bashStub("gen", 50);
		component.expanded = true;
		component.contentContainer = {
			render: (width: number) =>
				["bash", "$ gen", ...Array.from({ length: 50 }, (_, i) => `row ${i}`)].map((line) => line.padEnd(width)),
		};
		const rows = renderExecutionRail(component, 80, nativeOriginal, store);
		writeCase("tool", "expanded-50-rows", { cols: 80, rows: 24 }, rows);
	}

	// Real model ToolExecutionComponent fixtures. Their constructor identity
	// exercises the green toolExecution surface, including simple-three-row
	// rendering and the actual native tool call/result content path.
	for (const [name, component] of [
		["model-tool-simple-write", modelTool("write", { path: "src/app.rs", content: "line 1\nline 2" }, ["created"])],
		["model-tool-simple-read", modelTool("read", { path: "src/app.rs", offset: 10, limit: 5 }, ["line 10", "line 11"])],
		["model-tool-simple-bash", modelTool("bash", { command: "cargo test --all-targets" }, ["ok"])],
	] as const) {
		const rows = renderExecutionRail(component, 80, nativeOriginal, store);
		writeCaseAnnotated("tool", name, { cols: 80, rows: 24 }, [
			`component=${component.constructor.name}`,
			`expanded=${component.expanded}`,
			`hidden=${executionHiddenLineCount(component, "toolExecution")}`,
		], rows);
	}

	// For an actual ToolExecutionComponent without a built-in renderer, the
	// source estimator counts pretty-printed args and output rows. Capture its
	// decisions at 19/20/21 rather than assuming output length is the count.
	for (const n of [19, 20, 21]) {
		const component = modelTool("custom_tool", {}, Array.from({ length: n }, (_, i) => `result ${i + 1}`));
		const rows = renderExecutionRail(component, 80, nativeOriginal, store);
		writeCaseAnnotated("tool", `model-tool-output-${n}`, { cols: 80, rows: 24 }, [
			`component=${component.constructor.name}`,
			`expanded=${component.expanded}`,
			`hidden=${executionHiddenLineCount(component, "toolExecution")}`,
		], rows);
	}
}