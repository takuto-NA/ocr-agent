/**
 * Responsibility:
 * - Detect whether the frontend is running inside a Tauri webview.
 */
export function isTauriWebview(): boolean {
  // Guard: @tauri-apps/api requires an injected bridge. In a normal browser tab it won't exist.
  if (typeof window === "undefined") {
    return false;
  }

  const anyWindow = window as any;
  // Tauri v2 typically injects __TAURI_INTERNALS__ (and may also expose __TAURI__).
  if (typeof anyWindow.__TAURI__ !== "undefined") {
    return true;
  }
  if (typeof anyWindow.__TAURI_INTERNALS__ !== "undefined") {
    return true;
  }
  if (typeof anyWindow.__TAURI_IPC__ !== "undefined") {
    return true;
  }

  return false;
}

