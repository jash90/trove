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
          : 'Importowanie archiwum…'}
      </h2>
      <div
        className="progress-track"
        role="progressbar"
        aria-label="Postęp importu"
        aria-valuemin={0}
        aria-valuemax={progress.total}
        aria-valuenow={progress.processed}
        aria-valuetext={`${percentage}% · ${formatCount(progress.processed)} z ${formatCount(progress.total)} rekordów`}
      >
        <span style={{ width: `${percentage}%` }} />
      </div>
      <p className="progress-count" aria-live="polite">
        {formatCount(progress.processed)} z {formatCount(progress.total)} rekordów
      </p>
      <dl className="count-ledger count-ledger--four">
        <div><dt>Nowe</dt><dd>{formatCount(progress.imported)}</dd></div>
        <div><dt>Już obecne</dt><dd>{formatCount(progress.alreadyPresent)}</dd></div>
        <div><dt>Pominięte</dt><dd>{formatCount(progress.skipped)}</dd></div>
        <div><dt>Błędy</dt><dd>{formatCount(progress.failed)}</dd></div>
      </dl>
    </section>
  );
};
