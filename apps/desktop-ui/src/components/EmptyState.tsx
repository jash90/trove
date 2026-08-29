import { AppWindowMac, ClipboardX, LoaderCircle, TriangleAlert } from 'lucide-react';

type EmptyStateKind = 'loading' | 'empty' | 'error';
type EmptyStateSubject = 'history' | 'applications';

interface EmptyStateProps {
  kind: EmptyStateKind;
  /** Which list went quiet; the copy and the icon follow it. */
  subject?: EmptyStateSubject;
}

const STATE_COPY: Record<EmptyStateSubject, Record<EmptyStateKind, { title: string; detail: string }>> = {
  history: {
    loading: {
      title: 'Loading history…',
      detail: 'Sorting the most recent entries.',
    },
    empty: {
      title: 'The history is empty',
      detail: 'Copied items appear here automatically.',
    },
    error: {
      title: 'The history could not be loaded',
      detail: 'Try again in a moment.',
    },
  },
  applications: {
    loading: {
      title: 'Loading applications…',
      detail: 'Reading the installed bundles.',
    },
    empty: {
      title: 'No application matched',
      detail: 'Type less, or press Tab to return to the history.',
    },
    error: {
      title: 'The applications could not be loaded',
      detail: 'Try again in a moment.',
    },
  },
};

export const EmptyState = ({
  kind,
  subject = 'history',
}: EmptyStateProps): React.JSX.Element => {
  const copy = STATE_COPY[subject][kind];
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
      role={kind === 'error' ? 'alert' : 'status'}
      aria-live={kind === 'error' ? 'assertive' : 'polite'}
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
