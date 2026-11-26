import { BadgeCheck } from 'lucide-react';

import {
  validateImportSummary,
  type ImportSummary as ImportSummaryContract,
} from '../lib/contracts';
import { useT } from '../i18n';
import { formatCount } from '../lib/format';

interface ImportSummaryProps {
  summary: ImportSummaryContract;
}

export const ImportSummary = ({ summary }: ImportSummaryProps): React.JSX.Element => {
  const t = useT();
  const validSummary = validateImportSummary(summary);

  return (
    <section className="import-summary" aria-labelledby="import-summary-title">
      <div className="workflow-state-icon workflow-state-icon--success" aria-hidden="true">
        <BadgeCheck size={27} strokeWidth={1.7} />
      </div>
      <span className="workflow-kicker">{t('import.summary.kicker')}</span>
      <h2 id="import-summary-title">{t('import.summary.title')}</h2>
      <p className="workflow-count">{t('import.records', { count: validSummary.total, n: formatCount(validSummary.total) })}</p>
      <dl className="count-ledger count-ledger--four">
        <div><dt>{t('import.ledger.imported')}</dt><dd>{formatCount(validSummary.imported)}</dd></div>
        <div><dt>{t('import.ledger.alreadyPresent')}</dt><dd>{formatCount(validSummary.alreadyPresent)}</dd></div>
        <div><dt>{t('import.ledger.skipped')}</dt><dd>{formatCount(validSummary.skipped)}</dd></div>
        <div><dt>{t('import.ledger.failed')}</dt><dd>{formatCount(validSummary.failed)}</dd></div>
      </dl>
    </section>
  );
};
