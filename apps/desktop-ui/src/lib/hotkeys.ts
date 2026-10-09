// The grammar of a global shortcut: what a chord may be, and how a key press
// becomes one.
//
// Shared by the settings form and the palette. It lived in the settings form,
// which meant the palette imported the whole form to record a snap chord, and
// with it the settings window's code landed in the bundle the palette has to
// parse before it can draw.

/**
 * One canonical command-family primary at most: CommandOrControl and Command
 * resolve to the same physical key on a given platform, so accepting both
 * would register a shortcut the user cannot press. Control is a different
 * key entirely and may ride along beside a command-family primary — ⌘⌃ is
 * the chord Rectangle's corner snaps live on.
 */
const PRIMARY_MODIFIERS = new Map([
  ["commandorcontrol", "CommandOrControl"],
  ["command", "Command"],
  ["control", "Control"],
]);
const SECONDARY_MODIFIERS = new Map([
  ["alt", "Alt"],
  ["shift", "Shift"],
]);
const HOTKEY_KEY_PATTERN =
  /^(?:[A-Z0-9]|SPACE|ARROW(?:LEFT|RIGHT|UP|DOWN)|F(?:[1-9]|1\d|2[0-4]))$/u;
/** Keys the platform syntax spells out rather than showing as a character. */
const NAMED_KEYS: Record<string, string> = {
  SPACE: "Space",
  ARROWLEFT: "ArrowLeft",
  ARROWRIGHT: "ArrowRight",
  ARROWUP: "ArrowUp",
  ARROWDOWN: "ArrowDown",
};

export const normalizePlatformHotkey = (value: string): string => {
  const invalid = (): never => {
    throw new Error("invalid_hotkey");
  };
  const parts = value.split("+").map((part) => part.trim());
  if (parts.length < 2 || parts.some((part) => part.length === 0)) invalid();

  const upper = parts.at(-1)!.toUpperCase();
  if (!HOTKEY_KEY_PATTERN.test(upper)) invalid();
  // Named keys are spelled in title case by the platform shortcut syntax;
  // single characters and function keys stay uppercase.
  const key = NAMED_KEYS[upper] ?? upper;

  let primary: string | null = null;
  let controlHeld = false;
  const secondary = new Set<string>();
  for (const part of parts.slice(0, -1)) {
    const token = part.toLowerCase();
    const asPrimary = PRIMARY_MODIFIERS.get(token);
    if (asPrimary !== undefined) {
      // Control beside a command-family token is an extra modifier (⌘⌃),
      // not a second primary; Control twice, or two command-family tokens,
      // are one key held twice and remain nonsense.
      if (token === "control") {
        if (controlHeld) invalid();
        controlHeld = true;
        continue;
      }
      if (primary !== null) invalid();
      primary = asPrimary;
      continue;
    }
    const asSecondary = SECONDARY_MODIFIERS.get(token);
    if (asSecondary === undefined) return invalid();
    if (secondary.has(asSecondary)) return invalid();
    secondary.add(asSecondary);
  }
  // Control standing alone is itself a primary; beside a command-family one
  // it stays the extra modifier it was recorded as.
  if (primary === null && controlHeld) {
    primary = "Control";
    controlHeld = false;
  }
  // Alt on its own is enough — ⌥Space is an ordinary launcher shortcut, and the systems people
  // compare this against bind exactly that. Shift on its own is not: Shift+A is how a capital A
  // is typed, so a global binding on it would swallow ordinary typing everywhere.
  if (primary === null && !secondary.has("Alt")) invalid();

  return [
    ...(primary === null ? [] : [primary]),
    ...(secondary.has("Alt") ? ["Alt"] : []),
    ...(secondary.has("Shift") ? ["Shift"] : []),
    ...(controlHeld ? ["Control"] : []),
    key,
  ].join("+");
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
 * each stand on their own; ⌥Space is an ordinary launcher shortcut; ⌘⌃ together are two keys
 * and record as one chord.
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
    if (event.code === "Space") return "SPACE";
    if (/^F(?:[1-9]|1\d|2[0-4])$/u.test(event.code)) return event.code;
    if (/^Arrow(?:Left|Right|Up|Down)$/u.test(event.code))
      return event.code.toUpperCase();
    return null;
  })();
  if (key === null) return null;

  // ⌘⌃ are two different keys and a chord worth recording — Rectangle's
  // corner snaps live on it — so Control rides along beside a command
  // primary rather than being rejected as a second one.
  const primary = event.metaKey
    ? "CommandOrControl"
    : event.ctrlKey
      ? "Control"
      : null;
  if (primary === null && !event.altKey) return null;

  const parts = primary === null ? [] : [primary];
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.ctrlKey && event.metaKey) parts.push("Control");
  parts.push(key);
  try {
    return normalizePlatformHotkey(parts.join("+"));
  } catch {
    return null;
  }
};
