// The pure rules of the right-hand column's tabs (SidePanels.tsx, panel-open.ts).

export type InspectorTab = 'memory' | 'metrics' | 'profile';
export const TABS: InspectorTab[] = ['memory', 'metrics', 'profile'];
export const TAB_LABEL: Record<InspectorTab, string> = {
  memory: 'Memory',
  metrics: 'Metrics',
  profile: 'Profile',
};

/** The tab a column shows: the chosen one when the column offers it, else its first. */
export const shownTab = (chosen: InspectorTab, offered: InspectorTab[]): InspectorTab =>
  offered.includes(chosen) ? chosen : offered[0]!;
