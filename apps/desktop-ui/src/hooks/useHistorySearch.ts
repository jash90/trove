import { useEffect, useRef, useState } from 'react';

import type { ClipboardGateway } from '../lib/gateway';
import { HISTORY_PAGE_SIZE, type HistoryItem, type HistoryPage } from '../lib/contracts';

type HistorySearchStatus = 'loading' | 'ready' | 'error';

interface HistorySearchState {
  status: HistorySearchStatus;
  page: HistoryPage | null;
}

export interface UseHistorySearchResult {
  query: string;
  setQuery: (query: string) => void;
  /// What there is to show. `loading` means nothing yet — not "a newer query
  /// is on its way", which is what `refreshing` is for.
  status: HistorySearchStatus;
  /// True while a query is in flight over results that are already on screen.
  refreshing: boolean;
  items: HistoryItem[];
  page: HistoryPage | null;
}

export const useHistorySearch = (gateway: ClipboardGateway): UseHistorySearchResult => {
  const [query, setQuery] = useState('');
  const [state, setState] = useState<HistorySearchState>({
    status: 'loading',
    page: null,
  });
  const [refreshing, setRefreshing] = useState(false);
  const requestId = useRef(0);
  // Bumped when the core records something, to re-run the same query rather
  // than make the user retype to see what they just copied.
  const [revision, setRevision] = useState(0);
  // Whether any answer has come back yet. Until one has, there is nothing on
  // screen and no typing to coalesce, so the debounce would only be 150 ms of
  // empty palette at every launch.
  const answered = useRef(false);

  useEffect(
    () => gateway.onHistoryChanged?.(() => setRevision((value) => value + 1)),
    [gateway],
  );

  useEffect(() => {
    const id = ++requestId.current;
    let active = true;
    // The page on screen stays. Clearing it here — before the debounce has
    // even armed — emptied the list on every keystroke, which unmounted it
    // and took the scroll position and the selection with it. Typing should
    // narrow what is shown, not replace it with nothing twice per letter.
    setRefreshing(true);

    const run = (): void => {
      void gateway
        .search({ query, limit: HISTORY_PAGE_SIZE, cursor: null })
        .then((page) => {
          if (active && id === requestId.current) {
            answered.current = true;
            setState({ status: 'ready', page });
            setRefreshing(false);
          }
        })
        .catch(() => {
          if (active && id === requestId.current) {
            // A failure means we no longer know what matches, so the previous
            // rows must not stay on screen pretending to be the answer.
            answered.current = true;
            setState({ status: 'error', page: null });
            setRefreshing(false);
          }
        });
    };

    // The first, empty query goes out at once: the debounce exists to fold a
    // burst of keystrokes into one search, and a palette that has never shown
    // anything has had no keystrokes to fold.
    if (!answered.current && query === '') {
      run();
      return () => {
        active = false;
        if (id === requestId.current) {
          requestId.current += 1;
        }
      };
    }

    const timer = window.setTimeout(run, 150);

    return () => {
      active = false;
      window.clearTimeout(timer);
      if (id === requestId.current) {
        requestId.current += 1;
      }
    };
  }, [gateway, query, revision]);

  return {
    query,
    setQuery,
    status: state.status,
    refreshing,
    items: state.page?.items ?? [],
    page: state.page,
  };
};
