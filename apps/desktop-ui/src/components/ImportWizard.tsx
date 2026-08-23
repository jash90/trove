import { FileJson, FolderOpen, ShieldAlert, X } from 'lucide-react';
import { useEffect, useRef, useState, type KeyboardEventHandler } from 'react';

import { useModalFocus } from '../hooks/useModalFocus';
import type { ImportProgress as ImportProgressContract, ImportSummary } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { ImportAnalysis } from './ImportAnalysis';
import { ImportProgress as ImportProgressView } from './ImportProgress';
import { ImportSummary as ImportSummaryView } from './ImportSummary';

import type { ImportAnalysis as ImportAnalysisContract } from '../lib/contracts';

interface ImportWizardProps {
  gateway: ClipboardGateway;
  onClose?: () => void;
  pollIntervalMs?: number;
}

type WizardPhase =
  | { tag: 'idle' }
  | { tag: 'analyzing' }
  | { tag: 'confirm'; analysis: ImportAnalysisContract }
  | { tag: 'cancelling'; analysis: ImportAnalysisContract }
  | { tag: 'running'; runId: string; recovering: boolean }
  | { tag: 'complete'; summary: ImportSummary }
  | { tag: 'failed' };

// Every message here is a fixed literal. Gateway errors may embed a path or a
// payload fragment, so their text never reaches the DOM.
const ANALYSIS_ERROR = 'Nie udało się przeanalizować archiwum. Sprawdź, czy wskazany plik lub katalog jest eksportem Raycast albo SuperCmd.';
const DISCARD_ERROR = 'Nie udało się anulować przygotowanego importu. Spróbuj ponownie.';

