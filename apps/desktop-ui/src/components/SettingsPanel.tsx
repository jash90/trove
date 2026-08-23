import { KeyRound, Power, ShieldBan, Timer, X } from 'lucide-react';
import { useEffect, useRef, useState, type FormEventHandler, type KeyboardEventHandler } from 'react';

import { useModalFocus } from '../hooks/useModalFocus';
import type { AppSettings, StorageStats as StorageStatsContract } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { StorageStats } from './StorageStats';

const MAX_DENYLIST_ENTRIES = 200;
const MAX_DENYLIST_ENTRY_BYTES = 256;
const MIN_RETENTION_DAYS = 1;
const MAX_RETENTION_DAYS = 3_650;

/**
 * One canonical primary modifier at most: CommandOrControl, Command and Control
 * all resolve to the same physical key on a given platform, so accepting two of
 * them would register a shortcut the user cannot press.
 */
const PRIMARY_MODIFIERS = new Map([
  ['commandorcontrol', 'CommandOrControl'],
  ['command', 'Command'],
  ['control', 'Control'],
]);
const SECONDARY_MODIFIERS = new Map([
  ['alt', 'Alt'],
  ['shift', 'Shift'],
]);
const HOTKEY_KEY_PATTERN = /^(?:[A-Z0-9]|SPACE|F(?:[1-9]|1\d|2[0-4]))$/u;
const DENYLIST_ENTRY_PATTERN = /^[a-z0-9._-]+$/u;

export const normalizePlatformHotkey = (value: string): string => {
  const invalid = (): never => {
    throw new Error('invalid_hotkey');
  };
  const parts = value.split('+').map((part) => part.trim());
  if (parts.length < 2 || parts.some((part) => part.length === 0)) invalid();

  const upper = parts.at(-1)!.toUpperCase();
  if (!HOTKEY_KEY_PATTERN.test(upper)) invalid();
  // Named keys are spelled in title case by the platform shortcut syntax;
  // single characters and function keys stay uppercase.
  const key = upper === 'SPACE' ? 'Space' : upper;

  let primary: string | null = null;
  const secondary = new Set<string>();
  for (const part of parts.slice(0, -1)) {
    const token = part.toLowerCase();
    const asPrimary = PRIMARY_MODIFIERS.get(token);
    if (asPrimary !== undefined) {
      if (primary !== null) invalid();
      primary = asPrimary;
      continue;
    }
    const asSecondary = SECONDARY_MODIFIERS.get(token);
    if (asSecondary === undefined) return invalid();
    if (secondary.has(asSecondary)) return invalid();
    secondary.add(asSecondary);
  }
  if (primary === null) invalid();

  return [
    primary,
    ...(secondary.has('Alt') ? ['Alt'] : []),
    ...(secondary.has('Shift') ? ['Shift'] : []),
    key,
  ].join('+');
};

/**
 * Bundle identifiers and executable names share one rule so the same
 * application cannot be denied twice under two spellings. ASCII lowercase only:
 * locale-aware casing would fold differently per platform.
 */
export const normalizeExecutableDenylistEntry = (value: string): string => {
  const trimmed = value.trim();
  const lowered = trimmed.replace(/[A-Z]/gu, (letter) => letter.toLowerCase());
  if (
    lowered.length === 0 ||
    new TextEncoder().encode(lowered).length > MAX_DENYLIST_ENTRY_BYTES ||
    !DENYLIST_ENTRY_PATTERN.test(lowered)
  ) {
    throw new Error('invalid_denylist_entry');
  }
  return lowered;
};

export const normalizeDenylistEntries = (entries: readonly string[]): string[] => {
  const normalized = new Set<string>();
  for (const entry of entries) {
    if (entry.trim().length === 0) continue;
    normalized.add(normalizeExecutableDenylistEntry(entry));
  }
  if (normalized.size > MAX_DENYLIST_ENTRIES) {
    throw new Error('denylist_too_large');
  }
  return [...normalized];
};

interface SettingsPanelProps {
  gateway: ClipboardGateway;
  onClose?: () => void;
}

type StorageStatus = 'loading' | 'ready' | 'unavailable';

const TRANSACTION_ERROR = 'Nie udało się zastosować ustawień. Nic nie zostało zapisane.';
const RETENTION_ERROR = 'Podaj pełną liczbę dni od 1 do 3650.';
const HOTKEY_ERROR = 'Skrót musi zawierać modyfikator i jedną literę, cyfrę lub klawisz funkcyjny.';
const DENYLIST_ERROR = 'Lista wykluczeń zawiera nieprawidłowy wpis lub jest za długa.';

