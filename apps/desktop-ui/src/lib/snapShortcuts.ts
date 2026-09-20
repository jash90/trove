/**
 * The snap positions the keyboard can drive, one row per chord.
 *
 * The list mirrors `snap::SNAP_DEFAULTS` on the Rust side — the two have to
 * agree, and the pairing is checked by tests on both sides rather than by
 * anything at runtime. Rectangle's own defaults (inherited from Spectacle),
 * so a hand trained on either reaches for keys that already work.
 */
export interface SnapShortcut {
  id: string;
  label: string;
  chord: string;
}

export const SNAP_SHORTCUTS: readonly SnapShortcut[] = [
  { id: 'leftHalf', label: 'Left half', chord: 'CommandOrControl+Alt+ArrowLeft' },
  { id: 'rightHalf', label: 'Right half', chord: 'CommandOrControl+Alt+ArrowRight' },
  { id: 'topHalf', label: 'Top half', chord: 'CommandOrControl+Alt+ArrowUp' },
  { id: 'bottomHalf', label: 'Bottom half', chord: 'CommandOrControl+Alt+ArrowDown' },
  { id: 'topLeft', label: 'Top left', chord: 'CommandOrControl+Control+ArrowLeft' },
  { id: 'topRight', label: 'Top right', chord: 'CommandOrControl+Control+ArrowRight' },
  { id: 'bottomLeft', label: 'Bottom left', chord: 'CommandOrControl+Control+Shift+ArrowLeft' },
  { id: 'bottomRight', label: 'Bottom right', chord: 'CommandOrControl+Control+Shift+ArrowRight' },
  { id: 'maximize', label: 'Maximize', chord: 'CommandOrControl+Alt+F' },
  { id: 'center', label: 'Center', chord: 'CommandOrControl+Alt+C' },
];

/** The map a fresh install snaps with. */
export const defaultSnapShortcuts = (): Record<string, string> =>
  Object.fromEntries(SNAP_SHORTCUTS.map(({ id, chord }) => [id, chord]));
