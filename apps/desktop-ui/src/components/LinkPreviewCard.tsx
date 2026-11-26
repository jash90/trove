import { Globe, LoaderCircle, ShieldOff } from 'lucide-react';

import { t } from '../i18n';
import type { LinkPreview } from '../lib/contracts';

interface LinkPreviewCardProps {
  preview: LinkPreview;
}

/// What a link is, shown as an address rather than a line of text.
///
/// The title and icon are what the page said about itself, fetched once and
/// remembered. Without them — because fetching is off, or the page did not
/// answer — the address alone still reads as a place rather than a string.
export const LinkPreviewCard = ({ preview }: LinkPreviewCardProps): React.JSX.Element => (
  <div className="link-card">
    {preview.imageBase64 && preview.imageMime ? (
      // The page's own card, fetched by the core and handed over as bytes.
      // Never a remote address: the window itself reaches out for nothing.
      <img
        className="link-card__image"
        src={`data:${preview.imageMime};base64,${preview.imageBase64}`}
        alt=""
      />
    ) : preview.fetching ? (
      // Only while the core says an answer is still coming. A page that has no
      // picture of its own is the common case, and it must not leave something
      // turning here forever.
      <div className="link-card__image link-card__image--pending" role="status">
        <LoaderCircle className="image-preview__spinner" size={20} aria-hidden="true" />
        <span>{t('preview.loading')}</span>
      </div>
    ) : null}
    <div className="link-card__identity">
      {preview.iconBase64 && preview.iconMime ? (
        // A local blob, never a remote address: the page's icon is fetched by
        // the core and handed over as bytes, so the window itself never
        // reaches out.
        <img
          className="link-card__icon"
          src={`data:${preview.iconMime};base64,${preview.iconBase64}`}
          alt=""
        />
      ) : (
        <span className="link-card__icon link-card__icon--generic" aria-hidden="true">
          <Globe size={17} strokeWidth={1.8} />
        </span>
      )}
      <div className="link-card__names">
        {preview.title ? <strong className="link-card__title">{preview.title}</strong> : null}
        <span className="link-card__host">{preview.host}</span>
      </div>
    </div>
    {preview.rest && preview.rest !== '/' ? (
      <p className="link-card__path">{preview.rest}</p>
    ) : null}
    {preview.localOnly ? (
      <p className="link-card__note">
        <ShieldOff size={13} aria-hidden="true" />
        {t('link.localOnly')}
      </p>
    ) : null}
  </div>
);
