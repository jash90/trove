import { Database, FileArchive } from 'lucide-react';

import {
  validateStorageStats,
  type StorageStats as StorageStatsContract,
} from '../lib/contracts';
import { formatByteSize, formatCount } from '../lib/format';

interface StorageStatsProps {
  stats: StorageStatsContract | null;
  status: 'loading' | 'ready' | 'unavailable';
}

const safeStats = (stats: StorageStatsContract | null): StorageStatsContract | null => {
  if (!stats) return null;
  try {
    return validateStorageStats(stats);
  } catch {
    return null;
  }
};

export const StorageStats = ({ stats, status }: StorageStatsProps): React.JSX.Element => {
  const validated = safeStats(stats);
  const unavailable = status === 'unavailable' || (status === 'ready' && !validated);

  return (
    <section className="storage-stats" aria-labelledby="storage-stats-title">
      <div className="settings-section-heading">
        <span className="settings-section-icon" aria-hidden="true"><Database size={16} /></span>
        <div>
          <span className="workflow-kicker">Local storage</span>
          <h2 id="storage-stats-title">Data storage</h2>
        </div>
      </div>
      {status === 'loading' ? <p role="status">Obliczanie rozmiaru danych…</p> : null}
      {unavailable ? <p role="status">Storage figures are unavailable.</p> : null}
      {status === 'ready' && validated ? (
        <>
          <dl className="storage-ledger">
            <div>
              <dt><Database size={14} aria-hidden="true" /> Main database file</dt>
              <dd>{formatByteSize(validated.databaseBytes)}</dd>
            </div>
            <div>
              <dt><FileArchive size={14} aria-hidden="true" /> Blobs the database references</dt>
              <dd>{formatByteSize(validated.blobBytes)}</dd>
            </div>
          </dl>
          <p className="settings-help">
            The size of the main database file and the blobs it references; not the application's total disk use.
          </p>
          <p className="storage-counts">
            {formatCount(validated.eventCount)} events · {formatCount(validated.contentCount)} contents
          </p>
        </>
      ) : null}
    </section>
  );
};
