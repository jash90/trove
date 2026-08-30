import { normalizeForSearch } from './appSearch';
import type { KeyvaultSecret } from './contracts';

type MatchScore = 0 | 1 | 2 | 3;

/**
 * How well one secret answers the query.
 *
 * The slug is weighted like an application's name because it is what a person types: `openai`,
 * not "OpenAI production key". The display name and category still match, lower, so a secret
 * findable only by its description is findable at all.
 */
const scoreSecret = (secret: KeyvaultSecret, needle: string): MatchScore => {
  const slug = normalizeForSearch(secret.slug);
  if (slug.startsWith(needle)) return 3;
  if (slug.split(/[-_\s]+/u).some((word) => word.startsWith(needle))) return 2;
  if (slug.includes(needle)) return 1;
  if (normalizeForSearch(secret.name).includes(needle)) return 1;
  if (secret.category != null && normalizeForSearch(secret.category).includes(needle)) return 1;
  return 0;
};

/**
 * Narrows the vault's metadata to what the query matches, best first.
 *
 * Filtering is local on purpose: the list is fetched once and then searched here, so typing
 * costs nothing on the wire. The alternative — a request per keystroke — would spend the
 * vault's per-token rate limit in a few characters and turn a search into a string of refusals.
 *
 * An empty query matches nothing rather than everything. Secrets are not browsed the way the
 * history is; they are asked for by name, and a palette that listed every key the moment it
 * opened would put them in front of someone who was reaching for something else.
 */
export const filterSecrets = (
  secrets: readonly KeyvaultSecret[],
  query: string,
): KeyvaultSecret[] => {
  const needle = normalizeForSearch(query.trim());
  if (needle === '') return [];
  return secrets
    .map((secret, index) => ({ secret, index, score: scoreSecret(secret, needle) }))
    .filter((entry) => entry.score > 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .map((entry) => entry.secret);
};
