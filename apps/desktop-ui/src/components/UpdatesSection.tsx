import { RefreshCw } from "lucide-react";
import { useState } from "react";

import { t, useT } from "../i18n";
import type { UpdateInfo, UpdateProgress } from "../lib/contracts";

type UpdateGateway = {
  checkForUpdate(): Promise<UpdateInfo>;
  installUpdate(onProgress: (progress: UpdateProgress) => void): Promise<void>;
};

type Phase =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "checked"; info: UpdateInfo }
  | { kind: "installing"; info: UpdateInfo; progress: UpdateProgress | null };

/**
 * One sentence per refusal. The plugin's own messages are written for the
 * people who maintain it, and the core only ever sends a code.
 */
export const updateErrorMessage = (code: string): string => {
  switch (code) {
    case "update_unreachable":
      return t("updates.error.unreachable");
    case "update_no_build":
      return t("updates.error.noBuild");
    case "update_signature_invalid":
      return t("updates.error.signature");
    case "update_not_writable":
      return t("updates.error.notWritable");
    case "no_pending_update":
      return t("updates.error.noPending");
    default:
      return t("updates.error.generic");
  }
};

const errorCode = (error: unknown): string =>
  typeof error === "string"
    ? error
    : error instanceof Error
      ? error.message
      : "";

const percentOf = (progress: UpdateProgress | null): number | null =>
  progress && progress.total
    ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
    : null;

/**
 * Checking for, and installing, a newer Trove.
 *
 * Nothing here runs until a button is pressed: opening this tab does not go
 * online, and nothing else in the application checks behind the user's back.
 */
export const UpdatesSection = ({
  gateway,
}: {
  gateway: UpdateGateway;
}): React.JSX.Element => {
  const t = useT();
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [error, setError] = useState<string | null>(null);

  const check = async (): Promise<void> => {
    setError(null);
    setPhase({ kind: "checking" });
    try {
      setPhase({ kind: "checked", info: await gateway.checkForUpdate() });
    } catch (failure) {
      setError(updateErrorMessage(errorCode(failure)));
      setPhase({ kind: "idle" });
    }
  };

  const install = async (info: UpdateInfo): Promise<void> => {
    setError(null);
    setPhase({ kind: "installing", info, progress: null });
    try {
      // On success the application restarts underneath this call, so there is
      // nothing to do after it.
      await gateway.installUpdate((progress) =>
        setPhase({ kind: "installing", info, progress }),
      );
    } catch (failure) {
      setError(updateErrorMessage(errorCode(failure)));
      // The pending release was taken by the attempt; a fresh check is what
      // makes the install button mean something again.
      setPhase({ kind: "idle" });
    }
  };

  const info =
    phase.kind === "checked" || phase.kind === "installing" ? phase.info : null;
  const busy = phase.kind === "checking" || phase.kind === "installing";
  const percent = phase.kind === "installing" ? percentOf(phase.progress) : null;

  return (
    <section
      className="settings-section"
      aria-labelledby="settings-updates-title"
    >
      <h2 id="settings-updates-title">
        <RefreshCw size={15} aria-hidden="true" />
        {t("updates.title")}
      </h2>
      <p className="settings-help">
        {info ? t("updates.thisIs", { version: info.currentVersion }) : null}
        {t("updates.help")}
      </p>
      <div className="workflow-actions workflow-actions--start">
        <button type="button" onClick={() => void check()} disabled={busy}>
          {phase.kind === "checking" ? t("updates.checking") : t("updates.check")}
        </button>
      </div>

      {error ? (
        <p className="workflow-alert" role="alert">
          {error}
        </p>
      ) : null}

      {info && !info.available ? (
        <p className="workflow-status" role="status">
          {t("updates.latest", { version: info.currentVersion })}
        </p>
      ) : null}

      {info?.available ? (
        <div className="settings-notice">
          <p role="status">
            {t("updates.available", { version: info.version ?? "", current: info.currentVersion })}
          </p>
          {info.notes ? <p className="update-notes">{info.notes}</p> : null}
          {phase.kind === "installing" ? (
            <>
              <div
                className="progress-track"
                role="progressbar"
                aria-label={t("updates.downloadLabel")}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={percent ?? undefined}
              >
                <span style={{ width: `${percent ?? 0}%` }} />
              </div>
              <p className="progress-count" aria-live="polite">
                {percent === null
                  ? t("updates.downloading")
                  : percent < 100
                    ? t("updates.downloadingPercent", { percent })
                    : t("updates.installing")}
              </p>
            </>
          ) : (
            <div className="settings-notice-actions">
              <button
                type="button"
                className="workflow-primary"
                onClick={() => void install(info)}
              >
                {t("updates.install")}
              </button>
            </div>
          )}
        </div>
      ) : null}
    </section>
  );
};
