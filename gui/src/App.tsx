/**
 * Responsibility:
 * - Provide the MVP OCR job runner UI:
 *   - Select output directory (job root)
 *   - Add images/PDFs/folders via native dialogs
 *   - Start (enqueue -> run) via backend command
 *   - Poll progress (counts + estimated remaining time) and show logs
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { isTauriWebview } from "./tauri_env";
import { LogEntry, LogViewer, LogSource } from "./LogViewer";
import { CurrentTaskPreview, PreviewPanel } from "./PreviewPanel";

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
  estimated_time_remaining_seconds: number | null;
};

type JobLogResponse = {
  lines: string[];
};

type WatchFolderStatus = {
  is_running: boolean;
  inbox_directory_path: string | null;
  jobs_root_directory_path: string | null;
  last_error_message: string | null;
};

const PROGRESS_POLL_INTERVAL_MILLIS = 900;
const LOG_POLL_INTERVAL_MILLIS = 700;
const MAX_UI_LOG_LINES = 400;
const DEFAULT_LOG_VIEW_START_INDEX = 0;
const WATCH_STATUS_POLL_INTERVAL_MILLIS = 1100;

const LOCAL_STORAGE_WATCH_INBOX_DIRECTORY_PATH_KEY = "ocr-agent.watchInboxDirectoryPath";
const LOCAL_STORAGE_WATCH_JOBS_ROOT_DIRECTORY_PATH_KEY = "ocr-agent.watchJobsRootDirectoryPath";

const DEFAULT_DEEPSEEK_OCR2_BASE_IMAGE_SIZE_PIXELS = 1024;
const DEFAULT_DEEPSEEK_OCR2_INFERENCE_IMAGE_SIZE_PIXELS = 768;
const DEFAULT_DEEPSEEK_OCR2_ENABLE_CROP_MODE = true;

const DEFAULT_DEEPSEEK_OCR2_MARKDOWN_PROMPT = "<image>\n<|grounding|>Convert the document to markdown. ";
const DEFAULT_DEEPSEEK_OCR2_FREE_OCR_PROMPT = "<image>\nFree OCR. ";

function parseLogLineToEntry(rawLine: string, id: string, defaultSource: LogSource): LogEntry {
  const bracketMatch = rawLine.match(/^\[([^\]]+)\]\s?(.*)$/);
  if (bracketMatch === null) {
    return {
      id,
      raw: rawLine,
      tag: "log",
      message: rawLine,
      source: defaultSource
    };
  }

  const tag = bracketMatch[1] ?? "log";
  const message = bracketMatch[2] ?? "";

  const normalizedTag = tag.trim().toLowerCase();
  let source: LogSource = defaultSource;
  if (normalizedTag === "stdout") {
    source = "stdout";
  } else if (normalizedTag === "stderr") {
    source = "stderr";
  } else if (normalizedTag === "backend") {
    source = "backend";
  } else if (defaultSource === "ui") {
    source = "ui";
  }

  return {
    id,
    raw: rawLine,
    tag: `[${tag}]`,
    message,
    source
  };
}

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
  const [selectedInputPathCount, setSelectedInputPathCount] = useState<number>(0);
  const [jobStatus, setJobStatus] = useState<JobStatus | null>(null);
  const [isStartingRun, setIsStartingRun] = useState<boolean>(false);
  const [outputMarkdownFilenameOverride, setOutputMarkdownFilenameOverride] = useState<string>("");
  const [isMathDelimiterConversionEnabled, setIsMathDelimiterConversionEnabled] = useState<boolean>(true);
  const [deepseekOcr2ModelRevision, setDeepseekOcr2ModelRevision] = useState<string>("");
  const [deepseekOcr2MarkdownPrompt, setDeepseekOcr2MarkdownPrompt] = useState<string>(
    DEFAULT_DEEPSEEK_OCR2_MARKDOWN_PROMPT
  );
  const [deepseekOcr2BaseImageSizePixelsInput, setDeepseekOcr2BaseImageSizePixelsInput] = useState<string>(
    String(DEFAULT_DEEPSEEK_OCR2_BASE_IMAGE_SIZE_PIXELS)
  );
  const [deepseekOcr2InferenceImageSizePixelsInput, setDeepseekOcr2InferenceImageSizePixelsInput] = useState<string>(
    String(DEFAULT_DEEPSEEK_OCR2_INFERENCE_IMAGE_SIZE_PIXELS)
  );
  const [isDeepseekOcr2CropModeEnabled, setIsDeepseekOcr2CropModeEnabled] = useState<boolean>(
    DEFAULT_DEEPSEEK_OCR2_ENABLE_CROP_MODE
  );
  const [uiLogLines, setUiLogLines] = useState<string[]>([]);
  const [backendLogLines, setBackendLogLines] = useState<string[]>([]);
  const [currentTaskPreview, setCurrentTaskPreview] = useState<CurrentTaskPreview | null>(null);
  const [currentTaskPreviewImageUrl, setCurrentTaskPreviewImageUrl] = useState<string | null>(null);
  const [uiErrorMessage, setUiErrorMessage] = useState<string | null>(null);
  const [logViewStartIndex, setLogViewStartIndex] = useState<number>(DEFAULT_LOG_VIEW_START_INDEX);
  const [watchInboxDirectoryPath, setWatchInboxDirectoryPath] = useState<string>("");
  const [watchJobsRootDirectoryPath, setWatchJobsRootDirectoryPath] = useState<string>("");
  const [watchFolderStatus, setWatchFolderStatus] = useState<WatchFolderStatus | null>(null);

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

  const logEntries = useMemo<LogEntry[]>(() => {
    const entries: LogEntry[] = [];
    for (let index = 0; index < uiLogLines.length; index += 1) {
      const line = uiLogLines[index] ?? "";
      entries.push(parseLogLineToEntry(line, `ui-${index}`, "ui"));
    }
    for (let index = 0; index < backendLogLines.length; index += 1) {
      const line = backendLogLines[index] ?? "";
      entries.push(parseLogLineToEntry(line, `backend-${index}`, "backend"));
    }
    if (logViewStartIndex <= 0) {
      return entries;
    }
    return entries.slice(Math.min(logViewStartIndex, entries.length));
  }, [backendLogLines, logViewStartIndex, uiLogLines]);

  const canStart = useMemo(() => {
    if (jobRootDirectoryPath === null) {
      return false;
    }
    if (jobStatus?.is_running) {
      return false;
    }
    return selectedInputPathCount > 0;
  }, [jobRootDirectoryPath, jobStatus?.is_running, selectedInputPathCount]);

  useEffect(() => {
    if (!isRunningInsideTauri) {
      // Guard: When running as a normal browser tab (not in Tauri), native file dialogs are unavailable.
      setUiErrorMessage(
        "You are running in a normal browser tab. File pickers and OCR execution require the Tauri desktop app."
      );
    }
  }, [isRunningInsideTauri]);

  useEffect(() => {
    if (!isRunningInsideTauri) {
      return;
    }
    try {
      const inbox = window.localStorage.getItem(LOCAL_STORAGE_WATCH_INBOX_DIRECTORY_PATH_KEY) ?? "";
      const jobsRoot = window.localStorage.getItem(LOCAL_STORAGE_WATCH_JOBS_ROOT_DIRECTORY_PATH_KEY) ?? "";
      setWatchInboxDirectoryPath(inbox);
      setWatchJobsRootDirectoryPath(jobsRoot);
    } catch {
      // Guard: localStorage access may fail in some environments.
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
      return;
    }

    let cancelled = false;
    const intervalId = window.setInterval(async () => {
      if (cancelled) {
        return;
      }
      try {
        const status = await invoke<WatchFolderStatus>("get_watch_folder_status", {});
        setWatchFolderStatus(status);
      } catch {
        // Guard: watcher status polling should never break the main UI.
      }
    }, WATCH_STATUS_POLL_INTERVAL_MILLIS);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
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

  useEffect(() => {
    if (!isRunningInsideTauri) {
      // Guard: do not call invoke() outside of Tauri.
      return;
    }
    if (jobRootDirectoryPath === null) {
      setCurrentTaskPreview(null);
      return;
    }

    let cancelled = false;
    const intervalId = window.setInterval(async () => {
      if (cancelled) {
        return;
      }
      try {
        const preview = await invoke<CurrentTaskPreview | null>("get_current_task_preview", {
          jobRootDirectoryPath
        });
        setCurrentTaskPreview(preview);
      } catch {
        // Guard: preview polling should not spam errors.
      }
    }, PROGRESS_POLL_INTERVAL_MILLIS);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, [isRunningInsideTauri, jobRootDirectoryPath]);

  useEffect(() => {
    if (!isRunningInsideTauri) {
      // Guard: do not call invoke() outside of Tauri.
      setCurrentTaskPreviewImageUrl(null);
      return;
    }
    if (jobRootDirectoryPath === null) {
      setCurrentTaskPreviewImageUrl(null);
      return;
    }
    if (currentTaskPreview?.preview_image_file_path === null || currentTaskPreview?.preview_image_file_path === undefined) {
      setCurrentTaskPreviewImageUrl(null);
      return;
    }

    let cancelled = false;
    let createdObjectUrl: string | null = null;

    async function loadPreviewImage(): Promise<void> {
      try {
        const response = await invoke<{ mime_type: string; bytes: number[] } | null>(
          "get_current_task_preview_image_bytes",
          { jobRootDirectoryPath }
        );
        if (cancelled) {
          return;
        }
        if (response === null) {
          setCurrentTaskPreviewImageUrl(null);
          return;
        }

        const byteArray = new Uint8Array(response.bytes);
        const blob = new Blob([byteArray], { type: response.mime_type });
        createdObjectUrl = URL.createObjectURL(blob);
        setCurrentTaskPreviewImageUrl(createdObjectUrl);
      } catch {
        // Guard: preview should never break the core job runner UX.
        setCurrentTaskPreviewImageUrl(null);
      }
    }

    loadPreviewImage();

    return () => {
      cancelled = true;
      if (createdObjectUrl !== null) {
        URL.revokeObjectURL(createdObjectUrl);
      }
    };
  }, [currentTaskPreview?.preview_image_file_path, isRunningInsideTauri, jobRootDirectoryPath]);

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
      setSelectedInputPathCount(0);
      setOutputMarkdownFilenameOverride("");
      setUiLogLines([]);
      setBackendLogLines([]);
      setCurrentTaskPreview(null);
      setCurrentTaskPreviewImageUrl(null);
      setLogViewStartIndex(DEFAULT_LOG_VIEW_START_INDEX);
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
      setSelectedInputPathCount((previous) => previous + selectedPaths.length);
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
      setSelectedInputPathCount((previous) => previous + 1);
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

    const baseImageSizePixels = Number.parseInt(deepseekOcr2BaseImageSizePixelsInput, 10);
    if (!Number.isFinite(baseImageSizePixels) || baseImageSizePixels <= 0) {
      // Guard: refuse starting a job with invalid model sizing parameters.
      setUiErrorMessage("DeepSeek base image size must be a positive integer.");
      return;
    }
    const inferenceImageSizePixels = Number.parseInt(deepseekOcr2InferenceImageSizePixelsInput, 10);
    if (!Number.isFinite(inferenceImageSizePixels) || inferenceImageSizePixels <= 0) {
      // Guard: refuse starting a job with invalid model sizing parameters.
      setUiErrorMessage("DeepSeek inference image size must be a positive integer.");
      return;
    }
    const promptTrimmed = deepseekOcr2MarkdownPrompt.trim();
    if (promptTrimmed === "") {
      // Guard: model prompt is required for meaningful OCR output.
      setUiErrorMessage("DeepSeek prompt cannot be empty.");
      return;
    }

    try {
      setUiErrorMessage(null);
      appendUiLogLine("[run] starting…");
      setIsStartingRun(true);
      await invoke("probe_docker", {});
      await invoke("run_job", {
        jobRootDirectoryPath: currentJobRootDirectoryPath,
        outputMarkdownFilenameOverride:
          outputMarkdownFilenameOverride.trim() === "" ? null : outputMarkdownFilenameOverride.trim(),
        isMathDelimiterConversionEnabled,
        deepseekOcr2ModelRevision: deepseekOcr2ModelRevision.trim() === "" ? null : deepseekOcr2ModelRevision.trim(),
        deepseekOcr2MarkdownPrompt: promptTrimmed,
        deepseekOcr2BaseImageSizePixels: baseImageSizePixels,
        deepseekOcr2InferenceImageSizePixels: inferenceImageSizePixels,
        deepseekOcr2EnableCropMode: isDeepseekOcr2CropModeEnabled
      });
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
      setSelectedInputPathCount(0);
      setUiLogLines([]);
      setBackendLogLines([]);
      setCurrentTaskPreview(null);
      setCurrentTaskPreviewImageUrl(null);
      setLogViewStartIndex(DEFAULT_LOG_VIEW_START_INDEX);
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[reset] ERROR: ${errorMessage}`);
    }
  }

  async function handlePickWatchInboxDirectory(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("Folder picker is only available in the Tauri desktop app.");
      return;
    }
    try {
      const selectedDirectoryPath = await invoke<string | null>("pick_directory");
      if (selectedDirectoryPath === null) {
        return;
      }
      setWatchInboxDirectoryPath(selectedDirectoryPath);
      window.localStorage.setItem(LOCAL_STORAGE_WATCH_INBOX_DIRECTORY_PATH_KEY, selectedDirectoryPath);
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handlePickWatchJobsRootDirectory(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("Folder picker is only available in the Tauri desktop app.");
      return;
    }
    try {
      const selectedDirectoryPath = await invoke<string | null>("pick_directory");
      if (selectedDirectoryPath === null) {
        return;
      }
      setWatchJobsRootDirectoryPath(selectedDirectoryPath);
      window.localStorage.setItem(LOCAL_STORAGE_WATCH_JOBS_ROOT_DIRECTORY_PATH_KEY, selectedDirectoryPath);
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handleStartWatchFolder(): Promise<void> {
    if (!isRunningInsideTauri) {
      setUiErrorMessage("Automation is only available in the Tauri desktop app.");
      return;
    }
    const inbox = watchInboxDirectoryPath.trim();
    if (inbox === "") {
      setUiErrorMessage("Select an inbox directory first.");
      return;
    }
    try {
      setUiErrorMessage(null);
      appendUiLogLine("[watch-folder] starting…");
      await invoke("start_watch_folder", {
        inboxDirectoryPath: inbox,
        jobsRootDirectoryPath: watchJobsRootDirectoryPath.trim() === "" ? null : watchJobsRootDirectoryPath.trim()
      });
      appendUiLogLine("[watch-folder] started");
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[watch-folder] ERROR: ${errorMessage}`);
    }
  }

  async function handleStopWatchFolder(): Promise<void> {
    if (!isRunningInsideTauri) {
      return;
    }
    try {
      setUiErrorMessage(null);
      appendUiLogLine("[watch-folder] stopping…");
      await invoke("stop_watch_folder", {});
      appendUiLogLine("[watch-folder] stopped");
    } catch (error) {
      const errorMessage = String(error);
      setUiErrorMessage(errorMessage);
      appendUiLogLine(`[watch-folder] ERROR: ${errorMessage}`);
    }
  }

  async function handleOpenWatchInboxDirectory(): Promise<void> {
    if (!isRunningInsideTauri) {
      return;
    }
    const inbox = watchInboxDirectoryPath.trim();
    if (inbox === "") {
      return;
    }
    try {
      await invoke("open_in_file_manager", { targetPath: inbox });
    } catch (error) {
      setUiErrorMessage(String(error));
    }
  }

  async function handleOpenWatchJobsDirectory(): Promise<void> {
    if (!isRunningInsideTauri) {
      return;
    }
    const jobsRoot = watchJobsRootDirectoryPath.trim();
    if (jobsRoot === "") {
      return;
    }
    try {
      await invoke("open_in_file_manager", { targetPath: jobsRoot });
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

  const previewFallbackInferenceImageSizePixels = useMemo(() => {
    const parsed = Number.parseInt(deepseekOcr2InferenceImageSizePixelsInput, 10);
    if (Number.isFinite(parsed) && parsed > 0) {
      return parsed;
    }
    return DEFAULT_DEEPSEEK_OCR2_INFERENCE_IMAGE_SIZE_PIXELS;
  }, [deepseekOcr2InferenceImageSizePixelsInput]);

  function handleClearLogView(): void {
    setLogViewStartIndex(uiLogLines.length + backendLogLines.length);
  }

  return (
    <div className="appShell">
      <div className="appHeader">
        <div className="container" style={{ padding: 0 }}>
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
        </div>
      </div>

      <div className="appContent">
        <div className="container" style={{ height: "100%", padding: 0 }}>
          <div className="splitPane">
            <div className="scrollColumn">
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
                  Added items this session: <b>{selectedInputPathCount}</b>
                </div>
                <div className="label">
                  Inputs are copied into <span className="mono">input/</span> under the selected output directory.
                </div>
              </div>

              <div style={{ height: 14 }} />

              <div className="card">
                <div className="label">Automation (watch folder)</div>
                <div style={{ height: 8 }} />
                <div className="label">
                  Put files into a bundle folder, then create <span className="mono">.ready</span> to start OCR automatically.
                </div>
                <div style={{ height: 10 }} />
                <div className="row">
                  <button className="button" onClick={handlePickWatchInboxDirectory} disabled={!isRunningInsideTauri}>
                    Select inbox directory
                  </button>
                  <button
                    className="button"
                    onClick={handleOpenWatchInboxDirectory}
                    disabled={!isRunningInsideTauri || watchInboxDirectoryPath.trim() === ""}
                  >
                    Open inbox
                  </button>
                </div>
                <div style={{ height: 8 }} />
                <div className="mono">{watchInboxDirectoryPath.trim() === "" ? "(not selected)" : watchInboxDirectoryPath}</div>

                <div style={{ height: 10 }} />
                <div className="row">
                  <button className="button" onClick={handlePickWatchJobsRootDirectory} disabled={!isRunningInsideTauri}>
                    Select jobs root (optional)
                  </button>
                  <button
                    className="button"
                    onClick={handleOpenWatchJobsDirectory}
                    disabled={!isRunningInsideTauri || watchJobsRootDirectoryPath.trim() === ""}
                  >
                    Open jobs
                  </button>
                </div>
                <div style={{ height: 8 }} />
                <div className="mono">
                  {watchJobsRootDirectoryPath.trim() === "" ? "(default: inbox/jobs)" : watchJobsRootDirectoryPath}
                </div>

                <div style={{ height: 10 }} />
                <div className="row">
                  <button
                    className="button buttonPrimary"
                    onClick={handleStartWatchFolder}
                    disabled={!isRunningInsideTauri || watchFolderStatus?.is_running === true}
                  >
                    Start watch-folder
                  </button>
                  <button
                    className="button buttonDanger"
                    onClick={handleStopWatchFolder}
                    disabled={!isRunningInsideTauri || watchFolderStatus?.is_running !== true}
                  >
                    Stop
                  </button>
                </div>

                <div style={{ height: 10 }} />
                <div className="label">
                  Status: <b>{watchFolderStatus?.is_running === true ? "running" : "stopped"}</b>
                </div>
                {watchFolderStatus?.last_error_message ? (
                  <div className="label" style={{ color: "var(--danger)" }}>
                    Watch error: {watchFolderStatus.last_error_message}
                  </div>
                ) : null}
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

                <div className="label">Output Markdown filename (optional)</div>
                <div style={{ height: 8 }} />
                <input
                  className="input"
                  value={outputMarkdownFilenameOverride}
                  onChange={(event) => setOutputMarkdownFilenameOverride(event.target.value)}
                  placeholder="auto (unique)"
                  aria-label="Output Markdown filename"
                  disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                />
                <div style={{ height: 12 }} />

                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={isMathDelimiterConversionEnabled}
                    onChange={(event) => setIsMathDelimiterConversionEnabled(event.target.checked)}
                    disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                  />
                  <span className="toggleLabel">Convert math delimiters to $ / $$</span>
                </label>
                <div style={{ height: 12 }} />

                <details className="details" open>
                  <summary className="detailsSummary">DeepSeek-OCR-2 settings</summary>
                  <div style={{ height: 12 }} />

                  <div className="label">Model revision (optional, recommended for reproducibility)</div>
                  <div style={{ height: 8 }} />
                  <input
                    className="input"
                    value={deepseekOcr2ModelRevision}
                    onChange={(event) => setDeepseekOcr2ModelRevision(event.target.value)}
                    placeholder="(empty = default)"
                    aria-label="DeepSeek OCR2 model revision"
                    disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                  />
                  <div style={{ height: 12 }} />

                  <div className="label">Prompt preset</div>
                  <div style={{ height: 8 }} />
                  <select
                    className="select"
                    value={
                      deepseekOcr2MarkdownPrompt === DEFAULT_DEEPSEEK_OCR2_MARKDOWN_PROMPT
                        ? "markdown"
                        : deepseekOcr2MarkdownPrompt === DEFAULT_DEEPSEEK_OCR2_FREE_OCR_PROMPT
                          ? "free"
                          : "custom"
                    }
                    onChange={(event) => {
                      const selected = event.target.value;
                      if (selected === "markdown") {
                        setDeepseekOcr2MarkdownPrompt(DEFAULT_DEEPSEEK_OCR2_MARKDOWN_PROMPT);
                        return;
                      }
                      if (selected === "free") {
                        setDeepseekOcr2MarkdownPrompt(DEFAULT_DEEPSEEK_OCR2_FREE_OCR_PROMPT);
                        return;
                      }
                      // Guard: keep current prompt for custom.
                    }}
                    disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                    aria-label="Prompt preset"
                  >
                    <option value="markdown">Convert document to markdown</option>
                    <option value="free">Free OCR (no layout)</option>
                    <option value="custom">Custom</option>
                  </select>
                  <div style={{ height: 12 }} />

                  <div className="label">Prompt (supports newlines)</div>
                  <div style={{ height: 8 }} />
                  <textarea
                    className="textarea"
                    value={deepseekOcr2MarkdownPrompt}
                    onChange={(event) => setDeepseekOcr2MarkdownPrompt(event.target.value)}
                    rows={4}
                    aria-label="DeepSeek OCR2 prompt"
                    disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                  />
                  <div style={{ height: 12 }} />

                  <div className="label">Base image size (pixels)</div>
                  <div style={{ height: 8 }} />
                  <input
                    className="input"
                    value={deepseekOcr2BaseImageSizePixelsInput}
                    onChange={(event) => setDeepseekOcr2BaseImageSizePixelsInput(event.target.value)}
                    placeholder={String(DEFAULT_DEEPSEEK_OCR2_BASE_IMAGE_SIZE_PIXELS)}
                    aria-label="DeepSeek base image size pixels"
                    disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                  />
                  <div style={{ height: 12 }} />

                  <div className="label">Inference image size (pixels)</div>
                  <div style={{ height: 8 }} />
                  <input
                    className="input"
                    value={deepseekOcr2InferenceImageSizePixelsInput}
                    onChange={(event) => setDeepseekOcr2InferenceImageSizePixelsInput(event.target.value)}
                    placeholder={String(DEFAULT_DEEPSEEK_OCR2_INFERENCE_IMAGE_SIZE_PIXELS)}
                    aria-label="DeepSeek inference image size pixels"
                    disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                  />
                  <div style={{ height: 12 }} />

                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={isDeepseekOcr2CropModeEnabled}
                      onChange={(event) => setIsDeepseekOcr2CropModeEnabled(event.target.checked)}
                      disabled={!isRunningInsideTauri || jobStatus?.is_running === true}
                    />
                    <span className="toggleLabel">Enable crop mode</span>
                  </label>
                </details>
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
                    Estimated remaining:{" "}
                    {jobStatus?.estimated_time_remaining_seconds !== null &&
                    jobStatus?.estimated_time_remaining_seconds !== undefined
                      ? formatSecondsHuman(jobStatus.estimated_time_remaining_seconds)
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
              </div>
            </div>

            <div className="rightColumn">
              <div className="card previewCard">
                <div className="label">Preview</div>
                <div style={{ height: 8 }} />
                <PreviewPanel
                  preview={currentTaskPreview}
                  previewImageUrl={currentTaskPreviewImageUrl}
                  backendLogLines={backendLogLines}
                  fallbackInferenceImageSizePixels={previewFallbackInferenceImageSizePixels}
                />
              </div>

              <div className="card logCard">
                <div className="label">Logs</div>
                <div style={{ height: 10 }} />
                <div className="logCardBody">
                  <LogViewer entries={logEntries} isRunning={jobStatus?.is_running === true} onClearView={handleClearLogView} />
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

