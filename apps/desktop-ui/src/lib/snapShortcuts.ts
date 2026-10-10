import { t } from "../i18n";

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
 readonly label: string;
 chord: string;
}

export const SNAP_SHORTCUTS: readonly SnapShortcut[] = [
 {
  id: "leftHalf",
  get label() {
   return t("snap.leftHalf");
  },
  chord: "CommandOrControl+Alt+ArrowLeft",
 },
 {
  id: "rightHalf",
  get label() {
   return t("snap.rightHalf");
  },
  chord: "CommandOrControl+Alt+ArrowRight",
 },
 {
  id: "topHalf",
  get label() {
   return t("snap.topHalf");
  },
  chord: "CommandOrControl+Alt+ArrowUp",
 },
 {
  id: "bottomHalf",
  get label() {
   return t("snap.bottomHalf");
  },
  chord: "CommandOrControl+Alt+ArrowDown",
 },
 {
  id: "topLeft",
  get label() {
   return t("snap.topLeft");
  },
  chord: "CommandOrControl+Control+ArrowLeft",
 },
 {
  id: "topRight",
  get label() {
   return t("snap.topRight");
  },
  chord: "CommandOrControl+Control+ArrowRight",
 },
 {
  id: "bottomLeft",
  get label() {
   return t("snap.bottomLeft");
  },
  chord: "CommandOrControl+Control+Shift+ArrowLeft",
 },
 {
  id: "bottomRight",
  get label() {
   return t("snap.bottomRight");
  },
  chord: "CommandOrControl+Control+Shift+ArrowRight",
 },
 {
  id: "maximize",
  get label() {
   return t("snap.maximize");
  },
  chord: "CommandOrControl+Alt+F",
 },
 {
  id: "center",
  get label() {
   return t("snap.center");
  },
  chord: "CommandOrControl+Alt+C",
 },
];

/** The map a fresh install snaps with. */
export const defaultSnapShortcuts = (): Record<string, string> =>
 Object.fromEntries(SNAP_SHORTCUTS.map(({ id, chord }) => [id, chord]));
