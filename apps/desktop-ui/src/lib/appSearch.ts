import type { AppEntry } from './contracts';

/**
 * Deliberately the TypeScript mirror of `trove_core::normalize_search_text`
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

/**
 * Words are cut on every non-alphanumeric run, not only on whitespace:
 * "PDF-Expert", "Figma·2" and "Visual Studio Code" all owe the user a word
 * match on their parts, whatever the bundle's name separator happens to be.
 */
const wordsOf = (normalized: string): string[] =>
  normalized.split(/[^a-z0-9]+/u).filter((word) => word !== '');

const initialsOf = (words: readonly string[]): string =>
  words.map((word) => word.charAt(0)).join('');

/**
 * Whether every character of `needle` appears in `haystack` in order —
 * "chrm" finding "Chrome". Loose by design: it is the last rank before no
 * match at all, so it widens the net without ever outranking a tighter form.
 */
const isSubsequence = (needle: string, haystack: string): boolean => {
  let matched = 0;
  for (let index = 0; index < haystack.length; index += 1) {
    if (haystack.charAt(index) === needle.charAt(matched)) {
      matched += 1;
      if (matched === needle.length) return true;
    }
  }
  return needle.length === 0;
};

/**
 * The bundle directory's own name — the last path component without its
 * `.app` suffix. It is the second thing a user knows an application by: the
 * plist's display name and the file on disk disagree more often than either
 * admits, and a launcher that searches only one of them reports the other
 * as missing.
 */
const stemOf = (app: AppEntry): string => {
  const withoutSuffix = app.path.replace(/\.app$/u, '');
  const slash = withoutSuffix.lastIndexOf('/');
  return slash === -1 ? withoutSuffix : withoutSuffix.slice(slash + 1);
};

/**
 * The match ladder, best first:
 *
 * 6 — the query is the name's beginning
 * 5 — the query begins one of the name's words
 * 4 — the query begins the name's initials ("vsc" → "Visual Studio Code")
 * 3 — the query appears inside the name
 * 2 — the same first three steps against the bundle directory's name
 * 1 — the query's characters appear in the name in order ("chrm" → "Chrome"),
 *     or appear inside the bundle identifier
 *
 * Every rung exists because real typing falls through the ones above it:
 * abbreviations, hyphenated names, and a display name that is not the name
 * on the disk are each a whole class of "it is on the list but I cannot
 * search for it" otherwise.
 */
type MatchScore = 0 | 1 | 2 | 3 | 4 | 5 | 6;

const scoreApp = (app: AppEntry, needle: string): MatchScore => {
  const name = normalizeForSearch(app.name);
  if (name.startsWith(needle)) return 6;
  const words = wordsOf(name);
  if (words.some((word) => word.startsWith(needle))) return 5;
  if (initialsOf(words).startsWith(needle)) return 4;
  if (name.includes(needle)) return 3;

  const stem = normalizeForSearch(stemOf(app));
  if (stem.startsWith(needle)) return 2;
  if (
    wordsOf(stem).some((word) => word.startsWith(needle)) ||
    stem.includes(needle)
  ) {
    return 2;
  }

  if (isSubsequence(needle, name)) return 1;
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
 * first. The catalog is fetched per palette opening, so this runs on every
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
