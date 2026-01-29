/*!
Responsibility:
- Provide backend commands for the ocr-agent Tauri GUI:
  - Choose job output directory (job root)
  - Copy dropped inputs into job root
  - Run docker-compose based OCR (enqueue -> run)
  - Provide progress (via SQLite queue) + recent logs
  - Cancel a running job
*/

use std::{
  collections::{HashMap, VecDeque},
  ffi::OsStr,
  fs,
  io::{BufRead, BufReader},
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  sync::{Arc, Mutex},
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::{State, Wry};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use tauri_plugin_dialog::FilePath;

mod watch_folder;
use watch_folder::{
  default_poll_interval as default_watch_poll_interval,
  get_watch_folder_status as get_watch_folder_status_from_state,
  list_ready_bundle_directories,
  mark_bundle_failed,
  mark_bundle_processed,
  new_shared_watch_folder_state,
  start_watch_folder as start_watch_folder_with_callback,
  stop_watch_folder as stop_watch_folder_internal,
  try_lock_bundle_for_processing,
  SharedWatchFolderRuntimeState,
  WatchFolderConfig,
  WatchFolderStatus,
};

const DEFAULT_QUEUE_DATABASE_FILENAME: &str = "queue.sqlite3";
const DEFAULT_INPUT_DIRECTORY_NAME: &str = "input";
const DEFAULT_OUTPUT_DIRECTORY_NAME: &str = "output";
const DEFAULT_OUTPUT_MARKDOWN_FILENAME_EXTENSION: &str = ".md";
const DEFAULT_OUTPUT_MARKDOWN_FILENAME_PREFIX: &str = "ocr_output_";

const DEFAULT_JOB_SETTINGS_DIRECTORY_NAME: &str = ".ocr-agent";
const DEFAULT_JOB_SETTINGS_FILENAME: &str = "job.json";

const MAX_LOG_LINES: usize = 1500;
const MAX_COPY_COLLISION_ATTEMPTS: u32 = 1000;
const DOCKER_COMPOSE_SERVICE_NAME: &str = "ocr-agent";
const OCR_AGENT_REPO_ROOT_ENVIRONMENT_VARIABLE_NAME: &str = "OCR_AGENT_REPO_ROOT";
const MAX_PREVIEW_IMAGE_BYTES: u64 = 8_000_000;
const MAX_REPO_ROOT_SEARCH_DEPTH: usize = 8;

const DEFAULT_WATCH_JOBS_DIRECTORY_NAME: &str = "jobs";
const DEFAULT_WATCH_JOB_STATE_FILENAME: &str = "job_state.json";
const DEFAULT_WATCH_READY_FILENAME: &str = ".ready";

const OCR_AGENT_WATCH_INBOX_ENVIRONMENT_VARIABLE_NAME: &str = "OCR_AGENT_WATCH_INBOX";
const OCR_AGENT_WATCH_JOBS_ROOT_ENVIRONMENT_VARIABLE_NAME: &str = "OCR_AGENT_WATCH_JOBS_ROOT";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JobSettings {
  output_markdown_filename_override: Option<String>,
  last_output_markdown_filename: Option<String>,
  is_math_delimiter_conversion_enabled: Option<bool>,
  deepseek_ocr2_model_revision: Option<String>,
  deepseek_ocr2_markdown_prompt: Option<String>,
  deepseek_ocr2_base_image_size_pixels: Option<u32>,
  deepseek_ocr2_inference_image_size_pixels: Option<u32>,
  deepseek_ocr2_enable_crop_mode: Option<bool>,
}

fn job_settings_directory_path(job_root_directory_path: &Path) -> PathBuf {
  job_root_directory_path.join(DEFAULT_JOB_SETTINGS_DIRECTORY_NAME)
}

fn job_settings_file_path(job_root_directory_path: &Path) -> PathBuf {
  job_settings_directory_path(job_root_directory_path).join(DEFAULT_JOB_SETTINGS_FILENAME)
}

fn read_job_settings_best_effort(job_root_directory_path: &Path) -> JobSettings {
  let settings_path = job_settings_file_path(job_root_directory_path);
  if !settings_path.exists() {
    return JobSettings::default();
  }
  let Ok(raw) = fs::read_to_string(&settings_path) else {
    return JobSettings::default();
  };
  serde_json::from_str::<JobSettings>(&raw).unwrap_or_default()
}

fn write_job_settings(job_root_directory_path: &Path, settings: &JobSettings) -> Result<(), String> {
  let settings_directory_path = job_settings_directory_path(job_root_directory_path);
  fs::create_dir_all(&settings_directory_path).map_err(|error| error.to_string())?;
  let settings_path = job_settings_file_path(job_root_directory_path);
  let serialized = serde_json::to_string_pretty(settings).map_err(|error| error.to_string())?;
  fs::write(settings_path, serialized).map_err(|error| error.to_string())?;
  Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct JobStatus {
  job_root_directory_path: String,
  is_running: bool,
  start_unix_timestamp_millis: Option<i64>,
  total_tasks: i64,
  pending_tasks: i64,
  running_tasks: i64,
  completed_tasks: i64,
  failed_tasks: i64,
  last_error_message: Option<String>,
  estimated_time_remaining_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct JobLogResponse {
  lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CurrentTaskPreview {
  task_id: i64,
  task_kind: String,
  source_path: String,
  pdf_page_index: Option<i64>,
  pdf_total_pages: Option<i64>,
  preview_image_file_path: Option<String>,
  deepseek_inference_image_size_pixels: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct PreviewImageBytes {
  mime_type: String,
  bytes: Vec<u8>,
}

#[derive(Debug)]
struct RunningJobHandle {
  child: Arc<Mutex<Child>>,
  start_unix_timestamp_millis: i64,
}

#[derive(Default)]
struct JobRuntimeState {
  running_job_by_root: HashMap<PathBuf, RunningJobHandle>,
  log_lines_by_root: HashMap<PathBuf, VecDeque<String>>,
  job_state_file_path_by_root: HashMap<PathBuf, PathBuf>,
}

type SharedJobRuntimeState = Arc<Mutex<JobRuntimeState>>;

fn now_unix_timestamp_millis() -> i64 {
  let duration_since_epoch = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or(Duration::from_secs(0));
  duration_since_epoch.as_millis() as i64
}

fn ensure_job_directory_layout(job_root_directory_path: &Path) -> Result<(), String> {
  if job_root_directory_path.as_os_str().is_empty() {
    // Guard: refusing to operate on an empty path.
    return Err("job_root_directory_path is empty".to_string());
  }
  if !job_root_directory_path.exists() {
    // Guard: output directory must exist (user selected it).
    return Err("Selected output directory does not exist.".to_string());
  }
  if !job_root_directory_path.is_dir() {
    // Guard: output directory must be a directory.
    return Err("Selected output path is not a directory.".to_string());
  }

  let input_directory_path = job_root_directory_path.join(DEFAULT_INPUT_DIRECTORY_NAME);
  let output_directory_path = job_root_directory_path.join(DEFAULT_OUTPUT_DIRECTORY_NAME);
  fs::create_dir_all(&input_directory_path).map_err(|error| error.to_string())?;
  fs::create_dir_all(&output_directory_path).map_err(|error| error.to_string())?;
  fs::create_dir_all(job_settings_directory_path(job_root_directory_path)).map_err(|error| error.to_string())?;
  Ok(())
}

fn normalize_windows_path_lossy(path: &Path) -> String {
  let raw = path.to_string_lossy().to_string();
  if !cfg!(target_os = "windows") {
    return raw;
  }

  // Guard: std::fs::canonicalize can yield verbatim paths like \\?\C:\... which Docker can't parse in volume specs.
  if let Some(stripped) = raw.strip_prefix(r"\\?\") {
    if let Some(unc_stripped) = stripped.strip_prefix(r"UNC\") {
      return format!(r"\\{}", unc_stripped);
    }
    return stripped.to_string();
  }
  raw
}

fn normalize_windows_path_buf(path: &Path) -> PathBuf {
  PathBuf::from(normalize_windows_path_lossy(path))
}

fn file_path_to_string(file_path: FilePath) -> String {
  match file_path {
    FilePath::Path(path) => path.to_string_lossy().to_string(),
    FilePath::Url(url) => url.to_string(),
  }
}

fn repo_root_path() -> Result<PathBuf, String> {
  if let Ok(configured_repo_root) = std::env::var(OCR_AGENT_REPO_ROOT_ENVIRONMENT_VARIABLE_NAME) {
    let configured_repo_root = configured_repo_root.trim().to_string();
    if configured_repo_root.is_empty() {
      // Guard: ignore empty env var.
      return Err(format!(
        "{OCR_AGENT_REPO_ROOT_ENVIRONMENT_VARIABLE_NAME} is set but empty"
      ));
    }
    let configured_path = PathBuf::from(configured_repo_root);
    let canonical = configured_path
      .canonicalize()
      .map_err(|error| format!("Failed to canonicalize OCR_AGENT_REPO_ROOT: {error}"))?;
    return Ok(normalize_windows_path_buf(&canonical));
  }

  // Guard: support running the GUI binary from outside the repo by searching upward from the executable.
  if let Ok(exe_path) = std::env::current_exe() {
    if let Some(exe_directory_path) = exe_path.parent() {
      if let Some(found) = find_repo_root_by_walking_up(exe_directory_path) {
        return Ok(found);
      }
    }
  }

  let manifest_directory_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let repo_root_candidate = manifest_directory_path
    .parent()
    .and_then(|path| path.parent())
    .ok_or_else(|| "Failed to infer repo root from CARGO_MANIFEST_DIR".to_string())?;
  let canonical = repo_root_candidate
    .canonicalize()
    .map_err(|error| format!("Failed to canonicalize repo root: {error}"))?;
  Ok(normalize_windows_path_buf(&canonical))
}

fn find_repo_root_by_walking_up(start_directory_path: &Path) -> Option<PathBuf> {
  let mut current = start_directory_path.to_path_buf();
  for _ in 0..MAX_REPO_ROOT_SEARCH_DEPTH {
    let compose_candidate = current.join("compose.yaml");
    if compose_candidate.exists() {
      let canonical = current.canonicalize().ok()?;
      return Some(normalize_windows_path_buf(&canonical));
    }
    let parent = current.parent()?;
    current = parent.to_path_buf();
  }
  None
}

fn compose_file_path(repo_root: &Path) -> PathBuf {
  repo_root.join("compose.yaml")
}

fn build_docker_compose_base_command(repo_root: &Path) -> Command {
  let mut command = Command::new("docker");
  command.arg("compose");
  command.arg("-f");
  command.arg(compose_file_path(repo_root));
  command.arg("--project-directory");
  command.arg(repo_root);
  command
}

fn derive_compose_project_name(repo_root: &Path) -> String {
  repo_root
    .file_name()
    .and_then(|name| name.to_str())
    .map(|name| name.to_string())
    .unwrap_or_else(|| "ocr-agent".to_string())
}

fn derive_compose_service_image_name(repo_root: &Path, service_name: &str) -> String {
  // Compose default: {project}-{service}:latest
  // Example for this repo: ocr-agent-ocr-agent:latest
  let project_name = derive_compose_project_name(repo_root);
  format!("{project_name}-{service_name}:latest")
}

fn validate_docker_available() -> Result<(), String> {
  let output = Command::new("docker")
    .arg("version")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .map_err(|error| format!("Failed to run docker. Is Docker Desktop installed? {error}"))?;

  if output.status.success() {
    return Ok(());
  }

  let stderr = String::from_utf8_lossy(&output.stderr).to_string();
  Err(format!("Docker is not available.\n{stderr}"))
}

#[tauri::command]
fn probe_docker() -> Result<(), String> {
  validate_docker_available()?;

  let repo_root = repo_root_path()?;
  let compose_path = compose_file_path(&repo_root);
  if !compose_path.exists() {
    // Guard: without compose.yaml we cannot run the OCR engine.
    return Err(format!(
      "compose.yaml not found at: {}\nSet {OCR_AGENT_REPO_ROOT_ENVIRONMENT_VARIABLE_NAME} to your repo root.",
      compose_path.display()
    ));
  }

  let output = Command::new("docker")
    .arg("compose")
    .arg("version")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .map_err(|error| format!("Failed to run docker compose. {error}"))?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    return Err(format!("docker compose is not available.\n{stderr}"));
  }

  // Guard: give a fast, actionable error if the image isn't built yet.
  // NOTE:
  // `docker compose images` can return an empty list unless containers were created, so we instead
  // check the derived image name Compose uses by default.
  let derived_image_name = derive_compose_service_image_name(&repo_root, DOCKER_COMPOSE_SERVICE_NAME);
  let inspect_output = Command::new("docker")
    .arg("image")
    .arg("inspect")
    .arg(&derived_image_name)
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .output();

  if let Ok(inspect_output) = inspect_output {
    if !inspect_output.status.success() {
      return Err(format!(
        "Docker image for `{DOCKER_COMPOSE_SERVICE_NAME}` is not built.\nExpected image: {derived_image_name}\nRun: docker compose -f \"{}\" build",
        compose_path.display()
      ));
    }
  }

  Ok(())
}

#[tauri::command]
fn probe_gpu_passthrough() -> Result<String, String> {
  validate_docker_available()?;
  let repo_root = repo_root_path()?;

  let output = build_docker_compose_base_command(&repo_root)
    .arg("run")
    .arg("--rm")
    .arg(DOCKER_COMPOSE_SERVICE_NAME)
    .arg("nvidia-smi")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .map_err(|error| format!("Failed to run GPU probe (nvidia-smi). {error}"))?;

  if output.status.success() {
    return Ok(String::from_utf8_lossy(&output.stdout).to_string());
  }

  let stderr = String::from_utf8_lossy(&output.stderr).to_string();
  Err(format!(
    "GPU probe failed. Verify Docker Desktop GPU support and WSL2 GPU drivers.\n{stderr}"
  ))
}

#[tauri::command]
fn get_watch_folder_status(
  watch_folder_state: State<'_, SharedWatchFolderRuntimeState>,
) -> Result<WatchFolderStatus, String> {
  Ok(get_watch_folder_status_from_state(watch_folder_state.inner()))
}

#[tauri::command]
fn stop_watch_folder(watch_folder_state: State<'_, SharedWatchFolderRuntimeState>) -> Result<(), String> {
  stop_watch_folder_internal(watch_folder_state.inner());
  Ok(())
}

#[tauri::command]
fn start_watch_folder(
  inbox_directory_path: String,
  jobs_root_directory_path: Option<String>,
  job_runtime_state: State<'_, SharedJobRuntimeState>,
  watch_folder_state: State<'_, SharedWatchFolderRuntimeState>,
) -> Result<(), String> {
  let inbox_directory_path = PathBuf::from(inbox_directory_path);
  let jobs_root_directory_path = jobs_root_directory_path
    .and_then(|raw| {
      let trimmed = raw.trim().to_string();
      if trimmed.is_empty() {
        return None;
      }
      Some(trimmed)
    })
    .map(PathBuf::from)
    .unwrap_or_else(|| inbox_directory_path.join(DEFAULT_WATCH_JOBS_DIRECTORY_NAME));

  let config = WatchFolderConfig {
    inbox_directory_path,
    jobs_root_directory_path,
    poll_interval: default_watch_poll_interval(),
  };

  let poll_callback = make_watch_folder_poll_callback(job_runtime_state.inner().clone());

  start_watch_folder_with_callback(watch_folder_state.inner(), config, poll_callback)?;
  Ok(())
}

#[tauri::command]
async fn pick_output_directory(app_handle: tauri::AppHandle<Wry>) -> Result<Option<String>, String> {
  let (sender, receiver) = oneshot::channel::<Option<tauri_plugin_dialog::FilePath>>();
  app_handle.dialog().file().pick_folder(move |path| {
    // Guard: receiver side may be dropped if the request is cancelled.
    let _ = sender.send(path);
  });

  let selected_directory_path = receiver
    .await
    .map_err(|_| "Failed to receive folder picker result".to_string())?;

  let Some(directory_path) = selected_directory_path else {
    return Ok(None);
  };

  Ok(Some(file_path_to_string(directory_path)))
}

#[tauri::command]
async fn pick_directory(app_handle: tauri::AppHandle<Wry>) -> Result<Option<String>, String> {
  let (sender, receiver) = oneshot::channel::<Option<tauri_plugin_dialog::FilePath>>();
  app_handle.dialog().file().pick_folder(move |path| {
    // Guard: receiver side may be dropped if the request is cancelled.
    let _ = sender.send(path);
  });

  let selected_directory_path = receiver
    .await
    .map_err(|_| "Failed to receive folder picker result".to_string())?;

  let Some(directory_path) = selected_directory_path else {
    return Ok(None);
  };

  Ok(Some(file_path_to_string(directory_path)))
}

#[tauri::command]
async fn pick_input_files(app_handle: tauri::AppHandle<Wry>) -> Result<Option<Vec<String>>, String> {
  let (sender, receiver) = oneshot::channel::<Option<Vec<FilePath>>>();
  app_handle.dialog().file().pick_files(move |paths| {
    // Guard: receiver side may be dropped if the request is cancelled.
    let _ = sender.send(paths);
  });

  let selected_paths = receiver
    .await
    .map_err(|_| "Failed to receive file picker result".to_string())?;

  let Some(selected_paths) = selected_paths else {
    return Ok(None);
  };
  Ok(Some(selected_paths.into_iter().map(file_path_to_string).collect()))
}

#[tauri::command]
async fn pick_input_folder(app_handle: tauri::AppHandle<Wry>) -> Result<Option<String>, String> {
  let (sender, receiver) = oneshot::channel::<Option<FilePath>>();
  app_handle.dialog().file().pick_folder(move |path| {
    // Guard: receiver side may be dropped if the request is cancelled.
    let _ = sender.send(path);
  });

  let selected_path = receiver
    .await
    .map_err(|_| "Failed to receive folder picker result".to_string())?;

  let Some(selected_path) = selected_path else {
    return Ok(None);
  };
  Ok(Some(file_path_to_string(selected_path)))
}

fn sanitize_filename_for_copy(candidate_filename: &OsStr) -> String {
  let filename_string = candidate_filename.to_string_lossy().to_string();
  if filename_string.trim().is_empty() {
    // Guard: use a stable fallback name when filename is empty.
    return "input".to_string();
  }
  filename_string
    .replace('\\', "_")
    .replace('/', "_")
    .replace(':', "_")
}

fn split_filename_and_extension(filename: &str) -> (String, String) {
  let Some(dot_index) = filename.rfind('.') else {
    return (filename.to_string(), "".to_string());
  };
  if dot_index == 0 {
    // Guard: hidden files like ".env" are treated as "no extension" for collision suffixing.
    return (filename.to_string(), "".to_string());
  }
  let (stem, extension_with_dot) = filename.split_at(dot_index);
  (stem.to_string(), extension_with_dot.to_string())
}

fn sanitize_output_markdown_filename(user_input: &str) -> String {
  let trimmed = user_input.trim();
  if trimmed.is_empty() {
    // Guard: fallback to a stable base name when empty.
    return "output".to_string();
  }
  let mut sanitized = trimmed.to_string();
  sanitized = sanitized
    .replace('\\', "_")
    .replace('/', "_")
    .replace(':', "_")
    .replace('\n', "_")
    .replace('\r', "_")
    .replace('\t', "_")
    .replace(' ', "_");
  sanitized
}

fn ensure_markdown_extension(filename: &str) -> String {
  let lower = filename.to_lowercase();
  if lower.ends_with(".md") || lower.ends_with(".markdown") {
    return filename.to_string();
  }
  format!("{filename}{DEFAULT_OUTPUT_MARKDOWN_FILENAME_EXTENSION}")
}

fn derive_non_conflicting_markdown_output_path(
  job_root_directory_path: &Path,
  desired_filename: &str,
) -> Result<PathBuf, String> {
  derive_non_conflicting_destination_path(job_root_directory_path, desired_filename)
}

fn derive_default_unique_markdown_filename() -> String {
  // Guard: avoid collisions by embedding a timestamp.
  format!("{DEFAULT_OUTPUT_MARKDOWN_FILENAME_PREFIX}{}.md", now_unix_timestamp_millis())
}

fn derive_non_conflicting_destination_path(
  destination_directory_path: &Path,
  desired_filename: &str,
) -> Result<PathBuf, String> {
  let desired_path = destination_directory_path.join(desired_filename);
  if !desired_path.exists() {
    return Ok(desired_path);
  }

  let (stem, extension_with_dot) = split_filename_and_extension(desired_filename);
  for suffix_number in 2..=MAX_COPY_COLLISION_ATTEMPTS {
    let candidate_filename = format!("{stem}_{suffix_number}{extension_with_dot}");
    let candidate_path = destination_directory_path.join(candidate_filename);
    if !candidate_path.exists() {
      return Ok(candidate_path);
    }
  }

  Err(format!(
    "Too many name collisions while copying into: {} (base name: {desired_filename})",
    destination_directory_path.display()
  ))
}

fn copy_directory_recursively(source_directory_path: &Path, destination_directory_path: &Path) -> Result<u64, String> {
  if !source_directory_path.exists() {
    // Guard: do not silently ignore missing paths.
    return Err(format!("Input directory does not exist: {}", source_directory_path.display()));
  }
  if !source_directory_path.is_dir() {
    // Guard: this function only handles directories.
    return Err(format!("Not a directory: {}", source_directory_path.display()));
  }

  fs::create_dir_all(destination_directory_path).map_err(|error| error.to_string())?;

  let mut total_copied_files: u64 = 0;
  for entry in walkdir::WalkDir::new(source_directory_path) {
    let entry = entry.map_err(|error| error.to_string())?;
    let entry_path = entry.path();
    if entry_path.is_dir() {
      continue;
    }

    let relative_path = entry_path
      .strip_prefix(source_directory_path)
      .map_err(|error| error.to_string())?;

    let destination_path = destination_directory_path.join(relative_path);
    if let Some(parent_directory_path) = destination_path.parent() {
      fs::create_dir_all(parent_directory_path).map_err(|error| error.to_string())?;
    }

    fs::copy(entry_path, &destination_path).map_err(|error| error.to_string())?;
    total_copied_files += 1;
  }

  Ok(total_copied_files)
}

#[tauri::command]
fn job_add_inputs(job_root_directory_path: String, input_paths: Vec<String>) -> Result<(), String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  ensure_job_directory_layout(&job_root_directory_path)?;

  let input_directory_path = job_root_directory_path.join(DEFAULT_INPUT_DIRECTORY_NAME);
  fs::create_dir_all(&input_directory_path).map_err(|error| error.to_string())?;

  for input_path_string in input_paths {
    let input_path = PathBuf::from(input_path_string);
    if !input_path.exists() {
      // Guard: surface missing paths explicitly.
      return Err(format!("Dropped path does not exist: {}", input_path.display()));
    }

    if input_path.is_file() {
      let file_name = input_path
        .file_name()
        .map(sanitize_filename_for_copy)
        .unwrap_or_else(|| "input_file".to_string());

      let destination_path = derive_non_conflicting_destination_path(&input_directory_path, &file_name)?;
      fs::copy(&input_path, &destination_path).map_err(|error| error.to_string())?;
      continue;
    }

    if input_path.is_dir() {
      let directory_name = input_path
        .file_name()
        .map(sanitize_filename_for_copy)
        .unwrap_or_else(|| "input_directory".to_string());

      let destination_directory_path =
        derive_non_conflicting_destination_path(&input_directory_path, &directory_name)?;
      let _ = copy_directory_recursively(&input_path, &destination_directory_path)?;
      continue;
    }

    // Guard: unknown filesystem entry.
    return Err(format!("Unsupported dropped path type: {}", input_path.display()));
  }

  Ok(())
}

fn get_queue_database_path(job_root_directory_path: &Path) -> PathBuf {
  job_root_directory_path.join(DEFAULT_QUEUE_DATABASE_FILENAME)
}

fn query_current_running_task(queue_database_path: &Path) -> Result<Option<CurrentTaskPreview>, String> {
  if !queue_database_path.exists() {
    // Guard: queue might not exist until enqueue has run.
    return Ok(None);
  }

  let connection = Connection::open(queue_database_path).map_err(|error| error.to_string())?;
  let mut statement = connection
    .prepare(
      "SELECT task_id, task_kind, source_path, pdf_page_index, pdf_total_pages \
       FROM tasks WHERE status = 'running' ORDER BY task_id ASC LIMIT 1",
    )
    .map_err(|error| error.to_string())?;
  let mut rows = statement.query([]).map_err(|error| error.to_string())?;
  let Some(row) = rows.next().map_err(|error| error.to_string())? else {
    return Ok(None);
  };

  let task_id: i64 = row.get(0).map_err(|error| error.to_string())?;
  let task_kind: String = row.get(1).map_err(|error| error.to_string())?;
  let source_path: String = row.get(2).map_err(|error| error.to_string())?;
  let pdf_page_index: Option<i64> = row.get(3).map_err(|error| error.to_string())?;
  let pdf_total_pages: Option<i64> = row.get(4).map_err(|error| error.to_string())?;

  Ok(Some(CurrentTaskPreview {
    task_id,
    task_kind,
    source_path,
    pdf_page_index,
    pdf_total_pages,
    preview_image_file_path: None,
    deepseek_inference_image_size_pixels: None,
  }))
}

fn resolve_preview_image_path_for_task(job_root_directory_path: &Path, task: &CurrentTaskPreview) -> Option<PathBuf> {
  let task_kind_lower = task.task_kind.to_lowercase();
  if task_kind_lower == "image" {
    if let Some(relative) = task.source_path.strip_prefix("/data/input/") {
      return Some(job_root_directory_path.join(DEFAULT_INPUT_DIRECTORY_NAME).join(relative));
    }
    return Some(PathBuf::from(&task.source_path));
  }

  if task_kind_lower != "pdf_page" {
    return None;
  }
  let pdf_page_index = task.pdf_page_index?;
  let page_number_human = pdf_page_index + 1;
  let work_directory_path = job_root_directory_path
    .join(DEFAULT_OUTPUT_DIRECTORY_NAME)
    .join("work");
  Some(work_directory_path.join(format!(
    "pdf_{}_page_{}.png",
    task.task_id, page_number_human
  )))
}

fn infer_image_mime_type(image_file_path: &Path) -> String {
  let extension = image_file_path
    .extension()
    .and_then(|ext| ext.to_str())
    .unwrap_or("")
    .to_lowercase();
  if extension == "png" {
    return "image/png".to_string();
  }
  if extension == "jpg" || extension == "jpeg" {
    return "image/jpeg".to_string();
  }
  if extension == "webp" {
    return "image/webp".to_string();
  }
  if extension == "bmp" {
    return "image/bmp".to_string();
  }
  if extension == "gif" {
    return "image/gif".to_string();
  }
  "application/octet-stream".to_string()
}

fn query_status_counts(queue_database_path: &Path) -> Result<HashMap<String, i64>, String> {
  if !queue_database_path.exists() {
    // Guard: queue might not exist until enqueue has run.
    return Ok(HashMap::new());
  }

  let connection = Connection::open(queue_database_path).map_err(|error| error.to_string())?;
  let mut statement = connection
    .prepare("SELECT status, COUNT(*) FROM tasks GROUP BY status")
    .map_err(|error| error.to_string())?;

  let mut counts_by_status: HashMap<String, i64> = HashMap::new();
  let mut rows = statement.query([]).map_err(|error| error.to_string())?;
  while let Some(row) = rows.next().map_err(|error| error.to_string())? {
    let status: String = row.get(0).map_err(|error| error.to_string())?;
    let count: i64 = row.get(1).map_err(|error| error.to_string())?;
    counts_by_status.insert(status, count);
  }

  Ok(counts_by_status)
}

fn query_last_error_message(queue_database_path: &Path) -> Result<Option<String>, String> {
  if !queue_database_path.exists() {
    return Ok(None);
  }

  let connection = Connection::open(queue_database_path).map_err(|error| error.to_string())?;
  let mut statement = connection
    .prepare(
      "SELECT error_message FROM tasks WHERE status = 'failed' AND error_message IS NOT NULL ORDER BY task_id DESC LIMIT 1",
    )
    .map_err(|error| error.to_string())?;

  let mut rows = statement.query([]).map_err(|error| error.to_string())?;
  let Some(row) = rows.next().map_err(|error| error.to_string())? else {
    return Ok(None);
  };
  let error_message: String = row.get(0).map_err(|error| error.to_string())?;
  Ok(Some(error_message))
}

fn compute_estimated_time_remaining_seconds(
  start_unix_timestamp_millis: Option<i64>,
  total_tasks: i64,
  completed_tasks: i64,
) -> Option<i64> {
  let Some(start_millis) = start_unix_timestamp_millis else {
    // Guard: no start time available yet.
    return None;
  };

  if total_tasks <= 0 {
    // Guard: avoid division by zero.
    return None;
  }
  if completed_tasks <= 0 {
    // Guard: no samples yet.
    return None;
  }

  let elapsed_millis = now_unix_timestamp_millis().saturating_sub(start_millis);
  if elapsed_millis <= 0 {
    return None;
  }

  let average_millis_per_task = elapsed_millis / completed_tasks;
  let remaining_tasks = total_tasks.saturating_sub(completed_tasks);
  let remaining_millis = average_millis_per_task.saturating_mul(remaining_tasks);
  Some((remaining_millis / 1000).max(0))
}

#[tauri::command]
fn get_job_status(
  job_root_directory_path: String,
  job_runtime_state: State<'_, SharedJobRuntimeState>,
) -> Result<JobStatus, String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  ensure_job_directory_layout(&job_root_directory_path)?;

  let queue_database_path = get_queue_database_path(&job_root_directory_path);
  let counts_by_status = query_status_counts(&queue_database_path)?;
  let pending_tasks = *counts_by_status.get("pending").unwrap_or(&0);
  let running_tasks = *counts_by_status.get("running").unwrap_or(&0);
  let completed_tasks = *counts_by_status.get("completed").unwrap_or(&0);
  let failed_tasks = *counts_by_status.get("failed").unwrap_or(&0);
  let total_tasks = pending_tasks + running_tasks + completed_tasks + failed_tasks;

  let (is_running, start_unix_timestamp_millis) = {
    let locked_state = job_runtime_state.lock().map_err(|_| "State lock poisoned".to_string())?;
    let running_handle = locked_state.running_job_by_root.get(&job_root_directory_path);
    match running_handle {
      None => (false, None),
      Some(handle) => (true, Some(handle.start_unix_timestamp_millis)),
    }
  };

  let estimated_time_remaining_seconds = compute_estimated_time_remaining_seconds(
    start_unix_timestamp_millis,
    total_tasks,
    completed_tasks,
  );
  let last_error_message = query_last_error_message(&queue_database_path)?;

  Ok(JobStatus {
    job_root_directory_path: job_root_directory_path.to_string_lossy().to_string(),
    is_running,
    start_unix_timestamp_millis,
    total_tasks,
    pending_tasks,
    running_tasks,
    completed_tasks,
    failed_tasks,
    last_error_message,
    estimated_time_remaining_seconds,
  })
}

fn append_log_line(job_runtime_state: &SharedJobRuntimeState, job_root_directory_path: &Path, line: String) {
  let mut locked_state = match job_runtime_state.lock() {
    Ok(state) => state,
    Err(_) => return,
  };

  let lines = locked_state
    .log_lines_by_root
    .entry(job_root_directory_path.to_path_buf())
    .or_insert_with(VecDeque::new);
  lines.push_back(line);
  while lines.len() > MAX_LOG_LINES {
    lines.pop_front();
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JobStateStatus {
  Queued,
  Running,
  Completed,
  Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobState {
  status: JobStateStatus,
  job_id: String,
  job_root_directory_path: String,
  source_bundle_directory_path: Option<String>,
  accepted_unix_timestamp_millis: i64,
  started_unix_timestamp_millis: Option<i64>,
  finished_unix_timestamp_millis: Option<i64>,
  output_markdown_path: Option<String>,
  error_message: Option<String>,
}

fn job_state_file_path(job_root_directory_path: &Path) -> PathBuf {
  job_root_directory_path.join(DEFAULT_WATCH_JOB_STATE_FILENAME)
}

fn write_job_state(job_root_directory_path: &Path, state: &JobState) -> Result<(), String> {
  let serialized = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
  fs::write(job_state_file_path(job_root_directory_path), serialized).map_err(|error| error.to_string())?;
  Ok(())
}

fn read_job_state_best_effort(job_root_directory_path: &Path) -> Option<JobState> {
  let path = job_state_file_path(job_root_directory_path);
  let raw = fs::read_to_string(path).ok()?;
  serde_json::from_str::<JobState>(&raw).ok()
}

fn spawn_log_reader_thread(
  job_runtime_state: SharedJobRuntimeState,
  job_root_directory_path: PathBuf,
  stream: impl std::io::Read + Send + 'static,
  stream_name: &'static str,
) {
  std::thread::spawn(move || {
    let reader = BufReader::new(stream);
    for line_result in reader.lines() {
      let Ok(line) = line_result else {
        continue;
      };
      append_log_line(
        &job_runtime_state,
        &job_root_directory_path,
        format!("[{stream_name}] {line}"),
      );
    }
  });
}

fn spawn_job_process(job_runtime_state: SharedJobRuntimeState, job_root_directory_path: PathBuf) -> Result<(), String> {
  let repo_root = repo_root_path()?;
  let job_root_canonical = job_root_directory_path
    .canonicalize()
    .map_err(|error| format!("Failed to canonicalize job root: {error}"))?;
  let job_root_for_docker = normalize_windows_path_lossy(&job_root_canonical);

  // NOTE: We cannot rely on shell operators without invoking a shell. Use `bash -lc` inside container.
  let mut command = build_docker_compose_base_command(&repo_root);
  command.arg("run");
  command.arg("--rm");
  let settings = read_job_settings_best_effort(&job_root_directory_path);

  let is_math_delimiter_conversion_enabled = settings.is_math_delimiter_conversion_enabled.unwrap_or(true);
  let math_delimiter_style = if is_math_delimiter_conversion_enabled {
    "dollar"
  } else {
    "latex"
  };
  command.arg("-e");
  command.arg(format!("OCR_AGENT_MATH_DELIMITER_STYLE={math_delimiter_style}"));

  if let Some(model_revision) = settings.deepseek_ocr2_model_revision.as_deref() {
    let trimmed = model_revision.trim();
    if !trimmed.is_empty() {
      command.arg("-e");
      command.arg(format!("DEEPSEEK_OCR2_MODEL_REVISION={trimmed}"));
    }
  }
  if let Some(markdown_prompt) = settings.deepseek_ocr2_markdown_prompt.as_deref() {
    let encoded_prompt = markdown_prompt.replace("\r\n", "\n").replace('\n', "\\n");
    command.arg("-e");
    command.arg(format!("DEEPSEEK_OCR2_MARKDOWN_PROMPT={encoded_prompt}"));
  }
  if let Some(base_size_pixels) = settings.deepseek_ocr2_base_image_size_pixels {
    command.arg("-e");
    command.arg(format!("DEEPSEEK_OCR2_BASE_IMAGE_SIZE_PIXELS={base_size_pixels}"));
  }
  if let Some(image_size_pixels) = settings.deepseek_ocr2_inference_image_size_pixels {
    command.arg("-e");
    command.arg(format!("DEEPSEEK_OCR2_INFERENCE_IMAGE_SIZE_PIXELS={image_size_pixels}"));
  }
  if let Some(enable_crop_mode) = settings.deepseek_ocr2_enable_crop_mode {
    command.arg("-e");
    command.arg(format!(
      "DEEPSEEK_OCR2_ENABLE_CROP_MODE={}",
      if enable_crop_mode { "1" } else { "0" }
    ));
  }

  command.arg("-v");
  command.arg(format!("{job_root_for_docker}:/data"));
  command.arg(DOCKER_COMPOSE_SERVICE_NAME);
  command.arg("bash");
  command.arg("-lc");
  let desired_output_filename = match settings.output_markdown_filename_override.as_deref() {
    None => derive_default_unique_markdown_filename(),
    Some(filename) => ensure_markdown_extension(&sanitize_output_markdown_filename(filename)),
  };
  let output_markdown_path = derive_non_conflicting_markdown_output_path(
    &job_root_directory_path,
    &desired_output_filename,
  )?;
  let output_markdown_filename = output_markdown_path
    .file_name()
    .and_then(|name| name.to_str())
    .ok_or_else(|| "Failed to derive output markdown filename".to_string())?
    .to_string();

  let mut updated_settings = settings.clone();
  updated_settings.last_output_markdown_filename = Some(output_markdown_filename.clone());
  write_job_settings(&job_root_directory_path, &updated_settings)?;

  command.arg(format!(
    "python3 -m ocr_agent.cli enqueue /data/input && python3 -m ocr_agent.cli run --output-md \"/data/{output_markdown_filename}\""
  ));
  command.stdout(Stdio::piped());
  command.stderr(Stdio::piped());

  let mut child = command.spawn().map_err(|error| {
    format!(
      "Failed to start docker compose job. Is the image built and GPU enabled?\n{error}"
    )
  })?;

  let stdout = child.stdout.take();
  let stderr = child.stderr.take();

  let start_unix_timestamp_millis = now_unix_timestamp_millis();
  let child_handle = Arc::new(Mutex::new(child));

  {
    let mut locked_state = job_runtime_state.lock().map_err(|_| "State lock poisoned".to_string())?;
    if locked_state.running_job_by_root.contains_key(&job_root_directory_path) {
      // Guard: refuse to start two jobs for the same directory.
      return Err("A job is already running for this output directory.".to_string());
    }
    locked_state.running_job_by_root.insert(
      job_root_directory_path.clone(),
      RunningJobHandle {
        child: child_handle.clone(),
        start_unix_timestamp_millis,
      },
    );
    locked_state
      .log_lines_by_root
      .entry(job_root_directory_path.clone())
      .or_insert_with(VecDeque::new);

    // Guard: watcher-created jobs track their state in a separate file.
    if locked_state
      .job_state_file_path_by_root
      .contains_key(&job_root_directory_path)
    {
      let mut state = read_job_state_best_effort(&job_root_directory_path).unwrap_or(JobState {
        status: JobStateStatus::Queued,
        job_id: "unknown".to_string(),
        job_root_directory_path: job_root_directory_path.to_string_lossy().to_string(),
        source_bundle_directory_path: None,
        accepted_unix_timestamp_millis: now_unix_timestamp_millis(),
        started_unix_timestamp_millis: None,
        finished_unix_timestamp_millis: None,
        output_markdown_path: None,
        error_message: None,
      });
      state.status = JobStateStatus::Running;
      state.started_unix_timestamp_millis = Some(start_unix_timestamp_millis);
      let _ = write_job_state(&job_root_directory_path, &state);
    }
  }

  if let Some(stream) = stdout {
    spawn_log_reader_thread(job_runtime_state.clone(), job_root_directory_path.clone(), stream, "stdout");
  }
  if let Some(stream) = stderr {
    spawn_log_reader_thread(job_runtime_state.clone(), job_root_directory_path.clone(), stream, "stderr");
  }

  // Waiter thread: removes running state once done.
  let waiter_state = job_runtime_state.clone();
  let waiter_job_root = job_root_directory_path.clone();
  let waiter_child_handle = child_handle.clone();
  std::thread::spawn(move || {
    // IMPORTANT: Never hold the global runtime-state lock while waiting on the child process.
    // Otherwise, all status/log polling will block and the UI appears frozen.
    let exit_status_result = {
      let mut child_guard = match waiter_child_handle.lock() {
        Ok(guard) => guard,
        Err(_) => return,
      };
      child_guard.wait()
    };

    let exit_status = match exit_status_result {
      Ok(status) => status,
      Err(error) => {
        append_log_line(&waiter_state, &waiter_job_root, format!("[backend] wait error: {error}"));
        let mut locked_state = match waiter_state.lock() {
          Ok(state) => state,
          Err(_) => return,
        };
        locked_state.running_job_by_root.remove(&waiter_job_root);
        return;
      }
    };

    append_log_line(
      &waiter_state,
      &waiter_job_root,
      format!("[backend] finished: {exit_status}"),
    );

    let mut locked_state = match waiter_state.lock() {
      Ok(state) => state,
      Err(_) => return,
    };
    locked_state.running_job_by_root.remove(&waiter_job_root);

    let job_state_path = locked_state.job_state_file_path_by_root.remove(&waiter_job_root);
    drop(locked_state);

    // Guard: only watcher-created jobs register a job state path.
    let Some(job_state_path) = job_state_path else {
      return;
    };

    let mut state = read_job_state_best_effort(&waiter_job_root).unwrap_or(JobState {
      status: JobStateStatus::Running,
      job_id: "unknown".to_string(),
      job_root_directory_path: waiter_job_root.to_string_lossy().to_string(),
      source_bundle_directory_path: None,
      accepted_unix_timestamp_millis: now_unix_timestamp_millis(),
      started_unix_timestamp_millis: None,
      finished_unix_timestamp_millis: None,
      output_markdown_path: None,
      error_message: None,
    });
    state.finished_unix_timestamp_millis = Some(now_unix_timestamp_millis());

    if exit_status.success() {
      state.status = JobStateStatus::Completed;
      state.error_message = None;
      state.output_markdown_path = state
        .output_markdown_path
        .or_else(|| detect_last_output_markdown_path(&waiter_job_root));
    } else {
      state.status = JobStateStatus::Failed;
      state.error_message = Some(format!("OCR process failed: {exit_status}"));
    }

    // Guard: best-effort write; never panic from background thread.
    let _ = fs::write(job_state_path, serde_json::to_string_pretty(&state).unwrap_or_default());
  });

  Ok(())
}

fn is_any_job_running(job_runtime_state: &SharedJobRuntimeState) -> bool {
  let locked = match job_runtime_state.lock() {
    Ok(value) => value,
    Err(_) => return true,
  };
  !locked.running_job_by_root.is_empty()
}

fn derive_watch_job_id(source_bundle_directory_path: &Path) -> String {
  let base = source_bundle_directory_path
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or("bundle");
  let sanitized = base
    .replace('\\', "_")
    .replace('/', "_")
    .replace(':', "_")
    .replace(' ', "_");
  format!("{}_{}", now_unix_timestamp_millis(), sanitized)
}

fn copy_directory_recursively_with_exclusions(
  source_directory_path: &Path,
  destination_directory_path: &Path,
  excluded_filenames: &[&str],
) -> Result<u64, String> {
  if !source_directory_path.exists() {
    // Guard: do not silently ignore missing paths.
    return Err(format!(
      "Input directory does not exist: {}",
      source_directory_path.display()
    ));
  }
  if !source_directory_path.is_dir() {
    // Guard: this function only handles directories.
    return Err(format!("Not a directory: {}", source_directory_path.display()));
  }
  fs::create_dir_all(destination_directory_path).map_err(|error| error.to_string())?;

  let mut total_copied_files: u64 = 0;
  for entry in walkdir::WalkDir::new(source_directory_path) {
    let entry = entry.map_err(|error| error.to_string())?;
    let entry_path = entry.path();
    if entry_path.is_dir() {
      continue;
    }
    let file_name = entry_path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if excluded_filenames.contains(&file_name) {
      continue;
    }
    let relative_path = entry_path
      .strip_prefix(source_directory_path)
      .map_err(|error| error.to_string())?;
    let destination_path = destination_directory_path.join(relative_path);
    if let Some(parent_directory_path) = destination_path.parent() {
      fs::create_dir_all(parent_directory_path).map_err(|error| error.to_string())?;
    }
    fs::copy(entry_path, &destination_path).map_err(|error| error.to_string())?;
    total_copied_files += 1;
  }
  Ok(total_copied_files)
}

fn create_watch_job_from_bundle(
  job_runtime_state: SharedJobRuntimeState,
  jobs_root_directory_path: &Path,
  bundle_directory_path: &Path,
) -> Result<PathBuf, String> {
  let job_id = derive_watch_job_id(bundle_directory_path);
  let job_root_directory_path = jobs_root_directory_path.join(job_id);
  fs::create_dir_all(&job_root_directory_path).map_err(|error| error.to_string())?;
  ensure_job_directory_layout(&job_root_directory_path)?;

  let input_directory_path = job_root_directory_path.join(DEFAULT_INPUT_DIRECTORY_NAME);
  let excluded = [
    DEFAULT_WATCH_READY_FILENAME,
    ".processing",
    ".processed",
    ".failed",
  ];
  let _ = copy_directory_recursively_with_exclusions(bundle_directory_path, &input_directory_path, &excluded)?;

  let accepted_at = now_unix_timestamp_millis();
  let job_id_for_state = job_root_directory_path
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or("job")
    .to_string();
  let job_state = JobState {
    status: JobStateStatus::Queued,
    job_id: job_id_for_state,
    job_root_directory_path: job_root_directory_path.to_string_lossy().to_string(),
    source_bundle_directory_path: Some(bundle_directory_path.to_string_lossy().to_string()),
    accepted_unix_timestamp_millis: accepted_at,
    started_unix_timestamp_millis: None,
    finished_unix_timestamp_millis: None,
    output_markdown_path: None,
    error_message: None,
  };
  write_job_state(&job_root_directory_path, &job_state)?;

  {
    let mut locked_state = job_runtime_state.lock().map_err(|_| "State lock poisoned".to_string())?;
    locked_state
      .job_state_file_path_by_root
      .insert(job_root_directory_path.clone(), job_state_file_path(&job_root_directory_path));
  }

  spawn_job_process(job_runtime_state, job_root_directory_path.clone())?;
  Ok(job_root_directory_path)
}

fn make_watch_folder_poll_callback(
  shared_job_runtime_state: SharedJobRuntimeState,
) -> Arc<dyn Fn(&WatchFolderConfig) -> Result<(), String> + Send + Sync> {
  Arc::new(move |config: &WatchFolderConfig| {
    if is_any_job_running(&shared_job_runtime_state) {
      // Guard: enforce single-job execution on a single Windows host.
      return Ok(());
    }

    let bundle_directories = list_ready_bundle_directories(&config.inbox_directory_path)?;
    for bundle_directory_path in bundle_directories {
      let locked = try_lock_bundle_for_processing(&bundle_directory_path)?;
      if !locked {
        continue;
      }

      let create_result = create_watch_job_from_bundle(
        shared_job_runtime_state.clone(),
        &config.jobs_root_directory_path,
        &bundle_directory_path,
      );
      if let Err(error_message) = create_result {
        let _ = mark_bundle_failed(&bundle_directory_path, &error_message);
        return Err(error_message);
      }
      let _ = mark_bundle_processed(&bundle_directory_path);
      return Ok(());
    }

    Ok(())
  })
}

fn detect_last_output_markdown_path(job_root_directory_path: &Path) -> Option<String> {
  let settings = read_job_settings_best_effort(job_root_directory_path);
  let filename = settings.last_output_markdown_filename?;
  Some(job_root_directory_path.join(filename).to_string_lossy().to_string())
}

#[tauri::command]
fn run_job(
  job_root_directory_path: String,
  output_markdown_filename_override: Option<String>,
  is_math_delimiter_conversion_enabled: Option<bool>,
  deepseek_ocr2_model_revision: Option<String>,
  deepseek_ocr2_markdown_prompt: Option<String>,
  deepseek_ocr2_base_image_size_pixels: Option<u32>,
  deepseek_ocr2_inference_image_size_pixels: Option<u32>,
  deepseek_ocr2_enable_crop_mode: Option<bool>,
  job_runtime_state: State<'_, SharedJobRuntimeState>,
) -> Result<(), String> {
  validate_docker_available()?;

  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  ensure_job_directory_layout(&job_root_directory_path)?;

  let input_directory_path = job_root_directory_path.join(DEFAULT_INPUT_DIRECTORY_NAME);
  let has_any_input_files = walkdir::WalkDir::new(&input_directory_path)
    .into_iter()
    .filter_map(|entry| entry.ok())
    .any(|entry| entry.path().is_file());
  if !has_any_input_files {
    // Guard: prevent a confusing no-op run.
    return Err("No input files found under input/. Drop images or PDFs first.".to_string());
  }

  let mut settings = read_job_settings_best_effort(&job_root_directory_path);
  let override_candidate = output_markdown_filename_override
    .unwrap_or_default()
    .trim()
    .to_string();
  if override_candidate.is_empty() {
    settings.output_markdown_filename_override = None;
  } else {
    settings.output_markdown_filename_override = Some(override_candidate);
  }
  settings.is_math_delimiter_conversion_enabled = is_math_delimiter_conversion_enabled;

  settings.deepseek_ocr2_model_revision = deepseek_ocr2_model_revision;
  settings.deepseek_ocr2_markdown_prompt = deepseek_ocr2_markdown_prompt;

  if let Some(base_image_size_pixels) = deepseek_ocr2_base_image_size_pixels {
    if base_image_size_pixels <= 0 {
      // Guard: reject invalid sizes early.
      return Err("deepseek_ocr2_base_image_size_pixels must be > 0".to_string());
    }
    settings.deepseek_ocr2_base_image_size_pixels = Some(base_image_size_pixels);
  }

  if let Some(inference_image_size_pixels) = deepseek_ocr2_inference_image_size_pixels {
    if inference_image_size_pixels <= 0 {
      // Guard: reject invalid sizes early.
      return Err("deepseek_ocr2_inference_image_size_pixels must be > 0".to_string());
    }
    settings.deepseek_ocr2_inference_image_size_pixels = Some(inference_image_size_pixels);
  }

  settings.deepseek_ocr2_enable_crop_mode = deepseek_ocr2_enable_crop_mode;
  write_job_settings(&job_root_directory_path, &settings)?;

  spawn_job_process(job_runtime_state.inner().clone(), job_root_directory_path)?;
  Ok(())
}

#[tauri::command]
fn cancel_job(job_root_directory_path: String, job_runtime_state: State<'_, SharedJobRuntimeState>) -> Result<(), String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  let child_handle = {
    let locked_state = job_runtime_state.lock().map_err(|_| "State lock poisoned".to_string())?;
    let Some(running) = locked_state.running_job_by_root.get(&job_root_directory_path) else {
      // Guard: nothing to cancel.
      return Ok(());
    };
    running.child.clone()
  };

  let mut child_guard = child_handle.lock().map_err(|_| "Child lock poisoned".to_string())?;
  child_guard.kill().map_err(|error| error.to_string())?;
  append_log_line(
    job_runtime_state.inner(),
    &job_root_directory_path,
    "[backend] cancellation requested".to_string(),
  );
  Ok(())
}

#[tauri::command]
fn get_job_logs(job_root_directory_path: String, job_runtime_state: State<'_, SharedJobRuntimeState>) -> Result<JobLogResponse, String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  let locked_state = job_runtime_state.lock().map_err(|_| "State lock poisoned".to_string())?;
  let lines = locked_state
    .log_lines_by_root
    .get(&job_root_directory_path)
    .cloned()
    .unwrap_or_else(VecDeque::new)
    .into_iter()
    .collect::<Vec<String>>();
  Ok(JobLogResponse { lines })
}

#[tauri::command]
fn get_current_task_preview(job_root_directory_path: String) -> Result<Option<CurrentTaskPreview>, String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  ensure_job_directory_layout(&job_root_directory_path)?;

  let queue_database_path = get_queue_database_path(&job_root_directory_path);
  let Some(mut running_task) = query_current_running_task(&queue_database_path)? else {
    return Ok(None);
  };

  let settings = read_job_settings_best_effort(&job_root_directory_path);
  running_task.deepseek_inference_image_size_pixels = settings.deepseek_ocr2_inference_image_size_pixels;

  let preview_path = resolve_preview_image_path_for_task(&job_root_directory_path, &running_task);
  if let Some(preview_path) = preview_path {
    if preview_path.exists() {
      running_task.preview_image_file_path = Some(preview_path.to_string_lossy().to_string());
    } else {
      running_task.preview_image_file_path = Some(preview_path.to_string_lossy().to_string());
    }
  }

  Ok(Some(running_task))
}

#[tauri::command]
fn get_current_task_preview_image_bytes(job_root_directory_path: String) -> Result<Option<PreviewImageBytes>, String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  ensure_job_directory_layout(&job_root_directory_path)?;

  let queue_database_path = get_queue_database_path(&job_root_directory_path);
  let Some(running_task) = query_current_running_task(&queue_database_path)? else {
    return Ok(None);
  };
  let Some(image_path) = resolve_preview_image_path_for_task(&job_root_directory_path, &running_task) else {
    return Ok(None);
  };
  if !image_path.exists() {
    // Guard: preview can lag behind rendering; treat missing as "not ready".
    return Ok(None);
  }
  let metadata = fs::metadata(&image_path).map_err(|error| error.to_string())?;
  if !metadata.is_file() {
    // Guard: refuse non-files for preview reads.
    return Ok(None);
  }
  if metadata.len() > MAX_PREVIEW_IMAGE_BYTES {
    return Err(format!(
      "Preview image is too large to load in GUI ({} bytes).",
      metadata.len()
    ));
  }

  let bytes = fs::read(&image_path).map_err(|error| error.to_string())?;
  Ok(Some(PreviewImageBytes {
    mime_type: infer_image_mime_type(&image_path),
    bytes,
  }))
}

#[tauri::command]
fn reset_job_directory(job_root_directory_path: String) -> Result<(), String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  ensure_job_directory_layout(&job_root_directory_path)?;

  let queue_database_path = get_queue_database_path(&job_root_directory_path);
  let output_directory_path = job_root_directory_path.join(DEFAULT_OUTPUT_DIRECTORY_NAME);
  let settings = read_job_settings_best_effort(&job_root_directory_path);
  let output_markdown_path = settings
    .last_output_markdown_filename
    .as_deref()
    .map(|filename| job_root_directory_path.join(filename));

  if queue_database_path.exists() {
    fs::remove_file(queue_database_path).map_err(|error| error.to_string())?;
  }
  if let Some(output_markdown_path) = output_markdown_path {
    if output_markdown_path.exists() {
      fs::remove_file(output_markdown_path).map_err(|error| error.to_string())?;
    }
  }
  if output_directory_path.exists() && output_directory_path.is_dir() {
    fs::remove_dir_all(output_directory_path).map_err(|error| error.to_string())?;
  }

  // Recreate expected directories after reset.
  ensure_job_directory_layout(&job_root_directory_path)?;
  Ok(())
}

#[tauri::command]
fn open_in_file_manager(target_path: String) -> Result<(), String> {
  let target_path = PathBuf::from(target_path);
  if !target_path.exists() {
    // Guard: do not run shell command for missing targets.
    return Err(format!("Path does not exist: {}", target_path.display()));
  }

  #[cfg(target_os = "windows")]
  {
    Command::new("explorer")
      .arg(target_path)
      .spawn()
      .map_err(|error| error.to_string())?;
    return Ok(());
  }

  #[cfg(target_os = "macos")]
  {
    Command::new("open")
      .arg(target_path)
      .spawn()
      .map_err(|error| error.to_string())?;
    return Ok(());
  }

  #[cfg(target_os = "linux")]
  {
    Command::new("xdg-open")
      .arg(target_path)
      .spawn()
      .map_err(|error| error.to_string())?;
    return Ok(());
  }
}

fn main() {
  let job_runtime_state: SharedJobRuntimeState = Arc::new(Mutex::new(JobRuntimeState::default()));
  let watch_folder_state: SharedWatchFolderRuntimeState = new_shared_watch_folder_state();

  // Guard: allow headless-ish automation by environment variables (useful for future Slack agent wiring).
  // If these are set, the watcher starts immediately on app startup.
  if let Ok(inbox) = std::env::var(OCR_AGENT_WATCH_INBOX_ENVIRONMENT_VARIABLE_NAME) {
    let inbox_trimmed = inbox.trim().to_string();
    if !inbox_trimmed.is_empty() {
      let jobs_root = std::env::var(OCR_AGENT_WATCH_JOBS_ROOT_ENVIRONMENT_VARIABLE_NAME)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
      let inbox_directory_path = PathBuf::from(inbox_trimmed);
      let jobs_root_directory_path = jobs_root
        .map(PathBuf::from)
        .unwrap_or_else(|| inbox_directory_path.join(DEFAULT_WATCH_JOBS_DIRECTORY_NAME));

      let config = WatchFolderConfig {
        inbox_directory_path,
        jobs_root_directory_path,
        poll_interval: default_watch_poll_interval(),
      };
      let poll_callback = make_watch_folder_poll_callback(job_runtime_state.clone());
      let _ = start_watch_folder_with_callback(&watch_folder_state, config, poll_callback);
    }
  }

  tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .manage(job_runtime_state)
    .manage(watch_folder_state)
    .invoke_handler(tauri::generate_handler![
      probe_docker,
      probe_gpu_passthrough,
      pick_output_directory,
      pick_directory,
      pick_input_files,
      pick_input_folder,
      job_add_inputs,
      get_job_status,
      get_job_logs,
      get_current_task_preview,
      get_current_task_preview_image_bytes,
      run_job,
      cancel_job,
      reset_job_directory,
      open_in_file_manager,
      get_watch_folder_status,
      start_watch_folder,
      stop_watch_folder
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}

