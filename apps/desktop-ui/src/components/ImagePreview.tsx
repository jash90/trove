import { Image as ImageIcon, LoaderCircle } from 'lucide-react';

export type ThumbnailStatus = 'idle' | 'loading' | 'ready' | 'unavailable' | 'error';

interface ImagePreviewProps {
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
}

export const ImagePreview = ({
  thumbnailUrl,
  thumbnailStatus,
}: ImagePreviewProps): React.JSX.Element => {
  if (thumbnailStatus === 'loading') {
    return (
      <div className="image-preview__state" role="status">
        <LoaderCircle className="image-preview__spinner" size={25} aria-hidden="true" />
        <span>Wczytywanie miniatury…</span>
      </div>
    );
  }

  if (thumbnailStatus === 'ready' && thumbnailUrl) {
    return (
      <div className="image-preview">
        <img src={thumbnailUrl} alt="Clipboard image preview" />
      </div>
    );
  }

  return (
    <div className="image-preview__state">
      <ImageIcon size={27} aria-hidden="true" />
      <strong>The thumbnail is unavailable</strong>
      <span>You can still copy the entry if its source exists.</span>
    </div>
  );
};
