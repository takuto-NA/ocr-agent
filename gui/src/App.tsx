/**
 * Responsibility:
 * - Provide the MVP OCR job runner UI:
 *   - Select output directory (job root)
 *   - Drag-and-drop images/PDFs/folders
 *   - Start (enqueue -> run) via backend command
 *   - Poll progress (counts + ETA) and show logs
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

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
  const [jobRootDirectoryPath, setJobRootDirectoryPath] = useState<string | null>(null);
  const [droppedPathCount, setDroppedPathCount] = useState<number>(0);
  const [isDragging, setIsDragging] = useState<boolean>(false);
  const [jobStatus, setJobStatus] = useState<JobStatus | null>(null);
  const [logLines, setLogLines] = useState<string[]>([]);
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
    let unlistenPromise: Promise<() => void> | null = null;
    unlistenPromise = listen<string[]>("tauri://file-drop", async (event) => {
      const currentJobRootDirectoryPath = jobRootDirectoryPathRef.current;
      if (currentJobRootDirectoryPath === null) {
        setUiErrorMessage("Select an output directory before dropping files.");
        return;
      }

      const droppedPaths = event.payload ?? [];
      if (droppedPaths.length <= 0) {
        return;
      }

      try {
        setUiErrorMessage(null);
        await invoke("job_add_inputs", {
          jobRootDirectoryPath: currentJobRootDirectoryPath,
          inputPaths: droppedPaths
        });
        setDroppedPathCount((previous) => previous + droppedPaths.length);
      } catch (error) {
        setUiErrorMessage(String(error));
      }
    });

    const unlistenDragEnter = listen("tauri://drag-enter", () => setIsDragging(true));
    const unlistenDragLeave = listen("tauri://drag-leave", () => setIsDragging(false));

    return () => {
      if (unlistenPromise !== null) {
        void unlistenPromise.then((unlisten) => unlisten());
      }
      void unlistenDragEnter.then((unlisten) => unlisten());
      void unlistenDragLeave.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
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
        setLogLines(response.lines);
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
    try {
      setUiErrorMessage(null);
      const selectedDirectoryPath = await invoke<string | null>("pick_output_directory");
      if (selectedDirectoryPath === null) {
        return;
      }
      setJobRootDirectoryPath(selectedDirectoryPath);
      setDroppedPathCount(0);
      setLogLines([]);
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handleRunJob(): Promise<void> {
    const currentJobRootDirectoryPath = jobRootDirectoryPath;
    if (currentJobRootDirectoryPath === null) {
      setUiErrorMessage("Select an output directory first.");
      return;
    }

    try {
      setUiErrorMessage(null);
      await invoke("probe_docker", {});
      await invoke("run_job", { jobRootDirectoryPath: currentJobRootDirectoryPath });
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handleProbeGpu(): Promise<void> {
    try {
      setUiErrorMessage(null);
      const output = await invoke<string>("probe_gpu_passthrough", {});
      setLogLines((previous) => [...previous, "[gpu-probe] OK", output]);
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handleCancelJob(): Promise<void> {
    const currentJobRootDirectoryPath = jobRootDirectoryPath;
    if (currentJobRootDirectoryPath === null) {
      return;
    }
    try {
      setUiErrorMessage(null);
      await invoke("cancel_job", { jobRootDirectoryPath: currentJobRootDirectoryPath });
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handleOpenOutputDirectory(): Promise<void> {
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
    if (jobRootDirectoryPath === null) {
      return;
    }
    try {
      setUiErrorMessage(null);
      await invoke("reset_job_directory", { jobRootDirectoryPath });
      setDroppedPathCount(0);
      setLogLines([]);
    } catch (error) {
      setUiErrorMessage(String(error));
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
        <div className="label">{statusLabel}</div>
      </div>
      <div className="subtitle">
        Drag &amp; drop images or PDFs. PDFs are automatically queued per page, then merged into
        Markdown.
      </div>

      <div style={{ height: 14 }} />

      <div className="card">
        <div className="row">
          <button className="button buttonPrimary" onClick={handlePickOutputDirectory}>
            Select output directory (job root)
          </button>
          <button className="button" onClick={handleOpenOutputDirectory} disabled={jobRootDirectoryPath === null}>
            Open folder
          </button>
          <button className="button" onClick={handleResetJobDirectory} disabled={jobRootDirectoryPath === null || jobStatus?.is_running === true}>
            Reset job (delete queue/output)
          </button>
        </div>
        <div style={{ height: 10 }} />
        <div className="label">Job root</div>
        <div className="mono">{jobRootDirectoryPath ?? "(not selected)"}</div>
      </div>

      <div style={{ height: 14 }} />

      <div className={`card dropZone ${isDragging ? "dropZoneStrong" : ""}`}>
        <div style={{ fontWeight: 700 }}>Drop files or folders here</div>
        <div className="label">
          We copy your inputs into <span className="mono">input/</span> under the selected output
          directory.
        </div>
        <div className="label">
          Dropped items this session: <b>{droppedPathCount}</b>
        </div>
      </div>

      <div style={{ height: 14 }} />

      <div className="card">
        <div className="row">
          <button className="button buttonPrimary" onClick={handleRunJob} disabled={!canStart}>
            Start OCR (enqueue → run)
          </button>
          <button className="button" onClick={handleProbeGpu} disabled={jobStatus?.is_running === true}>
            Check GPU
          </button>
          <button className="button buttonDanger" onClick={handleCancelJob} disabled={!jobStatus?.is_running}>
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

