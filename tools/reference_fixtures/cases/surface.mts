// Rail surface geometry and per-surface row cells (rail-surface.ts) and the
// section wrapper (rail-section-block.ts), invoked directly against the fixed
// reference.

import { importReferenceModule } from "../lib/ctx.mts";
import { writeCase } from "../lib/emit.mts";

const {
	EditorSurfaceRenderer,
	railEditorSurface,
	railThinkingSurface,
	railUserMessageSurface,
	toolExecutionSurfaceForState,
} = await importReferenceModule("rail/rail-surface.ts");
const { RailSectionBlock } = await importReferenceModule("rail/rail-section-block.ts");

export function generateSurfaceCases(): void {
	// Pure geometry oracle across the fixture grid sizes.
	const geometry: string[] = [];
	geometry.push(`surface = railEditorSurface (height min 4 max 12 ratio 0.32)`);
	for (const [rows, cols] of [[16, 60], [24, 80], [40, 120]] as const) {
		const max = railEditorSurface.maxInputHeight(rows);
		geometry.push(`rows=${rows} cols=${cols} contentStartCol=${railEditorSurface.contentStartCol()} contentWidth(${cols})=${railEditorSurface.contentWidth(cols)} maxInputHeight=${max}`);
		for (const body of [1, 4, 8, 13]) {
			geometry.push(`  body=${body} targetInputHeight=${railEditorSurface.targetInputHeight(body, rows)}`);
		}
	}

	// Semantic surface type checks (transparent vs slate).
	geometry.push(`thinking bg styled=${railThinkingSurface.style.background !== ""} rail=${JSON.stringify(railThinkingSurface.style.rail)}`);
	geometry.push(`user bg styled=${railUserMessageSurface.style.background !== ""} rail=${JSON.stringify(railUserMessageSurface.style.rail)}`);
	for (const state of ["pending", "success", "error", "cancelled"] as const) {
		const s = toolExecutionSurfaceForState(state);
		geometry.push(`tool/${state} bg=${JSON.stringify(s.style.background)} rail=${JSON.stringify(s.style.rail)}`);
	}

	// Surface rows: rail column + background fill + reset.
	const cases: Array<[string, EditorSurfaceRenderer, number, string]> = [
		["editor", railEditorSurface, 80, ""],
		["editor-content", railEditorSurface, 80, "hello"],
		["thinking-empty", railThinkingSurface, 80, ""],
		["thinking-content", railThinkingSurface, 80, "thought process"],
		["user-empty", railUserMessageSurface, 80, ""],
		["user-content", railUserMessageSurface, 80, "prompt text"],
		["tool-pending", toolExecutionSurfaceForState("pending"), 80, ""],
		["tool-success", toolExecutionSurfaceForState("success"), 80, ""],
		["tool-error", toolExecutionSurfaceForState("error"), 80, ""],
		["tool-cancelled", toolExecutionSurfaceForState("cancelled"), 80, ""],
	];
	for (const [name, surface, width, content] of cases) {
		const rows = [surface.renderSurfaceRow(width, content)];
		writeCase("surface", name, { cols: width, rows: 1 }, rows);
	}

	// Editor surface blank-padding: one content line inside a min-height 4 row
	// surface: top_padding = floor((4-1)/2) = 1, remainder at the bottom.
	{
		const surface = new EditorSurfaceRenderer(
			{ ...railEditorSurface.style },
			{ minHeight: 4, maxHeight: 12, maxHeightRatio: 0.32 },
		);
		const body = [railEditorSurface.renderSurfaceRow(80, "only line")];
		const target = surface.targetInputHeight(body.length, 24);
		const padding = target - body.length;
		const top = Math.floor(padding / 2);
		const rows = [
			...Array.from({ length: top }, () => railEditorSurface.renderSurfaceRow(80, "")),
			...body,
			...Array.from({ length: padding - top }, () => railEditorSurface.renderSurfaceRow(80, "")),
		];
		writeCase("surface", "editor-blank-padding-1of4", { cols: 80, rows: 24 }, rows);

		// Long body (13 lines) is windowed to the 12-row max keeping the cursor.
		const longBody = Array.from({ length: 13 }, (_, i) => railEditorSurface.renderSurfaceRow(80, `row ${i + 1}`));
		const fittedRows = longBody.slice(0, 12);
		writeCase("surface", "editor-window-13-of-12", { cols: 80, rows: 24 }, fittedRows);
	}

	// RailSectionBlock: transparent thinking rail (no background) vs slate user.
	{
		const inner = { render: (_w: number) => ["alpha", "beta"], invalidate() {} };
		const thinkingBlock = new RailSectionBlock(inner, "assistantThinking");
		writeCase("surface", "section-thinking", { cols: 80, rows: 24 }, thinkingBlock.render(80));

		const userInner = { render: (_w: number) => ["gamma"], invalidate() {} };
		const userBlock = new RailSectionBlock(userInner, "userMessage");
		writeCase("surface", "section-user", { cols: 80, rows: 24 }, userBlock.render(80));
	}

	// Geometry decision record kept as a plain-text oracle alongside cells.
	writeCase("surface", "geometry", { cols: 120, rows: 40 }, geometry, "text");
}