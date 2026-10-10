import { PanelRightOpen } from "lucide-react";

import { t } from "../i18n";

import type {
  HistoryItem,
  LinkPreview as LinkPreviewContract,
  Preview,
} from "../lib/contracts";
import type { PaletteItem } from "../lib/paletteItems";
import type { PaletteView } from "./PaletteHeader";
import type { PreviewStatus } from "./PreviewPane";
import type { ThumbnailStatus } from "./ImagePreview";
import { CategoryTiles } from "./CategoryTiles";
import { EmptyState } from "./EmptyState";
import { PaletteList } from "./PaletteList";
import { PreviewPane } from "./PreviewPane";
import { SnapList } from "./SnapList";

interface PaletteWorkspaceProps {
  /** What the palette is showing — the home picker and the empty states follow it. */
  mode: PaletteView;
  /** Entering one of the categories from the home tiles. */
  onPickCategory: (mode: import("./PaletteHeader").PaletteMode) => void;
  /** The chat tile opens a window of its own. */
  onOpenChat: () => void;
  /** The home tile the arrows have landed on, or null while none is marked. */
  homeTileIndex: number | null;
  /** The snap chords as configured, one per action id. */
  snapChords: Record<string, string>;
  /** The Windows category's own keyboard selection. */
  snapSelectedKey: string | null;
  onSnapSelect: (id: string) => void;
  onSnapActivate: (id: string) => void;
  appsStatus: "loading" | "ready" | "error";
  status: "loading" | "ready" | "error";
  /** The single result list: applications and history, already ordered. */
  items: PaletteItem[];
  /** The shared keyboard selection, whatever kind of row it sits on. */
  selectedKey: string | null;
  /** Set when a launch was refused; cleared by the next attempt or selection. */
  launchError: string | null;
  preview: Preview | null;
  previewStatus: PreviewStatus;
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
  linkPreview: LinkPreviewContract | null;
  selectedItem: HistoryItem | null;
  mobilePreviewOpen: boolean;
  actions: React.ReactNode;
  onSelect: (entry: PaletteItem) => void;
  onActivate: (entry: PaletteItem) => void;
  onOpenPreview: () => void;
  onClosePreview: () => void;
  onRevealSource: () => void;
}

/// The palette's middle: one list of applications and clipboard history,
/// the preview column beside it.
///
/// One field drives the list and one keyboard selection moves through it.
/// Rows already on screen win over state changes — swapping the list for a
/// loading state would both flash and leave Enter steering at rows nobody
/// can see — so the states speak only when there is nothing to show.
export const PaletteWorkspace = ({
  mode,
  onPickCategory,
  onOpenChat,
  homeTileIndex,
  snapChords,
  snapSelectedKey,
  onSnapSelect,
  onSnapActivate,
  appsStatus,
  status,
  items,
  selectedKey,
  launchError,
  preview,
  previewStatus,
  thumbnailUrl,
  thumbnailStatus,
  linkPreview,
  selectedItem,
  mobilePreviewOpen,
  actions,
  onSelect,
  onActivate,
  onOpenPreview,
  onClosePreview,
  onRevealSource,
}: PaletteWorkspaceProps): React.JSX.Element => (
  <div className="palette-content">
    <div className="history-column">
      {/* Rendered whatever the list holds. Appearing and disappearing with the
          results moved the list's top edge on every keystroke at narrow
          widths, which is its own kind of jumping. */}
      <button
        type="button"
        className="preview-toggle"
        aria-label={t("workspace.showPreview")}
        disabled={items.length === 0 || mode === "windows"}
        onClick={onOpenPreview}
      >
        <PanelRightOpen size={15} aria-hidden="true" />
        {t("workspace.preview")}
      </button>
      {launchError ? (
        <p className="apps-launch-error" role="alert">
          {launchError}
        </p>
      ) : null}
      <div className="history-panel">
        {mode === "home" ? (
          <CategoryTiles
            onPick={onPickCategory}
            onOpenChat={onOpenChat}
            selectedIndex={homeTileIndex}
          />
        ) : null}
        {/* The Windows category is a list of its own, wearing the
              chords the settings currently hold. */}
        {mode === "windows" ? (
          <SnapList
            chords={snapChords}
            selectedKey={snapSelectedKey}
            onSelect={onSnapSelect}
            onActivate={onSnapActivate}
          />
        ) : null}
        {/* The states speak only when there is nothing to show, and about
            the side the view is showing: with modes on, the history's
            loading state is silent while applications are on screen and an
            applications problem is invisible while history is — a status
            nobody asked about is noise. Combined, both sides owe their
            state to the one panel, as they always did. */}
        {mode === "vault" ? (
          <>
            {/* The vault was asked for by entering the category, so an
                empty answer is worth saying rather than a silent panel.
                The list hook reports silence for every failure — an
                unpaired install included — and "no keys" is the honest
                sentence for all of them. */}
            {items.length === 0 ? (
              <EmptyState kind="empty" subject="vault" />
            ) : null}
          </>
        ) : mode === "apps" ? (
          <>
            {appsStatus === "error" && items.length === 0 ? (
              <EmptyState kind="error" subject="applications" />
            ) : null}
            {appsStatus === "loading" && items.length === 0 ? (
              <EmptyState kind="loading" subject="applications" />
            ) : null}
            {appsStatus === "ready" && items.length === 0 ? (
              <EmptyState kind="empty" subject="applications" />
            ) : null}
          </>
        ) : mode === "history" ? (
          <>
            {status === "error" ? <EmptyState kind="error" /> : null}
            {status === "loading" && items.length === 0 ? (
              <EmptyState kind="loading" />
            ) : null}
            {status === "ready" && items.length === 0 ? (
              <EmptyState kind="empty" />
            ) : null}
          </>
        ) : mode === "all" ? (
          <>
            {status === "error" && appsStatus !== "ready" ? (
              <EmptyState kind="error" />
            ) : null}
            {status === "error" &&
            appsStatus === "ready" &&
            items.length === 0 ? (
              <EmptyState kind="error" />
            ) : null}
            {status === "loading" && items.length === 0 ? (
              <EmptyState kind="loading" />
            ) : null}
            {status === "ready" &&
            appsStatus === "ready" &&
            items.length === 0 ? (
              <EmptyState kind="empty" />
            ) : null}
            {status === "ready" &&
            appsStatus === "loading" &&
            items.length === 0 ? (
              <EmptyState kind="loading" subject="applications" />
            ) : null}
            {status === "ready" &&
            appsStatus === "error" &&
            items.length === 0 ? (
              <EmptyState kind="error" subject="applications" />
            ) : null}
          </>
        ) : null}
        {items.length > 0 && mode !== "windows" ? (
          <PaletteList
            items={items}
            selectedKey={selectedKey}
            onSelect={onSelect}
            onActivate={onActivate}
          />
        ) : null}
      </div>
    </div>
    <div
      className={`preview-column${mobilePreviewOpen ? " is-mobile-open" : ""}`}
    >
      <PreviewPane
        preview={preview}
        status={previewStatus}
        thumbnailUrl={thumbnailUrl}
        thumbnailStatus={thumbnailStatus}
        linkPreview={linkPreview}
        selectedItem={selectedItem}
        onClose={onClosePreview}
        onRevealSource={onRevealSource}
        actions={actions}
      />
    </div>
  </div>
);
