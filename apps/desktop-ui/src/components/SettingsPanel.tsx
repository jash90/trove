import {
  Download,
  Globe,
  KeyRound,
  LayoutGrid,
  Move,
  Power,
  ShieldBan,
  Timer,
  Vault,
  X,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEventHandler,
  type KeyboardEventHandler,
} from "react";

import type {
  AppSettings,
  KeyvaultIdentity,
  PairingStarted,
  ExportSummary as ExportSummaryContract,
  KeyvaultSecret,
  ShortcutRelease,
  ShortcutStatus,
  StorageStats as StorageStatsContract,
} from "../lib/contracts";
import type { ClipboardGateway } from "../lib/gateway";
import { t, useT, type MessageKey } from "../i18n";
import {
  acceleratorFromKeyEvent,
  normalizePlatformHotkey,
} from "../lib/hotkeys";
import { SNAP_SHORTCUTS, defaultSnapShortcuts } from "../lib/snapShortcuts";
import { SettingsTabs, type SettingsTab } from "./SettingsTabs";
import { StorageStats } from "./StorageStats";
import { UpdatesSection } from "./UpdatesSection";

const MAX_DENYLIST_ENTRIES = 200;
const MAX_DENYLIST_ENTRY_BYTES = 256;
const MIN_RETENTION_DAYS = 1;
const MAX_RETENTION_DAYS = 3_650;

const DENYLIST_ENTRY_PATTERN = /^[a-z0-9._-]+$/u;

// The shortcut grammar lives in its own module so the palette can record a
// chord without pulling this whole form into the bundle it launches with.
export { acceleratorFromKeyEvent, normalizePlatformHotkey };


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
    throw new Error("invalid_denylist_entry");
  }
  return lowered;
};

export const normalizeDenylistEntries = (
  entries: readonly string[],
): string[] => {
  const normalized = new Set<string>();
  for (const entry of entries) {
    if (entry.trim().length === 0) continue;
    normalized.add(normalizeExecutableDenylistEntry(entry));
  }
  if (normalized.size > MAX_DENYLIST_ENTRIES) {
    throw new Error("denylist_too_large");
  }
  return [...normalized];
};

interface SettingsPanelProps {
  gateway: ClipboardGateway;
  onClose?: () => void;
}

type StorageStatus = "loading" | "ready" | "unavailable";

const TRANSACTION_ERROR: MessageKey = "settings.error.transaction";
const EXPORT_ERROR: MessageKey = "settings.error.export";
const RETENTION_ERROR: MessageKey = "settings.error.retention";
const HOTKEY_ERROR: MessageKey = "settings.error.hotkey";
const SNAP_ERROR: MessageKey = "settings.error.snap";
const SNAP_DUPLICATE_ERROR: MessageKey = "settings.error.snapDuplicate";
const DENYLIST_ERROR: MessageKey = "settings.error.denylist";
const KEYVAULT_SAVE_FIRST: MessageKey = "settings.keyvault.saveFirst";

/**
 * The code out of a rejected vault call. The core rejects with the bare code
 * string — not an Error — so both shapes are read; anything else is unknown.
 */
export const vaultErrorCode = (error: unknown): string =>
  typeof error === "string"
    ? error
    : error instanceof Error
      ? error.message
      : "";

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
  { id: "shortcut", get label() { return t("settings.tab.shortcut"); } },
  { id: "windows", get label() { return t("settings.tab.windows"); } },
  { id: "retention", get label() { return t("settings.tab.retention"); } },
  { id: "apps", get label() { return t("settings.tab.apps"); } },
  { id: "links", get label() { return t("settings.tab.links"); } },
  { id: "keyvault", get label() { return t("settings.tab.keyvault"); } },
  { id: "storage", get label() { return t("settings.tab.storage"); } },
  { id: "export", get label() { return t("settings.tab.export"); } },
];

/// Last, because it is reached for least, and only where there is an application to replace —
/// the browser preview has none.
const UPDATES_TAB: SettingsTab = {
  id: "updates",
  get label() {
    return t("settings.tab.updates");
  },
};