export const ImportWizard = ({
  gateway,
  onClose,
  pollIntervalMs = 400,
}: ImportWizardProps): React.JSX.Element => {
  const [phase, setPhase] = useState<WizardPhase>({ tag: 'idle' });
  const [progress, setProgress] = useState<ImportProgressContract | null>(null);
  const [error, setError] = useState<string | null>(null);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const modalFocus = useModalFocus({ active: true, initialFocusRef: primaryRef });

  const runId = phase.tag === 'running' ? phase.runId : null;

  useEffect(() => {
    if (runId === null) return;
    let cancelled = false;
    let timer: number | undefined;

    const poll = async (): Promise<void> => {
      let next: ImportProgressContract;
      try {
        next = await gateway.getImportStatus(runId);
      } catch {
        if (!cancelled) setPhase({ tag: 'failed' });
        return;
      }
      if (cancelled) return;
      if (next.runId !== runId) {
        setPhase({ tag: 'failed' });
        return;
      }
      if (next.state === 'failed') {
        setPhase({ tag: 'failed' });
        return;
      }
      if (next.state === 'completed' && next.summary !== null) {
        setPhase({ tag: 'complete', summary: next.summary });
        return;
      }
      setProgress(next);
      timer = window.setTimeout(() => void poll(), pollIntervalMs);
    };

    void poll();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [gateway, pollIntervalMs, runId]);

  const analyze = async (choose: () => Promise<string | null>): Promise<void> => {
    setError(null);
    // The path lives only inside this call. It is never stored in state, so it
    // cannot reach the DOM or a later gateway call.
    const path = await choose().catch(() => null);
    if (path === null) return;
    setPhase({ tag: 'analyzing' });
    try {
      const analysis = await gateway.analyzeImport(path);
      setPhase({ tag: 'confirm', analysis });
    } catch {
      setPhase({ tag: 'idle' });
      setError(ANALYSIS_ERROR);
    }
  };

  const start = async (analysis: ImportAnalysisContract): Promise<void> => {
    setError(null);
    setProgress(null);
    let handleRunId: string;
    try {
      handleRunId = (await gateway.startImport(analysis.analysisId)).runId;
    } catch {
      // The response was lost, not necessarily the run. The analysis ID is the
      // run ID by contract, so recovery polls it instead of discarding blindly.
      setPhase({ tag: 'running', runId: analysis.analysisId, recovering: true });
      return;
    }
    if (handleRunId !== analysis.analysisId) {
      setPhase({ tag: 'failed' });
      return;
    }
    setPhase({ tag: 'running', runId: handleRunId, recovering: false });
  };

  const cancel = async (analysis: ImportAnalysisContract): Promise<void> => {
    setError(null);
    setPhase({ tag: 'cancelling', analysis });
    try {
      await gateway.discardImportAnalysis(analysis.analysisId);
    } catch {
      setPhase({ tag: 'confirm', analysis });
      setError(DISCARD_ERROR);
      return;
    }
    // The prepared analysis is gone, so the dialog must not keep offering it.
    // Reset before closing: the host may keep this instance mounted.
    setProgress(null);
    setPhase({ tag: 'idle' });
    onClose?.();
  };

  const requestClose = (): void => {
    if (phase.tag === 'confirm') {
      void cancel(phase.analysis);
      return;
    }
    if (phase.tag === 'cancelling' || phase.tag === 'analyzing') return;
    onClose?.();
  };

  const handleKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    modalFocus.onKeyDown(event);
    if (event.key === 'Escape') {
      event.preventDefault();
      requestClose();
    }
  };

  // Choosing a source unmounts the button that had focus, so focus falls back to
  // the body and the dialog's own handler stops seeing keystrokes. React stops
  // propagation for events that do reach the dialog, so the two never overlap.
  const requestCloseRef = useRef(requestClose);
  requestCloseRef.current = requestClose;
  useEffect(() => {
    const onDocumentKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      requestCloseRef.current();
    };
    document.addEventListener('keydown', onDocumentKeyDown);
    return () => document.removeEventListener('keydown', onDocumentKeyDown);
  }, []);

  return (
    <div
      className="workflow-backdrop"
      data-testid="import-backdrop"
      onMouseDown={requestClose}
    >
      <div
        className="workflow-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="import-dialog-title"
        aria-describedby="import-dialog-description"
        onKeyDown={handleKeyDown}
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="workflow-dialog__header">
          <div>
            <span className="workflow-kicker">Import lokalny</span>
            <h1 id="import-dialog-title">Importuj historię</h1>
          </div>
        </header>

        <p id="import-dialog-description" className="workflow-warning">
          <ShieldAlert size={15} aria-hidden="true" />
          Archiwum może zawierać dane wrażliwe. Import odbywa się wyłącznie na tym
          urządzeniu i nic nie jest wysyłane na zewnątrz.
        </p>

        {error ? (
          <p className="workflow-alert" role="alert">
            {error}
          </p>
        ) : null}

        {phase.tag === 'idle' ? (
          <div className="workflow-choices">
            <button
              ref={primaryRef}
              type="button"
              className="workflow-primary"
              onClick={() => void analyze(() => gateway.chooseImportFile())}
            >
              <FileJson size={16} aria-hidden="true" />
              Wybierz plik JSON
            </button>
            <button
              type="button"
              onClick={() => void analyze(() => gateway.chooseImportDirectory())}
            >
              <FolderOpen size={16} aria-hidden="true" />
              Wybierz katalog eksportu
            </button>
          </div>
        ) : null}

        {phase.tag === 'analyzing' ? (
          <p className="workflow-pending" role="status">
            Analizowanie archiwum…
          </p>
        ) : null}

        {phase.tag === 'confirm' || phase.tag === 'cancelling' ? (
          <>
            <ImportAnalysis analysis={phase.analysis} />
            <div className="workflow-actions">
              <button
                type="button"
                disabled={phase.tag === 'cancelling'}
                onClick={() => void cancel(phase.analysis)}
              >
                {phase.tag === 'cancelling' ? 'Anulowanie…' : 'Anuluj import'}
              </button>
              <button
                type="button"
                className="workflow-primary"
                disabled={phase.tag === 'cancelling'}
                onClick={() => void start(phase.analysis)}
              >
                Rozpocznij import
              </button>
            </div>
          </>
        ) : null}

        {phase.tag === 'running' ? (
          <ImportProgressView
            progress={
              progress ?? {
                runId: phase.runId,
                state: 'running',
                processed: 0,
                total: 0,
                imported: 0,
                alreadyPresent: 0,
                skipped: 0,
                failed: 0,
                errorCode: null,
                summary: null,
              }
            }
            phase={phase.recovering && progress === null ? 'recovering' : 'running'}
          />
        ) : null}

        {phase.tag === 'complete' ? <ImportSummaryView summary={phase.summary} /> : null}

        {phase.tag === 'failed' ? (
          <section className="import-failure" aria-labelledby="import-failure-title">
            <h2 id="import-failure-title">Import nie został ukończony</h2>
            <p>
              Stan przebiegu jest nieznany. Otwórz import ponownie — powtórzenie tego
              samego archiwum nie utworzy duplikatów.
            </p>
          </section>
        ) : null}

        <button
          type="button"
          className="workflow-dismiss"
          aria-label="Zamknij import"
          onClick={requestClose}
        >
          <X size={16} aria-hidden="true" />
          Zamknij
        </button>
      </div>
    </div>
  );
};
