"""
Responsibility:
- Merge per-task Markdown into one Markdown file in enqueue order.
"""

from __future__ import annotations

from pathlib import Path

from ocr_agent.queue_store import QueueTask, TASK_KIND_IMAGE, TASK_KIND_PDF_PAGE


def merge_tasks_into_single_markdown(
    tasks_in_enqueue_order: list[QueueTask],
    merged_markdown_path: Path,
) -> None:
    merged_markdown_path.parent.mkdir(parents=True, exist_ok=True)

    merged_lines: list[str] = []
    merged_lines.append("# OCR Output")
    merged_lines.append("")

    for task in tasks_in_enqueue_order:
        if task.output_markdown_path is None:
            continue
        task_markdown_path = Path(task.output_markdown_path)
        if not task_markdown_path.exists():
            continue

        task_markdown = task_markdown_path.read_text(encoding="utf-8")
        if task_markdown.strip() == "":
            continue

        merged_lines.extend(_render_task_header_lines(task))
        merged_lines.append("")
        merged_lines.append(task_markdown)
        merged_lines.append("")
        merged_lines.append("---")
        merged_lines.append("")

    merged_markdown_path.write_text("\n".join(merged_lines).rstrip() + "\n", encoding="utf-8")


def _render_task_header_lines(task: QueueTask) -> list[str]:
    source_path = task.source_path

    if task.task_kind == TASK_KIND_IMAGE:
        return [f"## {source_path}", ""]

    if task.task_kind == TASK_KIND_PDF_PAGE:
        if task.pdf_page_index is None or task.pdf_total_pages is None:
            return [f"## {source_path}", ""]

        page_number_human = task.pdf_page_index + 1
        return [f"## {source_path} (page {page_number_human}/{task.pdf_total_pages})", ""]

    return [f"## {source_path}", ""]

