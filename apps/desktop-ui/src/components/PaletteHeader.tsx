import { Clipboard, Search } from 'lucide-react';
import type { ChangeEventHandler, KeyboardEventHandler, Ref } from 'react';

interface PaletteHeaderProps {
  query: string;
  selectedId: number | null;
  resultCount: number;
  searchInputRef?: Ref<HTMLInputElement>;
  onQueryChange: (query: string) => void;
  onKeyDown: KeyboardEventHandler<HTMLInputElement>;
}

export const PaletteHeader = ({
  query,
  selectedId,
  resultCount,
  searchInputRef,
  onQueryChange,
  onKeyDown,
}: PaletteHeaderProps): React.JSX.Element => {
  const handleChange: ChangeEventHandler<HTMLInputElement> = (event) => {
    onQueryChange(event.currentTarget.value);
  };

  return (
    <header className="palette-header">
      <div className="palette-header__masthead">
        <div className="palette-header__identity">
          <span className="palette-header__mark" aria-hidden="true">
            <Clipboard size={16} strokeWidth={1.8} />
          </span>
          <div>
            <p className="palette-eyebrow">Lokalne archiwum</p>
            <h1>Historia schowka</h1>
          </div>
        </div>
        <div className="capture-status" aria-label="Monitoring schowka aktywny">
          <span className="capture-status__dot" aria-hidden="true" />
          Nasłuch aktywny
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
        <kbd aria-label="Wyczyść wyszukiwanie klawiszem Escape">esc</kbd>
      </label>

      <div className="palette-header__ledger" aria-live="polite">
        <span>{resultCount.toLocaleString('pl-PL')} wyników</span>
        <span aria-hidden="true">↑↓ wybierz</span>
        <span aria-hidden="true">↵ wklej</span>
      </div>
    </header>
  );
};
