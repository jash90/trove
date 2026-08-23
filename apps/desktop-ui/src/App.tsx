import type { KeyboardEventHandler } from 'react';

import { EmptyState } from './components/EmptyState';
import { FilterRail } from './components/FilterRail';
import { HistoryList } from './components/HistoryList';
import { PaletteHeader } from './components/PaletteHeader';
import { useHistorySearch } from './hooks/useHistorySearch';
import { useKeyboardNavigation } from './hooks/useKeyboardNavigation';
import {
  GatewayProvider,
  useGateway,
  type ClipboardGateway,
} from './lib/gateway';

interface AppProps {
  gateway?: ClipboardGateway;
}

const ClipboardPalette = (): React.JSX.Element => {
  const gateway = useGateway();
  const { query, setQuery, status, items } = useHistorySearch(gateway);
  const handleActivate = (eventId: number): void => {
    void gateway.copyEvent(eventId, false).catch(() => undefined);
  };
  const navigation = useKeyboardNavigation({
    items,
    onActivate: handleActivate,
    onEscape: () => setQuery(''),
  });
  const handleKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    navigation.handleKeyDown(event);
  };
  const handleQueryChange = (nextQuery: string): void => {
    setQuery(nextQuery);
  };

  return (
    <main className="palette-stage" role="application" aria-label="Historia schowka">
      <section className="palette-shell" aria-label="Paleta historii schowka">
        <PaletteHeader
          query={query}
          selectedId={navigation.selectedId}
          resultCount={items.length}
          onQueryChange={handleQueryChange}
          onKeyDown={handleKeyDown}
        />
        <FilterRail query={query} onQueryChange={handleQueryChange} />
        <div className="history-panel">
          {status === 'loading' ? <EmptyState kind="loading" /> : null}
          {status === 'error' ? <EmptyState kind="error" /> : null}
          {status === 'ready' && items.length === 0 ? <EmptyState kind="empty" /> : null}
          {status === 'ready' && items.length > 0 ? (
            <HistoryList
              items={items}
              selectedId={navigation.selectedId}
              onSelect={navigation.setSelectedId}
              onActivate={handleActivate}
            />
          ) : null}
        </div>
        <footer className="palette-footer">
          <span>Dwuklik wkleja zaznaczony wpis</span>
          <span className="palette-footer__privacy">Tylko na tym urządzeniu</span>
        </footer>
      </section>
    </main>
  );
};

export const App = ({ gateway }: AppProps): React.JSX.Element => (
  <GatewayProvider gateway={gateway}>
    <ClipboardPalette />
  </GatewayProvider>
);
