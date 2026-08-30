import { Download, Globe, KeyRound, Power, ShieldBan, Timer, Vault, X } from 'lucide-react';
import { useEffect, useRef, useState, type FormEventHandler, type KeyboardEventHandler } from 'react';

import type {
  AppSettings,
  ExportSummary as ExportSummaryContract,
  KeyvaultSecret,
  StorageStats as StorageStatsContract,
} from '../lib/contracts';
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

const TRANSACTION_ERROR = 'The settings could not be applied. Nothing was saved.';
const EXPORT_ERROR =
  'The export could not be written. Check that the directory is empty and writable.';
const RETENTION_ERROR = 'Give a whole number of days from 1 to 3650.';
const HOTKEY_ERROR = 'A shortcut needs a modifier and one letter, digit or function key.';
const DENYLIST_ERROR = 'The exclusion list holds an invalid entry, or is too long.';
const KEYVAULT_SAVE_FIRST =
  'Save the vault address, token and private key first — the pane reads what is saved.';

/**
 * The code out of a rejected vault call. The core rejects with the bare code
 * string — not an Error — so both shapes are read; anything else is unknown.
 */
export const vaultErrorCode = (error: unknown): string =>
  typeof error === 'string' ? error : error instanceof Error ? error.message : '';

/** One plain sentence per vault denial. Codes only reach here; never values. */
export const keyvaultErrorMessage = (code: string): string => {
  switch (code) {
    case 'keyvault_device_identity_invalid':
      return 'The device vault identity at ~/.config/keyvault/agent.json could not be read.';
    case 'keyvault_not_configured':
      return KEYVAULT_SAVE_FIRST;
    case 'keyvault_invalid_config':
    case 'keyvault_invalid_url':
    case 'keyvault_invalid_token':
    case 'keyvault_invalid_private_key':
      return 'The saved vault configuration is incomplete or malformed — check all three fields.';
    case 'keyvault_unauthorized':
      return 'The token was refused — create a new one in the vault.';
    case 'keyvault_agent_access_disabled':
      return 'The vault will not hand this secret to agents — enable agent access for it there.';
    case 'keyvault_not_found':
      return 'No such secret inside this token’s scope.';
    case 'keyvault_rate_limited':
      return 'The vault allows one read a second — try again in a moment.';
    case 'keyvault_decrypt_failed':
      return 'The private key does not match the one the vault seals to.';
    case 'keyvault_transport_failed':
      return 'The vault could not be reached. Check the address and the connection.';
    default:
      return 'The vault answered with something this pane could not read.';
  }
};

