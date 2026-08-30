import type { AppEntry } from './contracts';

/**
 * Deliberately the TypeScript mirror of `clipboard_core::normalize_search_text`
 * (lowercase, `ł` folded, NFD diacritics stripped): the catalog arrives
 * normalized on the Rust side only for ordering, and a user typing "lodz" on
 * any keyboard must find "Łódź" here without another round trip.
 */
export const normalizeForSearch = (value: string): string =>
  value
    .toLocaleLowerCase('en-US')
    .replaceAll('ł', 'l')
    .normalize('NFD')
    .replace(/\p{Diacritic}/gu, '');

type MatchScore = 0 | 1 | 2 | 3;

const scoreApp = (app: AppEntry, needle: string): MatchScore => {
  const name = normalizeForSearch(app.name);
  if (name.startsWith(needle)) return 3;
  if (name.split(/\s+/u).some((word) => word.startsWith(needle))) return 2;
  if (name.includes(needle)) return 1;
  if (
    app.bundleId !== null &&
    normalizeForSearch(app.bundleId).includes(needle)
  ) {
    return 1;
  }
  return 0;
};

/**
 * Narrows the whole catalog to what the typed query matches, best matches
 * first. The catalog is fetched once per mode entry, so this runs on every
 * keystroke — it is pure, allocation-light, and never touches the bridge.
 *
 * Ties keep the backend's alphabetical order: a stable handoff, not a
 * reshuffle on every render.
 */
export const filterApps = (apps: AppEntry[], query: string): AppEntry[] => {
  const trimmed = query.trim();
  if (trimmed === '') return apps;
  const needle = normalizeForSearch(trimmed);
  if (needle === '') return apps;

  return apps
    .map((app, index) => ({ app, index, score: scoreApp(app, needle) }))
    .filter((entry) => entry.score > 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .map((entry) => entry.app);
};
