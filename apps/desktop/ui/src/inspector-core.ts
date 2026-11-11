// The pure rules of the right-hand column's tabs (SidePanels.tsx, panel-open.ts).

export type InspectorTab = 'card' | 'memory' | 'metrics';
export const TABS: InspectorTab[] = ['card', 'memory', 'metrics'];
export const TAB_LABEL: Record<InspectorTab, string> = { card: 'Card', memory: 'Memory', metrics: 'Metrics' };

/** The tab a column shows: the chosen one when the column offers it, else its first. */
export const shownTab = (chosen: InspectorTab, offered: InspectorTab[]): InspectorTab =>
  offered.includes(chosen) ? chosen : offered[0]!;
