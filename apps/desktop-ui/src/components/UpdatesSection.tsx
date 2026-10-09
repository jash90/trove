import { RefreshCw } from "lucide-react";
import { useState } from "react";

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
      return "The release server could not be reached. Check the connection and try again.";
    case "update_no_build":
      return "There is no build of the latest release for this Mac.";
    case "update_signature_invalid":
      return "The downloaded update did not carry Trove's signature, so it was not installed.";
    case "update_not_writable":
      return "Trove could not replace itself. Move it to Applications, or check that you can write there, and try again.";
    case "no_pending_update":
      return "Check for updates again before installing.";
    default:
      return "The update did not complete. Trove is unchanged.";
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
        Updates
      </h2>
      <p className="settings-help">
        {info ? `This is Trove ${info.currentVersion}. ` : null}
        Trove looks for a new release only when you ask it to. An update is
        installed only if it carries Trove's signature, and the application
        restarts to finish.
      </p>
      <div className="workflow-actions workflow-actions--start">
        <button type="button" onClick={() => void check()} disabled={busy}>
          {phase.kind === "checking" ? "Checking…" : "Check for updates"}
        </button>
      </div>

      {error ? (
        <p className="workflow-alert" role="alert">
          {error}
        </p>
      ) : null}

      {info && !info.available ? (
        <p className="workflow-status" role="status">
          Trove {info.currentVersion} is the latest version.
        </p>
      ) : null}

      {info?.available ? (
        <div className="settings-notice">
          <p role="status">
            Trove {info.version} is available — you have {info.currentVersion}.
          </p>
          {info.notes ? <p className="update-notes">{info.notes}</p> : null}
          {phase.kind === "installing" ? (
            <>
              <div
                className="progress-track"
                role="progressbar"
                aria-label="Update download"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={percent ?? undefined}
              >
                <span style={{ width: `${percent ?? 0}%` }} />
              </div>
              <p className="progress-count" aria-live="polite">
                {percent === null
                  ? "Downloading…"
                  : percent < 100
                    ? `Downloading… ${percent}%`
                    : "Installing and restarting…"}
              </p>
            </>
          ) : (
            <div className="settings-notice-actions">
              <button
                type="button"
                className="workflow-primary"
                onClick={() => void install(info)}
              >
                Install and restart
              </button>
            </div>
          )}
        </div>
      ) : null}
    </section>
  );
};
