// A stand-in for the Tauri core, injected into the browser before the
// interface loads.
//
// The interface picks its gateway by looking for `window.__TAURI_INTERNALS__`:
// with it present, every call goes through `invoke`, exactly as it does in
// the desktop shell. Answering those calls here lets the real interface be
// photographed on invented data — no clipboard, no database, no network, and
// no change to the application's own code.
//
// Everything below is fictional. The source application names are ordinary
// macOS applications; the entries, people, paths and keys are made up.

(() => {
  const DAY = 86_400_000;
  // A fixed "now" so every run renders the same dates.
  const NOW = new Date(2026, 8, 18, 9, 42).getTime();
  const at = (minutesAgo) => NOW - minutesAgo * 60_000;

  // ---------------------------------------------------------------- images --

  const canvasPng = (width, height, draw) => {
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    draw(canvas.getContext("2d"), width, height);
    return canvas.toDataURL("image/png").split(",")[1];
  };

  const roundRect = (ctx, x, y, w, h, r) => {
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.arcTo(x + w, y, x + w, y + h, r);
    ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r);
    ctx.arcTo(x, y, x + w, y, r);
    ctx.closePath();
  };

  // An application icon: a rounded tile in the app's colours with its
  // initial. The real core renders the bundle's own icon through NSWorkspace.
  const appIcon = (letter, from, to) =>
    canvasPng(64, 64, (ctx) => {
      const gradient = ctx.createLinearGradient(0, 0, 64, 64);
      gradient.addColorStop(0, from);
      gradient.addColorStop(1, to);
      roundRect(ctx, 4, 4, 56, 56, 13);
      ctx.fillStyle = gradient;
      ctx.fill();
      ctx.fillStyle = "#ffffff";
      ctx.font = "600 30px 'Space Grotesk Variable', sans-serif";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(letter, 32, 34);
    });

  // The copied image: a mock-up of a dashboard card, drawn rather than
  // taken from anywhere.
  const designMock = () =>
    canvasPng(640, 400, (ctx, w, h) => {
      ctx.fillStyle = "#f4f2ec";
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = "#171d26";
      ctx.fillRect(0, 0, w, 48);
      ctx.fillStyle = "#fffefa";
      ctx.font = "600 18px 'Space Grotesk Variable', sans-serif";
      ctx.fillText("Weekly sign-ups", 24, 31);
      roundRect(ctx, 24, 72, 592, 300, 12);
      ctx.fillStyle = "#fffefa";
      ctx.fill();
      const bars = [120, 150, 110, 190, 230, 210, 260];
      bars.forEach((value, index) => {
        roundRect(ctx, 60 + index * 76, 340 - value, 44, value, 6);
        ctx.fillStyle = index === bars.length - 1 ? "#b45309" : "#234e9d";
        ctx.fill();
      });
      ctx.fillStyle = "#69717d";
      ctx.font = "14px 'JetBrains Mono Variable', monospace";
      ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].forEach((day, index) =>
        ctx.fillText(day, 64 + index * 76, 362),
      );
    });

  // The picture a documentation page nominates for itself (og:image).
  const ogImage = () =>
    canvasPng(600, 315, (ctx, w, h) => {
      const gradient = ctx.createLinearGradient(0, 0, w, h);
      gradient.addColorStop(0, "#171d26");
      gradient.addColorStop(1, "#234e9d");
      ctx.fillStyle = gradient;
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = "#fffefa";
      ctx.font = "600 38px 'Space Grotesk Variable', sans-serif";
      ctx.fillText("Clipboard API", 40, 150);
      ctx.fillStyle = "#c9d6ef";
      ctx.font = "20px 'Source Serif 4 Variable', serif";
      ctx.fillText("Read and write the system clipboard", 40, 190);
    });

  const siteIcon = () => appIcon("M", "#1b1b1b", "#3a3a3a");

  // ---------------------------------------------------------------- data --

  const history = [
    {
      eventId: 1201,
      kind: "text",
      minutesAgo: 3,
      sourceAppName: "Slack",
      pinned: true,
      preview:
        "Standup moves to 9:30 on Thursday — same room, and please bring the Q4 roadmap draft.",
      occurrences: [3, 60 * 26, 60 * 50],
    },
    {
      eventId: 1202,
      kind: "code",
      minutesAgo: 7,
      sourceAppName: "Terminal",
      preview: "git rebase -i --autosquash origin/main",
      occurrences: [7, 60 * 5, 60 * 29, 60 * 76],
    },
    {
      eventId: 1203,
      kind: "link",
      minutesAgo: 12,
      sourceAppName: "Safari",
      preview: "https://developer.mozilla.org/en-US/docs/Web/API/Clipboard_API",
      occurrences: [12],
    },
    {
      eventId: 1204,
      kind: "image",
      minutesAgo: 18,
      sourceAppName: "Figma",
      preview: "Image · 640 × 400",
      byteSize: 48_210,
      hasThumbnail: true,
      occurrences: [18],
    },
    {
      eventId: 1205,
      kind: "color",
      minutesAgo: 21,
      sourceAppName: "Figma",
      preview: "#234E9D",
      occurrences: [21, 60 * 3],
    },
    {
      eventId: 1206,
      kind: "text",
      minutesAgo: 34,
      sourceAppName: "Mail",
      preview:
        "Faktura za wrzesień — termin płatności mija 30 września. Proszę o potwierdzenie odbioru.",
      occurrences: [34],
    },
    {
      eventId: 1207,
      kind: "file",
      minutesAgo: 47,
      sourceAppName: "Finder",
      preview: "/Users/demo/Documents/Invoices/invoice-2026-0917.pdf",
      byteSize: 0,
      occurrences: [47],
    },
    {
      eventId: 1208,
      kind: "code",
      minutesAgo: 63,
      sourceAppName: "Visual Studio Code",
      pinned: true,
      preview:
        "SELECT customer_id, SUM(total) AS revenue\nFROM orders\nWHERE created_at >= date('now', '-30 days')\nGROUP BY customer_id\nORDER BY revenue DESC\nLIMIT 10;",
      occurrences: [63, DAY / 60_000 + 12],
    },
    {
      eventId: 1209,
      kind: "link",
      minutesAgo: 95,
      sourceAppName: "Safari",
      preview: "https://github.com/tauri-apps/tauri",
      occurrences: [95],
    },
    {
      eventId: 1210,
      kind: "text",
      minutesAgo: 130,
      sourceAppName: "Notes",
      preview:
        "Shipping address: Orchard Lane 12, flat 4, 00-950 Warsaw (fictional, for testing)",
      occurrences: [130],
    },
    {
      eventId: 1211,
      kind: "html",
      minutesAgo: 190,
      sourceAppName: "Mail",
      preview: "<p>Thanks for the quick turnaround — <strong>approved</strong>.</p>",
      occurrences: [190],
    },
  ].map((entry) => ({
    eventId: entry.eventId,
    globalId: `0198f000-0000-7000-8000-00000000${entry.eventId}`,
    kind: entry.kind,
    capturedAtMs: at(entry.minutesAgo),
    sourceAppName: entry.sourceAppName,
    pinned: entry.pinned ?? false,
    preview: entry.preview,
    byteSize: entry.byteSize ?? new TextEncoder().encode(entry.preview).length,
    hasThumbnail: entry.hasThumbnail ?? false,
    occurrenceCount: entry.occurrences.length,
    occurrences: entry.occurrences.map(at),
  }));

  const APP_COLOURS = [
    ["Calendar", "com.apple.iCal", "/System/Applications", "#ef4444", "#b91c1c"],
    ["Figma", "com.figma.Desktop", "/Applications", "#a259ff", "#0acf83"],
    ["Finder", "com.apple.finder", "/System/Library/CoreServices", "#38bdf8", "#1d4ed8"],
    ["Mail", "com.apple.mail", "/System/Applications", "#60a5fa", "#2563eb"],
    ["Notes", "com.apple.Notes", "/System/Applications", "#facc15", "#ca8a04"],
    ["Safari", "com.apple.Safari", "/Applications", "#22d3ee", "#0369a1"],
    ["Slack", "com.tinyspeck.slackmacgap", "/Applications", "#e01e5a", "#4a154b"],
    ["System Settings", "com.apple.systempreferences", "/System/Applications", "#9ca3af", "#4b5563"],
    ["Terminal", "com.apple.Terminal", "/System/Applications/Utilities", "#374151", "#111827"],
    ["Visual Studio Code", "com.microsoft.VSCode", "/Applications", "#3b82f6", "#1e3a8a"],
    ["Xcode", "com.apple.dt.Xcode", "/Applications", "#60a5fa", "#1e40af"],
  ];
  const apps = APP_COLOURS.map(([name, bundleId, dir]) => ({
    name,
    bundleId,
    path: `${dir}/${name}.app`,
  }));
  const iconCache = new Map();
  const iconFor = (path) => {
    const entry = APP_COLOURS.find(([name, , dir]) => `${dir}/${name}.app` === path);
    if (!entry) return null;
    if (!iconCache.has(path)) {
      iconCache.set(path, appIcon(entry[0][0], entry[3], entry[4]));
    }
    return { mimeType: "image/png", base64: iconCache.get(path) };
  };

  const vault = [
    { slug: "anthropic-api", name: "Anthropic API key", category: "ai" },
    { slug: "openai-api", name: "OpenAI API key", category: "ai" },
    { slug: "github-deploy", name: "GitHub deploy token", category: "dev" },
    { slug: "postmark-server", name: "Postmark server token", category: "email" },
    { slug: "sentry-dsn", name: "Sentry DSN", category: "monitoring" },
    { slug: "stripe-test", name: "Stripe test secret", category: "payments" },
  ];

  const snapShortcuts = {
    leftHalf: "CommandOrControl+Alt+ArrowLeft",
    rightHalf: "CommandOrControl+Alt+ArrowRight",
    topHalf: "CommandOrControl+Alt+ArrowUp",
    bottomHalf: "CommandOrControl+Alt+ArrowDown",
    topLeft: "CommandOrControl+Control+ArrowLeft",
    topRight: "CommandOrControl+Control+ArrowRight",
    bottomLeft: "CommandOrControl+Control+Shift+ArrowLeft",
    bottomRight: "CommandOrControl+Control+Shift+ArrowRight",
    maximize: "CommandOrControl+Alt+F",
    center: "CommandOrControl+Alt+C",
  };

  let settings = {
    schemaVersion: 1,
    hotkey: "CommandOrControl+Space",
    autostart: true,
    retentionDays: 180,
    denylistedApps: ["com.apple.Passwords", "com.apple.keychainaccess", "com.1password.1password"],
    linkPreviews: true,
    paletteModes: true,
    dockIcon: false,
    snapShortcuts,
    keyvault: { url: null, token: null, privateJwk: null },
  };

  let chatSettings = {
    provider: "anthropic",
    model: "claude-sonnet-4-5",
    // Not a key: a visibly fake string so the window treats chat as set up.
    keys: { zai: "", openai: "", openrouter: "", anthropic: "demo-key-not-real" },
  };

  const CHAT_REPLY = [
    "Here is a release note you can paste as is:",
    "",
    "## Trove 1.8",
    "",
    "- **Windows category** — move the window you just left to a half, a quarter or the centre, straight from the palette.",
    "- **Arrow keys on the home screen** — walk the category tiles and press Enter to open one.",
    "- Snapping always targets the window you were last in, however the palette was opened.",
    "",
    "| Shortcut | Action |",
    "| --- | --- |",
    "| `⌘⌥←` | Left half |",
    "| `⌘⌥F` | Maximize |",
  ].join("\n");

  // ---------------------------------------------------------------- IPC --

  const callbacks = new Map();
  const listeners = [];
  let nextCallback = 1;
  let nextListener = 1;
  let chatSeq = 0;

  const emit = (event, payload) => {
    for (const listener of listeners) {
      if (listener.event !== event) continue;
      const callback = callbacks.get(listener.handler);
      callback?.({ event, id: listener.id, payload });
    }
  };

  const copy = (value) => JSON.parse(JSON.stringify(value));

  const search = ({ query, limit }) => {
    const needle = query.trim().toLocaleLowerCase("pl-PL");
    const fold = (text) =>
      text.toLocaleLowerCase("pl-PL").normalize("NFD").replace(/\p{M}/gu, "").replace(/ł/g, "l");
    const items = needle
      ? history.filter((item) =>
          fold(`${item.preview} ${item.sourceAppName ?? ""}`).includes(fold(needle)),
        )
      : history;
    return { items: copy(items.slice(0, limit)), nextCursor: null, rankedTruncated: false };
  };

  const preview = (eventId) => {
    const item = history.find((candidate) => candidate.eventId === eventId);
    if (!item) throw new Error("history_event_not_found");
    return {
      eventId,
      kind: item.kind,
      mimeType: item.kind === "image" ? "image/png" : item.kind === "html" ? "text/html" : "text/plain",
      text: item.kind === "image" ? null : item.preview,
      byteSize: item.byteSize,
      sourceAppName: item.sourceAppName,
      sourcePath: item.kind === "file" ? item.preview : null,
      sourceExists: item.kind === "file",
    };
  };

  const linkPreview = (eventId) => {
    if (eventId === 1203) {
      return {
        host: "developer.mozilla.org",
        rest: "/en-US/docs/Web/API/Clipboard_API",
        title: "Clipboard API - Web APIs | MDN",
        iconMime: "image/png",
        iconBase64: siteIcon(),
        imageMime: "image/png",
        imageBase64: ogImage(),
        localOnly: false,
        fetching: false,
      };
    }
    if (eventId === 1209) {
      return {
        host: "github.com",
        rest: "/tauri-apps/tauri",
        title: "tauri-apps/tauri: Build smaller, faster, and more secure desktop applications",
        iconMime: null,
        iconBase64: null,
        imageMime: null,
        imageBase64: null,
        localOnly: false,
        fetching: false,
      };
    }
    return null;
  };

  const chatSend = (messages) => {
    const id = `demo-${chatSeq++}`;
    const chunks = CHAT_REPLY.split(/(?<=\n)/u);
    setTimeout(() => emit("chat-delta", { id, part: "reasoning", text: "Summarising the changelog.\n" }), 40);
    chunks.forEach((text, index) =>
      setTimeout(() => emit("chat-delta", { id, part: "answer", text }), 80 + 40 * index),
    );
    setTimeout(() => emit("chat-done", { id }), 120 + 40 * chunks.length);
    void messages;
    return { id };
  };

  const commands = {
    search_history: ({ request }) => search(request),
    get_preview: ({ eventId }) => preview(eventId),
    get_thumbnail: ({ eventId }) =>
      eventId === 1204 ? { mimeType: "image/png", base64: designMock() } : null,
    get_link_preview: ({ eventId }) => linkPreview(eventId),
    set_pinned: ({ eventId, pinned }) => {
      const item = history.find((candidate) => candidate.eventId === eventId);
      if (item) item.pinned = pinned;
    },
    delete_event: () => undefined,
    copy_event: ({ plainText }) => ({ mode: "pasted", plainText }),
    reveal_source: () => undefined,
    open_settings_window: () => undefined,
    get_settings: () => copy(settings),
    save_settings: ({ settings: next }) => {
      settings = copy(next);
      return copy(settings);
    },
    get_storage_stats: () => ({
      contentCount: 18_342,
      eventCount: 24_907,
      databaseBytes: 41_943_040,
      blobBytes: 222_298_112,
    }),
    get_shortcut_status: () => ({
      hotkey: settings.hotkey,
      registered: true,
      heldBySystem: false,
      releasedIds: [],
    }),
    free_summoning_shortcut: () => "alreadyFree",
    restore_system_shortcut: () => "alreadyFree",
    open_keyboard_settings_window: () => undefined,
    list_apps: () => copy(apps),
    get_app_icon: ({ path }) => iconFor(path),
    launch_app: () => undefined,
    open_accessibility_settings_window: () => undefined,
    keyvault_list: () => copy(vault),
    keyvault_copy_secret: () => undefined,
    keyvault_identity: () => ({ paired: true, url: "https://vault.example.com" }),
    keyvault_pair_cancel: () => undefined,
    keyvault_reset_pairing: () => undefined,
    chat_send: ({ messages }) => chatSend(messages),
    chat_stop: () => true,
    chat_list_models: () => ["claude-sonnet-4-5", "claude-haiku-4-5", "claude-opus-4-1"],
    get_chat_settings: () => copy(chatSettings),
    save_chat_settings: ({ settings: next }) => {
      chatSettings = copy(next);
      return copy(chatSettings);
    },
    copy_chat_text: () => undefined,
    open_external_url: () => undefined,
    open_chat_window: () => undefined,
    snap_window: () => true,
    save_generated_file: () => undefined,
    choose_import_file: () => null,
    discard_import_analysis: () => undefined,
    // Tauri's own plugins.
    "plugin:event|listen": ({ event, handler }) => {
      const id = nextListener++;
      listeners.push({ id, event, handler });
      return id;
    },
    "plugin:event|unlisten": ({ eventId }) => {
      const index = listeners.findIndex((listener) => listener.id === eventId);
      if (index >= 0) listeners.splice(index, 1);
    },
    "plugin:autostart|is_enabled": () => settings.autostart,
    "plugin:autostart|enable": () => undefined,
    "plugin:autostart|disable": () => undefined,
  };

  const label = window.location.hash === "#settings" ? "settings" : window.location.hash === "#chat" ? "chat" : "main";

  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => undefined };
  window.__TAURI_INTERNALS__ = {
    metadata: {
      currentWindow: { label },
      currentWebview: { windowLabel: label, label },
    },
    transformCallback: (callback, once) => {
      const id = nextCallback++;
      callbacks.set(id, (payload) => {
        if (once) callbacks.delete(id);
        return callback?.(payload);
      });
      return id;
    },
    unregisterCallback: (id) => callbacks.delete(id),
    convertFileSrc: (path) => path,
    invoke: async (cmd, args = {}) => {
      const handler = commands[cmd];
      if (handler) return handler(args);
      // Window chrome (hide, close, focus) has nothing to act on here.
      if (cmd.startsWith("plugin:window|") || cmd.startsWith("plugin:webview|")) return null;
      console.warn(`[fake-tauri] unhandled command: ${cmd}`);
      return null;
    },
  };
})();
