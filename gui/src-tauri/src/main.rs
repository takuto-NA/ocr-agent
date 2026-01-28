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
use serde::Serialize;
use tauri::{DragDropEvent, Emitter, State, Wry, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use tauri_plugin_dialog::FilePath;

const DEFAULT_QUEUE_DATABASE_FILENAME: &str = "queue.sqlite3";
const DEFAULT_INPUT_DIRECTORY_NAME: &str = "input";
const DEFAULT_OUTPUT_DIRECTORY_NAME: &str = "output";
const DEFAULT_OUTPUT_MARKDOWN_FILENAME: &str = "output.md";

const MAX_LOG_LINES: usize = 1500;
const DOCKER_COMPOSE_SERVICE_NAME: &str = "ocr-agent";
const OCR_AGENT_REPO_ROOT_ENVIRONMENT_VARIABLE_NAME: &str = "OCR_AGENT_REPO_ROOT";

#[derive(Debug, Clone, Serialize)]
struct DragDropPayload {
  event: String,
  paths: Vec<String>,
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
  eta_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct JobLogResponse {
  lines: Vec<String>,
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

      let destination_path = input_directory_path.join(file_name);
      fs::copy(&input_path, &destination_path).map_err(|error| error.to_string())?;
      continue;
    }

    if input_path.is_dir() {
      let directory_name = input_path
        .file_name()
        .map(sanitize_filename_for_copy)
        .unwrap_or_else(|| "input_directory".to_string());

      let destination_directory_path = input_directory_path.join(directory_name);
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

fn compute_eta_seconds(
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

  let eta_seconds = compute_eta_seconds(start_unix_timestamp_millis, total_tasks, completed_tasks);
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
    eta_seconds,
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
  command.arg("-v");
  command.arg(format!("{job_root_for_docker}:/data"));
  command.arg(DOCKER_COMPOSE_SERVICE_NAME);
  command.arg("bash");
  command.arg("-lc");
  command.arg("python3 -m ocr_agent.cli enqueue /data/input && python3 -m ocr_agent.cli run --output-md /data/output.md");
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
  });

  Ok(())
}

#[tauri::command]
fn run_job(job_root_directory_path: String, job_runtime_state: State<'_, SharedJobRuntimeState>) -> Result<(), String> {
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
fn reset_job_directory(job_root_directory_path: String) -> Result<(), String> {
  let job_root_directory_path = PathBuf::from(job_root_directory_path);
  ensure_job_directory_layout(&job_root_directory_path)?;

  let queue_database_path = get_queue_database_path(&job_root_directory_path);
  let output_directory_path = job_root_directory_path.join(DEFAULT_OUTPUT_DIRECTORY_NAME);
  let output_markdown_path = job_root_directory_path.join(DEFAULT_OUTPUT_MARKDOWN_FILENAME);

  if queue_database_path.exists() {
    fs::remove_file(queue_database_path).map_err(|error| error.to_string())?;
  }
  if output_markdown_path.exists() {
    fs::remove_file(output_markdown_path).map_err(|error| error.to_string())?;
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

  tauri::Builder::default()
    .on_window_event(|window, event| {
      let WindowEvent::DragDrop(drag_event) = event else {
        return;
      };

      match drag_event {
        DragDropEvent::Enter { paths, .. } => {
          let _ = window.emit(
            "ocr_drag_drop",
            DragDropPayload {
              event: "enter".to_string(),
              paths: paths.iter().map(|p| p.to_string_lossy().to_string()).collect(),
            },
          );
        }
        DragDropEvent::Over { .. } => {}
        DragDropEvent::Drop { paths, .. } => {
          let _ = window.emit(
            "ocr_drag_drop",
            DragDropPayload {
              event: "drop".to_string(),
              paths: paths.iter().map(|p| p.to_string_lossy().to_string()).collect(),
            },
          );
        }
        DragDropEvent::Leave => {
          let _ = window.emit(
            "ocr_drag_drop",
            DragDropPayload {
              event: "leave".to_string(),
              paths: Vec::new(),
            },
          );
        }
        _ => {}
      }
    })
    .plugin(tauri_plugin_dialog::init())
    .manage(job_runtime_state)
    .invoke_handler(tauri::generate_handler![
      probe_docker,
      probe_gpu_passthrough,
      pick_output_directory,
      pick_input_files,
      pick_input_folder,
      job_add_inputs,
      get_job_status,
      get_job_logs,
      run_job,
      cancel_job,
      reset_job_directory,
      open_in_file_manager
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}

