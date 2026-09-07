// Scrollbar cells: the fixed reference's real railScrollbarGeometry and real
// drawRailScrollbar with a synthetic layout tree (same shape as Rail's own
// rail-scrollbar tests). Locks thumb sizing, top/mid/bottom placement and the
// drag preview/commit contract via the geometry numbers.

import { importReferenceModule } from "../lib/ctx.mts";
import { writeCase } from "../lib/emit.mts";

const {
	railScrollbarGeometry,
	drawRailScrollbar,
	markRailScrollbarView,
	isRailScrollbarView,
} = await importReferenceModule("rail/rail-scrollbar.ts");

function fakeScrollView(over: { currentScrollTop?: number; currentScrollbar?: string } = {}): any {
	const view: any = {
		primary: true,
		currentScrollbar: over.currentScrollbar ?? "always",
		currentScrollTop: over.currentScrollTop ?? 0,
		transientScrollbarVisible: false,
		get scrollTop() {
			return this.currentScrollTop;
		},
		setScrollbar(value: string) {
			this.currentScrollbar = value;
		},
		hideTransientScrollbar() {
			this.transientScrollbarVisible = false;
		},
	};
	return view;
}

function fakeLayout(scrollView: any, totalRows: number, viewportHeight = 20, y = 2): any {
	const box = {
		scrollView,
		rect: { x: 0, y, width: 80, height: viewportHeight },
		clip: { x: 0, y, width: 80, height: viewportHeight },
		children: [],
		scrollContentLines: Array.from({ length: totalRows }, () => " ".repeat(80)),
	};
	return { primaryScrollView: scrollView, root: { children: [box] } };
}

function screen(rows: number): string[] {
	return Array.from({ length: rows }, () => " ".repeat(80));
}

export function generateScrollbarCases(): void {
	// Geometry oracle at top / middle / bottom and at the overflow boundary.
	const geometry: string[] = [];
	for (const [scrollTop, label] of [[0, "top"], [40, "mid"], [80, "bottom"]] as const) {
		const scrollView = fakeScrollView({ currentScrollTop: scrollTop });
		const layout = fakeLayout(scrollView, 100, 20, 0);
		const g = railScrollbarGeometry(layout, scrollView);
		geometry.push(`${label}: ${JSON.stringify(g)}`);
	}
	// No overflow -> no scrollbar.
	{
		const scrollView = fakeScrollView({ currentScrollTop: 0 });
		const layout = fakeLayout(scrollView, 20, 20, 0);
		geometry.push(`content=track -> ${JSON.stringify(railScrollbarGeometry(layout, scrollView))}`);
	}
	// Draw output: marked view renders the blue thumb on the geometry rows.
	{
		const scrollView = fakeScrollView({ currentScrollTop: 10 });
		markRailScrollbarView(scrollView);
		const layout = fakeLayout(scrollView, 100, 20, 0);
		const out = drawRailScrollbar(screen(24), layout, 80);
		writeCase("scrollbar", "thumb-mid", { cols: 80, rows: 24 }, out);
		geometry.push(`marked=${isRailScrollbarView(scrollView)} scrollbar-mode=${scrollView.currentScrollbar}`);
	}
	// Top and bottom placement.
	{
		const top = fakeScrollView({ currentScrollTop: 0 });
		markRailScrollbarView(top);
		writeCase("scrollbar", "thumb-top", { cols: 80, rows: 24 }, drawRailScrollbar(screen(24), fakeLayout(top, 100, 20, 0), 80));

		const bottom = fakeScrollView({ currentScrollTop: 80 });
		markRailScrollbarView(bottom);
		writeCase("scrollbar", "thumb-bottom", { cols: 80, rows: 24 }, drawRailScrollbar(screen(24), fakeLayout(bottom, 100, 20, 0), 80));
	}
	// Drag preview (pending scrollTop is not committed until release): the
	// geometry is derived from the pending scrollTop, matching the preview
	// contract; no release event in the reference's pure state.
	{
		const drag = fakeScrollView({ currentScrollTop: 60 });
		markRailScrollbarView(drag);
		const layout = fakeLayout(drag, 100, 20, 0);
		const g = railScrollbarGeometry(layout, drag);
		geometry.push(`drag-preview at 60: ${JSON.stringify(g)}`);
		writeCase("scrollbar", "geometry", { cols: 80, rows: 24 }, geometry, "text");
	}
}