import {
  Download,
  Globe,
  KeyRound,
  LayoutGrid,
  Power,
  ScanSearch,
  ShieldBan,
  Timer,
  Vault,
  X,
} from 'lucide-react';
import { useEffect, useRef, useState, type FormEventHandler, type KeyboardEventHandler } from 'react';

import type {
  AppSettings,
  KeyvaultIdentity,
  PairingStarted,
  ExportSummary as ExportSummaryContract,
  KeyvaultSecret,
  ShortcutRelease,
  ShortcutStatus,
  StorageStats as StorageStatsContract,
  TypeSafeScanProgress,
} from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { PrivacyTab } from './PrivacyTab';
import { SettingsTabs, type SettingsTab } from './SettingsTabs';
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
  // Alt on its own is enough — ⌥Space is an ordinary launcher shortcut, and the systems people
  // compare this against bind exactly that. Shift on its own is not: Shift+A is how a capital A
  // is typed, so a global binding on it would swallow ordinary typing everywhere.
  if (primary === null && !secondary.has('Alt')) invalid();

  return [
    ...(primary === null ? [] : [primary]),
    ...(secondary.has('Alt') ? ['Alt'] : []),
    ...(secondary.has('Shift') ? ['Shift'] : []),
    key,
  ].join('+');
};

/**
 * The shortcut a key press describes, or nothing when it does not describe one yet.
 *
 * Reads `code` rather than `key`: with Alt held, macOS reports composed characters in `key`, so
 * ⌥K arrives as `˚` and the shortcut would record a character nobody can type on purpose.
 *
 * Returns null while only modifiers are down — a combination is not finished until a real key
 * joins it — and when the only modifier held is Shift, because Shift+A is how a capital A is
 * typed and a global binding on it would swallow ordinary typing everywhere else. ⌘, ⌃ and ⌥
 * each stand on their own; ⌥Space is an ordinary launcher shortcut.
 *
 * The candidate goes through {@link normalizePlatformHotkey} rather than being assembled into
 * final form here, so there is one place that decides what a valid shortcut is.
 */
