import type { AppEntry, HistoryItem, KeyvaultSecret } from './contracts';

/** One selectable row of the palette, whatever kind of thing it opens. */
export type PaletteItem =
  | { kind: 'app'; app: AppEntry }
  | { kind: 'vault'; secret: KeyvaultSecret }
  | { kind: 'history'; item: HistoryItem };

export const keyOfItem = (entry: PaletteItem): string => {
  if (entry.kind === 'app') return entry.app.path;
  // Slugs are unique within a vault, and prefixed so they cannot collide with a history key.
  if (entry.kind === 'vault') return `v${entry.secret.slug}`;
  return `h${entry.item.eventId}`;
};

/**
 * The palette's single result list.
 *
 * With no query the palette is a launcher: applications first, the history
 * underneath, and Enter on a fresh palette opens the first application.
 * With a query the ranked lists — vault secrets and applications by their
 * match, history by the backend's ranking — are interleaved one for one,
 * secrets first at each depth, then applications: a fair merge of orders
 * nobody can honestly compare, rather than a made-up common score. Each
 * list's own order is never disturbed, and whichever side runs out first
 * simply stops contributing.
 *
 * Secrets lead because naming one is a deliberate act: a person typing
 * `openai` with a vault configured is reaching for the key, not hoping to
 * find it below whatever else matched. They are absent without a query —
 * the catalog is not even fetched then — so the launcher view is unchanged.
 */
export const buildPaletteItems = (
  apps: readonly AppEntry[],
  history: readonly HistoryItem[],
  query: string,
  secrets: readonly KeyvaultSecret[] = [],
): PaletteItem[] => {
  if (query.trim() === '') {
    return [
      ...apps.map((app): PaletteItem => ({ kind: 'app', app })),
      ...history.map((item): PaletteItem => ({ kind: 'history', item })),
    ];
  }
  const merged: PaletteItem[] = [];
  let secretIndex = 0;
  let appIndex = 0;
  let historyIndex = 0;
  while (
    secretIndex < secrets.length ||
    appIndex < apps.length ||
    historyIndex < history.length
  ) {
    if (secretIndex < secrets.length) {
      merged.push({ kind: 'vault', secret: secrets[secretIndex]! });
      secretIndex += 1;
    }
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