export const SettingsPanel = ({ gateway, onClose }: SettingsPanelProps): React.JSX.Element => {
  const [persisted, setPersisted] = useState<AppSettings | null>(null);
  const [hotkey, setHotkey] = useState('');
  const [autostart, setAutostart] = useState(false);
  const [nativeAutostart, setNativeAutostart] = useState<boolean | null>(null);
  const [unlimitedRetention, setUnlimitedRetention] = useState(false);
  const [retentionDays, setRetentionDays] = useState('');
  const [denylist, setDenylist] = useState('');
  const [stats, setStats] = useState<StorageStatsContract | null>(null);
  const [storageStatus, setStorageStatus] = useState<StorageStatus>('loading');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [pending, setPending] = useState(false);
  const hotkeyRef = useRef<HTMLInputElement>(null);
  const modalFocus = useModalFocus({
    active: persisted !== null,
    initialFocusRef: hotkeyRef,
    focusKey: persisted === null ? 'loading' : 'loaded',
  });

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const [settings, native] = await Promise.all([
        gateway.getSettings(),
        gateway.isAutostartEnabled().catch(() => null),
      ]);
      if (cancelled) return;
      setPersisted(settings);
      setHotkey(settings.hotkey);
      setAutostart(settings.autostart);
      setNativeAutostart(native);
      setUnlimitedRetention(settings.retentionDays === null);
      setRetentionDays(settings.retentionDays === null ? '' : String(settings.retentionDays));
      setDenylist(settings.denylistedApps.join('\n'));
    })();
    return () => {
      cancelled = true;
    };
  }, [gateway]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const next = await gateway.getStorageStats();
        if (cancelled) return;
        setStats(next);
        setStorageStatus('ready');
      } catch {
        if (!cancelled) setStorageStatus('unavailable');
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [gateway]);

  // The form is noValidate on purpose: native constraint validation would
  // silently swallow the submit for an out-of-range number and the user would
  // never see why. All validation below reports one consistent message.
  const handleSubmit: FormEventHandler<HTMLFormElement> = (event) => {
    event.preventDefault();
    if (pending || persisted === null) return;
    setSaved(false);
    setError(null);

    let nextHotkey: string;
    try {
      nextHotkey = normalizePlatformHotkey(hotkey);
    } catch {
      setError(HOTKEY_ERROR);
      return;
    }

    let nextRetention: number | null = null;
    if (!unlimitedRetention) {
      const parsed = Number(retentionDays);
      if (
        retentionDays.trim().length === 0 ||
        !Number.isInteger(parsed) ||
        parsed < MIN_RETENTION_DAYS ||
        parsed > MAX_RETENTION_DAYS
      ) {
        setError(RETENTION_ERROR);
        return;
      }
      nextRetention = parsed;
    }

    let nextDenylist: string[];
    try {
      nextDenylist = normalizeDenylistEntries(denylist.split('\n'));
    } catch {
      setError(DENYLIST_ERROR);
      return;
    }

    const next: AppSettings = {
      schemaVersion: persisted.schemaVersion,
      hotkey: nextHotkey,
      autostart,
      retentionDays: nextRetention,
      denylistedApps: nextDenylist,
    };

    void (async () => {
      setPending(true);
      // Autostart lives in the OS, not in the database, so it cannot join the
      // settings transaction. Apply it first and undo it if persistence fails,
      // then re-read the real state instead of trusting the rollback.
      const autostartChanged = nativeAutostart !== null && nativeAutostart !== autostart;
      if (autostartChanged) {
        try {
          await gateway.setAutostartEnabled(autostart);
        } catch {
          setError(TRANSACTION_ERROR);
          setNativeAutostart(await gateway.isAutostartEnabled().catch(() => null));
          setPending(false);
          return;
        }
      }
      try {
        const stored = await gateway.saveSettings(next);
        setPersisted(stored);
        setSaved(true);
      } catch {
        if (autostartChanged) {
          await gateway.setAutostartEnabled(!autostart).catch(() => undefined);
        }
        setError(TRANSACTION_ERROR);
      }
      setNativeAutostart(await gateway.isAutostartEnabled().catch(() => null));
      setPending(false);
    })();
  };

  const handleKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    modalFocus.onKeyDown(event);
    if (event.key === 'Escape') {
      event.preventDefault();
      onClose?.();
    }
  };

  const autostartMismatch =
    persisted !== null && nativeAutostart !== null && persisted.autostart !== nativeAutostart;

  return (
    <div className="workflow-backdrop" data-testid="settings-backdrop">
      <div
        className="workflow-dialog workflow-dialog--wide"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-dialog-title"
        aria-describedby="settings-dialog-description"
        onKeyDown={handleKeyDown}
      >
        <header className="workflow-dialog__header">
          <div>
            <span className="workflow-kicker">Konfiguracja lokalna</span>
            <h1 id="settings-dialog-title">Ustawienia</h1>
          </div>
        </header>

        <p id="settings-dialog-description" className="workflow-warning">
          Ustawienia pozostają lokalne. Nic nie jest synchronizowane ani wysyłane poza
          to urządzenie.
        </p>

        {persisted === null ? (
          <p className="workflow-pending">Wczytywanie ustawień…</p>
        ) : (
          <form className="settings-form" noValidate onSubmit={handleSubmit}>
            {error ? (
              <p className="workflow-alert" role="alert">
                {error}
              </p>
            ) : null}
            {saved && !error ? (
              <p className="workflow-status" role="status">
                Ustawienia zapisane.
              </p>
            ) : null}
            {autostartMismatch ? (
              <p className="workflow-status" role="status">
                Stan autostartu różni się od zapisanego ustawienia.{' '}
                <span>zapisane: {persisted.autostart ? 'włączony' : 'wyłączony'}</span>
                {', '}
                <span>system: {nativeAutostart ? 'włączony' : 'wyłączony'}</span>
              </p>
            ) : null}

            <section className="settings-section" aria-labelledby="settings-hotkey-title">
              <div className="settings-section-heading">
                <span className="settings-section-icon" aria-hidden="true">
                  <KeyRound size={16} />
                </span>
                <h2 id="settings-hotkey-title">Skrót i uruchamianie</h2>
              </div>
              <label className="settings-field" htmlFor="settings-hotkey">
                <span>Skrót globalny</span>
                <input
                  ref={hotkeyRef}
                  id="settings-hotkey"
                  type="text"
                  autoComplete="off"
                  spellCheck={false}
                  value={hotkey}
                  onChange={(event) => setHotkey(event.currentTarget.value)}
                />
              </label>
              <p className="settings-help">
                Aplikację przywołuje <kbd>⌘⇧Space</kbd>; ponowne wciśnięcie ją
                chowa. Aktywny skrót systemowy nie zmieni się na tym ekranie —
                zapisana wartość zacznie obowiązywać po ponownym uruchomieniu.
              </p>
              <label className="settings-toggle" htmlFor="settings-autostart">
                <input
                  id="settings-autostart"
                  type="checkbox"
                  checked={autostart}
                  onChange={(event) => setAutostart(event.currentTarget.checked)}
                />
                <span>
                  <Power size={14} aria-hidden="true" /> Uruchamiaj przy logowaniu
                </span>
              </label>
            </section>

            <section className="settings-section" aria-labelledby="settings-retention-title">
              <div className="settings-section-heading">
                <span className="settings-section-icon" aria-hidden="true">
                  <Timer size={16} />
                </span>
                <h2 id="settings-retention-title">Retencja historii</h2>
              </div>
              <label className="settings-toggle" htmlFor="settings-retention-unlimited">
                <input
                  id="settings-retention-unlimited"
                  type="checkbox"
                  checked={unlimitedRetention}
                  onChange={(event) => setUnlimitedRetention(event.currentTarget.checked)}
                />
                <span>Bez limitu retencji</span>
              </label>
              <label className="settings-field" htmlFor="settings-retention-days">
                <span>Dni przechowywania</span>
                <input
                  id="settings-retention-days"
                  type="number"
                  min={MIN_RETENTION_DAYS}
                  max={MAX_RETENTION_DAYS}
                  step={1}
                  disabled={unlimitedRetention}
                  value={retentionDays}
                  onChange={(event) => setRetentionDays(event.currentTarget.value)}
                />
              </label>
              <p className="settings-help">
                Historia jest domyślnie nieograniczona. Włączenie retencji trwale usuwa
                wpisy starsze niż podana liczba dni.
              </p>
            </section>

            <section className="settings-section" aria-labelledby="settings-denylist-title">
              <div className="settings-section-heading">
                <span className="settings-section-icon" aria-hidden="true">
                  <ShieldBan size={16} />
                </span>
                <h2 id="settings-denylist-title">Aplikacje wykluczone</h2>
              </div>
              <label className="settings-field" htmlFor="settings-denylist">
                <span>Identyfikatory pakietów lub nazwy plików wykonywalnych</span>
                <textarea
                  id="settings-denylist"
                  rows={4}
                  spellCheck={false}
                  value={denylist}
                  onChange={(event) => setDenylist(event.currentTarget.value)}
                />
              </label>
              <p className="settings-help">
                Jeden wpis w wierszu, maksymalnie {MAX_DENYLIST_ENTRIES}. Treść skopiowana
                w tych aplikacjach nie trafia do historii.
              </p>
            </section>

            <StorageStats stats={stats} status={storageStatus} />

            <div className="workflow-actions">
              <button type="submit" className="workflow-primary" disabled={pending}>
                {pending ? 'Zapisywanie…' : 'Zapisz ustawienia'}
              </button>
            </div>
          </form>
        )}

        <button
          type="button"
          className="workflow-dismiss"
          aria-label="Zamknij ustawienia"
          onClick={onClose}
        >
          <X size={16} aria-hidden="true" />
          Zamknij
        </button>
      </div>
    </div>
  );
};
