// Rail reference fixture generator (test-only). Invokes the fixed
// pi-rail-ui renderer functions (see RAIL_REF_DIR, default the pinned
// reference directory) and writes normalized cell grids under
// tests/fixtures/rail/.
//
// Run from the repo root:
//   RAIL_REF_DIR=<dir> npm --prefix tools/reference_fixtures run generate
//
// The harness references the pinned Pi 0.84.4 packages from the reference
// directory's node_modules; Node's newer/global Pi must not substitute.

import * as fs from "node:fs";
import * as path from "node:path";
import { setupDeterministicEnv } from "./lib/ctx.mts";
import { PROVENANCE, writeProvenance, FIXTURE_ROOT } from "./lib/emit.mts";

// initTheme("dark") must run before any case module reads theme state, so the
// case groups are loaded lazily with dynamic import.
setupDeterministicEnv();

const runs = [
	["editor", "./cases/editor.mts", "generateEditorCases"],
	["editor-click", "./cases/editor.mts", "generateEditorClickCases"],
	["paste", "./cases/editor.mts", "generatePasteCases"],
	["slash", "./cases/editor.mts", "generateSlashCases"],
	["surface", "./cases/surface.mts", "generateSurfaceCases"],
	["tool", "./cases/tool.mts", "generateToolCases"],
	["user", "./cases/user.mts", "generateUserCases"],
	["thinking", "./cases/thinking.mts", "generateThinkingCases"],
	["footer", "./cases/footer.mts", "generateFooterCases"],
	["scrollbar", "./cases/scrollbar.mts", "generateScrollbarCases"],
	["markdown", "./cases/markdown.mts", "generateMarkdownCases"],
] as const;

writeProvenance();
for (const [group, modulePath, fnName] of runs) {
	clearDir(group);
	const before = countDir(group);
	try {
		const mod = await import(modulePath);
		(mod as Record<string, () => void>)[fnName]();
	} catch (error) {
		console.error(`[generate] group ${group} FAILED:`, (error as Error).message);
		process.exitCode = 1;
		continue;
	}
	const after = countDir(group);
	console.log(`[generate] ${group}: ${before} -> ${after} files`);
}

// The word/paragraph selection oracle is one JSON document (every display
// cell of each sample line) rather than a rail-group of cell grids, so it is
// generated on its own path under tests/fixtures/.
try {
	const wordOracle = await import("./cases/word_oracle.mts");
	wordOracle.generateWordOracleCases();
} catch (error) {
	console.error("[generate] group word-oracle FAILED:", (error as Error).message);
	process.exitCode = 1;
}

console.log(
	`[generate] provenance:\n${JSON.stringify(
		{
			rail_commit: PROVENANCE.rail_commit,
			pi: PROVENANCE.pi,
			theme: PROVENANCE.theme,
			node: PROVENANCE.node,
			tz: PROVENANCE.tz,
		},
		null,
		2,
	)}`,
);
console.log(`[generate] done. exit=${process.exitCode ?? 0}`);

export function countDir(group: string): number {
	const dir = path.join(FIXTURE_ROOT, group);
	if (!fs.existsSync(dir)) return 0;
	return fs.readdirSync(dir).filter((name) => name.endsWith(".json")).length;
}

function clearDir(group: string): void {
	fs.rmSync(path.join(FIXTURE_ROOT, group), { recursive: true, force: true });
}