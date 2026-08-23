import { useEffect, useRef, useState } from 'react';

import type { ClipboardGateway } from '../lib/gateway';
import type { HistoryItem, HistoryPage } from '../lib/contracts';

type HistorySearchStatus = 'loading' | 'ready' | 'error';

interface HistorySearchState {
  status: HistorySearchStatus;
  page: HistoryPage | null;
}

export interface UseHistorySearchResult {
  query: string;
  setQuery: (query: string) => void;
  status: HistorySearchStatus;
  items: HistoryItem[];
  page: HistoryPage | null;
}

export const useHistorySearch = (gateway: ClipboardGateway): UseHistorySearchResult => {
  const [query, setQuery] = useState('');
  const [state, setState] = useState<HistorySearchState>({
    status: 'loading',
    page: null,
  });
  const requestId = useRef(0);

  useEffect(() => {
    const id = ++requestId.current;
    let active = true;
    setState({ status: 'loading', page: null });

    const timer = window.setTimeout(() => {
      void gateway
        .search({ query, limit: 80, cursor: null })
        .then((page) => {
          if (active && id === requestId.current) {
            setState({ status: 'ready', page });
          }
        })
        .catch(() => {
          if (active && id === requestId.current) {
            setState({ status: 'error', page: null });
          }
        });
    }, 150);

    return () => {
      active = false;
      window.clearTimeout(timer);
      if (id === requestId.current) {
        requestId.current += 1;
      }
    };
  }, [gateway, query]);

  return {
    query,
    setQuery,
    status: state.status,
    items: state.page?.items ?? [],
    page: state.page,
  };
};
