import type { AppEntry, HistoryItem, KeyvaultSecret } from './contracts';
import { normalizeForSearch } from './appSearch';

/** One selectable row of the palette, whatever kind of thing it opens. */
export type PaletteItem =
  | { kind: 'command'; enabled: boolean | null; pending: boolean }
  | { kind: 'app'; app: AppEntry }
  | { kind: 'vault'; secret: KeyvaultSecret }
  | { kind: 'history'; item: HistoryItem };

export const keyOfItem = (entry: PaletteItem): string => {
  if (entry.kind === 'command') return 'command:keep-awake';
  if (entry.kind === 'app') return entry.app.path;
  // Slugs are unique within a vault, and prefixed so they cannot collide with a history key.
  if (entry.kind === 'vault') return `v${entry.secret.slug}`;
  return `h${entry.item.eventId}`;
};

export const matchesKeepAwake = (query: string): boolean => {
  const normalized = query.normalize('NFD').replace(/\p{M}/gu, '')
    .replace(/ł/giu, 'l').trim().toLowerCase().replace(/\s+/gu, ' ');
  return normalized.length >= 4 && [
    'keep awake', 'caffeine', 'amphetamine', 'nie usypiaj',
    'blokuj wygaszanie', 'nie wygaszaj ekranu', 'zapobiegaj uspieniu',
    'wylacz usypianie', 'bez usypiania',
  ].some((alias) => alias.startsWith(normalized));
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
      // Secrets reach this branch from the vault category, which browses
      // them with no query at all; everywhere else they arrive filtered to
      // nothing here, so the launcher view is unchanged.
      ...secrets.map((secret): PaletteItem => ({ kind: 'vault', secret })),
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
  if (apps.length === 0 || history.length === 0) return merged;
  // A contains-only app must not displace a clipboard prefix in the combined view.
  const needle = normalizeForSearch(query.trim());
  const prefixRank = (entry: PaletteItem): number => {
    const text = entry.kind === 'app' ? entry.app.name : entry.kind === 'history' ? entry.item.preview : '';
    return normalizeForSearch(text).startsWith(needle) ? 0 : 1;
  };
  const results = merged.filter((entry) => entry.kind !== 'vault')
    .sort((left, right) => prefixRank(left) - prefixRank(right));
  let index = 0;
  return merged.map((entry) => entry.kind === 'vault' ? entry : results[index++]!);
};
