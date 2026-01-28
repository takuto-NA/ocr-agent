/**
 * Responsibility:
 * - Render log entries with best-practice UX:
 *   - Follow-tail (auto-scroll) with user override when scrolling up
 *   - Search and source filtering
 *   - “Jump to latest” and “Clear view” (client-side) controls
 */
import { useEffect, useMemo, useRef, useState } from "react";

export type LogSource = "ui" | "stdout" | "stderr" | "backend" | "unknown";

export type LogEntry = {
  id: string;
  source: LogSource;
  tag: string;
  message: string;
  raw: string;
};

const DEFAULT_IS_FOLLOW_TAIL_ENABLED = true;
const DEFAULT_VISIBLE_TAIL_LINE_LIMIT = 1200;
const SCROLL_BOTTOM_SNAP_THRESHOLD_PIXELS = 24;

type LogViewerProps = {
  entries: LogEntry[];
  isRunning: boolean;
  onClearView: () => void;
  className?: string;
};

function isNearScrollBottom(scrollElement: HTMLElement): boolean {
  const remainingPixels = scrollElement.scrollHeight - scrollElement.scrollTop - scrollElement.clientHeight;
  return remainingPixels <= SCROLL_BOTTOM_SNAP_THRESHOLD_PIXELS;
}

function formatSourceLabel(source: LogSource): string {
  if (source === "stdout") {
    return "stdout";
  }
  if (source === "stderr") {
    return "stderr";
  }
  if (source === "backend") {
    return "backend";
  }
  if (source === "ui") {
    return "ui";
  }
  return "other";
}

export function LogViewer(props: LogViewerProps) {
  const { entries, isRunning, onClearView, className } = props;

  const [isFollowTailEnabled, setIsFollowTailEnabled] = useState<boolean>(DEFAULT_IS_FOLLOW_TAIL_ENABLED);
  const [isStdoutVisible, setIsStdoutVisible] = useState<boolean>(true);
  const [isStderrVisible, setIsStderrVisible] = useState<boolean>(true);
  const [isUiVisible, setIsUiVisible] = useState<boolean>(true);
  const [searchQuery, setSearchQuery] = useState<string>("");

  const scrollContainerRef = useRef<HTMLDivElement | null>(null);
  const lastRenderedEntryCountRef = useRef<number>(0);

  const filteredEntries = useMemo(() => {
    const trimmedSearchQuery = searchQuery.trim().toLowerCase();
    const sourceEnabled = (source: LogSource): boolean => {
      if (source === "stdout") {
        return isStdoutVisible;
      }
      if (source === "stderr") {
        return isStderrVisible;
      }
      if (source === "ui") {
        return isUiVisible;
      }
      if (source === "backend") {
        return true;
      }
      return true;
    };

    const visible = entries.filter((entry) => sourceEnabled(entry.source));
    if (trimmedSearchQuery === "") {
      return visible;
    }
    return visible.filter((entry) => entry.raw.toLowerCase().includes(trimmedSearchQuery));
  }, [entries, isStdoutVisible, isStderrVisible, isUiVisible, searchQuery]);

  const tailEntries = useMemo(() => {
    if (filteredEntries.length <= DEFAULT_VISIBLE_TAIL_LINE_LIMIT) {
      return filteredEntries;
    }
    return filteredEntries.slice(filteredEntries.length - DEFAULT_VISIBLE_TAIL_LINE_LIMIT);
  }, [filteredEntries]);

  const hasAnyEntries = tailEntries.length > 0;

  useEffect(() => {
    const scrollElement = scrollContainerRef.current;
    if (scrollElement === null) {
      return;
    }

    const hasNewEntries = entries.length !== lastRenderedEntryCountRef.current;
    lastRenderedEntryCountRef.current = entries.length;
    if (!hasNewEntries) {
      return;
    }

    if (!isFollowTailEnabled) {
      return;
    }

    scrollElement.scrollTop = scrollElement.scrollHeight;
  }, [entries.length, isFollowTailEnabled]);

  function handleScroll(): void {
    const scrollElement = scrollContainerRef.current;
    if (scrollElement === null) {
      return;
    }
    if (isNearScrollBottom(scrollElement)) {
      return;
    }
    if (!isFollowTailEnabled) {
      return;
    }

    // Guard: user intentionally scrolled away from the tail; stop auto-scrolling.
    setIsFollowTailEnabled(false);
  }

  function handleJumpToLatest(): void {
    const scrollElement = scrollContainerRef.current;
    if (scrollElement === null) {
      return;
    }
    scrollElement.scrollTop = scrollElement.scrollHeight;
    setIsFollowTailEnabled(true);
  }

  function handleToggleFollowTail(): void {
    const next = !isFollowTailEnabled;
    setIsFollowTailEnabled(next);
    if (!next) {
      return;
    }
    // Guard: when enabling follow-tail, ensure user immediately sees the latest output.
    const scrollElement = scrollContainerRef.current;
    if (scrollElement === null) {
      return;
    }
    scrollElement.scrollTop = scrollElement.scrollHeight;
  }

  return (
    <div className={className ?? ""}>
      <div className="logToolbar">
        <div className="logToolbarRow">
          <div className="logToolbarLeft">
            <button className="button buttonSmall" onClick={handleToggleFollowTail} disabled={!hasAnyEntries}>
              Follow tail: <b>{isFollowTailEnabled ? "ON" : "OFF"}</b>
            </button>
            <button className="button buttonSmall" onClick={handleJumpToLatest} disabled={!hasAnyEntries}>
              Jump to latest
            </button>
            <button className="button buttonSmall" onClick={onClearView} disabled={!hasAnyEntries}>
              Clear view
            </button>
          </div>
          <div className="logToolbarRight">
            <div className="label">
              {isRunning ? "Streaming logs…" : "Logs"}
              {" · "}
              {tailEntries.length}/{filteredEntries.length}/{entries.length}
            </div>
          </div>
        </div>

        <div className="logToolbarRow">
          <div className="logFilters">
            <label className="toggle">
              <input
                type="checkbox"
                checked={isUiVisible}
                onChange={(event) => setIsUiVisible(event.target.checked)}
              />
              <span className="toggleLabel">UI</span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={isStdoutVisible}
                onChange={(event) => setIsStdoutVisible(event.target.checked)}
              />
              <span className="toggleLabel">stdout</span>
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={isStderrVisible}
                onChange={(event) => setIsStderrVisible(event.target.checked)}
              />
              <span className="toggleLabel">stderr</span>
            </label>
          </div>
          <input
            className="input"
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            placeholder="Search logs…"
            aria-label="Search logs"
          />
        </div>
      </div>

      <div className="logScrollArea" ref={scrollContainerRef} onScroll={handleScroll} aria-label="log output">
        {!hasAnyEntries ? (
          <div className="label">Logs will appear here.</div>
        ) : (
          tailEntries.map((entry) => (
            <div className={`logRow logRowSource_${formatSourceLabel(entry.source)}`} key={entry.id}>
              <span className="logTag">{entry.tag}</span>
              <pre className="logMessage">{entry.message}</pre>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

