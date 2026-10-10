import { Archive, CheckCircle2, CircleAlert, CircleSlash } from 'lucide-react';

import { t } from '../i18n';
import { formatCount } from '../lib/format';
import type { ImportAnalysis as ImportAnalysisContract } from '../lib/contracts';

interface ImportAnalysisProps {
  analysis: ImportAnalysisContract;
}

export const ImportAnalysis = ({ analysis }: ImportAnalysisProps): React.JSX.Element => (
  <section className="import-analysis" aria-labelledby="import-analysis-title">
    <div className="workflow-state-icon" aria-hidden="true">
      <Archive size={25} strokeWidth={1.7} />
    </div>
    <div>
      <span className="workflow-kicker">{t('import.analysis.kicker')}</span>
      <h2 id="import-analysis-title">{t('import.analysis.title')}</h2>
      <p className="workflow-count">{t('import.records', { count: analysis.total, n: formatCount(analysis.total) })}</p>
    </div>
    <dl className="count-ledger count-ledger--three">
      <div>
        <dt>
          <CheckCircle2 size={14} aria-hidden="true" /> {t('import.analysis.ready')}
        </dt>
        <dd>{formatCount(analysis.candidateRecords)}</dd>
      </div>
      <div>
        <dt>
          <CircleSlash size={14} aria-hidden="true" /> {t('import.analysis.noSource')}
        </dt>
        <dd>{formatCount(analysis.skipped)}</dd>
      </div>
      <div>
        <dt>
          <CircleAlert size={14} aria-hidden="true" /> {t('import.analysis.rejected')}
        </dt>
        <dd>{formatCount(analysis.failed)}</dd>
      </div>
    </dl>
  </section>
);
