"""
Responsibility:
- Merge per-task Markdown into one Markdown file in enqueue order.
"""

from __future__ import annotations

from pathlib import Path
import re

from ocr_agent.config import (
    MATH_DELIMITER_STYLE_DOLLAR,
    MarkdownPostProcessingSettings,
)
from ocr_agent.queue_store import QueueTask, TASK_KIND_IMAGE, TASK_KIND_PDF_PAGE


def merge_tasks_into_single_markdown(
    tasks_in_enqueue_order: list[QueueTask],
    merged_markdown_path: Path,
    post_processing_settings: MarkdownPostProcessingSettings,
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
        merged_lines.append(_post_process_task_markdown(task_markdown, post_processing_settings))
        merged_lines.append("")
        merged_lines.append("---")
        merged_lines.append("")

    merged_markdown_path.write_text("\n".join(merged_lines).rstrip() + "\n", encoding="utf-8")


_INLINE_LATEX_MATH_PATTERN = re.compile(r"\\\((.+?)\\\)", flags=re.DOTALL)
_BLOCK_LATEX_MATH_PATTERN = re.compile(r"\\\[(.+?)\\\]", flags=re.DOTALL)
_FENCE_START_PATTERN = re.compile(r"^(\s*)(`{3,}|~{3,})")


def _post_process_task_markdown(
    task_markdown: str, post_processing_settings: MarkdownPostProcessingSettings
) -> str:
    if post_processing_settings.math_delimiter_style != MATH_DELIMITER_STYLE_DOLLAR:
        return task_markdown
    return _convert_latex_math_delimiters_to_dollar(task_markdown)


def _convert_latex_math_delimiters_to_dollar(markdown_text: str) -> str:
    """
    Convert LaTeX-style delimiters to dollar-style delimiters:
    - \\( ... \\)  -> $...$
    - \\[ ... \\]  -> $$\\n...\\n$$

    Guard:
    - Do not rewrite fenced code blocks (``` / ~~~).
    """

    lines = markdown_text.splitlines(keepends=True)
    converted_chunks: list[str] = []
    is_inside_fenced_code_block = False
    fence_marker: str | None = None

    current_non_code_chunk: list[str] = []

    def flush_non_code_chunk() -> None:
        if not current_non_code_chunk:
            return
        raw_chunk = "".join(current_non_code_chunk)
        converted_chunks.append(_convert_latex_math_delimiters_in_plain_markdown(raw_chunk))
        current_non_code_chunk.clear()

    for line in lines:
        fence_match = _FENCE_START_PATTERN.match(line)
        if fence_match is None:
            if is_inside_fenced_code_block:
                converted_chunks.append(line)
            else:
                current_non_code_chunk.append(line)
            continue

        indent, marker = fence_match.group(1), fence_match.group(2)
        if indent.strip() != "":
            # Guard: indented fences are treated as plain text.
            if is_inside_fenced_code_block:
                converted_chunks.append(line)
            else:
                current_non_code_chunk.append(line)
            continue

        if not is_inside_fenced_code_block:
            flush_non_code_chunk()
            is_inside_fenced_code_block = True
            fence_marker = marker
            converted_chunks.append(line)
            continue

        if fence_marker is not None and marker.startswith(fence_marker[0]):
            is_inside_fenced_code_block = False
            fence_marker = None
            converted_chunks.append(line)
            continue

        converted_chunks.append(line)

    flush_non_code_chunk()
    return "".join(converted_chunks)


def _convert_latex_math_delimiters_in_plain_markdown(markdown_text: str) -> str:
    def replace_block(match: re.Match[str]) -> str:
        content = match.group(1)
        content = content.strip("\n")
        return f"$$\n{content}\n$$"

    def replace_inline(match: re.Match[str]) -> str:
        content = match.group(1)
        content = content.strip()
        return f"${content}$"

    after_block = _BLOCK_LATEX_MATH_PATTERN.sub(replace_block, markdown_text)
    return _INLINE_LATEX_MATH_PATTERN.sub(replace_inline, after_block)


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

