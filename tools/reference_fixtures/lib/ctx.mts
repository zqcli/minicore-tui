// Shared harness for invoking the fixed reference (pi-rail-ui @ pinned commit
// with Pi 0.84.4) headlessly. Determinism contract:
//   - every fixture run calls initTheme("dark") so Pi's real dark theme
//     singleton drives keyHint style (dim/muted), exactly like an ordinary
//     Rail startup before a user picks a custom theme;
//   - TZ is forced to UTC so Intl en-US timestamp/date output is stable;
//   - the fake editor TUI carries only rows/columns/requestRender, matching
//     the shape Rail's own headless editor tests use.
//
// Rail modules and Pi packages are loaded from REF_DIR at runtime. This is
// deliberate: a checked-out fixture generator must not silently fall back to
// a newer package installed in the repository, the user's home directory, or
// a global Pi installation.

import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const PINNED_RAIL_COMMIT = "1d0dd1611a4d9546c64fe9f5b5c966253fb88eba";
export const PINNED_PI_VERSION = "0.84.4";
export const PI_AGENT_CORE_PACKAGE = "@earendil-works/pi-agent-core";
export const PI_AI_PACKAGE = "@earendil-works/pi-ai";
export const PI_CODING_AGENT_PACKAGE = "@earendil-works/pi-coding-agent";
export const PI_TUI_PACKAGE = "@earendil-works/pi-tui";

const fixtureDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoDir = path.resolve(fixtureDir, "..", "..");
const configuredRefDir = process.env["RAIL_REF_DIR"]?.trim();
export const REF_DIR = path.resolve(
	configuredRefDir || path.join(repoDir, "..", "pi-rail-ui-ref-r1"),
);

function fail(message: string): never {
	throw new Error(`[reference] ${message}`);
}

function readJson(file: string): Record<string, any> {
	try {
		return JSON.parse(fs.readFileSync(file, "utf8")) as Record<string, any>;
	} catch (error) {
		fail(`cannot read JSON ${file}: ${(error as Error).message}`);
	}
}

function git(refDir: string, args: string[]): { status: number | null; stdout: string; stderr: string } {
	const result = spawnSync("git", ["-C", refDir, ...args], { encoding: "utf8" });
	return {
		status: result.status,
		stdout: result.stdout ?? "",
		stderr: result.stderr ?? "",
	};
}

function isPinnedPiPackageLocation(location: string): boolean {
	return /(?:^|\/)node_modules\/@earendil-works\/pi-[^/]+$/u.test(location);
}

/** Validate source provenance and the complete installed Pi package tree. */
export function validateReference(): void {
	if (!fs.existsSync(REF_DIR) || !fs.statSync(REF_DIR).isDirectory()) {
		fail(`RAIL_REF_DIR does not exist or is not a directory: ${REF_DIR}`);
	}

	const head = git(REF_DIR, ["rev-parse", "--verify", "HEAD"]);
	if (head.status !== 0) fail(`RAIL_REF_DIR is not a Git checkout: ${head.stderr.trim()}`);
	if (head.stdout.trim() !== PINNED_RAIL_COMMIT) {
		fail(`Rail HEAD ${head.stdout.trim()} != pinned ${PINNED_RAIL_COMMIT}`);
	}
	const tracked = git(REF_DIR, ["status", "--porcelain", "--untracked-files=no"]);
	if (tracked.status !== 0) fail(`cannot inspect Rail checkout: ${tracked.stderr.trim()}`);
	if (tracked.stdout.trim()) {
		fail(`Rail checkout has tracked modifications:\n${tracked.stdout.trim()}`);
	}

	const manifest = readJson(path.join(REF_DIR, "package.json"));
	const lock = readJson(path.join(REF_DIR, "package-lock.json"));
	const rootDependencies = {
		...(manifest.dependencies ?? {}),
		...(manifest.devDependencies ?? {}),
	};
	for (const packageName of [PI_AGENT_CORE_PACKAGE, PI_AI_PACKAGE, PI_CODING_AGENT_PACKAGE, PI_TUI_PACKAGE]) {
		if (rootDependencies[packageName] !== PINNED_PI_VERSION) {
			fail(`Rail package.json ${packageName} is not pinned to ${PINNED_PI_VERSION}`);
		}
	}

	const lockPackages = lock.packages as Record<string, any> | undefined;
	if (!lockPackages) fail("Rail package-lock.json has no packages table");
	const piLocations = Object.keys(lockPackages).filter(isPinnedPiPackageLocation);
	if (piLocations.length === 0) fail("Rail package-lock.json contains no Pi packages");
	for (const location of piLocations) {
		const lockEntry = lockPackages[location];
		if (lockEntry?.version !== PINNED_PI_VERSION) {
			fail(`Rail lock entry ${location} is ${lockEntry?.version ?? "missing"}, expected ${PINNED_PI_VERSION}`);
		}
		const packageDir = path.join(REF_DIR, location);
		let stat: fs.Stats;
		try {
			stat = fs.lstatSync(packageDir);
		} catch (error) {
			fail(`locked Pi package is not installed: ${packageDir} (${(error as Error).message})`);
		}
		if (!stat.isDirectory() || stat.isSymbolicLink()) {
			fail(`locked Pi package must be a real directory, not a symlink: ${packageDir}`);
		}
		const packageManifest = readJson(path.join(packageDir, "package.json"));
		const expectedName = location.match(/(?:^|\/)(@earendil-works\/pi-[^/]+)$/u)?.[1];
		if (packageManifest.name !== expectedName || packageManifest.version !== PINNED_PI_VERSION) {
			fail(
				`installed package ${location} reports ${packageManifest.name}@${packageManifest.version}; ` +
				`expected ${expectedName}@${PINNED_PI_VERSION}`,
			);
		}
	}
}

