import { Search } from 'lucide-react';
import type { ChangeEventHandler, KeyboardEventHandler, Ref } from 'react';

import { formatCount } from '../lib/format';
import { TypeFilter } from './TypeFilter';

/** One category the palette can be showing. */
export type PaletteMode = "history" | "apps" | "vault" | "windows";

/**
 * What the palette is showing: one of the four categories, `'home'` — the
 * category chooser the palette opens on — or `'all'`, the combined list the
 * settings can restore, where the field drives everything at once and the
 * category control is not on screen at all.
 */
export type PaletteView = PaletteMode | 'all' | 'home';

/** The categories in picker order. */
export const PALETTE_CATEGORIES: readonly {
  mode: PaletteMode;
  key: string;
  label: string;
}[] = [
  { mode: 'apps', key: '1', label: 'Applications' },
  { mode: 'history', key: '2', label: 'Clipboard history' },
  { mode: 'vault', key: '3', label: 'Key vault' },
  { mode: 'windows', key: '4', label: 'Windows' },
];

/** The chat tile: not a palette view but a window of its own, opened from
 * the chooser like the rest and living beside it. */
export const PALETTE_CHAT_CATEGORY = { key: '5', label: 'Chat' } as const;

interface PaletteHeaderProps {
  query: string;
  mode: PaletteView;
  /** The option id the field should point at, in either list. */
  activeDescendant?: string;
  resultCount: number;
  /** True when the history list is full, so more entries match than are shown. */
  resultsTruncated: boolean;
  /** True while a newer history query is on its way over results on screen. */
  refreshing: boolean;
  searchInputRef?: Ref<HTMLInputElement>;
  onQueryChange: (query: string) => void;
  onKeyDown: KeyboardEventHandler<HTMLInputElement>;
}

/// The whole top of the palette: one row.
///
/// It used to carry a title, a logo, a static "listening" badge and two
/// buttons above the search field. None of it was ever read twice, and all
/// of it pushed the results down. What a person summons a palette for is
/// the field and the list, so that is what the top is now.
///
/// The field drives whichever category it names — history over the bridge,
/// applications on the client, the vault's metadata once asked — and on
/// `home` typing means history, the palette's own core. The categories are
/// the home tiles and their keys (1/2/3/4, Tab, ⌘1–⌘4); nothing else
/// competes with the field for the top of the palette.
export const PaletteHeader = ({
  query,
  mode,
  activeDescendant,
  resultCount,
  resultsTruncated,
  refreshing,
  searchInputRef,
  onQueryChange,
  onKeyDown,
}: PaletteHeaderProps): React.JSX.Element => {
  const handleChange: ChangeEventHandler<HTMLInputElement> = (event) => {
    onQueryChange(event.currentTarget.value);
  };
  const combined = mode === 'all';
  const onHome = mode === 'home';
  const searchLabel = mode === 'apps'
    ? 'Search applications'
    : mode === 'vault'
      ? 'Search the vault'
      : mode === 'windows'
        ? 'Arrange windows'
        : 'Search history';
  const searchPlaceholder = onHome
    ? 'Search history, or pick a category…'
    : mode === 'apps'
      ? 'Search applications…'
      : mode === 'vault'
        ? 'Search the vault…'
        : mode === 'windows'
          ? 'Pick an arrangement…'
          : mode === 'history'
            ? 'Search history…'
            : 'Search applications and history…';

  return (
    <header className="palette-header">
      <label className="search-field" htmlFor="history-search">
        <Search className="search-field__icon" size={19} strokeWidth={1.8} aria-hidden="true" />
        <span className="sr-only">{searchLabel}</span>
        <input
          ref={searchInputRef}
          id="history-search"
          type="search"
          autoComplete="off"
          spellCheck={false}
          value={query}
          placeholder={searchPlaceholder}
          aria-controls="apps-results history-results"
          aria-autocomplete="list"
          aria-label={searchLabel}
          aria-activedescendant={activeDescendant}
          onChange={handleChange}
          onKeyDown={onKeyDown}
        />
        {/* The list is a page, not the whole history: saying "80 results"
            when thousands match reads as a total and is simply untrue.
            Announced only once it settles — mid-typing it would read out a
            new number on every letter. */}
        <span
          className={`search-field__count${refreshing ? ' is-refreshing' : ''}`}
          aria-live="polite"
          aria-busy={refreshing}
        >
          {formatCount(resultCount)}
          {resultsTruncated ? '+' : ''}
        </span>
      </label>
      {mode === 'history' || mode === 'all' ? (
        <TypeFilter query={query} onQueryChange={onQueryChange} />
      ) : null}
    </header>
  );
};
