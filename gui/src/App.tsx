/**
 * Responsibility:
 * - Provide the MVP OCR job runner UI:
 *   - Select output directory (job root)
 *   - Drag-and-drop images/PDFs/folders
 *   - Start (enqueue -> run) via backend command
 *   - Poll progress (counts + ETA) and show logs
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { isTauriWebview } from "./tauri_env";

type JobStatus = {
  job_root_directory_path: string;
  is_running: boolean;
  start_unix_timestamp_millis: number | null;
  total_tasks: number;
  pending_tasks: number;
  running_tasks: number;
  completed_tasks: number;
  failed_tasks: number;
  last_error_message: string | null;
  eta_seconds: number | null;
};

type JobLogResponse = {
  lines: string[];
};

const PROGRESS_POLL_INTERVAL_MILLIS = 900;
const LOG_POLL_INTERVAL_MILLIS = 700;
const MAX_UI_LOG_LINES = 400;

function formatSecondsHuman(totalSeconds: number): string {
  const safeSeconds = Math.max(0, Math.floor(totalSeconds));
  const minutes = Math.floor(safeSeconds / 60);
  const seconds = safeSeconds % 60;
  if (minutes <= 0) {
    return `${seconds}s`;
  }
  return `${minutes}m ${seconds}s`;
}

export function App() {
  const isRunningInsideTauri = useMemo(() => isTauriWebview(), []);
  const [jobRootDirectoryPath, setJobRootDirectoryPath] = useState<string | null>(null);
  const [droppedPathCount, setDroppedPathCount] = useState<number>(0);
  const [jobStatus, setJobStatus] = useState<JobStatus | null>(null);
  const [isStartingRun, setIsStartingRun] = useState<boolean>(false);
  const [uiLogLines, setUiLogLines] = useState<string[]>([]);
  const [backendLogLines, setBackendLogLines] = useState<string[]>([]);
  const [uiErrorMessage, setUiErrorMessage] = useState<string | null>(null);

  const jobRootDirectoryPathRef = useRef<string | null>(null);
  jobRootDirectoryPathRef.current = jobRootDirectoryPath;

  const percentCompleted = useMemo(() => {
    if (jobStatus === null) {
      return 0;
    }
    if (jobStatus.total_tasks <= 0) {
      return 0;
    }
    const ratio = jobStatus.completed_tasks / jobStatus.total_tasks;
    return Math.max(0, Math.min(1, ratio));
  }, [jobStatus]);

  const logLines = useMemo(() => {
    // UI logs are ephemeral user actions; backend logs are engine output.
    return [...uiLogLines, ...backendLogLines];
  }, [uiLogLines, backendLogLines]);

  const canStart = useMemo(() => {
    if (jobRootDirectoryPath === null) {
      return false;
    }
    if (jobStatus?.is_running) {
      return false;
    }
    return droppedPathCount > 0;
  }, [jobRootDirectoryPath, jobStatus?.is_running, droppedPathCount]);

  useEffect(() => {
    if (!isRunningInsideTauri) {
      // Guard: When running as a normal browser tab (not in Tauri), native file dialogs are unavailable.
      setUiErrorMessage(
        "You are running in a normal browser tab. File pickers and OCR execution require the Tauri desktop app."
      );
    }
  }, [isRunningInsideTauri]);

  useEffect(() => {
    if (isRunningInsideTauri) {
      // Guard: clear the “browser tab” warning when Tauri is detected.
      setUiErrorMessage(null);
    }
  }, [isRunningInsideTauri]);

  useEffect(() => {
    if (!isRunningInsideTauri) {
      // Guard: do not call invoke() outside of Tauri.
      return;
    }
    if (jobRootDirectoryPath === null) {
      return;
    }

    let cancelled = false;
    const intervalId = window.setInterval(async () => {
      if (cancelled) {
        return;
      }
      try {
        const status = await invoke<JobStatus>("get_job_status", {
          jobRootDirectoryPath
        });
        setJobStatus(status);
      } catch (error) {
        setUiErrorMessage(String(error));
      }
    }, PROGRESS_POLL_INTERVAL_MILLIS);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, [jobRootDirectoryPath]);

  useEffect(() => {
    if (!isRunningInsideTauri) {
      // Guard: do not call invoke() outside of Tauri.
      return;
    }
    if (jobRootDirectoryPath === null) {
      return;
    }

    let cancelled = false;
    const intervalId = window.setInterval(async () => {
      if (cancelled) {
        return;
      }
      try {
        const response = await invoke<JobLogResponse>("get_job_logs", {
          jobRootDirectoryPath
        });
        setBackendLogLines(response.lines);
      } catch {
        // Guard: log polling should not spam errors when job isn't running yet.
      }
    }, LOG_POLL_INTERVAL_MILLIS);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, [jobRootDirectoryPath]);

  async function handlePickOutputDirectory(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("Folder picker is only available in the Tauri desktop app.");
      return;
    }
    try {
      setUiErrorMessage(null);
      const selectedDirectoryPath = await invoke<string | null>("pick_output_directory");
      if (selectedDirectoryPath === null) {
        return;
      }
      setJobRootDirectoryPath(selectedDirectoryPath);
      setDroppedPathCount(0);
      setUiLogLines([]);
      setBackendLogLines([]);
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  function appendUiLogLine(line: string): void {
    setUiLogLines((previous) => {
      const next = [...previous, line];
      if (next.length <= MAX_UI_LOG_LINES) {
        return next;
      }
      return next.slice(next.length - MAX_UI_LOG_LINES);
    });
  }

  async function handleAddInputFiles(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("Input selection is only available in the Tauri desktop app.");
      return;
    }
    const currentJobRootDirectoryPath = jobRootDirectoryPathRef.current;
    if (currentJobRootDirectoryPath === null) {
      setUiErrorMessage("Select an output directory first.");
      return;
    }

    try {
      setUiErrorMessage(null);
      appendUiLogLine("[inputs] selecting files…");
      const selectedPaths = await invoke<string[] | null>("pick_input_files");
      if (selectedPaths === null || selectedPaths.length <= 0) {
        appendUiLogLine("[inputs] cancelled");
        return;
      }
      appendUiLogLine(`[inputs] adding ${selectedPaths.length} path(s)…`);
      await invoke("job_add_inputs", {
        jobRootDirectoryPath: currentJobRootDirectoryPath,
        inputPaths: selectedPaths
      });
      setDroppedPathCount((previous) => previous + selectedPaths.length);
      appendUiLogLine("[inputs] added");
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[inputs] ERROR: ${errorMessage}`);
    }
  }

  async function handleAddInputFolder(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("Input selection is only available in the Tauri desktop app.");
      return;
    }
    const currentJobRootDirectoryPath = jobRootDirectoryPathRef.current;
    if (currentJobRootDirectoryPath === null) {
      setUiErrorMessage("Select an output directory first.");
      return;
    }

    try {
      setUiErrorMessage(null);
      appendUiLogLine("[inputs] selecting folder…");
      const selectedFolder = await invoke<string | null>("pick_input_folder");
      if (selectedFolder === null) {
        appendUiLogLine("[inputs] cancelled");
        return;
      }
      appendUiLogLine("[inputs] adding 1 folder…");
      await invoke("job_add_inputs", {
        jobRootDirectoryPath: currentJobRootDirectoryPath,
        inputPaths: [selectedFolder]
      });
      setDroppedPathCount((previous) => previous + 1);
      appendUiLogLine("[inputs] added");
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[inputs] ERROR: ${errorMessage}`);
    }
  }

  async function handleRunJob(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("OCR execution is only available in the Tauri desktop app.");
      return;
    }
    if (isStartingRun) {
      // Guard: prevent accidental double-click starting two jobs.
      return;
    }
    const currentJobRootDirectoryPath = jobRootDirectoryPath;
    if (currentJobRootDirectoryPath === null) {
      setUiErrorMessage("Select an output directory first.");
      return;
    }

    try {
      setUiErrorMessage(null);
      appendUiLogLine("[run] starting…");
      setIsStartingRun(true);
      await invoke("probe_docker", {});
      await invoke("run_job", { jobRootDirectoryPath: currentJobRootDirectoryPath });
      appendUiLogLine("[run] started");
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[run] ERROR: ${errorMessage}`);
    } finally {
      setIsStartingRun(false);
    }
  }

  async function handleProbeGpu(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("GPU check is only available in the Tauri desktop app.");
      return;
    }
    try {
      setUiErrorMessage(null);
      appendUiLogLine("[gpu-probe] running…");
      const output = await invoke<string>("probe_gpu_passthrough", {});
      appendUiLogLine("[gpu-probe] OK");
      appendUiLogLine(output);
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[gpu-probe] ERROR: ${errorMessage}`);
    }
  }

  async function handleCancelJob(): Promise<void> {
    if (!isRunningInsideTauri) {
      return;
    }
    const currentJobRootDirectoryPath = jobRootDirectoryPath;
    if (currentJobRootDirectoryPath === null) {
      return;
    }
    try {
      setUiErrorMessage(null);
      appendUiLogLine("[cancel] requested");
      await invoke("cancel_job", { jobRootDirectoryPath: currentJobRootDirectoryPath });
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[cancel] ERROR: ${errorMessage}`);
    }
  }

  async function handleOpenOutputDirectory(): Promise<void> {
    if (!isRunningInsideTauri) {
      return;
    }
    if (jobRootDirectoryPath === null) {
      return;
    }
    try {
      await invoke("open_in_file_manager", { targetPath: jobRootDirectoryPath });
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handleResetJobDirectory(): Promise<void> {
    if (!isRunningInsideTauri) {
      return;
    }
    if (jobRootDirectoryPath === null) {
      return;
    }
    try {
      setUiErrorMessage(null);
      appendUiLogLine("[reset] starting…");
      await invoke("reset_job_directory", { jobRootDirectoryPath });
      setDroppedPathCount(0);
      setUiLogLines([]);
      setBackendLogLines([]);
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[reset] ERROR: ${errorMessage}`);
    }
  }

  const statusLabel = useMemo(() => {
    if (jobRootDirectoryPath === null) {
      return "No job directory selected.";
    }
    if (jobStatus === null) {
      return "Preparing…";
    }
    if (jobStatus.is_running) {
      return "Running…";
    }
    if (jobStatus.total_tasks <= 0) {
      return "Ready.";
    }
    if (jobStatus.failed_tasks > 0) {
      return "Finished (with failures).";
    }
    if (jobStatus.completed_tasks >= jobStatus.total_tasks) {
      return "Finished.";
    }
    return "Ready.";
  }, [jobRootDirectoryPath, jobStatus]);

  return (
    <div className="container">
      <div className="titleRow">
        <h1 className="title">ocr-agent</h1>
        <div className="label">
          {statusLabel} · Tauri: <b>{isRunningInsideTauri ? "yes" : "no"}</b>
        </div>
      </div>
      <div className="subtitle">
        Select an output directory, then add input images/PDFs (or a folder). PDFs are automatically queued per page,
        then merged into Markdown.
      </div>

      <div style={{ height: 14 }} />

      <div className="card">
        <div className="row">
          <button className="button buttonPrimary" onClick={handlePickOutputDirectory}>
            Select output directory (job root)
          </button>
          <button
            className="button"
            onClick={handleOpenOutputDirectory}
            disabled={!isRunningInsideTauri || jobRootDirectoryPath === null}
          >
            Open folder
          </button>
          <button
            className="button"
            onClick={handleResetJobDirectory}
            disabled={!isRunningInsideTauri || jobRootDirectoryPath === null || jobStatus?.is_running === true}
          >
            Reset job (delete queue/output)
          </button>
        </div>
        <div style={{ height: 10 }} />
        <div className="label">Job root</div>
        <div className="mono">{jobRootDirectoryPath ?? "(not selected)"}</div>
      </div>

      <div style={{ height: 14 }} />

      <div className="card">
        <div className="row">
          <button
            className="button"
            onClick={handleAddInputFiles}
            disabled={!isRunningInsideTauri || jobRootDirectoryPath === null || jobStatus?.is_running === true}
          >
            Add files (images/PDF)
          </button>
          <button
            className="button"
            onClick={handleAddInputFolder}
            disabled={!isRunningInsideTauri || jobRootDirectoryPath === null || jobStatus?.is_running === true}
          >
            Add folder
          </button>
        </div>
        <div style={{ height: 10 }} />
        <div className="label">
          Added items this session: <b>{droppedPathCount}</b>
        </div>
        <div className="label">
          Inputs are copied into <span className="mono">input/</span> under the selected output directory.
        </div>
      </div>

      <div style={{ height: 14 }} />

      <div className="card">
        <div className="row">
          <button
            className="button buttonPrimary"
            onClick={handleRunJob}
            disabled={!isRunningInsideTauri || !canStart || isStartingRun}
          >
            Start OCR (enqueue → run)
          </button>
          <button
            className="button"
            onClick={handleProbeGpu}
            disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
          >
            Check GPU
          </button>
          <button
            className="button buttonDanger"
            onClick={handleCancelJob}
            disabled={!isRunningInsideTauri || !jobStatus?.is_running}
          >
            Cancel
          </button>
        </div>

        <div style={{ height: 12 }} />

        <div className="label">Progress</div>
        <div className="progressOuter">
          <div className="progressInner" style={{ width: `${Math.round(percentCompleted * 100)}%` }} />
        </div>
        <div style={{ height: 8 }} />

        <div className="row" style={{ justifyContent: "space-between", width: "100%" }}>
          <div className="label">
            {jobStatus
              ? `${jobStatus.completed_tasks}/${jobStatus.total_tasks} completed · ${jobStatus.pending_tasks} pending · ${jobStatus.failed_tasks} failed`
              : "—"}
          </div>
          <div className="label">
            ETA:{" "}
            {jobStatus?.eta_seconds !== null && jobStatus?.eta_seconds !== undefined
              ? formatSecondsHuman(jobStatus.eta_seconds)
              : "—"}
          </div>
        </div>

        {jobStatus?.last_error_message ? (
          <>
            <div style={{ height: 10 }} />
            <div className="label" style={{ color: "var(--danger)" }}>
              Last error: {jobStatus.last_error_message}
            </div>
          </>
        ) : null}

        {uiErrorMessage ? (
          <>
            <div style={{ height: 10 }} />
            <div className="label" style={{ color: "var(--danger)" }}>
              {uiErrorMessage}
            </div>
          </>
        ) : null}

        <div className="logBox" aria-label="log output">
          {logLines.length <= 0 ? (
            <div className="label">Logs will appear here.</div>
          ) : (
            logLines.map((line, index) => (
              <pre className="logLine" key={`${index}-${line.slice(0, 20)}`}>
                {line}
              </pre>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

