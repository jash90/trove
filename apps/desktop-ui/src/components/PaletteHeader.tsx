import { Search } from 'lucide-react';
import type { ChangeEventHandler, KeyboardEventHandler, Ref } from 'react';

import { formatCount } from '../lib/format';
import { TypeFilter } from './TypeFilter';

export type PaletteMode = 'history' | 'apps';

interface PaletteHeaderProps {
  mode: PaletteMode;
  query: string;
  selectedId: number | null;
  resultCount: number;
  /** True when the list is full, so more entries match than are shown. */
  resultsTruncated: boolean;
  /** True while a newer query is on its way over results already on screen. */
  refreshing: boolean;
  /** The launcher option id the field should point at, ready-made. */
  appsActiveDescendant?: string;
  searchInputRef?: Ref<HTMLInputElement>;
  onQueryChange: (query: string) => void;
  onKeyDown: KeyboardEventHandler<HTMLInputElement>;
}

/// The whole top of the palette: one row.
///
/// It used to carry a title, a logo, a static "listening" badge and two
/// buttons above the search field. None of it was ever read twice, and all
/// of it pushed the results down. What a person summons a clipboard palette for
/// is the field and the list, so that is what the top is now.
///
/// The field is shared by both modes: it never remounts, so focus survives
/// the Tab toggle, and only its labels and the list they point at change.
export const PaletteHeader = ({
  mode,
  query,
  selectedId,
  resultCount,
  resultsTruncated,
  refreshing,
  appsActiveDescendant,
  searchInputRef,
  onQueryChange,
  onKeyDown,
}: PaletteHeaderProps): React.JSX.Element => {
  const handleChange: ChangeEventHandler<HTMLInputElement> = (event) => {
    onQueryChange(event.currentTarget.value);
  };
  const inAppsMode = mode === 'apps';

  return (
    <header className="palette-header">
      <label className="search-field" htmlFor="history-search">
        <Search className="search-field__icon" size={19} strokeWidth={1.8} aria-hidden="true" />
        <span className="sr-only">{inAppsMode ? 'Search applications' : 'Search history'}</span>
        <input
          ref={searchInputRef}
          id="history-search"
          type="search"
          autoComplete="off"
          spellCheck={false}
          value={query}
          placeholder={
            inAppsMode
              ? 'Szukaj zainstalowanej aplikacji…'
              : 'Szukaj tekstu, aplikacji lub operatora…'
          }
          aria-controls={inAppsMode ? 'apps-results' : 'history-results'}
          aria-autocomplete="list"
          aria-label={inAppsMode ? 'Search applications' : 'Search history'}
          aria-activedescendant={
            inAppsMode
              ? appsActiveDescendant
              : selectedId === null
                ? undefined
                : `history-option-${selectedId}`
          }
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
      {/* The type filter emits `type:` tokens the history search understands;
          pointed at applications they would be noise on the screen. */}
      {inAppsMode ? null : <TypeFilter query={query} onQueryChange={onQueryChange} />}
    </header>
  );
};
