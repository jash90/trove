import { useEffect, useRef, useState } from 'react';

import { thumbnailDataUrl } from '../lib/format';
import type { ClipboardGateway } from '../lib/gateway';
import type { ThumbnailStatus } from '../components/ImagePreview';

interface ThumbnailState {
  status: ThumbnailStatus;
  url: string | null;
}

export const useThumbnail = (
  gateway: ClipboardGateway,
  eventId: number | null,
  enabled: boolean,
): ThumbnailState => {
  const [state, setState] = useState<ThumbnailState>({ status: 'idle', url: null });
  const requestId = useRef(0);

  useEffect(() => {
    const id = ++requestId.current;
    let active = true;

    if (eventId === null || !enabled) {
      setState({ status: 'idle', url: null });
      return () => {
        active = false;
      };
    }

    setState({ status: 'loading', url: null });
    void gateway
      .getThumbnail(eventId)
      .then((thumbnail) => {
        if (!active || id !== requestId.current) return;
        if (!thumbnail) {
          setState({ status: 'unavailable', url: null });
          return;
        }
        try {
          setState({ status: 'ready', url: thumbnailDataUrl(thumbnail) });
        } catch {
          setState({ status: 'error', url: null });
        }
      })
      .catch(() => {
        if (active && id === requestId.current) {
          setState({ status: 'error', url: null });
        }
      });

    return () => {
      active = false;
      if (id === requestId.current) requestId.current += 1;
    };
  }, [enabled, eventId, gateway]);

  return state;
};
