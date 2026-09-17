import { AppWindowMac, ClipboardList, Search, Vault } from 'lucide-react';
import type { ChangeEventHandler, KeyboardEventHandler, Ref } from 'react';

import { formatCount } from '../lib/format';
import { TypeFilter } from './TypeFilter';

/** One category the palette can be showing. */
export type PaletteMode = 'history' | 'apps' | 'vault';

/**
 * What the palette is showing: one of the three categories, `'home'` — the
 * category chooser the palette opens on — or `'all'`, the combined list the
 * settings can restore, where the field drives everything at once and the
 * category control is not on screen at all.
 */
export type PaletteView = PaletteMode | 'all' | 'home';

/** The categories in picker order: Applications, Clipboard history, Key vault. */
export const PALETTE_CATEGORIES: readonly {
  mode: PaletteMode;
  key: string;
  label: string;
}[] = [
  { mode: 'apps', key: '1', label: 'Applications' },
  { mode: 'history', key: '2', label: 'Clipboard history' },
  { mode: 'vault', key: '3', label: 'Key vault' },
];

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
  onModeChange: (mode: PaletteMode) => void;
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
/// `home` typing means history, the palette's own core. The category
/// control beside it names all three: Tab and ⌘1/⌘2/⌘3 reach them from
/// the field, and a click works too.
export const PaletteHeader = ({
  query,
  mode,
  activeDescendant,
  resultCount,
  resultsTruncated,
  refreshing,
  searchInputRef,
  onQueryChange,
  onModeChange,
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
      : 'Search history';
  const searchPlaceholder = onHome
    ? 'Search history, or pick a category…'
    : mode === 'apps'
      ? 'Search applications…'
      : mode === 'vault'
        ? 'Search the vault…'
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
      {combined ? null : (
      <div className="palette-mode" role="group" aria-label="Palette categories">
        <button
          type="button"
          className="palette-mode__option"
          aria-pressed={mode === 'history'}
          title="Clipboard history (2)"
          onClick={() => onModeChange('history')}
        >
          <ClipboardList size={14} strokeWidth={1.8} aria-hidden="true" />
          History
        </button>
        <button
          type="button"
          className="palette-mode__option"
          aria-pressed={mode === 'apps'}
          title="Applications (1)"
          onClick={() => onModeChange('apps')}
        >
          <AppWindowMac size={14} strokeWidth={1.8} aria-hidden="true" />
          Apps
        </button>
        <button
          type="button"
          className="palette-mode__option"
          aria-pressed={mode === 'vault'}
          title="Key vault (3)"
          onClick={() => onModeChange('vault')}
        >
          <Vault size={14} strokeWidth={1.8} aria-hidden="true" />
          Vault
        </button>
      </div>
      )}
      {mode === 'history' || mode === 'all' ? (
        <TypeFilter query={query} onQueryChange={onQueryChange} />
      ) : null}
    </header>
  );
};
