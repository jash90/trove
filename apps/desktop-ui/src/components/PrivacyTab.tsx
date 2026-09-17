import { useEffect, useRef } from 'react';

import type { TypeSafeScanProgress } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

interface PrivacyTabProps {
  gateway: ClipboardGateway;
  apiKey: string;
  onApiKeyChange: (key: string) => void;
  saved: boolean;
  onSaveKey: () => void;
  scan: TypeSafeScanProgress | null;
  onScanChange: (scan: TypeSafeScanProgress | null) => void;
}

const SCAN_ERROR_SENTENCES: Record<string, string> = {
  scan_key_missing: 'Add the TypeSafe API key first — the scan runs with yours.',
  scan_key_refused: 'The provider refused this key. Check it in the TypeSafe console.',
  scan_network_error: 'TypeSafe could not be reached. Try again in a moment.',
  scan_rate_limited: 'TypeSafe is rate-limiting. Wait a moment and scan again.',
  scan_provider_refused: 'TypeSafe refused the request. Try again in a moment.',
  scan_history_unavailable: 'The history could not be read for scanning.',
};

/// The privacy scan tab: one deliberate look through the local history,
/// with the owner's key, for entries that should not be sitting in it.
///
/// Nothing here ever runs on its own. The scan starts when this button is
/// pressed, sends the text of each entry and nothing else, and its flags
/// live in this scan alone — nothing is written to the database, and a
/// restarted application remembers none of it.
export const PrivacyTab = ({
  gateway,
  apiKey,
  onApiKeyChange,
  saved,
  onSaveKey,
  scan,
  onScanChange,
}: PrivacyTabProps): React.JSX.Element => {
  const polling = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (polling.current !== null) window.clearInterval(polling.current);
    };
  }, []);

  const startScan = (): void => {
    void gateway
      .typesafeScanStart?.()
      .then(() => {
        if (polling.current !== null) window.clearInterval(polling.current);
        polling.current = window.setInterval(() => {
          void gateway
            .typesafeScanStatus?.()
            .then((progress) => {
              onScanChange(progress);
              if (progress.state !== 'running' && polling.current !== null) {
                window.clearInterval(polling.current);
                polling.current = null;
              }
            })
            .catch(() => undefined);
        }, 500);
      })
      .catch(() => undefined);
  };

  const stopScan = (): void => {
    void gateway.typesafeScanStop?.().catch(() => undefined);
  };

  const running = scan?.state === 'running';
  const progress =
    scan !== null && scan.total > 0 ? Math.round((scan.processed / scan.total) * 100) : null;
  const settled = scan !== null && scan.state !== 'running';

  return (
    <section className="settings-section" aria-labelledby="settings-privacy-title">
      <div className="settings-section-heading">
        <span className="settings-section-icon" aria-hidden="true">
          <ScanSearchForIcon />
        </span>
        <h2 id="settings-privacy-title">Privacy scan</h2>
      </div>

      <label className="settings-field" htmlFor="settings-typesafe-key">
        <span>TypeSafe API key</span>
        <input
          id="settings-typesafe-key"
          type="password"
          value={apiKey}
          spellCheck={false}
          autoComplete="off"
          placeholder="apikey_…"
          onChange={(event) => onApiKeyChange(event.currentTarget.value)}
        />
      </label>
      <div className="privacy-scan__actions">
        <button
          type="button"
          className="privacy-scan__run"
          disabled={apiKey.trim() === '' || running}
          onClick={onSaveKey}
        >
          {saved ? 'Saved' : 'Save key'}
        </button>
        <button
          type="button"
          className="privacy-scan__run"
          disabled={apiKey.trim() === '' || running}
          onClick={startScan}
        >
          Scan history
        </button>
        {running ? (
          <button type="button" className="privacy-scan__stop" onClick={stopScan}>
            Stop
          </button>
        ) : null}
      </div>
      <p className="settings-help">
        The scan sends the text of each clipboard entry — nothing else, no application
        names, no timestamps — to TypeSafe's judgment API, with your key, when you press
        the button. Never on its own. Entries the model is sure about are listed below
        for you to delete; its unsureness flags nothing.
      </p>

      {running && progress !== null ? (
        <>
          <div
            className="privacy-scan__bar"
            role="progressbar"
            aria-valuenow={progress}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div className="privacy-scan__fill" style={{ width: `${progress}%` }} />
          </div>
          <p className="settings-help">
            {scan?.processed} / {scan?.total} entries
          </p>
        </>
      ) : null}

      {scan?.state === 'failed' ? (
        <p className="apps-launch-error" role="alert">
          {SCAN_ERROR_SENTENCES[scan.errorCode ?? ''] ??
            'The scan could not finish. Try again in a moment.'}
        </p>
      ) : null}

      {settled && scan.state === 'completed' ? (
        <p className="settings-help">
          {scan.total === 0
            ? 'The history holds nothing to scan.'
            : scan.flagged.length === 0
              ? `${scan.processed} entries scanned; nothing was sure enough to flag.`
              : `${scan.processed} entries scanned, ${scan.flagged.length} worth a look:`}
        </p>
      ) : null}

      {scan !== null && scan.flagged.length > 0 ? (
        <ul className="privacy-scan__flags">
          {scan.flagged.map((flag) => (
            <li key={flag.eventId} className="privacy-scan__flag">
              <span className="privacy-scan__flag-preview">{flag.preview}</span>
              <span className="privacy-scan__flag-probability">
                {flag.probability.toFixed(2)}
              </span>
              <button
                type="button"
                className="privacy-scan__flag-delete"
                onClick={() => {
                  void gateway.deleteEvent(flag.eventId).catch(() => undefined);
                }}
              >
                Delete
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
};

/// Local stand-in for the lucide icon, so this file owns its imports.
const ScanSearchForIcon = (): React.JSX.Element => (
  <svg
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <circle cx="11" cy="11" r="8" />
    <path d="m21 21-4.3-4.3" />
  </svg>
);
