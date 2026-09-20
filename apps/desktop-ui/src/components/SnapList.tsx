import { SNAP_SHORTCUTS } from "../lib/snapShortcuts";

interface SnapListProps {
  /** The chords as configured, one per action id — settings, not defaults. */
  chords: Record<string, string>;
  selectedKey: string | null;
  onSelect: (id: string) => void;
  onActivate: (id: string) => void;
}

/**
 * The Windows category: one row per snap position, arranged like the tiles
 * it answers to, wearing the chords the settings currently hold.
 *
 * The keyboard lives in the search field and moves through here as a
 * listbox; the rows are real buttons so the mouse works too, exactly like
 * every other row in the palette.
 */
export const SnapList = ({
  chords,
  selectedKey,
  onSelect,
  onActivate,
}: SnapListProps): React.JSX.Element => (
  <div
    className="palette-categories"
    role="listbox"
    aria-label="Window arrangements"
    id="snap-results"
  >
    {SNAP_SHORTCUTS.map(({ id, label }, index) => (
      <button
        key={id}
        type="button"
        role="option"
        id={`snap-option-${index}`}
        aria-selected={id === selectedKey}
        className="palette-category"
        data-selected={id === selectedKey ? "true" : undefined}
        onClick={() => {
          onSelect(id);
          onActivate(id);
        }}
      >
        <span className="palette-category__body">
          <span className="palette-category__title">{label}</span>
          <span className="palette-category__detail">
            Moves the window you were working in.
          </span>
        </span>
        <kbd className="palette-category__key">{chords[id] ?? ""}</kbd>
      </button>
    ))}
  </div>
);
