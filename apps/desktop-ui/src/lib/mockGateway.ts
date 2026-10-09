import type { ClipboardGateway } from "./gateway";
import {
  SYNTHETIC_APPS,
  SYNTHETIC_APP_ICON,
  SYNTHETIC_HISTORY_ITEMS,
  SYNTHETIC_IMPORT_PROGRESS,
  SYNTHETIC_KEYVAULT_SECRETS,
  SYNTHETIC_SETTINGS,
  SYNTHETIC_STORAGE_STATS,
} from "./fixtures";

let historyItems = SYNTHETIC_HISTORY_ITEMS.map((item) => ({ ...item }));
let chatSeq = 0;
let chatStreamListener:
  | ((event: import("./contracts").ChatStreamEvent) => void)
  | null = null;
let chatSettings: import("./contracts").ChatSettings = {
  provider: "zai",
  model: "glm-4.6",
  keys: { zai: "", openai: "", openrouter: "", anthropic: "" },
};
let settings = {
  ...SYNTHETIC_SETTINGS,
  denylistedApps: [...SYNTHETIC_SETTINGS.denylistedApps],
  keyvault: { ...SYNTHETIC_SETTINGS.keyvault },
};
let autostartEnabled = SYNTHETIC_SETTINGS.autostart;

export const mockGateway: ClipboardGateway = {
  search: async (request) => {
    const normalizedQuery = request.query.trim().toLocaleLowerCase("en-US");
    const matches = normalizedQuery
      ? historyItems.filter((item) =>
          `${item.preview} ${item.sourceAppName ?? ""}`
            .toLocaleLowerCase("en-US")
            .includes(normalizedQuery),
        )
      : historyItems;
    return {
      items: matches.slice(0, request.limit).map((item) => ({ ...item })),
      nextCursor: null,
      rankedTruncated: false,
    };
  },
  preview: async (eventId) => {
    const item = historyItems.find(
      (candidate) => candidate.eventId === eventId,
    );
    if (!item) throw new Error("history_event_not_found");
    return {
      eventId: item.eventId,
      kind: item.kind,
      mimeType: item.kind === "image" ? "image/png" : "text/plain",
      text: item.kind === "image" ? null : item.preview,
      byteSize: item.byteSize,
      sourceAppName: item.sourceAppName,
      sourcePath:
        item.kind === "file"
          ? "/synthetic/archiwum/notatka-syntetyczna.pdf"
          : null,
      sourceExists: false,
    };
  },
  setPinned: async (eventId, pinned) => {
    const index = historyItems.findIndex((item) => item.eventId === eventId);
    if (index < 0) throw new Error("history_event_not_found");
    historyItems = historyItems.map((item, itemIndex) =>
      itemIndex === index ? { ...item, pinned } : item,
    );
  },
  deleteEvent: async (eventId) => {
    const remaining = historyItems.filter((item) => item.eventId !== eventId);
    if (remaining.length === historyItems.length)
      throw new Error("history_event_not_found");
    historyItems = remaining;
  },
  copyEvent: async (_eventId, plainText) => ({ mode: "copied", plainText }),
  // The browser preview offers the encrypted shape, so the password step is
  // reachable without a real export.
  chooseImportFile: async () => "synthetic://clipboard-export.rayconfig",
  chooseImportDirectory: async () => "synthetic://clipboard-export",
  // A path ending in .rayconfig demands a password, so the browser preview
  // walks the same steps the encrypted flow does on a real export.
  analyzeImport: async (path, password) => {
    if (path.toLowerCase().endsWith(".rayconfig")) {
      if (password === undefined)
        throw new Error("rayconfig_password_required");
      if (password !== "synthetic")
        throw new Error("rayconfig_password_invalid");
    }
    return {
      analysisId: SYNTHETIC_IMPORT_PROGRESS.runId,
      total: 3,
      candidateRecords: 3,
      skipped: 0,
      failed: 0,
    };
  },
  startImport: async (_analysisId) => ({
    runId: SYNTHETIC_IMPORT_PROGRESS.runId,
  }),
  discardImportAnalysis: async (_analysisId) => undefined,
  getImportStatus: async (_runId) => ({
    ...SYNTHETIC_IMPORT_PROGRESS,
    summary: SYNTHETIC_IMPORT_PROGRESS.summary
      ? { ...SYNTHETIC_IMPORT_PROGRESS.summary }
      : null,
  }),
  revealSource: async () => {
    throw new Error("source_unavailable");
  },
  // Nothing to open in a browser preview; the settings page is reachable by
  // hand at #settings.
  openSettingsWindow: async () => undefined,
  linkPreview: async (eventId) => {
    const item = historyItems.find(
      (candidate) => candidate.eventId === eventId,
    );
    if (!item || item.kind !== "link") return null;
    return {
      host: "example.invalid",
      rest: "/synthetic-document",
      title: "Synthetic document title",
      iconMime: null,
      iconBase64: null,
      imageMime: null,
      imageBase64: null,
      localOnly: false,
      fetching: false,
    };
  },
  chooseExportDirectory: async () => "synthetic://clipboard-export",
  exportHistory: async () => ({ records: 4, images: 1, withoutPayload: 1 }),
  // The browser preview is never out of date: there is no release behind it,
  // and an install that resolved would promise a restart that cannot happen.
  checkForUpdate: async () => ({
    currentVersion: "0.0.0-preview",
    available: false,
    version: null,
    notes: null,
  }),
  installUpdate: async () => {
    throw new Error("no_pending_update");
  },
  onOpenSettingsTab: () => () => undefined,
  // The browser preview has no vault behind it; the settings pane still gets
  // a list to show, and copies that went nowhere but never fail.
  keyvaultList: async () =>
    SYNTHETIC_KEYVAULT_SECRETS.map((secret) => ({ ...secret })),
  keyvaultCopySecret: async () => undefined,
  // The mock pairs instantly. Nothing here talks to a vault, and a fake that made callers wait
  // would only teach the tests to tolerate a spinner.
  keyvaultPairStart: async () => ({
    fingerprint: "A1B2-C3D4",
    url: "https://vault.example.invalid/pair?code=synthetic-code",
    code: "synthetic-code",
    expiresAt: Date.now() + 30 * 60 * 1000,
  }),
  keyvaultPairPoll: async () => ({ status: "paired" as const }),
  keyvaultPairCancel: async () => undefined,
  keyvaultIdentity: async () => ({ paired: false, url: null }),
  keyvaultResetPairing: async () => undefined,
  // The browser preview has no core behind it, so nothing ever changes.
  onHistoryChanged: () => () => undefined,
  onAppsChanged: () => () => undefined,
  // Chat in the preview: a canned answer that streams the same way the
  // real one does, so the window can be laid out without a provider.
  chatSend: (messages) => {
    const id = `mock-${chatSeq++}`;
    const last = messages[messages.length - 1]?.content ?? "";
    // Markdown on purpose: the preview renders the answer the way the real
    // window does, so the renderer can be checked without a provider.
    const reply = [
      `Preview answer to **${last.slice(0, 40) || "your message"}**.`,
      "",
      "## What renders here",
      "",
      "- headings, **bold**, *italics*, and `inline code`",
      "- lists, and tables",
      "",
      "| feature | state |",
      "| --- | --- |",
      "| markdown | on |",
      "| code blocks | saveable |",
      "",
      "```ts",
      "const answer = 42;",
      "export default answer;",
      "```",
      "",
      "```html",
      '<!doctype html><html><body style="font-family:sans-serif">',
      '<h1 style="color:crimson">Live HTML</h1>',
      "<button onclick=\"this.textContent='clicked';parent.parent.postMessage({kind:'artifact-probe',text:'html-script-ran'},'*')\">click me</button>",
      "</body></html>",
      "```",
      "",
      "```jsx",
      'import { useEffect, useState } from "react";',
      "export default function App() {",
      "  const [n, setN] = useState(0);",
      "  useEffect(() => {",
      "    parent.parent.postMessage({ kind: 'artifact-probe', text: 'jsx-mounted' }, '*');",
      "  }, []);",
      '  return <h2 style={{color:"teal"}} onClick={() => setN(n + 1)}>React artifact {n}</h2>;',
      "}",
      "```",
      "",
      "> A quotation, for completeness.",
    ].join("\n");
    const emit = chatStreamListener;
    // Streamed by lines rather than words: fences stay whole, which is the
    // shape a real stream produces at its chunk boundaries anyway.
    // A reasoning model's shape: the thinking first, the answer after —
    // the stream the preview renders is the stream the real one produces.
    const thinking = "Considering the request briefly.\n";
    const chunks = reply.split(/(?<=\n)/u);
    setTimeout(
      () => emit?.({ kind: "delta", id, part: "reasoning", text: thinking }),
      60,
    );
    chunks.forEach((chunk, index) => {
      setTimeout(
        () => emit?.({ kind: "delta", id, part: "answer", text: chunk }),
        120 + 60 * (index + 1),
      );
    });
    setTimeout(
      () => emit?.({ kind: "done", id }),
      180 + 60 * (chunks.length + 1),
    );
    return Promise.resolve({ id });
  },
  chatStop: async () => true,
  chatListModels: async () => [
    "glm-4.5-air",
    "glm-4.5-flash",
    "glm-4.5",
    "glm-4.6",
  ],
  saveGeneratedFile: async () => true,
  copyChatText: async () => undefined,
  openExternalUrl: async () => undefined,
  getChatSettings: async () => ({ ...chatSettings }),
  saveChatSettings: async (next) => {
    chatSettings = { ...next };
    return { ...chatSettings };
  },
  openChatWindow: async () => undefined,
  // A browser preview cannot move anyone's windows; resolving true keeps
  // the palette's flow walkable where Tauri is absent.
  snapWindow: async () => true,
  onChatEvent: (listener) => {
    chatStreamListener = listener;
    return () => {
      chatStreamListener = null;
    };
  },
  onLinkPreviewReady: () => () => undefined,
  getThumbnail: async (eventId) =>
    eventId === 103 ? { mimeType: "image/png", base64: "c3ludGhldGlj" } : null,
  getSettings: async () => ({
    ...settings,
    denylistedApps: [...settings.denylistedApps],
    keyvault: { ...settings.keyvault },
  }),
  isAutostartEnabled: async () => autostartEnabled,
  setAutostartEnabled: async (enabled) => {
    autostartEnabled = enabled;
  },
  saveSettings: async (nextSettings) => {
    settings = {
      ...nextSettings,
      denylistedApps: [...nextSettings.denylistedApps],
      keyvault: { ...nextSettings.keyvault },
    };
    return {
      ...settings,
      denylistedApps: [...settings.denylistedApps],
      keyvault: { ...settings.keyvault },
    };
  },
  getStorageStats: async () => ({ ...SYNTHETIC_STORAGE_STATS }),
  // Copies, as everywhere else in this gateway: a consumer mutating its
  // answer must not bend the next one.
  listApps: async () => SYNTHETIC_APPS.map((app) => ({ ...app })),
  // A browser preview cannot start applications; resolving rather than
  // rejecting keeps the palette's flow walkable where Tauri is absent.
  launchApp: async () => undefined,
  getAppIcon: async () => ({ ...SYNTHETIC_APP_ICON }),
  // There is no System Settings to open outside Tauri; resolving keeps the
  // browser preview walkable, same as launching an application does.
  openAccessibilitySettings: async () => undefined,
  // Nothing holds a chord in a browser, so the preview never shows the notice.
  getShortcutStatus: async () => ({
    hotkey: settings.hotkey,
    registered: true,
    heldBySystem: false,
    releasedIds: [],
  }),
  freeSummoningShortcut: async () => "alreadyFree" as const,
  restoreSystemShortcut: async () => "alreadyFree" as const,
  openKeyboardSettings: async () => undefined,
};
