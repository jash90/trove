import { ClipboardX, LoaderCircle, TriangleAlert } from 'lucide-react';

type EmptyStateKind = 'loading' | 'empty' | 'error';

interface EmptyStateProps {
  kind: EmptyStateKind;
}

const STATE_COPY: Record<EmptyStateKind, { title: string; detail: string }> = {
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
};

export const EmptyState = ({ kind }: EmptyStateProps): React.JSX.Element => {
  const copy = STATE_COPY[kind];
  const Icon = kind === 'loading' ? LoaderCircle : kind === 'error' ? TriangleAlert : ClipboardX;

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