export const acceleratorFromKeyEvent = (event: {
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}): string | null => {
  const key = (() => {
    if (/^Key[A-Z]$/u.test(event.code)) return event.code.slice(3);
    if (/^Digit[0-9]$/u.test(event.code)) return event.code.slice(5);
    if (event.code === 'Space') return 'SPACE';
    if (/^F(?:[1-9]|1\d|2[0-4])$/u.test(event.code)) return event.code;
    return null;
  })();
  if (key === null) return null;

  // Both would be two primaries, which cannot be one physical key.
  if (event.metaKey && event.ctrlKey) return null;
  const primary = event.metaKey ? 'CommandOrControl' : event.ctrlKey ? 'Control' : null;
  if (primary === null && !event.altKey) return null;

  const parts = primary === null ? [] : [primary];
  if (event.altKey) parts.push('Alt');
  if (event.shiftKey) parts.push('Shift');
  parts.push(key);
  try {
    return normalizePlatformHotkey(parts.join('+'));
  } catch {
    return null;
  }
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
/** What to say when a pairing stops for a reason that is not success. */
/** How long a pairing code is still good for, in words rather than a timestamp. */
/// The settings, one tab each. Ordered by how often a setting is reached for, not by when it was
/// written: the shortcut is the thing people come here to change, and storage sits before export
/// because one is a state and the other an action.
///
/// Labels are short because a tab label is not a heading — seven of them share one row, and the
/// full names are in the section headings where there is room for them.
const SETTINGS_TABS: readonly SettingsTab[] = [
  { id: 'shortcut', label: 'Shortcut' },
  { id: 'retention', label: 'Retention' },
  { id: 'apps', label: 'Apps' },
  { id: 'links', label: 'Links' },
  { id: 'keyvault', label: 'Keyvault' },
  { id: 'privacy', label: 'Privacy' },
  { id: 'storage', label: 'Storage' },
  { id: 'export', label: 'Export' },
];

export const expiryLabel = (expiresAt: number | null, now: number): string | null => {
  if (expiresAt === null) return null;
  const minutes = Math.floor((expiresAt - now) / 60_000);
  if (minutes <= 0) return 'This code has expired. Press Connect again.';
  if (minutes === 1) return 'This code works for about a minute more.';
  return `This code works for about ${minutes} more minutes.`;
};

export const pairingMessage = (status: string): string => {
  switch (status) {
    case 'paired':
      return 'Connected. This device has its own key and its own token now.';
    case 'expired':
      return 'That connection request expired. Start again.';
    case 'alreadyClaimed':
      return 'That connection was already collected — by something other than this application. Start again, and approve only the fingerprint shown here.';
    case 'notFound':
      return 'That connection request no longer exists. Start again.';
    default:
      return 'Connecting stopped unexpectedly.';
  }
};

export const keyvaultErrorMessage = (code: string): string => {
  switch (code) {
    case 'keyvault_vault_api_not_advertised':
      return 'That address answers, but nothing there says where its vault is. Check the address, or update the vault so its page advertises one.';
    case 'keyvault_no_vault_address':
      return 'Nothing here names a vault yet. Type its address above, then connect.';
    case 'keyvault_pairing_page_unknown':
      return 'This vault has not published where its web interface lives, so there is nowhere to send you to approve.';
    case 'keyvault_browser_failed':
      return 'Could not open a browser to finish connecting.';
    case 'keyvault_pairing_failed':
      return 'Could not start connecting to that vault.';
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
    case 'keyvault_pairing_payload_invalid':
      return 'Connecting got as far as the vault answering, but what it sent back was not a complete identity. The vault deployment is misconfigured.';
    case 'keyvault_device_identity_missing':
      return 'This device has no vault identity yet. Use Connect to pair it.';
    case 'keyvault_bad_response':
      return 'The vault replied in a shape this version does not understand.';
    case 'keyvault_invalid_slug':
      return 'That secret name is not one the vault can hold.';
    case 'keyvault_envelope_invalid':
    case 'keyvault_envelope_unsupported_version':
    case 'keyvault_envelope_too_large':
      return 'The sealed answer from the vault was not one this version can open.';
    case 'keyvault_transport_failed':
      return 'The vault could not be reached. Check the address and the connection.';
    default:
      return 'The vault answered with something this pane could not read.';
  }
};

/**
 * What to tell the user about a change to the system's shortcut table.
 *
 * `needsLogout` is the case worth spelling out rather than folding into
 * success: the preference is written and will hold, but the running session has
 * not picked it up, so the shortcut does nothing until the user logs back in.
 * Reporting that as done would send them off to press a key that still opens
 * Spotlight.
 */
export const shortcutReleaseNotice = (outcome: ShortcutRelease): string | null => {
  switch (outcome) {
    case 'applied':
      return null;
    case 'alreadyFree':
      return 'Nothing in the system was holding that shortcut.';
    case 'needsLogout':
      return 'Saved. It takes effect after you log out and back in.';
    default:
      return 'Trove could not change the system shortcut. Open Keyboard Shortcuts, turn the conflicting one off, then try again.';
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
  const [paletteModes, setPaletteModes] = useState(true);
  const [dockIcon, setDockIcon] = useState(false);
  const [vaultUrl, setVaultUrl] = useState('');
  const [privacyKey, setPrivacyKey] = useState('');
  const [privacySaved, setPrivacySaved] = useState(false);
  const [scan, setScan] = useState<TypeSafeScanProgress | null>(null);
  const [vaultSecrets, setVaultSecrets] = useState<KeyvaultSecret[] | null>(null);
  const [pairing, setPairing] = useState<PairingStarted | null>(null);
  // Ticked by the poll below, so the time left is honest rather than frozen at whatever it was
  // when the pairing started.
  const [now, setNow] = useState(() => Date.now());
  const [copied, setCopied] = useState(false);
  const [activeTab, setActiveTab] = useState<string>(SETTINGS_TABS[0]!.id);
  const [pairNotice, setPairNotice] = useState<string | null>(null);
  // What the device actually knows, as opposed to what the settings row remembers. The row has
  // been wrong before, and showing a value nobody is using is how the last confusion started.
  const [identity, setIdentity] = useState<KeyvaultIdentity | null>(null);
  const [vaultBusy, setVaultBusy] = useState(false);
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [vaultCopiedSlug, setVaultCopiedSlug] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportSummary, setExportSummary] = useState<ExportSummaryContract | null>(null);
  const [storageStatus, setStorageStatus] = useState<StorageStatus>('loading');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [pending, setPending] = useState(false);
  // Read from the system rather than remembered: what holds a chord is decided
  // outside this application and can change while it runs.
  const [shortcutStatus, setShortcutStatus] = useState<ShortcutStatus | null>(null);
  const [shortcutBusy, setShortcutBusy] = useState(false);
  const [shortcutNotice, setShortcutNotice] = useState<string | null>(null);
  const hotkeyRef = useRef<HTMLInputElement>(null);
  const runShortcutChange = async (
    change: (() => Promise<ShortcutRelease>) | undefined,
  ): Promise<void> => {
    if (change === undefined) return;
    setShortcutBusy(true);
    setShortcutNotice(null);
    try {
      setShortcutNotice(shortcutReleaseNotice(await change()));
      // Read the system back rather than assuming the change took: the point of
      // the row is to say what is true now, not what was asked for.
      setShortcutStatus((await gateway.getShortcutStatus?.().catch(() => null)) ?? null);
    } finally {
      setShortcutBusy(false);
    }
  };
  // Its own window now, so the first field takes focus when the settings
  // arrive — no trap to build, because there is nothing behind it to escape to.
  useEffect(() => {
    if (persisted !== null) hotkeyRef.current?.focus();
  }, [persisted]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const [settings, native, paired, shortcut] = await Promise.all([
        gateway.getSettings(),
        gateway.isAutostartEnabled().catch(() => null),
        gateway.keyvaultIdentity().catch(() => null),
        gateway.getShortcutStatus?.().catch(() => null) ?? Promise.resolve(null),
      ]);
      if (cancelled) return;
      setShortcutStatus(shortcut);
      setPersisted(settings);
      setIdentity(paired);
      setHotkey(settings.hotkey);
      setAutostart(settings.autostart);
      setNativeAutostart(native);
      setUnlimitedRetention(settings.retentionDays === null);
      setRetentionDays(settings.retentionDays === null ? '' : String(settings.retentionDays));
      setDenylist(settings.denylistedApps.join('\n'));
      setLinkPreviews(settings.linkPreviews);
      setPaletteModes(settings.paletteModes);
      setDockIcon(settings.dockIcon);
      void gateway
        .getTypeSafeSettings?.()
        .then((typesafe) => setPrivacyKey(typesafe.apiKey))
        .catch(() => undefined);
      setVaultUrl(settings.keyvault.url ?? '');
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
      paletteModes,
      dockIcon,
      // Overrides over the device identity file, each independent of the
      // other. A blank field travels as an absent one, meaning "use the
      // device's value". The private key is never sent: it is not ours to hold.
      // The address is remembered so the pane can show it and Connect can start from it. The
      // token is not ours to hold: it comes from pairing.
      keyvault: { url: vaultUrl.trim() || null, token: null },
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
  /**
   * Starts pairing: the core mints this device a keypair and opens the browser to approve it.
   *
   * Only the vault's address is needed, and only to find the vault — nothing is typed in, and no
   * key is ever pasted here, because the key this device will use does not exist until this runs.
   */
  const connectToVault = async (): Promise<void> => {
    if (vaultBusy || pairing !== null) return;
    // Blank is the ordinary case after the first pairing: the core knows where the vault is and
    // refusing here is what made re-pairing impossible, since pairing clears this very field.
    const address = vaultUrl.trim();
    setVaultBusy(true);
    setPairNotice(null);
    setVaultError(null);
    try {
      setPairing(await gateway.keyvaultPairStart(address));
      setCopied(false);
    } catch (error) {
      setVaultError(keyvaultErrorMessage(vaultErrorCode(error)));
    }
    setVaultBusy(false);
  };

  const paired = identity?.paired === true;

  /**
   * Forgets the pairing so the device can connect again.
   *
   * The address it knew is kept in the field, so pairing again is one press rather than a
   * retyping — which is the whole point of remembering it.
   */
  const resetPairing = async (): Promise<void> => {
    setVaultError(null);
    setVaultSecrets(null);
    setPairNotice(null);
    const previous = identity?.url ?? '';
    try {
      await gateway.keyvaultResetPairing();
      setIdentity(await gateway.keyvaultIdentity());
      setVaultUrl(previous);
      setPairNotice('Pairing forgotten. Connect again when you are ready.');
    } catch (error) {
      setVaultError(keyvaultErrorMessage(vaultErrorCode(error)));
    }
  };

  const cancelPairing = async (): Promise<void> => {
    setPairing(null);
    setPairNotice(null);
    await gateway.keyvaultPairCancel();
  };

  // Ask the core, on a timer, whether the browser has approved yet.
  //
  // Every answer other than "pending" ends the pairing, so the interval is torn down with it —
  // a finished pairing that went on being polled would keep asking a question already answered.
  useEffect(() => {
    if (pairing === null) return;
    let stopped = false;
    const timer = setInterval(() => {
      void (async () => {
        setNow(Date.now());
        try {
          const { status } = await gateway.keyvaultPairPoll();
          if (stopped || status === 'pending') return;
          setPairing(null);
          setPairNotice(pairingMessage(status));
          // A pairing that worked leaves this install configured, so the list it could not fetch
          // a moment ago is worth fetching now.
          if (status === 'paired') {
            // Pairing clears the overrides in the database, so the fields on screen are now
            // showing values nobody stored. Left alone they keep warning that they outrank the
            // pairing that just replaced them, which is the opposite of true — and if anyone
            // then presses Save, the stale text is written back and really does break it.
            try {
              const [settings, paired] = await Promise.all([
                gateway.getSettings(),
                gateway.keyvaultIdentity().catch(() => null),
              ]);
              setPersisted(settings);
              setIdentity(paired);
              setVaultUrl(settings.keyvault.url ?? '');
            } catch {
              /* the pairing stands regardless; the next open reloads these anyway */
            }
            try {
              setVaultSecrets(await gateway.keyvaultList());
              setVaultError(null);
            } catch {
              /* the pairing still stands; the list can be retried by hand */
            }
          }
        } catch (error) {
          if (stopped) return;
          setPairing(null);
          setVaultError(keyvaultErrorMessage(vaultErrorCode(error)));
        }
      })();
    }, 2000);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [pairing, gateway]);

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
            <span className="workflow-kicker">Local configuration</span>
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

            <SettingsTabs tabs={SETTINGS_TABS} active={activeTab} onSelect={setActiveTab} />
            <div
              className="settings-panel"
              role="tabpanel"
              id={`settings-panel-${activeTab}`}
              aria-labelledby={`settings-tab-${activeTab}`}
            >
              {activeTab === 'shortcut' ? (
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
                      // Recorded, not typed: nobody should have to know the accelerator syntax
                      // to change a shortcut. Read-only rather than a button so the field keeps
                      // its textbox role and shows what is set.
                      readOnly
                      value={hotkey}
                      onKeyDown={(event) => {
                        if (event.key === 'Escape') return;
                        event.preventDefault();
                        const recorded = acceleratorFromKeyEvent(event);
                        if (recorded !== null) setHotkey(recorded);
                      }}
                    />
                  </label>
                  {shortcutStatus !== null && shortcutStatus.heldBySystem ? (
                    <div className="settings-notice" role="status">
                      <p>
                        <strong>{shortcutStatus.hotkey.replace('CommandOrControl', '⌘')}</strong> is
                        a system shortcut, so macOS answers it before Trove ever sees it. Trove can
                        turn that system shortcut off for you.
                      </p>
                      <div className="settings-notice-actions">
                        <button
                          type="button"
                          disabled={shortcutBusy}
                          onClick={() => void runShortcutChange(gateway.freeSummoningShortcut)}
                        >
                          Free it for Trove
                        </button>
                        <button
                          type="button"
                          onClick={() => void gateway.openKeyboardSettings?.().catch(() => undefined)}
                        >
                          Open Keyboard Shortcuts
                        </button>
                      </div>
                    </div>
                  ) : null}
                  {shortcutStatus !== null &&
                  !shortcutStatus.heldBySystem &&
                  shortcutStatus.releasedIds.length > 0 ? (
                    <div className="settings-notice" role="status">
                      <p>Trove turned a system shortcut off to free this combination.</p>
                      <div className="settings-notice-actions">
                        <button
                          type="button"
                          disabled={shortcutBusy}
                          onClick={() => void runShortcutChange(gateway.restoreSystemShortcut)}
                        >
                          Give it back to the system
                        </button>
                      </div>
                    </div>
                  ) : null}
                  {shortcutStatus !== null && !shortcutStatus.registered ? (
                    <div className="settings-notice" role="status">
                      <p>
                        Another application is holding this combination, so Trove could not register
                        it. Record a different one above, or quit whatever is holding it.
                      </p>
                    </div>
                  ) : null}
                  {shortcutNotice !== null ? (
                    <p className="settings-help" role="status">
                      {shortcutNotice}
                    </p>
                  ) : null}
                  <p className="settings-help">
                    Click the field and press the combination you want — hold <kbd>⌘</kbd>,
                    <kbd>⌃</kbd> or <kbd>⌥</kbd> and press a key. Shift alone is not recorded: it
                    would swallow ordinary typing everywhere else. Saving changes the
                    active shortcut immediately; if the new one is already taken by another
                    application, the previous one stays in force.
                  </p>
                  <label className="settings-toggle" htmlFor="settings-autostart">
                    <input
                      id="settings-autostart"
                      type="checkbox"
                      checked={autostart}
                      onChange={(event) => setAutostart(event.currentTarget.checked)}
                    />
                    <span>
                      <Power size={14} aria-hidden="true" /> Launch at login
                    </span>
                  </label>
                  <label className="settings-toggle" htmlFor="settings-dock-icon">
                    <input
                      id="settings-dock-icon"
                      type="checkbox"
                      checked={dockIcon}
                      onChange={(event) => setDockIcon(event.currentTarget.checked)}
                    />
                    <span>
                      <LayoutGrid size={14} aria-hidden="true" /> Show in the Dock
                    </span>
                  </label>
                  <p className="settings-help">
                    Off, Trove lives on the menu bar alone — no Dock tile and no ⌘Tab entry,
                    which suits a window that is summoned by a keystroke and put away again.
                    On, the tile appears and clicking it summons the palette. Saving applies
                    it immediately; macOS only.
                  </p>
                </section>
              ) : null}

              {activeTab === 'retention' ? (
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
                    <span>Days kept</span>
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
              ) : null}

              {activeTab === 'apps' ? (
                <section className="settings-section" aria-labelledby="settings-denylist-title">
                  <div className="settings-section-heading">
                    <span className="settings-section-icon" aria-hidden="true">
                      <ShieldBan size={16} />
                    </span>
                    <h2 id="settings-denylist-title">Applications</h2>
                  </div>
                  <label className="settings-toggle">
                    <input
                      type="checkbox"
                      checked={paletteModes}
                      onChange={(event) => setPaletteModes(event.currentTarget.checked)}
                    />
                    Open the palette on its categories
                  </label>
                  <p className="settings-help">
                    On, the palette opens on three categories — Applications, Clipboard history
                    and the Key vault — picked with 1/2/3 or Tab, and typing means the history.
                    Off, one combined list answers everything at once. Takes effect the next time
                    the palette is summoned.
                  </p>
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
              ) : null}

              {activeTab === 'storage' ? (
                <StorageStats stats={stats} status={storageStatus} />
              ) : null}

              {activeTab === 'links' ? (
                <section className="settings-section" aria-labelledby="settings-links-title">
                  <h2 id="settings-links-title">
                    <Globe size={15} aria-hidden="true" />
                    Link previews
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
              ) : null}

              {activeTab === 'privacy' ? (
                <PrivacyTab
                  gateway={gateway}
                  apiKey={privacyKey}
                  onApiKeyChange={setPrivacyKey}
                  saved={privacySaved}
                  onSaveKey={async () => {
                    try {
                      await gateway.saveTypeSafeSettings?.({ apiKey: privacyKey.trim() });
                      setPrivacySaved(true);
                      setTimeout(() => setPrivacySaved(false), 1500);
                    } catch {
                      /* the row says what it can; a silent no-op here is honest */
                    }
                  }}
                  scan={scan}
                  onScanChange={setScan}
                />
              ) : null}

              {activeTab === 'keyvault' ? (
                <section className="settings-section" aria-labelledby="settings-keyvault-title">
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
                      placeholder="https://your-vault.example.com"
                      // Locked once paired, and showing what the device actually uses rather than
                      // what the settings row remembers. Editing it would change nothing — reads go
                      // to the pairing — and a field that looks editable and is ignored is precisely
                      // the trap this pane used to be.
                      value={paired ? (identity?.url ?? '') : vaultUrl}
                      disabled={paired}
                      onChange={(event) => setVaultUrl(event.currentTarget.value)}
                    />
                  </label>
                  <p className="settings-help">
                    {paired ? (
                      <>
                        This device is paired. It has its own key and its own token, and neither can
                        be typed in here. <strong>Reset</strong> forgets the pairing so you can
                        connect again — it only forgets it locally, so the device stays listed in the
                        vault until the next pairing retires it or you revoke it there.
                      </>
                    ) : (
                      <>
                        <strong>Connect</strong> pairs this device: it generates a key here, sends
                        only the public half, and the browser hands back a token of its own — nothing
                        is pasted, and the key never leaves this machine. Put in the address you open
                        your vault at in a browser. The vault answers with sealed envelopes; a copied
                        key goes to the clipboard without ever being recorded in the history or shown
                        here.
                      </>
                    )}
                  </p>
                  <div className="workflow-actions workflow-actions--start">
                    <button
                      type="button"
                      onClick={() => void connectToVault()}
                      disabled={vaultBusy || pending || paired || pairing !== null}
                    >
                      {pairing !== null ? 'Waiting for approval…' : 'Connect'}
                    </button>
                    {paired ? (
                      <button
                        type="button"
                        onClick={() => void resetPairing()}
                        disabled={vaultBusy || pending || pairing !== null}
                      >
                        Reset
                      </button>
                    ) : null}
                    <button
                      type="button"
                      onClick={() => void testVaultConnection()}
                      disabled={vaultBusy || pending}
                    >
                      {vaultBusy ? 'Talking to the vault…' : 'Test connection'}
                    </button>
                  </div>
                  {pairing !== null ? (
                    <div className="settings-pairing" role="status">
                      <p>
                        A browser was opened to approve this. It is your <strong>default</strong>
                        browser, which may not be the one you are signed into the vault in — if the
                        page asks you to sign in again, copy the link below and open it where you
                        already are.
                      </p>

                      <label className="settings-field" htmlFor="settings-pairing-url">
                        <span>Pairing link</span>
                        <input id="settings-pairing-url" readOnly value={pairing.url} />
                      </label>
                      <div className="workflow-actions workflow-actions--start">
                        <button
                          type="button"
                          onClick={() => {
                            // Best effort: a refused clipboard leaves the link on screen to select by
                            // hand, which is worse but not a dead end.
                            void navigator.clipboard
                              ?.writeText(pairing.url)
                              .then(() => setCopied(true))
                              .catch(() => setCopied(false));
                          }}
                        >
                          {copied ? 'Copied' : 'Copy link'}
                        </button>
                        <button type="button" onClick={() => void cancelPairing()}>
                          Cancel
                        </button>
                      </div>

                      <p className="settings-help">
                        Or open <code>/pair</code> on your vault and paste this code:
                      </p>
                      <p className="settings-pair-code">{pairing.code}</p>

                      <p className="settings-help">
                        Check the page shows this fingerprint — it is what ties that page to this
                        application.
                      </p>
                      <p className="settings-pair-fingerprint">{pairing.fingerprint}</p>

                      {expiryLabel(pairing.expiresAt, now) !== null ? (
                        <p className="settings-help">{expiryLabel(pairing.expiresAt, now)}</p>
                      ) : null}
                    </div>
                  ) : null}
                  {pairNotice !== null ? (
                    <p className="workflow-status" role="status">
                      {pairNotice}
                    </p>
                  ) : null}
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
              ) : null}

              {activeTab === 'export' ? (
                <section className="settings-section" aria-labelledby="settings-export-title">
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
              ) : null}
            </div>

            <div className="workflow-actions">
              <button type="submit" className="workflow-primary" disabled={pending}>
                {pending ? 'Saving…' : 'Save settings'}
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
          Close
        </button>
      </main>
    </div>
  );
};
