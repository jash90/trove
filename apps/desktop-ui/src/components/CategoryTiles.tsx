import { AppWindowMac, ClipboardList, MessageSquareText, Vault } from 'lucide-react';

import {
  PALETTE_CATEGORIES,
  PALETTE_CHAT_CATEGORY,
  type PaletteMode,
} from './PaletteHeader';

interface CategoryTilesProps {
  onPick: (mode: PaletteMode) => void;
  /** The chat tile opens a window rather than a list of the palette's own. */
  onOpenChat: () => void;
}

const TILE_META: Record<PaletteMode, { icon: typeof Vault; detail: string }> = {
  apps: {
    icon: AppWindowMac,
    detail: 'The whole installed catalog, searched as you type.',
  },
  history: {
    icon: ClipboardList,
    detail: 'Everything this machine has copied, newest first.',
  },
  vault: {
    icon: Vault,
    detail: 'The keys your paired vault holds, by name.',
  },
};

/// The home view: the palette's categories, flying out as they do.
///
/// The palette opens here rather than in any one list, because which list
/// someone came for is a fact about them, not about the application. Each
/// tile is a real button — the mouse works — and the digits on the tiles
/// are the keys the field answers while home is showing: 1, 2, 3, in the
/// order the tiles sit in.
export const CategoryTiles = ({
  onPick,
  onOpenChat,
}: CategoryTilesProps): React.JSX.Element => (
  <div className="palette-categories" role="group" aria-label="Palette categories">
    {PALETTE_CATEGORIES.map(({ mode, key, label }) => {
      const { icon: Icon, detail } = TILE_META[mode];
      return (
        <button
          key={mode}
          type="button"
          className="palette-category"
          onClick={() => onPick(mode)}
        >
          <span className="palette-category__icon" aria-hidden="true">
            <Icon size={20} strokeWidth={1.8} />
          </span>
          <span className="palette-category__body">
            <span className="palette-category__title">{label}</span>
            <span className="palette-category__detail">{detail}</span>
          </span>
          <kbd className="palette-category__key">{key}</kbd>
        </button>
      );
    })}
    <button
      type="button"
      className="palette-category"
      onClick={onOpenChat}
    >
      <span className="palette-category__icon" aria-hidden="true">
        <MessageSquareText size={20} strokeWidth={1.8} />
      </span>
      <span className="palette-category__body">
        <span className="palette-category__title">{PALETTE_CHAT_CATEGORY.label}</span>
        <span className="palette-category__detail">
          Talk to a model, in a window of its own.
        </span>
      </span>
      <kbd className="palette-category__key">{PALETTE_CHAT_CATEGORY.key}</kbd>
    </button>
  </div>
);
