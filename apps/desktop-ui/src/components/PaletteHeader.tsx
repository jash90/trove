import { Search } from 'lucide-react';
import type { ChangeEventHandler, KeyboardEventHandler, Ref } from 'react';

import { formatCount } from '../lib/format';
import { TypeFilter } from './TypeFilter';

interface PaletteHeaderProps {
  query: string;
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
/// The field drives both lists at once — applications filtered on the
/// client, history over the bridge — so it points `aria-controls` at both
/// and `aria-activedescendant` at whichever row either list has selected.
export const PaletteHeader = ({
  query,
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

  return (
    <header className="palette-header">
      <label className="search-field" htmlFor="history-search">
        <Search className="search-field__icon" size={19} strokeWidth={1.8} aria-hidden="true" />
        <span className="sr-only">Search applications, secrets and history</span>
        <input
          ref={searchInputRef}
          id="history-search"
          type="search"
          autoComplete="off"
          spellCheck={false}
          value={query}
          placeholder="Szukaj w aplikacjach i historii…"
          aria-controls="apps-results history-results"
          aria-autocomplete="list"
          aria-label="Search applications, secrets and history"
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
      <TypeFilter query={query} onQueryChange={onQueryChange} />
    </header>
  );
};
