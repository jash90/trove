import { useEffect, useRef, useState } from 'react';

import type { LinkPreview } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

interface UseLinkPreviewResult {
  preview: LinkPreview | null;
}

/// What is held, and which entry it belongs to.
///
/// Kept together so a preview can never be shown against the wrong row, and so
/// a list that reshuffles under the selection does not wipe what is on screen.
interface HeldPreview {
  eventId: number;
  preview: LinkPreview | null;
}

/// Asks what a link points at.
///
/// The core answers at once with the address and fetches the page behind it, so
/// this never waits on a slow site; when the fetch lands the core says so and
/// the question is asked again.
export const useLinkPreview = (
  gateway: ClipboardGateway,
  eventId: number | null,
  enabled: boolean,
): UseLinkPreviewResult => {
  const [held, setHeld] = useState<HeldPreview | null>(null);
  const [revision, setRevision] = useState(0);
  const requestId = useRef(0);

  useEffect(
    () =>
      gateway.onLinkPreviewReady?.((readyId) => {
        if (readyId === eventId) setRevision((value) => value + 1);
      }),
    [gateway, eventId],
  );

  useEffect(() => {
    if (eventId === null || !enabled) return;
    const id = ++requestId.current;
    let active = true;
    void gateway
      .linkPreview(eventId)
      .then((preview) => {
        if (active && id === requestId.current) setHeld({ eventId, preview });
      })
      .catch(() => {
        // A link that could not be described shows as ordinary text. There is
        // nothing here the user could act on, so nothing is said about it.
        if (active && id === requestId.current) setHeld({ eventId, preview: null });
      });
    return () => {
      active = false;
      if (id === requestId.current) requestId.current += 1;
    };
  }, [gateway, eventId, enabled, revision]);

  return {
    // Shown only against the entry it was fetched for. Anything else is a
    // leftover from a row the user has already moved past.
    preview: held !== null && held.eventId === eventId ? held.preview : null,
  };
};