export const SettingsPanel = ({ gateway, onClose }: SettingsPanelProps): React.JSX.Element => {
  const [persisted, setPersisted] = useState<AppSettings | null>(null);
  const [hotkey, setHotkey] = useState('');
  const [autostart, setAutostart] = useState(false);
  const [nativeAutostart, setNativeAutostart] = useState<boolean | null>(null);
  const [unlimitedRetention, setUnlimitedRetention] = useState(false);
  const [retentionDays, setRetentionDays] = useState('');
  const [denylist, setDenylist] = useState('');
  const [stats, setStats] = useState<StorageStatsContract | null>(null);
  const [linkPreviews, setLinkPreviews] = useState(true);
  const [vaultUrl, setVaultUrl] = useState('');
  const [vaultToken, setVaultToken] = useState('');
  const [vaultSecrets, setVaultSecrets] = useState<KeyvaultSecret[] | null>(null);
  const [vaultBusy, setVaultBusy] = useState(false);
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [vaultCopiedSlug, setVaultCopiedSlug] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportSummary, setExportSummary] = useState<ExportSummaryContract | null>(null);
  const [storageStatus, setStorageStatus] = useState<StorageStatus>('loading');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [pending, setPending] = useState(false);
  const hotkeyRef = useRef<HTMLInputElement>(null);
  // Its own window now, so the first field takes focus when the settings
  // arrive — no trap to build, because there is nothing behind it to escape to.
  useEffect(() => {
    if (persisted !== null) hotkeyRef.current?.focus();
  }, [persisted]);

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
      setLinkPreviews(settings.linkPreviews);
      setVaultUrl(settings.keyvault.url ?? '');
      setVaultToken(settings.keyvault.token ?? '');
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
      linkPreviews,
      // Overrides over the device identity file, each independent of the
      // other. A blank field travels as an absent one, meaning "use the
      // device's value". The private key is never sent: it is not ours to hold.
      keyvault: {
        url: vaultUrl.trim() || null,
        token: vaultToken.trim() || null,
      },
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

  /// Asks the vault what this token may read. Metadata only: what comes back
  /// is slugs and names, and a refusal arrives as a code this pane translates.
  const testVaultConnection = async (): Promise<void> => {
    if (vaultBusy) return;
    setVaultBusy(true);
    setVaultError(null);
    setVaultCopiedSlug(null);
    try {
      setVaultSecrets(await gateway.keyvaultList());
    } catch (error) {
      setVaultSecrets(null);
      setVaultError(keyvaultErrorMessage(vaultErrorCode(error)));
    }
    setVaultBusy(false);
  };

  /// Puts one secret on the clipboard. The value never enters this window:
  /// the result says it worked, and that is all there is to show.
  const copyVaultSecret = async (slug: string): Promise<void> => {
    if (vaultBusy) return;
    setVaultBusy(true);
    setVaultError(null);
    setVaultCopiedSlug(null);
    try {
      await gateway.keyvaultCopySecret(slug);
      setVaultCopiedSlug(slug);
    } catch (error) {
      setVaultError(keyvaultErrorMessage(vaultErrorCode(error)));
    }
    setVaultBusy(false);
  };

  /// Writes the whole history somewhere the user picks.
  ///
  /// The chosen path never enters state: it goes straight to the command and
  /// dies with this call, the same rule the import wizard follows.
  const runExport = async (): Promise<void> => {
    setError(null);
    setExportSummary(null);
    const directory = await gateway.chooseExportDirectory().catch(() => null);
    if (directory === null) return;
    setExporting(true);
    try {
      setExportSummary(await gateway.exportHistory(directory));
    } catch {
      // The failure may carry a path, so only a fixed sentence is shown.
      setError(EXPORT_ERROR);
    }
    setExporting(false);
  };

  const handleKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      onClose?.();
    }
  };

  const autostartMismatch =
    persisted !== null && nativeAutostart !== null && persisted.autostart !== nativeAutostart;

  return (
    <div className="settings-page">
      <main
        className="settings-page__sheet"
        aria-labelledby="settings-dialog-title"
        aria-describedby="settings-dialog-description"
        onKeyDown={handleKeyDown}
      >
        <header className="workflow-dialog__header">
          <div>
            <span className="workflow-kicker">Konfiguracja lokalna</span>
            <h1 id="settings-dialog-title">Settings</h1>
          </div>
        </header>

        <p id="settings-dialog-description" className="workflow-warning">
          Settings stay local. Nothing is synchronised or sent beyond
          this device.
        </p>

        {persisted === null ? (
          <p className="workflow-pending">Loading settings…</p>
        ) : (
          <form className="settings-form" noValidate onSubmit={handleSubmit}>
            {error ? (
              <p className="workflow-alert" role="alert">
                {error}
              </p>
            ) : null}
            {saved && !error ? (
              <p className="workflow-status" role="status">
                Settings saved.
              </p>
            ) : null}
            {autostartMismatch ? (
              <p className="workflow-status" role="status">
                The autostart state differs from the saved setting.{' '}
                <span>saved: {persisted.autostart ? 'on' : 'off'}</span>
                {', '}
                <span>system: {nativeAutostart ? 'on' : 'off'}</span>
              </p>
            ) : null}

            <section className="settings-section" aria-labelledby="settings-hotkey-title">
              <div className="settings-section-heading">
                <span className="settings-section-icon" aria-hidden="true">
                  <KeyRound size={16} />
                </span>
                <h2 id="settings-hotkey-title">Shortcut and startup</h2>
              </div>
              <label className="settings-field" htmlFor="settings-hotkey">
                <span>Global shortcut</span>
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
                The application is summoned by <kbd>⌘⇧Space</kbd>; pressing it again
                hides it. Saving changes the active shortcut immediately. If the new one
                is already taken by another application, the previous one stays in force.
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
                <h2 id="settings-retention-title">History retention</h2>
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
                History is unlimited by default. Turning retention on permanently deletes
                entries older than the given number of days.
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
                <span>Bundle identifiers or executable names</span>
                <textarea
                  id="settings-denylist"
                  rows={4}
                  spellCheck={false}
                  value={denylist}
                  onChange={(event) => setDenylist(event.currentTarget.value)}
                />
              </label>
              <p className="settings-help">
                One entry per line, at most {MAX_DENYLIST_ENTRIES}. Content copied
                in these applications never reaches the history.
              </p>
            </section>

            <StorageStats stats={stats} status={storageStatus} />

            <section className="workflow-section" aria-labelledby="settings-links-title">
              <h2 id="settings-links-title">
                <Globe size={15} aria-hidden="true" />
                Preview stron
              </h2>
              <label className="settings-toggle">
                <input
                  type="checkbox"
                  checked={linkPreviews}
                  onChange={(event) => setLinkPreviews(event.currentTarget.checked)}
                />
                Fetch the page title and icon
              </label>
              <p className="settings-help">
                This is the only place the application talks to the network. On, it
                means opening the palette queries the pages visible in the list —
                each of them then learns that you are looking at your clipboard. The result
                is remembered, so the same page is asked once. Local and private
                addresses are never queried.
              </p>
            </section>

            <section className="workflow-section" aria-labelledby="settings-keyvault-title">
              <h2 id="settings-keyvault-title">
                <Vault size={15} aria-hidden="true" />
                Keyvault
              </h2>
              <label className="settings-field" htmlFor="settings-keyvault-url">
                <span>Vault address</span>
                <input
                  id="settings-keyvault-url"
                  type="url"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="https://your-vault.convex.site"
                  value={vaultUrl}
                  onChange={(event) => setVaultUrl(event.currentTarget.value)}
                />
              </label>
              <label className="settings-field" htmlFor="settings-keyvault-token">
                <span>Agent token</span>
                <input
                  id="settings-keyvault-token"
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  value={vaultToken}
                  onChange={(event) => setVaultToken(event.currentTarget.value)}
                />
              </label>
              <p className="settings-help">
                Both fields are optional: leave them blank and this application uses the
                device’s shared vault identity at <code>~/.config/keyvault/agent.json</code>,
                which is also where the private key lives — it is never stored here. Fill one in
                only to point this install at a different vault. The vault answers with sealed
                envelopes; a copied key goes to the clipboard without ever being recorded in the
                history or shown here.
              </p>
              <div className="workflow-actions workflow-actions--start">
                <button
                  type="button"
                  onClick={() => void testVaultConnection()}
                  disabled={vaultBusy || pending}
                >
                  {vaultBusy ? 'Talking to the vault…' : 'Test connection'}
                </button>
              </div>
              {vaultError ? (
                <p className="workflow-alert" role="alert">
                  {vaultError}
                </p>
              ) : null}
              {vaultCopiedSlug !== null ? (
                <p className="workflow-status" role="status">
                  {vaultCopiedSlug} is on the clipboard. Paste it where it is needed.
                </p>
              ) : null}
              {vaultSecrets !== null ? (
                vaultSecrets.length === 0 ? (
                  <p className="workflow-status" role="status">
                    The token can read no secrets.
                  </p>
                ) : (
                  <ul className="settings-keyvault-list" aria-label="Vault secrets">
                    {vaultSecrets.map((secret) => (
                      <li key={secret.slug}>
                        <span>{secret.name}</span>
                        <span className="settings-keyvault-slug">{secret.slug}</span>
                        <button
                          type="button"
                          aria-label={`Copy ${secret.slug}`}
                          disabled={vaultBusy || pending}
                          onClick={() => void copyVaultSecret(secret.slug)}
                        >
                          Copy
                        </button>
                      </li>
                    ))}
                  </ul>
                )
              ) : null}
            </section>

            <section className="workflow-section" aria-labelledby="settings-export-title">
              <h2 id="settings-export-title">
                <Download size={15} aria-hidden="true" />
                History export
              </h2>
              <p className="settings-help">
                Writes the whole history in SuperCmd format — {'clipboard.json'},
                {' clipboard.csv'} i katalog {'images'} z obrazami. Ten sam format
                importer czyta z powrotem.
              </p>
              <div className="workflow-actions workflow-actions--start">
                <button type="button" onClick={() => void runExport()} disabled={exporting}>
                  {exporting ? 'Exporting…' : 'Export history'}
                </button>
              </div>
              {exportSummary ? (
                <p className="workflow-status" role="status">
                  Wrote {exportSummary.records} entries, of which {exportSummary.images}{' '}
                  carry an image. {exportSummary.withoutPayload} bez zapisanej contents —
                  were exported as metadata only.
                </p>
              ) : null}
            </section>

            <div className="workflow-actions">
              <button type="submit" className="workflow-primary" disabled={pending}>
                {pending ? 'Zapisywanie…' : 'Save settings'}
              </button>
            </div>
          </form>
        )}

        <button
          type="button"
          className="workflow-dismiss"
          aria-label="Close settings"
          onClick={onClose}
        >
          <X size={16} aria-hidden="true" />
          Zamknij
        </button>
      </main>
    </div>
  );
};
