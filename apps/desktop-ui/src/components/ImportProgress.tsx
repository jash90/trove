import { LoaderCircle } from 'lucide-react';

import { formatCount } from '../lib/format';
import type { ImportProgress as ImportProgressContract } from '../lib/contracts';

interface ImportProgressProps {
  progress: ImportProgressContract;
  phase?: 'recovering' | 'running';
}

export const ImportProgress = ({
  progress,
  phase = 'running',
}: ImportProgressProps): React.JSX.Element => {
  const percentage = progress.total === 0
    ? 0
    : Math.min(100, Math.round((progress.processed / progress.total) * 100));

  return (
    <section className="import-progress" aria-labelledby="import-progress-title">
      <LoaderCircle className="workflow-spinner" size={28} aria-hidden="true" />
      <span className="workflow-kicker">
        {phase === 'recovering' ? 'Odzyskiwanie przebiegu' : 'Import lokalny'}
      </span>
      <h2 id="import-progress-title">
        {phase === 'recovering'
          ? 'Odzyskiwanie uruchomionego importu…'
          : 'Importing the archive…'}
      </h2>
      <div
        className="progress-track"
        role="progressbar"
        aria-label="Import progress"
        aria-valuemin={0}
        aria-valuemax={progress.total}
        aria-valuenow={progress.processed}
        aria-valuetext={`${percentage}% · ${formatCount(progress.processed)} of ${formatCount(progress.total)} records`}
      >
        <span style={{ width: `${percentage}%` }} />
      </div>
      <p className="progress-count" aria-live="polite">
        {formatCount(progress.processed)} of {formatCount(progress.total)} records
      </p>
      <dl className="count-ledger count-ledger--four">
        <div><dt>Nowe</dt><dd>{formatCount(progress.imported)}</dd></div>
        <div><dt>Already present</dt><dd>{formatCount(progress.alreadyPresent)}</dd></div>
        <div><dt>Skipped</dt><dd>{formatCount(progress.skipped)}</dd></div>
        <div><dt>Failed</dt><dd>{formatCount(progress.failed)}</dd></div>
      </dl>
    </section>
  );
};
