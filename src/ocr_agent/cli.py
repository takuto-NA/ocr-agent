"""
Responsibility:
- Provide a simple CLI to enqueue inputs, process the queue, and merge Markdown.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import time

from ocr_agent import __version__
from ocr_agent.config import DeepSeekOcr2Settings, MarkdownPostProcessingSettings, RuntimePaths
from ocr_agent.deepseek_ocr2_runner import DeepSeekOcr2Runner
from ocr_agent.input_discovery import (
    InputDiscoveryReport,
    SUPPORTED_IMAGE_FILE_EXTENSIONS,
    SUPPORTED_PDF_FILE_EXTENSIONS,
    discover_input_paths_in_enqueue_order,
    split_image_and_pdf_paths,
)
from ocr_agent.markdown_merge import merge_tasks_into_single_markdown
from ocr_agent.pdf_render import get_pdf_total_pages, render_pdf_page_to_image_file
from ocr_agent.queue_store import QueueStore, TASK_KIND_PDF_PAGE


DEFAULT_QUEUE_DATABASE_PATH = Path("/data/queue.sqlite3")
DEFAULT_OUTPUT_DIRECTORY_PATH = Path("/data/output")
DEFAULT_MERGED_MARKDOWN_PATH = Path("/data/output.md")

EXIT_CODE_NOTHING_ENQUEUED = 2

UNSAFE_DELETION_PATH_STRINGS = {"", "/", ".", ".."}


def main() -> None:
    argument_parser = argparse.ArgumentParser(prog="ocr-agent")
    argument_parser.add_argument(
        "--version",
        action="version",
        version=f"%(prog)s {__version__}",
    )
    subparsers = argument_parser.add_subparsers(dest="command", required=True)

    enqueue_parser = subparsers.add_parser("enqueue", help="Enqueue images/folders/PDFs")
    enqueue_parser.add_argument("inputs", nargs="+", help="Input paths (files or folders)")
    enqueue_parser.add_argument(
        "--queue-db",
        dest="queue_database_path",
        default=str(DEFAULT_QUEUE_DATABASE_PATH),
        help="SQLite queue database path",
    )

    run_parser = subparsers.add_parser("run", help="Process queue and write merged Markdown")
    run_parser.add_argument(
        "--queue-db",
        dest="queue_database_path",
        default=str(DEFAULT_QUEUE_DATABASE_PATH),
        help="SQLite queue database path",
    )
    run_parser.add_argument(
        "--output-dir",
        dest="output_directory_path",
        default=str(DEFAULT_OUTPUT_DIRECTORY_PATH),
        help="Directory for intermediate outputs",
    )
    run_parser.add_argument(
        "--output-md",
        dest="merged_markdown_path",
        default=str(DEFAULT_MERGED_MARKDOWN_PATH),
        help="Merged Markdown output file path",
    )
    run_parser.add_argument(
        "--save-model-results",
        dest="save_model_results",
        action="store_true",
        help="Ask the model to save its own artifacts under output-dir",
    )
    run_parser.add_argument(
        "--fail-fast",
        dest="fail_fast",
        action="store_true",
        help="Stop immediately when a task fails",
    )

    status_parser = subparsers.add_parser("status", help="Show queue status counts")
    status_parser.add_argument(
        "--queue-db",
        dest="queue_database_path",
        default=str(DEFAULT_QUEUE_DATABASE_PATH),
        help="SQLite queue database path",
    )

    reset_parser = subparsers.add_parser("reset", help="Delete all tasks and (optionally) outputs")
    reset_parser.add_argument(
        "--queue-db",
        dest="queue_database_path",
        default=str(DEFAULT_QUEUE_DATABASE_PATH),
        help="SQLite queue database path",
    )
    reset_parser.add_argument(
        "--output-dir",
        dest="output_directory_path",
        default=str(DEFAULT_OUTPUT_DIRECTORY_PATH),
        help="Directory for intermediate outputs",
    )
    reset_parser.add_argument(
        "--output-md",
        dest="merged_markdown_path",
        default=str(DEFAULT_MERGED_MARKDOWN_PATH),
        help="Merged Markdown output file path",
    )
    reset_parser.add_argument(
        "--delete-outputs",
        dest="delete_outputs",
        action="store_true",
        help="Also delete output-dir and output-md",
    )
    reset_parser.add_argument(
        "--yes",
        dest="yes",
        action="store_true",
        help="Confirm destructive reset",
    )

    args = argument_parser.parse_args()

    if args.command == "enqueue":
        _run_enqueue_command(
            input_argument_strings=list(args.inputs),
            queue_database_path=Path(args.queue_database_path),
        )
        return

    if args.command == "run":
        _run_run_command(
            queue_database_path=Path(args.queue_database_path),
            output_directory_path=Path(args.output_directory_path),
            merged_markdown_path=Path(args.merged_markdown_path),
            save_model_results=bool(args.save_model_results),
            fail_fast=bool(args.fail_fast),
        )
        return

    if args.command == "status":
        _run_status_command(queue_database_path=Path(args.queue_database_path))
        return

    if args.command == "reset":
        _run_reset_command(
            queue_database_path=Path(args.queue_database_path),
            output_directory_path=Path(args.output_directory_path),
            merged_markdown_path=Path(args.merged_markdown_path),
            delete_outputs=bool(args.delete_outputs),
            yes=bool(args.yes),
        )
        return


def _run_enqueue_command(input_argument_strings: list[str], queue_database_path: Path) -> None:
    queue_store = QueueStore(queue_database_path)
    queue_store.initialize()

    input_paths = [Path(argument_string) for argument_string in input_argument_strings]
    discovery_report = discover_input_paths_in_enqueue_order(input_paths)
    expanded_paths = discovery_report.supported_file_paths_in_enqueue_order
    _print_enqueue_discovery_report(discovery_report)

    created_unix_timestamp_seconds = int(time.time())
    image_file_paths, pdf_file_paths = split_image_and_pdf_paths(expanded_paths)

    image_tasks_added_count = queue_store.enqueue_image_tasks(
        image_file_paths=image_file_paths,
        created_unix_timestamp_seconds=created_unix_timestamp_seconds,
    )

    pdf_tasks_added_count = 0
    for pdf_file_path in pdf_file_paths:
        pdf_total_pages = get_pdf_total_pages(pdf_file_path)
        pdf_tasks_added_count += queue_store.enqueue_pdf_page_tasks(
            pdf_file_path=pdf_file_path,
            pdf_total_pages=pdf_total_pages,
            created_unix_timestamp_seconds=created_unix_timestamp_seconds,
        )

    total_tasks_added_count = image_tasks_added_count + pdf_tasks_added_count
    if total_tasks_added_count == 0:
        # Guard: Avoid "successfully did nothing" for first-time users.
        print("Nothing was enqueued. Check your input paths and file types.")
        print(_render_supported_file_types_help())
        raise SystemExit(EXIT_CODE_NOTHING_ENQUEUED)

    print(
        f"Enqueued: image_tasks={image_tasks_added_count}, pdf_page_tasks={pdf_tasks_added_count}"
    )


def _run_run_command(
    queue_database_path: Path,
    output_directory_path: Path,
    merged_markdown_path: Path,
    *,
    save_model_results: bool,
    fail_fast: bool,
) -> None:
    queue_store = QueueStore(queue_database_path)
    queue_store.initialize()

    runtime_paths = RuntimePaths.from_arguments(
        queue_database_path=queue_database_path,
        output_directory_path=output_directory_path,
        merged_markdown_path=merged_markdown_path,
    )
    runtime_paths.work_directory_path.mkdir(parents=True, exist_ok=True)
    runtime_paths.per_task_markdown_directory_path.mkdir(parents=True, exist_ok=True)

    deepseek_settings = DeepSeekOcr2Settings.from_environment()
    deepseek_runner = DeepSeekOcr2Runner(settings=deepseek_settings)
    post_processing_settings = MarkdownPostProcessingSettings.from_environment()

    processed_tasks_count = 0
    failed_tasks_count = 0
    while True:
        next_task = queue_store.fetch_next_pending_task()
        if next_task is None:
            break

        queue_store.mark_task_running(next_task.task_id)
        try:
            task_markdown_path = _process_task_to_markdown(
                deepseek_runner=deepseek_runner,
                runtime_paths=runtime_paths,
                task=next_task,
                save_model_results=save_model_results,
            )
            queue_store.mark_task_completed(next_task.task_id, task_markdown_path)
            processed_tasks_count += 1
        except Exception as exception:
            queue_store.mark_task_failed(next_task.task_id, repr(exception))
            failed_tasks_count += 1
            print(f"Task failed (task_id={next_task.task_id}): {repr(exception)}")
            if fail_fast:
                raise

    tasks_in_enqueue_order = queue_store.fetch_tasks_in_enqueue_order()
    merge_tasks_into_single_markdown(
        tasks_in_enqueue_order,
        runtime_paths.merged_markdown_path,
        post_processing_settings,
    )

    print(
        f"Processed {processed_tasks_count} task(s), failed {failed_tasks_count} task(s). "
        f"Merged into {runtime_paths.merged_markdown_path}"
    )

def _run_status_command(queue_database_path: Path) -> None:
    queue_store = QueueStore(queue_database_path)
    queue_store.initialize()
    status_counts = queue_store.fetch_status_counts()
    if not status_counts:
        print("Queue is empty.")
        return

    for status, count in sorted(status_counts.items()):
        print(f"{status}: {count}")


def _print_enqueue_discovery_report(discovery_report: InputDiscoveryReport) -> None:
    missing_input_paths = discovery_report.missing_input_paths
    unsupported_input_file_paths = discovery_report.unsupported_input_file_paths
    directories_with_no_supported_files = discovery_report.directories_with_no_supported_files
    unknown_input_paths = discovery_report.unknown_input_paths

    if missing_input_paths:
        print("Missing input path(s):")
        _print_paths(missing_input_paths)

    if unsupported_input_file_paths:
        print("Unsupported input file(s):")
        _print_paths(unsupported_input_file_paths)
        print(_render_supported_file_types_help())

    if directories_with_no_supported_files:
        print("Directory contains no supported files:")
        _print_paths(directories_with_no_supported_files)
        print(_render_supported_file_types_help())

    if unknown_input_paths:
        print("Unknown input path type (not a file or directory):")
        _print_paths(unknown_input_paths)


def _print_paths(paths: list[Path]) -> None:
    for path in paths:
        print(f"- {path}")


def _render_supported_file_types_help() -> str:
    supported_image_extensions = ", ".join(sorted(SUPPORTED_IMAGE_FILE_EXTENSIONS))
    supported_pdf_extensions = ", ".join(sorted(SUPPORTED_PDF_FILE_EXTENSIONS))
    return (
        "Supported file types:\n"
        f"- Images: {supported_image_extensions}\n"
        f"- PDFs: {supported_pdf_extensions}"
    )


def _run_reset_command(
    queue_database_path: Path,
    output_directory_path: Path,
    merged_markdown_path: Path,
    *,
    delete_outputs: bool,
    yes: bool,
) -> None:
    if not yes:
        # Guard: destructive command must be explicitly confirmed.
        print("Refusing to reset without --yes.")
        return

    queue_store = QueueStore(queue_database_path)
    queue_store.initialize()
    deleted_tasks_count = queue_store.delete_all_tasks()
    print(f"Deleted {deleted_tasks_count} task(s) from queue.")

    if not delete_outputs:
        return

    _delete_output_paths_safely(
        output_directory_path=output_directory_path,
        merged_markdown_path=merged_markdown_path,
    )


def _delete_output_paths_safely(
    *,
    output_directory_path: Path,
    merged_markdown_path: Path,
) -> None:
    # Guard: Avoid accidentally deleting dangerous paths (root, drive root, etc).
    if _is_unsafe_deletion_target(output_directory_path):
        print(f"Refusing to delete output-dir: unsafe path: {output_directory_path}")
        return

    if output_directory_path.exists() and output_directory_path.is_dir():
        shutil.rmtree(output_directory_path)
        print(f"Deleted output-dir: {output_directory_path}")

    if merged_markdown_path.exists() and merged_markdown_path.is_file():
        merged_markdown_path.unlink()
        print(f"Deleted output-md: {merged_markdown_path}")


def _is_unsafe_deletion_target(directory_path: Path) -> bool:
    directory_path_string = str(directory_path).strip()
    if directory_path_string in UNSAFE_DELETION_PATH_STRINGS:
        return True

    # Guard: refuse deleting filesystem roots (POSIX "/" or Windows drive roots like "C:\").
    try:
        resolved = directory_path.resolve()
    except Exception:
        # Guard: if the path cannot be resolved, treat it as unsafe for deletion.
        return True

    if resolved == resolved.parent:
        return True

    return False

def _process_task_to_markdown(
    deepseek_runner: DeepSeekOcr2Runner,
    runtime_paths: RuntimePaths,
    task: object,
    *,
    save_model_results: bool,
) -> Path:
    # Guard: Keep task typing explicit without deeply nesting logic.
    from ocr_agent.queue_store import QueueTask, TASK_KIND_IMAGE

    if not isinstance(task, QueueTask):
        raise TypeError("task must be QueueTask")

    image_file_path = _resolve_task_image_path(runtime_paths=runtime_paths, task=task)

    task_output_directory_path = runtime_paths.output_directory_path / f"task_{task.task_id}"
    inferred_markdown = deepseek_runner.infer_markdown_from_image(
        image_file_path=image_file_path,
        output_directory_path=task_output_directory_path,
        save_results=save_model_results,
    )

    task_markdown_path = runtime_paths.per_task_markdown_directory_path / f"task_{task.task_id}.md"
    task_markdown_path.write_text(inferred_markdown, encoding="utf-8")
    return task_markdown_path


def _resolve_task_image_path(runtime_paths: RuntimePaths, task: object) -> Path:
    from ocr_agent.queue_store import QueueTask, TASK_KIND_IMAGE

    if not isinstance(task, QueueTask):
        raise TypeError("task must be QueueTask")

    if task.task_kind == TASK_KIND_IMAGE:
        return Path(task.source_path)

    if task.task_kind != TASK_KIND_PDF_PAGE:
        raise ValueError("Unsupported task kind")

    if task.pdf_page_index is None:
        raise ValueError("pdf_page_index is required for pdf_page task")

    pdf_file_path = Path(task.source_path)
    rendered_image_file_path = (
        runtime_paths.work_directory_path
        / f"pdf_{task.task_id}_page_{task.pdf_page_index + 1}.png"
    )

    if rendered_image_file_path.exists():
        return rendered_image_file_path

    return render_pdf_page_to_image_file(
        pdf_file_path=pdf_file_path,
        pdf_page_index=task.pdf_page_index,
        output_image_file_path=rendered_image_file_path,
    )


if __name__ == "__main__":
    main()