function insideReference(file: string): string {
	const resolved = path.resolve(REF_DIR, file);
	const prefix = REF_DIR.endsWith(path.sep) ? REF_DIR : `${REF_DIR}${path.sep}`;
	if (resolved !== REF_DIR && !resolved.startsWith(prefix)) {
		fail(`reference module escapes RAIL_REF_DIR: ${file}`);
	}
	return resolved;
}

export function referenceFilePath(relativePath: string): string {
	return insideReference(relativePath);
}

export async function importReferenceModule(relativePath: string): Promise<any> {
	return import(pathToFileURL(referenceFilePath(relativePath)).href);
}

export function resolveReferencePackage(packageName: string, fromDir = REF_DIR): string {
	let current = path.resolve(fromDir);
	let packageRoot: string | undefined;
	while (true) {
		const candidate = path.join(current, "node_modules", packageName);
		if (fs.existsSync(path.join(candidate, "package.json"))) {
			packageRoot = candidate;
			break;
		}
		if (current === REF_DIR) break;
		const parent = path.dirname(current);
		if (parent === current || (parent !== REF_DIR && !parent.startsWith(`${REF_DIR}${path.sep}`))) break;
		current = parent;
	}
	if (!packageRoot) fail(`cannot resolve ${packageName} below RAIL_REF_DIR`);

	const manifest = readJson(path.join(packageRoot, "package.json"));
	const exportRoot = manifest.exports?.["."];
	const target = typeof exportRoot === "string" ? exportRoot : exportRoot?.import ?? manifest.module ?? manifest.main;
	if (typeof target !== "string") fail(`${packageName} has no import entry point`);
	const entry = path.resolve(packageRoot, target);
	if (!fs.existsSync(entry)) fail(`${packageName} entry does not exist: ${entry}`);
	const referenceNodeModules = path.join(REF_DIR, "node_modules") + path.sep;
	if (!packageRoot.startsWith(referenceNodeModules)) {
		fail(`resolved ${packageName} outside RAIL_REF_DIR: ${entry}`);
	}
	return entry;
}

export async function importReferencePackage(packageName: string, fromDir = REF_DIR): Promise<any> {
	return import(pathToFileURL(resolveReferencePackage(packageName, fromDir)).href);
}

validateReference();

// Pi's coding-agent package ships a nested pi-tui copy. Its keyHint singleton
// reads bindings from that nested instance, so register the real application
// bindings there rather than accidentally configuring another Pi copy.
const { initTheme } = await importReferencePackage(PI_CODING_AGENT_PACKAGE);
const { CURSOR_MARKER } = await importReferencePackage(PI_TUI_PACKAGE);
const { KEYBINDINGS } = await importReferenceModule(
	"node_modules/@earendil-works/pi-coding-agent/dist/core/keybindings.js",
);
const { KeybindingsManager, TUI_KEYBINDINGS, setKeybindings } = await importReferenceModule(
	"node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-tui/dist/index.js",
);
export const PI_DARK = "dark";

export function setupDeterministicEnv(): void {
	process.env.TZ = "UTC";
	initTheme(PI_DARK);
	// Pi's app registers its own bindings on top of the TUI ones; without this
	// the singleton `getKeybindings()` has no `app.tools.expand` and keyHint
	// renders an empty keyname. Register the real Pi default (ctrl+o) so the
	// collapseHint captures match the running app.
	setKeybindings(new KeybindingsManager({ ...TUI_KEYBINDINGS, ...KEYBINDINGS }));
}

/** Pi 0.84.4 dark palette values that the theme singleton exposes for the
 * only themed keys Rail consults headlessly (muted/dim hint text). */
export const PI_DARK_MUTED: [number, number, number] = [128, 128, 128];
export const PI_DARK_DIM: [number, number, number] = [102, 102, 102];

export type ThemeLike = { fg(name: string, value: string): string };

/** ThemeLike mirroring the Pi dark theme for muted/dim. Rail resolves its own
 * rail/tool colors from ui-style.json explicit RGB, so this theme is only
 * consulted for hint keyname styling. */
export function mockTheme(): ThemeLike {
	const fg = (name: string, value: string) => {
		if (name === "muted") return `\x1b[38;2;${PI_DARK_MUTED.join(";")}m${value}`;
		if (name === "dim") return `\x1b[38;2;${PI_DARK_DIM.join(";")}m${value}`;
		return value;
	};
	return { fg };
}

export type FakeTui = {
	terminal: { rows: number; columns: number };
	requestRender(): void;
};

export function fakeTui(rows: number, columns: number): FakeTui {
	return { terminal: { rows, columns }, requestRender() {} };
}

/** Construct the same real Pi keybinding manager used by the reference TUI. */
export function referenceKeybindings(): InstanceType<typeof KeybindingsManager> {
	return new KeybindingsManager({ ...TUI_KEYBINDINGS, ...KEYBINDINGS });
}

export function passthrough<T>(text: T): T {
	return text;
}

export function editorTheme() {
	return {
		borderColor: passthrough,
		selectList: {
			selectedPrefix: passthrough,
			selectedText: passthrough,
			description: passthrough,
			scrollInfo: passthrough,
			noMatch: passthrough,
		},
	};
}

export { CURSOR_MARKER };

/** Freeze the process clock so wall-time invariants (footer duration) are
 * reproducible. Returns a restore function. */
export function withFixedNow(nowMs: number, run: () => void): void {
	const original = Date.now;
	Date.now = () => nowMs;
	try {
		run();
	} finally {
		Date.now = original;
	}
}