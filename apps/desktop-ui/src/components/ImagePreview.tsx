import { Image as ImageIcon, LoaderCircle } from 'lucide-react';

import { useT } from '../i18n';

export type ThumbnailStatus = 'idle' | 'loading' | 'ready' | 'unavailable' | 'error';

interface ImagePreviewProps {
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
}

export const ImagePreview = ({
  thumbnailUrl,
  thumbnailStatus,
}: ImagePreviewProps): React.JSX.Element => {
  const t = useT();
  if (thumbnailStatus === 'loading') {
    return (
      <div className="image-preview__state" role="status">
        <LoaderCircle className="image-preview__spinner" size={25} aria-hidden="true" />
        <span>{t('image.loading')}</span>
      </div>
    );
  }

  if (thumbnailStatus === 'ready' && thumbnailUrl) {
    return (
      <div className="image-preview">
        <img src={thumbnailUrl} alt={t('image.alt')} />
      </div>
    );
  }

  return (
    <div className="image-preview__state">
      <ImageIcon size={27} aria-hidden="true" />
      <strong>{t('image.unavailable.title')}</strong>
      <span>{t('image.unavailable.detail')}</span>
    </div>
  );
};