export const expiryLabel = (
  expiresAt: number | null,
  now: number,
): string | null => {
  if (expiresAt === null) return null;
  const minutes = Math.floor((expiresAt - now) / 60_000);
  if (minutes <= 0) return t("settings.keyvault.expiry.expired");
  return t("settings.keyvault.expiry.minutes", { count: minutes });
};

export const pairingMessage = (status: string): string => {
  switch (status) {
    case "paired":
      return t("settings.pairing.paired");
    case "expired":
      return t("settings.pairing.expired");
    case "alreadyClaimed":
      return t("settings.pairing.alreadyClaimed");
    case "notFound":
      return t("settings.pairing.notFound");
    default:
      return t("settings.pairing.unknown");
  }
};

export const keyvaultErrorMessage = (code: string): string => {
  switch (code) {
    case "keyvault_vault_api_not_advertised":
      return t("settings.keyvault.error.apiNotAdvertised");
    case "keyvault_no_vault_address":
      return t("settings.keyvault.error.noAddress");
    case "keyvault_pairing_page_unknown":
      return t("settings.keyvault.error.pairingPageUnknown");
    case "keyvault_browser_failed":
      return t("settings.keyvault.error.browserFailed");
    case "keyvault_pairing_failed":
      return t("settings.keyvault.error.pairingFailed");
    case "keyvault_device_identity_invalid":
      return t("settings.keyvault.error.identityInvalid");
    case "keyvault_not_configured":
      return t(KEYVAULT_SAVE_FIRST);
    case "keyvault_invalid_config":
    case "keyvault_invalid_url":
    case "keyvault_invalid_token":
    case "keyvault_invalid_private_key":
      return t("settings.keyvault.error.invalidConfig");
    case "keyvault_unauthorized":
      return t("settings.keyvault.error.unauthorized");
    case "keyvault_agent_access_disabled":
      return t("settings.keyvault.error.agentAccessDisabled");
    case "keyvault_not_found":
      return t("settings.keyvault.error.notFound");
    case "keyvault_rate_limited":
      return t("settings.keyvault.error.rateLimited");
    case "keyvault_decrypt_failed":
      return t("settings.keyvault.error.decryptFailed");
    case "keyvault_pairing_payload_invalid":
      return t("settings.keyvault.error.payloadInvalid");
    case "keyvault_device_identity_missing":
      return t("settings.keyvault.error.identityMissing");
    case "keyvault_bad_response":
      return t("settings.keyvault.error.badResponse");
    case "keyvault_invalid_slug":
      return t("settings.keyvault.error.invalidSlug");
    case "keyvault_envelope_invalid":
    case "keyvault_envelope_unsupported_version":
    case "keyvault_envelope_too_large":
      return t("settings.keyvault.error.envelope");
    case "keyvault_transport_failed":
      return t("settings.keyvault.error.transport");
    default:
      return t("settings.keyvault.error.unknown");
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
export const shortcutReleaseNotice = (
  outcome: ShortcutRelease,
): string | null => {
  switch (outcome) {
    case "applied":
      return null;
    case "alreadyFree":
      return t("settings.shortcut.release.alreadyFree");
    case "needsLogout":
      return t("settings.shortcut.release.needsLogout");
    default:
      return t("settings.shortcut.release.failed");
  }
};

export const SettingsPanel = ({
  gateway,
  onClose,
}: SettingsPanelProps): React.JSX.Element => {
  const t = useT();
  const [persisted, setPersisted] = useState<AppSettings | null>(null);
  const [hotkey, setHotkey] = useState("");
  const [autostart, setAutostart] = useState(false);
  const [nativeAutostart, setNativeAutostart] = useState<boolean | null>(null);
  const [unlimitedRetention, setUnlimitedRetention] = useState(false);
  const [retentionDays, setRetentionDays] = useState("");
  const [denylist, setDenylist] = useState("");
  const [stats, setStats] = useState<StorageStatsContract | null>(null);
  const [linkPreviews, setLinkPreviews] = useState(true);
  const [paletteModes, setPaletteModes] = useState(true);
  const [dockIcon, setDockIcon] = useState(false);
  const [snapShortcuts, setSnapShortcuts] =
    useState<Record<string, string>>(defaultSnapShortcuts);
  const [vaultUrl, setVaultUrl] = useState("");
  const [vaultSecrets, setVaultSecrets] = useState<KeyvaultSecret[] | null>(
    null,
  );
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
  const [exportSummary, setExportSummary] =
    useState<ExportSummaryContract | null>(null);
  const [storageStatus, setStorageStatus] = useState<StorageStatus>("loading");
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [pending, setPending] = useState(false);
  // Read from the system rather than remembered: what holds a chord is decided
  // outside this application and can change while it runs.
  const [shortcutStatus, setShortcutStatus] = useState<ShortcutStatus | null>(
    null,
  );
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
      setShortcutStatus(
        (await gateway.getShortcutStatus?.().catch(() => null)) ?? null,
      );
    } catch {
      // The change was refused before it began — the application had not
      // finished starting — so nothing in the system moved.
      setShortcutNotice(t("settings.shortcut.starting"));
    } finally {
      setShortcutBusy(false);
    }
  };
  const { checkForUpdate, installUpdate, onOpenSettingsTab } = gateway;
  const updates =
    checkForUpdate && installUpdate
      ? {
          checkForUpdate: checkForUpdate.bind(gateway),
          installUpdate: installUpdate.bind(gateway),
        }
      : null;
  const tabs = updates ? [...SETTINGS_TABS, UPDATES_TAB] : SETTINGS_TABS;

  // The menu bar's "Check for updates…" lands here: it opens this window and
  // names the tab, and the answer lives on that tab.
  useEffect(
    () =>
      onOpenSettingsTab?.call(gateway, (tab) => {
        if (tabs.some((candidate) => candidate.id === tab)) setActiveTab(tab);
      }),
    // `tabs` is derived from the gateway alone, so the gateway is the dependency.
    [gateway],
  );

  // Its own window now, so the first field takes focus when the settings
  // arrive — no trap to build, because there is nothing behind it to escape to.
  useEffect(() => {
    if (persisted !== null) hotkeyRef.current?.focus();
  }, [persisted]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const [settings, native, paired] = await Promise.all([
        gateway.getSettings(),
        gateway.isAutostartEnabled().catch(() => null),
        gateway.keyvaultIdentity().catch(() => null),
      ]);
      if (cancelled) return;
      setPersisted(settings);
      setIdentity(paired);
      setHotkey(settings.hotkey);
      setAutostart(settings.autostart);
      setNativeAutostart(native);
      setUnlimitedRetention(settings.retentionDays === null);
      setRetentionDays(
        settings.retentionDays === null ? "" : String(settings.retentionDays),
      );
      setDenylist(settings.denylistedApps.join("\n"));
      setLinkPreviews(settings.linkPreviews);
      setPaletteModes(settings.paletteModes);
      setDockIcon(settings.dockIcon);
      setSnapShortcuts({
        ...defaultSnapShortcuts(),
        ...settings.snapShortcuts,
      });
      setVaultUrl(settings.keyvault.url ?? "");
    })();
    return () => {
      cancelled = true;
    };
  }, [gateway]);

  // The storage figures and the shortcut's standing are facts about the
  // machine, not about this form: the history grows while the window is
  // hidden, and another application can take the chord at any time. This
  // window is created hidden at launch and shown, never remounted, so reading
  // them once at mount answered for a moment long past. They are read again
  // whenever the window comes forward.
  const liveRequest = useRef(0);
  const refreshLive = useCallback(() => {
    const id = ++liveRequest.current;
    const current = (): boolean => id === liveRequest.current;
    void gateway
      .getStorageStats()
      .then((next) => {
        if (!current()) return;
        setStats(next);
        setStorageStatus("ready");
      })
      .catch(() => {
        if (current()) setStorageStatus("unavailable");
      });
    void (
      gateway.getShortcutStatus?.().catch(() => null) ?? Promise.resolve(null)
    ).then((status) => {
      if (current()) setShortcutStatus(status);
    });
  }, [gateway]);

  // On mount only when somebody can see the answer. The window is mounted
  // hidden at every launch, and a full-table count plus a read of the system
  // shortcut table is work the launch should not wait behind for a window
  // nobody opened; its first focus pays for it instead. The browser preview
  // and the tests have no window at all, and read straight away.
  useEffect(() => {
    try {
      void getCurrentWindow()
        .isVisible()
        .then((visible) => {
          if (visible) refreshLive();
        })
        .catch(() => refreshLive());
    } catch {
      refreshLive();
    }
    return () => {
      liveRequest.current++;
    };
  }, [refreshLive]);

  useEffect(() => {
    let stop: (() => void) | null = null;
    let cancelled = false;
    // try/catch around the call itself, not only the promise: outside a Tauri
    // window getCurrentWindow throws where it stands.
    try {
      void getCurrentWindow()
        .onFocusChanged(({ payload: focused }) => {
          if (focused) refreshLive();
        })
        .then((unlisten) => {
          if (cancelled) unlisten();
          else stop = unlisten;
        })
        .catch(() => undefined);
    } catch {
      /* no window to listen to; the mount read above is all there is */
    }
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [refreshLive]);

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
      setError(t(HOTKEY_ERROR));
      return;
    }

    let nextSnapShortcuts: Record<string, string>;
    try {
      // Every chord distinct, and none of them the summoning shortcut: a
      // collision would be a race the keyboard settles by accident.
      const taken = new Set<string>([nextHotkey]);
      nextSnapShortcuts = {};
      for (const { id } of SNAP_SHORTCUTS) {
        const chord = normalizePlatformHotkey(snapShortcuts[id] ?? "");
        if (taken.has(chord)) {
          setError(t(SNAP_DUPLICATE_ERROR));
          return;
        }
        taken.add(chord);
        nextSnapShortcuts[id] = chord;
      }
    } catch {
      setError(t(SNAP_ERROR));
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
        setError(t(RETENTION_ERROR));
        return;
      }
      nextRetention = parsed;
    }

    let nextDenylist: string[];
    try {
      nextDenylist = normalizeDenylistEntries(denylist.split("\n"));
    } catch {
      setError(t(DENYLIST_ERROR));
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
      snapShortcuts: nextSnapShortcuts,
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
      const autostartChanged =
        nativeAutostart !== null && nativeAutostart !== autostart;
      if (autostartChanged) {
        try {
          await gateway.setAutostartEnabled(autostart);
        } catch {
          setError(t(TRANSACTION_ERROR));
          setNativeAutostart(
            await gateway.isAutostartEnabled().catch(() => null),
          );
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
        setError(t(TRANSACTION_ERROR));
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
    const previous = identity?.url ?? "";
    try {
      await gateway.keyvaultResetPairing();
      setIdentity(await gateway.keyvaultIdentity());
      setVaultUrl(previous);
      setPairNotice(t("settings.keyvault.forgotten"));
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
          if (stopped || status === "pending") return;
          setPairing(null);
          setPairNotice(pairingMessage(status));
          // A pairing that worked leaves this install configured, so the list it could not fetch
          // a moment ago is worth fetching now.
          if (status === "paired") {
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
              setVaultUrl(settings.keyvault.url ?? "");
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
      setError(t(EXPORT_ERROR));
    }
    setExporting(false);
  };

  const handleKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose?.();
    }
  };

  const autostartMismatch =
    persisted !== null &&
    nativeAutostart !== null &&
    persisted.autostart !== nativeAutostart;

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
            <span className="workflow-kicker">{t("settings.kicker")}</span>
            <h1 id="settings-dialog-title">{t("settings.title")}</h1>
          </div>
        </header>

        <p id="settings-dialog-description" className="workflow-warning">
          {t("settings.description")}
        </p>

        {persisted === null ? (
          <p className="workflow-pending">{t("settings.loading")}</p>
        ) : (
          <form className="settings-form" noValidate onSubmit={handleSubmit}>
            {error ? (
              <p className="workflow-alert" role="alert">
                {error}
              </p>
            ) : null}
            {saved && !error ? (
              <p className="workflow-status" role="status">
                {t("settings.saved")}
              </p>
            ) : null}
            {autostartMismatch ? (
              <p className="workflow-status" role="status">
                {t("settings.autostart.mismatch")}{" "}
                <span>
                  {t("settings.autostart.saved", {
                    state: persisted.autostart ? t("settings.on") : t("settings.off"),
                  })}
                </span>
                {", "}
                <span>
                  {t("settings.autostart.system", {
                    state: nativeAutostart ? t("settings.on") : t("settings.off"),
                  })}
                </span>
              </p>
            ) : null}

            <SettingsTabs
              tabs={tabs}
              active={activeTab}
              onSelect={setActiveTab}
            />
            <div
              className="settings-panel"
              role="tabpanel"
              id={`settings-panel-${activeTab}`}
              aria-labelledby={`settings-tab-${activeTab}`}
            >
              {activeTab === "shortcut" ? (
                <section
                  className="settings-section"
                  aria-labelledby="settings-hotkey-title"
                >
                  <div className="settings-section-heading">
                    <span className="settings-section-icon" aria-hidden="true">
                      <KeyRound size={16} />
                    </span>
                    <h2 id="settings-hotkey-title">{t("settings.shortcut.title")}</h2>
                  </div>
                  <label className="settings-field" htmlFor="settings-hotkey">
                    <span>{t("settings.shortcut.global")}</span>
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
                        if (event.key === "Escape") return;
                        event.preventDefault();
                        const recorded = acceleratorFromKeyEvent(event);
                        if (recorded !== null) setHotkey(recorded);
                      }}
                    />
                  </label>
                  {shortcutStatus !== null && shortcutStatus.heldBySystem ? (
                    <div className="settings-notice" role="status">
                      <p>
                        <strong>
                          {shortcutStatus.hotkey.replace(
                            "CommandOrControl",
                            "⌘",
                          )}
                        </strong>{" "}
                        {t("settings.shortcut.heldBySystem")}
                      </p>
                      <div className="settings-notice-actions">
                        <button
                          type="button"
                          disabled={shortcutBusy}
                          onClick={() =>
                            void runShortcutChange(
                              gateway.freeSummoningShortcut,
                            )
                          }
                        >
                          {t("settings.shortcut.free")}
                        </button>
                        <button
                          type="button"
                          onClick={() =>
                            void gateway
                              .openKeyboardSettings?.()
                              .catch(() => undefined)
                          }
                        >
                          {t("settings.shortcut.openKeyboard")}
                        </button>
                      </div>
                    </div>
                  ) : null}
                  {shortcutStatus !== null &&
                  !shortcutStatus.heldBySystem &&
                  shortcutStatus.releasedIds.length > 0 ? (
                    <div className="settings-notice" role="status">
                      <p>
                        {t("settings.shortcut.released")}
                      </p>
                      <div className="settings-notice-actions">
                        <button
                          type="button"
                          disabled={shortcutBusy}
                          onClick={() =>
                            void runShortcutChange(
                              gateway.restoreSystemShortcut,
                            )
                          }
                        >
                          {t("settings.shortcut.restore")}
                        </button>
                      </div>
                    </div>
                  ) : null}
                  {shortcutStatus !== null && !shortcutStatus.registered ? (
                    <div className="settings-notice" role="status">
                      <p>
                        {t("settings.shortcut.notRegistered")}
                      </p>
                    </div>
                  ) : null}
                  {shortcutNotice !== null ? (
                    <p className="settings-help" role="status">
                      {shortcutNotice}
                    </p>
                  ) : null}
                  <p className="settings-help">
                    {t("settings.shortcut.help.before")}{" "}
                    <kbd>⌘</kbd>,<kbd>⌃</kbd> {t("settings.shortcut.help.or")}{" "}
                    <kbd>⌥</kbd> {t("settings.shortcut.help.after")}
                  </p>
                  <label
                    className="settings-toggle"
                    htmlFor="settings-autostart"
                  >
                    <input
                      id="settings-autostart"
                      type="checkbox"
                      checked={autostart}
                      onChange={(event) =>
                        setAutostart(event.currentTarget.checked)
                      }
                    />
                    <span>
                      <Power size={14} aria-hidden="true" /> {t("settings.autostart")}
                    </span>
                  </label>
                  <label
                    className="settings-toggle"
                    htmlFor="settings-dock-icon"
                  >
                    <input
                      id="settings-dock-icon"
                      type="checkbox"
                      checked={dockIcon}
                      onChange={(event) =>
                        setDockIcon(event.currentTarget.checked)
                      }
                    />
                    <span>
                      <LayoutGrid size={14} aria-hidden="true" />{" "}
                      {t("settings.dockIcon")}
                    </span>
                  </label>
                  <p className="settings-help">
                    {t("settings.dockIcon.help")}
                  </p>
                </section>
              ) : null}

              {activeTab === "windows" ? (
                <section
                  className="settings-section"
                  aria-labelledby="settings-windows-title"
                >
                  <div className="settings-section-heading">
                    <span className="settings-section-icon" aria-hidden="true">
                      <Move size={16} />
                    </span>
                    <h2 id="settings-windows-title">{t("settings.windows.title")}</h2>
                  </div>
                  <p className="settings-help">
                    {t("settings.windows.help")}
                  </p>
                  {SNAP_SHORTCUTS.map(({ id, label }) => (
                    <label
                      className="settings-field"
                      htmlFor={`settings-snap-${id}`}
                      key={id}
                    >
                      <span>{label}</span>
                      <input
                        id={`settings-snap-${id}`}
                        type="text"
                        autoComplete="off"
                        spellCheck={false}
                        // Recorded, not typed — the same contract as the global
                        // shortcut field.
                        readOnly
                        value={snapShortcuts[id] ?? ""}
                        onKeyDown={(event) => {
                          if (event.key === "Escape") return;
                          event.preventDefault();
                          const recorded = acceleratorFromKeyEvent(event);
                          if (recorded !== null) {
                            setSnapShortcuts((previous) => ({
                              ...previous,
                              [id]: recorded,
                            }));
                          }
                        }}
                      />
                    </label>
                  ))}
                </section>
              ) : null}

              {activeTab === "retention" ? (
                <section
                  className="settings-section"
                  aria-labelledby="settings-retention-title"
                >
                  <div className="settings-section-heading">
                    <span className="settings-section-icon" aria-hidden="true">
                      <Timer size={16} />
                    </span>
                    <h2 id="settings-retention-title">{t("settings.retention.title")}</h2>
                  </div>
                  <label
                    className="settings-toggle"
                    htmlFor="settings-retention-unlimited"
                  >
                    <input
                      id="settings-retention-unlimited"
                      type="checkbox"
                      checked={unlimitedRetention}
                      onChange={(event) =>
                        setUnlimitedRetention(event.currentTarget.checked)
                      }
                    />
                    <span>{t("settings.retention.unlimited")}</span>
                  </label>
                  <label
                    className="settings-field"
                    htmlFor="settings-retention-days"
                  >
                    <span>{t("settings.retention.days")}</span>
                    <input
                      id="settings-retention-days"
                      type="number"
                      min={MIN_RETENTION_DAYS}
                      max={MAX_RETENTION_DAYS}
                      step={1}
                      disabled={unlimitedRetention}
                      value={retentionDays}
                      onChange={(event) =>
                        setRetentionDays(event.currentTarget.value)
                      }
                    />
                  </label>
                  <p className="settings-help">
                    {t("settings.retention.help")}
                  </p>
                </section>
              ) : null}

              {activeTab === "apps" ? (
                <section
                  className="settings-section"
                  aria-labelledby="settings-denylist-title"
                >
                  <div className="settings-section-heading">
                    <span className="settings-section-icon" aria-hidden="true">
                      <ShieldBan size={16} />
                    </span>
                    <h2 id="settings-denylist-title">{t("settings.apps.title")}</h2>
                  </div>
                  <label className="settings-toggle">
                    <input
                      type="checkbox"
                      checked={paletteModes}
                      onChange={(event) =>
                        setPaletteModes(event.currentTarget.checked)
                      }
                    />
                    {t("settings.apps.paletteModes")}
                  </label>
                  <p className="settings-help">
                    {t("settings.apps.paletteModes.help")}
                  </p>
                  <label className="settings-field" htmlFor="settings-denylist">
                    <span>{t("settings.apps.denylist")}</span>
                    <textarea
                      id="settings-denylist"
                      rows={4}
                      spellCheck={false}
                      value={denylist}
                      onChange={(event) =>
                        setDenylist(event.currentTarget.value)
                      }
                    />
                  </label>
                  <p className="settings-help">
                    {t("settings.apps.denylist.help", { max: MAX_DENYLIST_ENTRIES })}
                  </p>
                </section>
              ) : null}

              {activeTab === "storage" ? (
                <StorageStats stats={stats} status={storageStatus} />
              ) : null}

              {activeTab === "links" ? (
                <section
                  className="settings-section"
                  aria-labelledby="settings-links-title"
                >
                  <h2 id="settings-links-title">
                    <Globe size={15} aria-hidden="true" />
                    {t("settings.links.title")}
                  </h2>
                  <label className="settings-toggle">
                    <input
                      type="checkbox"
                      checked={linkPreviews}
                      onChange={(event) =>
                        setLinkPreviews(event.currentTarget.checked)
                      }
                    />
                    {t("settings.links.fetch")}
                  </label>
                  <p className="settings-help">
                    {t("settings.links.help")}
                  </p>
                </section>
              ) : null}

              {activeTab === "keyvault" ? (
                <section
                  className="settings-section"
                  aria-labelledby="settings-keyvault-title"
                >
                  <h2 id="settings-keyvault-title">
                    <Vault size={15} aria-hidden="true" />
                    {t("settings.tab.keyvault")}
                  </h2>
                  <label
                    className="settings-field"
                    htmlFor="settings-keyvault-url"
                  >
                    <span>{t("settings.keyvault.address")}</span>
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
                      value={paired ? (identity?.url ?? "") : vaultUrl}
                      disabled={paired}
                      onChange={(event) =>
                        setVaultUrl(event.currentTarget.value)
                      }
                    />
                  </label>
                  <p className="settings-help">
                    {paired ? (
                      <>
                        {t("settings.keyvault.paired.before")}{" "}
                        <strong>{t("settings.keyvault.reset")}</strong>{" "}
                        {t("settings.keyvault.paired.after")}
                      </>
                    ) : (
                      <>
                        <strong>{t("settings.keyvault.connect")}</strong>{" "}
                        {t("settings.keyvault.unpaired")}
                      </>
                    )}
                  </p>
                  <div className="workflow-actions workflow-actions--start">
                    <button
                      type="button"
                      onClick={() => void connectToVault()}
                      disabled={
                        vaultBusy || pending || paired || pairing !== null
                      }
                    >
                      {pairing !== null
                        ? t("settings.keyvault.waiting")
                        : t("settings.keyvault.connect")}
                    </button>
                    {paired ? (
                      <button
                        type="button"
                        onClick={() => void resetPairing()}
                        disabled={vaultBusy || pending || pairing !== null}
                      >
                        {t("settings.keyvault.reset")}
                      </button>
                    ) : null}
                    <button
                      type="button"
                      onClick={() => void testVaultConnection()}
                      disabled={vaultBusy || pending}
                    >
                      {vaultBusy
                        ? t("settings.keyvault.talking")
                        : t("settings.keyvault.test")}
                    </button>
                  </div>
                  {pairing !== null ? (
                    <div className="settings-pairing" role="status">
                      <p>
                        {t("settings.keyvault.browser.before")}{" "}
                        <strong>{t("settings.keyvault.browser.default")}</strong>{" "}
                        {t("settings.keyvault.browser.after")}
                      </p>

                      <label
                        className="settings-field"
                        htmlFor="settings-pairing-url"
                      >
                        <span>{t("settings.keyvault.pairingLink")}</span>
                        <input
                          id="settings-pairing-url"
                          readOnly
                          value={pairing.url}
                        />
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
                          {copied ? t("settings.keyvault.copied") : t("settings.keyvault.copyLink")}
                        </button>
                        <button
                          type="button"
                          onClick={() => void cancelPairing()}
                        >
                          {t("settings.keyvault.cancel")}
                        </button>
                      </div>

                      <p className="settings-help">
                        {t("settings.keyvault.code.before")} <code>/pair</code>{" "}
                        {t("settings.keyvault.code.after")}
                      </p>
                      <p className="settings-pair-code">{pairing.code}</p>

                      <p className="settings-help">
                        {t("settings.keyvault.fingerprint")}
                      </p>
                      <p className="settings-pair-fingerprint">
                        {pairing.fingerprint}
                      </p>

                      {expiryLabel(pairing.expiresAt, now) !== null ? (
                        <p className="settings-help">
                          {expiryLabel(pairing.expiresAt, now)}
                        </p>
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
                      {t("settings.keyvault.secretCopied", { slug: vaultCopiedSlug })}
                    </p>
                  ) : null}
                  {vaultSecrets !== null ? (
                    vaultSecrets.length === 0 ? (
                      <p className="workflow-status" role="status">
                        {t("settings.keyvault.noSecrets")}
                      </p>
                    ) : (
                      <ul
                        className="settings-keyvault-list"
                        aria-label={t("settings.keyvault.secrets")}
                      >
                        {vaultSecrets.map((secret) => (
                          <li key={secret.slug}>
                            <span>{secret.name}</span>
                            <span className="settings-keyvault-slug">
                              {secret.slug}
                            </span>
                            <button
                              type="button"
                              aria-label={t("settings.keyvault.copySecret", { slug: secret.slug })}
                              disabled={vaultBusy || pending}
                              onClick={() => void copyVaultSecret(secret.slug)}
                            >
                              {t("settings.keyvault.copy")}
                            </button>
                          </li>
                        ))}
                      </ul>
                    )
                  ) : null}
                </section>
              ) : null}

              {activeTab === "export" ? (
                <section
                  className="settings-section"
                  aria-labelledby="settings-export-title"
                >
                  <h2 id="settings-export-title">
                    <Download size={15} aria-hidden="true" />
                    {t("settings.export.title")}
                  </h2>
                  <p className="settings-help">
                    {t("settings.export.help")}
                  </p>
                  <div className="workflow-actions workflow-actions--start">
                    <button
                      type="button"
                      onClick={() => void runExport()}
                      disabled={exporting}
                    >
                      {exporting ? t("settings.export.pending") : t("settings.export.run")}
                    </button>
                  </div>
                  {exportSummary ? (
                    <p className="workflow-status" role="status">
                      {t("settings.export.summary", {
                        count: exportSummary.records,
                        images: exportSummary.images,
                      })}{" "}
                      {t("settings.export.withoutPayload", {
                        count: exportSummary.withoutPayload,
                      })}
                    </p>
                  ) : null}
                </section>
              ) : null}

              {activeTab === "updates" && updates ? (
                <UpdatesSection gateway={updates} />
              ) : null}
            </div>

            <div className="workflow-actions">
              <button
                type="submit"
                className="workflow-primary"
                disabled={pending}
              >
                {pending ? t("settings.saving") : t("settings.save")}
              </button>
            </div>
          </form>
        )}

        <button
          type="button"
          className="workflow-dismiss"
          aria-label={t("settings.closeLabel")}
          onClick={onClose}
        >
          <X size={16} aria-hidden="true" />
          {t("settings.close")}
        </button>
      </main>
    </div>
  );
};
