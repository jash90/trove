import { LoaderCircle } from 'lucide-react';

import { useT } from '../i18n';
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
  const t = useT();
  const percentage = progress.total === 0
    ? 0
    : Math.min(100, Math.round((progress.processed / progress.total) * 100));

  const processedOfTotal = t('import.progress.count', {
    count: progress.total,
    processed: formatCount(progress.processed),
    total: formatCount(progress.total),
  });

  return (
    <section className="import-progress" aria-labelledby="import-progress-title">
      <LoaderCircle className="workflow-spinner" size={28} aria-hidden="true" />
      <span className="workflow-kicker">
        {phase === 'recovering' ? t('import.progress.recoveringKicker') : t('import.kicker')}
      </span>
      <h2 id="import-progress-title">
        {phase === 'recovering'
          ? t('import.progress.recovering')
          : t('import.progress.running')}
      </h2>
      <div
        className="progress-track"
        role="progressbar"
        aria-label={t('import.progress.label')}
        aria-valuemin={0}
        aria-valuemax={progress.total}
        aria-valuenow={progress.processed}
        aria-valuetext={`${percentage}% · ${processedOfTotal}`}
      >
        <span style={{ width: `${percentage}%` }} />
      </div>
      <p className="progress-count" aria-live="polite">
        {processedOfTotal}
      </p>
      <dl className="count-ledger count-ledger--four">
        <div><dt>{t('import.ledger.new')}</dt><dd>{formatCount(progress.imported)}</dd></div>
        <div><dt>{t('import.ledger.alreadyPresent')}</dt><dd>{formatCount(progress.alreadyPresent)}</dd></div>
        <div><dt>{t('import.ledger.skipped')}</dt><dd>{formatCount(progress.skipped)}</dd></div>
        <div><dt>{t('import.ledger.failed')}</dt><dd>{formatCount(progress.failed)}</dd></div>
      </dl>
    </section>
  );
};
