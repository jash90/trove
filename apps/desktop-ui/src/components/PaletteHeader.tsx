import { Search } from 'lucide-react';
import type { ChangeEventHandler, KeyboardEventHandler, Ref } from 'react';

import { formatCount } from '../lib/format';
import { TypeFilter } from './TypeFilter';

interface PaletteHeaderProps {
  query: string;
  selectedId: number | null;
  resultCount: number;
  /** True when the list is full, so more entries match than are shown. */
  resultsTruncated: boolean;
  searchInputRef?: Ref<HTMLInputElement>;
  onQueryChange: (query: string) => void;
  onKeyDown: KeyboardEventHandler<HTMLInputElement>;
}

/// The whole top of the palette: one row.
///
/// It used to carry a title, a logo, a static "listening" badge and two
/// buttons above the search field. None of it was ever read twice, and all of
/// it pushed the results down. What a person summons a clipboard palette for
/// is the field and the list, so that is what the top is now.
export const PaletteHeader = ({
  query,
  selectedId,
  resultCount,
  resultsTruncated,
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
        <span className="sr-only">Przeszukaj historię</span>
        <input
          ref={searchInputRef}
          id="history-search"
          type="search"
          autoComplete="off"
          spellCheck={false}
          value={query}
          placeholder="Szukaj tekstu, aplikacji lub operatora…"
          aria-controls="history-results"
          aria-autocomplete="list"
          aria-label="Przeszukaj historię"
          aria-activedescendant={
            selectedId === null ? undefined : `history-option-${selectedId}`
          }
          onChange={handleChange}
          onKeyDown={onKeyDown}
        />
        {/* The list is a page, not the whole history: saying "80 wyników"
            when thousands match reads as a total and is simply untrue. */}
        <span className="search-field__count" aria-live="polite">
          {formatCount(resultCount)}
          {resultsTruncated ? '+' : ''}
        </span>
      </label>
      <TypeFilter query={query} onQueryChange={onQueryChange} />
    </header>
  );
};
