import { useEffect, useRef, useState } from 'react';

import type { Preview } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import type { PreviewStatus } from '../components/PreviewPane';

interface SelectedPreviewState {
  status: PreviewStatus;
  preview: Preview | null;
}

export const useSelectedPreview = (
  gateway: ClipboardGateway,
  eventId: number | null,
): SelectedPreviewState => {
  const [state, setState] = useState<SelectedPreviewState>({
    status: 'idle',
    preview: null,
  });
  const requestId = useRef(0);

  useEffect(() => {
    const id = ++requestId.current;
    let active = true;

    if (eventId === null) {
      setState({ status: 'idle', preview: null });
      return () => {
        active = false;
      };
    }

    setState({ status: 'loading', preview: null });
    void gateway
      .preview(eventId)
      .then((preview) => {
        if (active && id === requestId.current && preview.eventId === eventId) {
          setState({ status: 'ready', preview });
        }
      })
      .catch(() => {
        if (active && id === requestId.current) {
          setState({ status: 'error', preview: null });
        }
      });

    return () => {
      active = false;
      if (id === requestId.current) requestId.current += 1;
    };
  }, [eventId, gateway]);

  return state;
};
