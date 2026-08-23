import { Archive, CheckCircle2, CircleAlert } from 'lucide-react';

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
      <span className="workflow-kicker">Analiza zakończona</span>
      <h2 id="import-analysis-title">Archiwum gotowe do importu</h2>
      <p className="workflow-count">{formatCount(analysis.total)} rekordów</p>
    </div>
    <dl className="count-ledger">
      <div>
        <dt>
          <CheckCircle2 size={14} aria-hidden="true" /> Gotowe
        </dt>
        <dd>{formatCount(analysis.candidateRecords)}</dd>
      </div>
      <div>
        <dt>
          <CircleAlert size={14} aria-hidden="true" /> Odrzucone w analizie
        </dt>
        <dd>{formatCount(analysis.failed)}</dd>
      </div>
    </dl>
  </section>
);
