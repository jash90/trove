import { Clipboard, Import, Search, Settings } from 'lucide-react';
import type { ChangeEventHandler, KeyboardEventHandler, Ref } from 'react';

import { formatCount } from '../lib/format';

interface PaletteHeaderProps {
  query: string;
  selectedId: number | null;
  resultCount: number;
  /** True when the list is full, so more entries match than are shown. */
  resultsTruncated: boolean;
  searchInputRef?: Ref<HTMLInputElement>;
  importButtonRef?: Ref<HTMLButtonElement>;
  settingsButtonRef?: Ref<HTMLButtonElement>;
  onQueryChange: (query: string) => void;
  onKeyDown: KeyboardEventHandler<HTMLInputElement>;
  onOpenImport: () => void;
  onOpenSettings: () => void;
}

export const PaletteHeader = ({
  query,
  selectedId,
  resultCount,
  resultsTruncated,
  searchInputRef,
  importButtonRef,
  settingsButtonRef,
  onQueryChange,
  onKeyDown,
  onOpenImport,
  onOpenSettings,
}: PaletteHeaderProps): React.JSX.Element => {
  const handleChange: ChangeEventHandler<HTMLInputElement> = (event) => {
    onQueryChange(event.currentTarget.value);
  };

  return (
    <header className="palette-header">
      <div className="palette-header__masthead">
        <h1 className="palette-header__title">
          <span className="palette-header__mark" aria-hidden="true">
            <Clipboard size={15} strokeWidth={1.8} />
          </span>
          Historia schowka
        </h1>
        <div className="palette-header__tools">
          <div className="capture-status" aria-label="Monitoring schowka aktywny">
            <span className="capture-status__dot" aria-hidden="true" />
            Nasłuch
          </div>
          <button
            ref={importButtonRef}
            type="button"
            className="header-action"
            aria-label="Importuj archiwum"
            onClick={onOpenImport}
          >
            <Import size={15} aria-hidden="true" />
            <span>Import</span>
          </button>
          <button
            ref={settingsButtonRef}
            type="button"
            className="header-action"
            aria-label="Otwórz ustawienia"
            onClick={onOpenSettings}
          >
            <Settings size={15} aria-hidden="true" />
            <span>Ustawienia</span>
          </button>
        </div>
      </div>

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


    </header>
  );
};
