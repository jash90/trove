import { AppWindowMac, ClipboardX, LoaderCircle, TriangleAlert } from 'lucide-react';

import { useT } from '../i18n';

type EmptyStateKind = 'loading' | 'empty' | 'error';
type EmptyStateSubject = 'history' | 'applications' | 'vault';

interface EmptyStateProps {
  kind: EmptyStateKind;
  /** Which list went quiet; the copy and the icon follow it. */
  subject?: EmptyStateSubject;
  /**
   * Drops the live-region semantics. Two sections share the palette now,
   * and two live regions announcing at once is noise, not information:
   * the applications section's placeholder states are visible but silent,
   * leaving the announcements to whichever list the user is reading.
   */
  quiet?: boolean;
}

export const EmptyState = ({
  kind,
  subject = 'history',
  quiet = false,
}: EmptyStateProps): React.JSX.Element => {
  const t = useT();
  const copy = {
    title: t(`empty.${subject}.${kind}.title`),
    detail: t(`empty.${subject}.${kind}.detail`),
  };
  const Icon =
    kind === 'loading'
      ? LoaderCircle
      : kind === 'error'
        ? TriangleAlert
        : subject === 'applications'
          ? AppWindowMac
          : ClipboardX;

  return (
    <section
      className={`empty-state empty-state--${kind}`}
      role={quiet ? undefined : kind === 'error' ? 'alert' : 'status'}
      aria-live={quiet ? undefined : kind === 'error' ? 'assertive' : 'polite'}
    >
      <span className="empty-state__icon" aria-hidden="true">
        <Icon size={24} strokeWidth={1.6} />
      </span>
      <div>
        <h2>{copy.title}</h2>
        <p>{copy.detail}</p>
      </div>
    </section>
  );
};
