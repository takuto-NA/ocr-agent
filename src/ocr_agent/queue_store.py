"""
Responsibility:
- Persist and manage the task queue in SQLite.
- Preserve enqueue order deterministically for merged Markdown ordering.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import sqlite3
import time
from typing import Iterable


TASK_STATUS_PENDING = "pending"
TASK_STATUS_RUNNING = "running"
TASK_STATUS_COMPLETED = "completed"
TASK_STATUS_FAILED = "failed"

TASK_KIND_IMAGE = "image"
TASK_KIND_PDF_PAGE = "pdf_page"

DEFAULT_SQLITE_CONNECT_TIMEOUT_SECONDS = 30.0
DEFAULT_SQLITE_CONNECT_MAX_RETRIES = 5
DEFAULT_SQLITE_CONNECT_RETRY_SLEEP_SECONDS = 0.4


@dataclass(frozen=True)
class QueueTask:
    task_id: int
    task_kind: str
    source_path: str
    pdf_page_index: int | None
    pdf_total_pages: int | None
    created_unix_timestamp_seconds: int
    status: str
    output_markdown_path: str | None
    error_message: str | None


class QueueStore:
    def __init__(self, queue_database_path: Path) -> None:
        self._queue_database_path = queue_database_path

    def initialize(self) -> None:
        self._queue_database_path.parent.mkdir(parents=True, exist_ok=True)
        with self._connect() as connection:
            connection.execute(
                """
                CREATE TABLE IF NOT EXISTS tasks (
                  task_id INTEGER PRIMARY KEY AUTOINCREMENT,
                  task_kind TEXT NOT NULL,
                  source_path TEXT NOT NULL,
                  pdf_page_index INTEGER NULL,
                  pdf_total_pages INTEGER NULL,
                  created_unix_timestamp_seconds INTEGER NOT NULL,
                  status TEXT NOT NULL,
                  output_markdown_path TEXT NULL,
                  error_message TEXT NULL
                )
                """
            )
            connection.commit()

    def enqueue_image_tasks(
        self, image_file_paths: Iterable[Path], created_unix_timestamp_seconds: int
    ) -> int:
        tasks_added_count = 0
        with self._connect() as connection:
            for image_file_path in image_file_paths:
                connection.execute(
                    """
                    INSERT INTO tasks (
                      task_kind, source_path, pdf_page_index, pdf_total_pages,
                      created_unix_timestamp_seconds, status
                    )
                    VALUES (?, ?, NULL, NULL, ?, ?)
                    """,
                    (
                        TASK_KIND_IMAGE,
                        str(image_file_path),
                        created_unix_timestamp_seconds,
                        TASK_STATUS_PENDING,
                    ),
                )
                tasks_added_count += 1
            connection.commit()
        return tasks_added_count

    def enqueue_pdf_page_tasks(
        self,
        pdf_file_path: Path,
        pdf_total_pages: int,
        created_unix_timestamp_seconds: int,
    ) -> int:
        if pdf_total_pages <= 0:
            # Guard: A PDF must have at least one page to be meaningful.
            return 0

        tasks_added_count = 0
        with self._connect() as connection:
            for pdf_page_index in range(pdf_total_pages):
                connection.execute(
                    """
                    INSERT INTO tasks (
                      task_kind, source_path, pdf_page_index, pdf_total_pages,
                      created_unix_timestamp_seconds, status
                    )
                    VALUES (?, ?, ?, ?, ?, ?)
                    """,
                    (
                        TASK_KIND_PDF_PAGE,
                        str(pdf_file_path),
                        pdf_page_index,
                        pdf_total_pages,
                        created_unix_timestamp_seconds,
                        TASK_STATUS_PENDING,
                    ),
                )
                tasks_added_count += 1
            connection.commit()
        return tasks_added_count

    def fetch_next_pending_task(self) -> QueueTask | None:
        with self._connect() as connection:
            row = connection.execute(
                """
                SELECT task_id, task_kind, source_path, pdf_page_index, pdf_total_pages,
                       created_unix_timestamp_seconds, status, output_markdown_path, error_message
                FROM tasks
                WHERE status = ?
                ORDER BY task_id ASC
                LIMIT 1
                """,
                (TASK_STATUS_PENDING,),
            ).fetchone()
        return self._row_to_task(row)

    def mark_task_running(self, task_id: int) -> None:
        with self._connect() as connection:
            connection.execute(
                "UPDATE tasks SET status = ? WHERE task_id = ?",
                (TASK_STATUS_RUNNING, task_id),
            )
            connection.commit()

    def mark_task_completed(self, task_id: int, output_markdown_path: Path) -> None:
        with self._connect() as connection:
            connection.execute(
                """
                UPDATE tasks
                SET status = ?, output_markdown_path = ?, error_message = NULL
                WHERE task_id = ?
                """,
                (TASK_STATUS_COMPLETED, str(output_markdown_path), task_id),
            )
            connection.commit()

    def mark_task_failed(self, task_id: int, error_message: str) -> None:
        with self._connect() as connection:
            connection.execute(
                """
                UPDATE tasks
                SET status = ?, error_message = ?
                WHERE task_id = ?
                """,
                (TASK_STATUS_FAILED, error_message, task_id),
            )
            connection.commit()

    def fetch_tasks_in_enqueue_order(self) -> list[QueueTask]:
        with self._connect() as connection:
            rows = connection.execute(
                """
                SELECT task_id, task_kind, source_path, pdf_page_index, pdf_total_pages,
                       created_unix_timestamp_seconds, status, output_markdown_path, error_message
                FROM tasks
                ORDER BY task_id ASC
                """
            ).fetchall()

        tasks: list[QueueTask] = []
        for row in rows:
            task = self._row_to_task(row)
            if task is None:
                continue
            tasks.append(task)
        return tasks

    def fetch_status_counts(self) -> dict[str, int]:
        with self._connect() as connection:
            rows = connection.execute(
                "SELECT status, COUNT(*) FROM tasks GROUP BY status"
            ).fetchall()
        return {str(status): int(count) for status, count in rows}

    def delete_all_tasks(self) -> int:
        with self._connect() as connection:
            cursor = connection.execute("DELETE FROM tasks")
            deleted_rows_count = int(cursor.rowcount)
            connection.commit()
        return deleted_rows_count

    def _connect(self) -> sqlite3.Connection:
        last_exception: Exception | None = None
        for attempt_index in range(DEFAULT_SQLITE_CONNECT_MAX_RETRIES):
            try:
                connection = sqlite3.connect(
                    str(self._queue_database_path),
                    timeout=DEFAULT_SQLITE_CONNECT_TIMEOUT_SECONDS,
                )
                connection.row_factory = sqlite3.Row
                return connection
            except sqlite3.OperationalError as exception:
                last_exception = exception
                # Guard: Windows bind mounts can transiently fail with "disk I/O error".
                # Retrying avoids flaky first-run failures.
                if "disk I/O error" not in str(exception):
                    raise
                if attempt_index >= DEFAULT_SQLITE_CONNECT_MAX_RETRIES - 1:
                    raise
                time.sleep(DEFAULT_SQLITE_CONNECT_RETRY_SLEEP_SECONDS)

        if last_exception is not None:
            raise last_exception
        raise RuntimeError("Failed to connect to SQLite queue database")

    @staticmethod
    def _row_to_task(row: sqlite3.Row | None) -> QueueTask | None:
        if row is None:
            return None
        return QueueTask(
            task_id=int(row["task_id"]),
            task_kind=str(row["task_kind"]),
            source_path=str(row["source_path"]),
            pdf_page_index=None if row["pdf_page_index"] is None else int(row["pdf_page_index"]),
            pdf_total_pages=None if row["pdf_total_pages"] is None else int(row["pdf_total_pages"]),
            created_unix_timestamp_seconds=int(row["created_unix_timestamp_seconds"]),
            status=str(row["status"]),
            output_markdown_path=None
            if row["output_markdown_path"] is None
            else str(row["output_markdown_path"]),
            error_message=None if row["error_message"] is None else str(row["error_message"]),
        )

