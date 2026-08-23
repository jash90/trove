import { BadgeCheck } from 'lucide-react';

import {
  validateImportSummary,
  type ImportSummary as ImportSummaryContract,
} from '../lib/contracts';
import { formatCount } from '../lib/format';

interface ImportSummaryProps {
  summary: ImportSummaryContract;
}

export const ImportSummary = ({ summary }: ImportSummaryProps): React.JSX.Element => {
  const validSummary = validateImportSummary(summary);

  return (
    <section className="import-summary" aria-labelledby="import-summary-title">
      <div className="workflow-state-icon workflow-state-icon--success" aria-hidden="true">
        <BadgeCheck size={27} strokeWidth={1.7} />
      </div>
      <span className="workflow-kicker">Every record accounted for</span>
      <h2 id="import-summary-title">Import complete</h2>
      <p className="workflow-count">{formatCount(validSummary.total)} records</p>
      <dl className="count-ledger count-ledger--four">
        <div><dt>Zaimportowane</dt><dd>{formatCount(validSummary.imported)}</dd></div>
        <div><dt>Already present</dt><dd>{formatCount(validSummary.alreadyPresent)}</dd></div>
        <div><dt>Skipped</dt><dd>{formatCount(validSummary.skipped)}</dd></div>
        <div><dt>Failed</dt><dd>{formatCount(validSummary.failed)}</dd></div>
      </dl>
    </section>
  );
};
