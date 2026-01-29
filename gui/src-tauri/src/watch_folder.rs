/*!
Responsibility:
- Provide a simple watch-folder based ingestion loop for the Tauri GUI.
- Detect completed inbox bundles (via a `.ready` marker), then create job roots and trigger OCR runs.
*/

use std::{
  fs,
  fs::OpenOptions,
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
  },
  thread,
  time::Duration,
};

use serde::Serialize;

const DEFAULT_WATCH_POLL_INTERVAL_MILLIS: u64 = 1000;
const WATCH_READY_FILENAME: &str = ".ready";
const WATCH_PROCESSING_FILENAME: &str = ".processing";
const WATCH_PROCESSED_FILENAME: &str = ".processed";
const WATCH_FAILED_FILENAME: &str = ".failed";

#[derive(Debug, Clone, Serialize)]
pub struct WatchFolderStatus {
  pub is_running: bool,
  pub inbox_directory_path: Option<String>,
  pub jobs_root_directory_path: Option<String>,
  pub last_error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WatchFolderConfig {
  pub inbox_directory_path: PathBuf,
  pub jobs_root_directory_path: PathBuf,
  pub poll_interval: Duration,
}

#[derive(Default)]
pub(crate) struct WatchFolderRuntimeState {
  running_thread: Option<thread::JoinHandle<()>>,
  stop_requested: Arc<AtomicBool>,
  inbox_directory_path: Option<PathBuf>,
  jobs_root_directory_path: Option<PathBuf>,
  last_error_message: Option<String>,
}

pub type SharedWatchFolderRuntimeState = Arc<Mutex<WatchFolderRuntimeState>>;

pub fn new_shared_watch_folder_state() -> SharedWatchFolderRuntimeState {
  Arc::new(Mutex::new(WatchFolderRuntimeState::default()))
}

pub fn get_watch_folder_status(state: &SharedWatchFolderRuntimeState) -> WatchFolderStatus {
  let locked = match state.lock() {
    Ok(value) => value,
    Err(_) => {
      // Guard: state lock poisoned.
      return WatchFolderStatus {
        is_running: false,
        inbox_directory_path: None,
        jobs_root_directory_path: None,
        last_error_message: Some("Watch folder state lock poisoned".to_string()),
      };
    }
  };

  WatchFolderStatus {
    is_running: locked.running_thread.is_some(),
    inbox_directory_path: locked
      .inbox_directory_path
      .as_ref()
      .map(|p| p.to_string_lossy().to_string()),
    jobs_root_directory_path: locked
      .jobs_root_directory_path
      .as_ref()
      .map(|p| p.to_string_lossy().to_string()),
    last_error_message: locked.last_error_message.clone(),
  }
}

pub fn stop_watch_folder(state: &SharedWatchFolderRuntimeState) {
  let (stop_flag, join_handle) = {
    let mut locked = match state.lock() {
      Ok(value) => value,
      Err(_) => return,
    };
    let stop_flag = locked.stop_requested.clone();
    stop_flag.store(true, Ordering::SeqCst);
    (stop_flag, locked.running_thread.take())
  };

  // Guard: join outside of lock to avoid deadlocks.
  drop(stop_flag);
  if let Some(handle) = join_handle {
    let _ = handle.join();
  }
}

pub fn start_watch_folder(
  state: &SharedWatchFolderRuntimeState,
  config: WatchFolderConfig,
  poll_once_callback: Arc<dyn Fn(&WatchFolderConfig) -> Result<(), String> + Send + Sync>,
) -> Result<(), String> {
  if config.inbox_directory_path.as_os_str().is_empty() {
    // Guard: empty inbox path is meaningless.
    return Err("inbox_directory_path is empty".to_string());
  }
  if config.jobs_root_directory_path.as_os_str().is_empty() {
    // Guard: empty jobs root is meaningless.
    return Err("jobs_root_directory_path is empty".to_string());
  }

  {
    let mut locked = state.lock().map_err(|_| "Watch folder state lock poisoned".to_string())?;
    if locked.running_thread.is_some() {
      // Guard: prevent double-start.
      return Err("Watch folder is already running.".to_string());
    }
    locked.stop_requested = Arc::new(AtomicBool::new(false));
    locked.inbox_directory_path = Some(config.inbox_directory_path.clone());
    locked.jobs_root_directory_path = Some(config.jobs_root_directory_path.clone());
    locked.last_error_message = None;
  }

  let shared_state_for_thread = state.clone();
  let stop_flag = {
    let locked = state.lock().map_err(|_| "Watch folder state lock poisoned".to_string())?;
    locked.stop_requested.clone()
  };

  let thread_handle = thread::spawn(move || loop {
    if stop_flag.load(Ordering::SeqCst) {
      return;
    }

    let poll_result = poll_once_callback.as_ref()(&config);
    if let Err(message) = poll_result {
      // Guard: store last error but keep the watcher alive.
      let mut locked = match shared_state_for_thread.lock() {
        Ok(value) => value,
        Err(_) => return,
      };
      locked.last_error_message = Some(message);
    }

    thread::sleep(config.poll_interval);
  });

  let mut locked = state.lock().map_err(|_| "Watch folder state lock poisoned".to_string())?;
  locked.running_thread = Some(thread_handle);
  Ok(())
}

pub fn default_poll_interval() -> Duration {
  Duration::from_millis(DEFAULT_WATCH_POLL_INTERVAL_MILLIS)
}

pub fn list_ready_bundle_directories(inbox_directory_path: &Path) -> Result<Vec<PathBuf>, String> {
  if !inbox_directory_path.exists() {
    // Guard: inbox must exist to be watchable.
    return Err(format!(
      "Inbox directory does not exist: {}",
      inbox_directory_path.display()
    ));
  }
  if !inbox_directory_path.is_dir() {
    // Guard: inbox must be a directory.
    return Err(format!(
      "Inbox path is not a directory: {}",
      inbox_directory_path.display()
    ));
  }

  let mut candidates: Vec<PathBuf> = vec![];
  let entries = fs::read_dir(inbox_directory_path).map_err(|error| error.to_string())?;
  for entry_result in entries {
    let entry = entry_result.map_err(|error| error.to_string())?;
    let path = entry.path();
    if !path.is_dir() {
      continue;
    }
    if !path.join(WATCH_READY_FILENAME).exists() {
      continue;
    }
    if path.join(WATCH_PROCESSED_FILENAME).exists() {
      continue;
    }
    if path.join(WATCH_FAILED_FILENAME).exists() {
      continue;
    }
    candidates.push(path);
  }

  candidates.sort();
  Ok(candidates)
}

pub fn try_lock_bundle_for_processing(bundle_directory_path: &Path) -> Result<bool, String> {
  let processing_marker_path = bundle_directory_path.join(WATCH_PROCESSING_FILENAME);
  let create_result = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&processing_marker_path);

  if create_result.is_ok() {
    return Ok(true);
  }

  // Guard: if marker exists, another poller already owns it.
  if processing_marker_path.exists() {
    return Ok(false);
  }

  Err("Failed to create .processing marker".to_string())
}

pub fn mark_bundle_processed(bundle_directory_path: &Path) -> Result<(), String> {
  let processed_path = bundle_directory_path.join(WATCH_PROCESSED_FILENAME);
  fs::write(processed_path, "").map_err(|error| error.to_string())?;

  let processing_path = bundle_directory_path.join(WATCH_PROCESSING_FILENAME);
  if processing_path.exists() {
    let _ = fs::remove_file(processing_path);
  }
  Ok(())
}

pub fn mark_bundle_failed(bundle_directory_path: &Path, error_message: &str) -> Result<(), String> {
  let failed_path = bundle_directory_path.join(WATCH_FAILED_FILENAME);
  fs::write(failed_path, error_message).map_err(|error| error.to_string())?;

  let processing_path = bundle_directory_path.join(WATCH_PROCESSING_FILENAME);
  if processing_path.exists() {
    let _ = fs::remove_file(processing_path);
  }
  Ok(())
}

