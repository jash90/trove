import type { AppEntry, HistoryItem } from './contracts';

/** One selectable row of the palette, whatever kind of thing it opens. */
export type PaletteItem = { kind: 'app'; app: AppEntry } | { kind: 'history'; item: HistoryItem };

export const keyOfItem = (entry: PaletteItem): string =>
  entry.kind === 'app' ? entry.app.path : `h${entry.item.eventId}`;

/**
 * The palette's single result list.
 *
 * With no query the palette is a launcher: applications first, the history
 * underneath, and Enter on a fresh palette opens the first application.
 * With a query the two ranked lists — applications by their match, history
 * by the backend's ranking — are interleaved one for one, applications
 * first at each depth: a fair merge of two orders nobody can honestly
 * compare, rather than a made-up common score. Each list's own order is
 * never disturbed, and whichever side runs out first simply stops
 * contributing.
 */
export const buildPaletteItems = (
  apps: readonly AppEntry[],
  history: readonly HistoryItem[],
  query: string,
): PaletteItem[] => {
  if (query.trim() === '') {
    return [
      ...apps.map((app): PaletteItem => ({ kind: 'app', app })),
      ...history.map((item): PaletteItem => ({ kind: 'history', item })),
    ];
  }
  const merged: PaletteItem[] = [];
  let appIndex = 0;
  let historyIndex = 0;
  while (appIndex < apps.length || historyIndex < history.length) {
    if (appIndex < apps.length) {
      merged.push({ kind: 'app', app: apps[appIndex]! });
      appIndex += 1;
    }
    if (historyIndex < history.length) {
      merged.push({ kind: 'history', item: history[historyIndex]! });
      historyIndex += 1;
    }
  }
  return merged;
};
