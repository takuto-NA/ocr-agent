"""
Responsibility:
- Centralize configuration to avoid magic numbers and keep behavior reproducible.
"""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path


DEFAULT_MODEL_NAME = "deepseek-ai/DeepSeek-OCR-2"

# Model-card suggested settings (named to avoid magic numbers).
DEFAULT_BASE_IMAGE_SIZE_PIXELS = 1024
DEFAULT_INFERENCE_IMAGE_SIZE_PIXELS = 768
DEFAULT_ENABLE_CROP_MODE = True

# Prompt variants from model card.
DEFAULT_MARKDOWN_CONVERSION_PROMPT = "<image>\n<|grounding|>Convert the document to markdown. "

# PDF rendering defaults (named to avoid magic numbers).
DEFAULT_PDF_RENDER_DPI = 200

# Markdown post-processing defaults.
MATH_DELIMITER_STYLE_LATEX = "latex"  # Use \( \) and \[ \]
MATH_DELIMITER_STYLE_DOLLAR = "dollar"  # Use $ and $$
DEFAULT_MATH_DELIMITER_STYLE = MATH_DELIMITER_STYLE_DOLLAR


@dataclass(frozen=True)
class DeepSeekOcr2Settings:
    model_name: str
    model_revision: str | None
    markdown_prompt: str
    base_image_size_pixels: int
    inference_image_size_pixels: int
    enable_crop_mode: bool

    @staticmethod
    def from_environment() -> "DeepSeekOcr2Settings":
        def decode_escaped_newlines(value: str) -> str:
            # Guard: GUI passes prompts through environment variables where literal newlines are fragile.
            # Support both actual newlines and escaped sequences like "\\n".
            return value.replace("\\n", "\n")

        model_name = os.getenv("DEEPSEEK_OCR2_MODEL_NAME", DEFAULT_MODEL_NAME)
        model_revision_raw = os.getenv("DEEPSEEK_OCR2_MODEL_REVISION", "").strip()
        model_revision = model_revision_raw if model_revision_raw != "" else None
        markdown_prompt = decode_escaped_newlines(
            os.getenv(
            "DEEPSEEK_OCR2_MARKDOWN_PROMPT",
            DEFAULT_MARKDOWN_CONVERSION_PROMPT,
            )
        )
        base_image_size_pixels = int(
            os.getenv(
                "DEEPSEEK_OCR2_BASE_IMAGE_SIZE_PIXELS",
                str(DEFAULT_BASE_IMAGE_SIZE_PIXELS),
            )
        )
        inference_image_size_pixels = int(
            os.getenv(
                "DEEPSEEK_OCR2_INFERENCE_IMAGE_SIZE_PIXELS",
                str(DEFAULT_INFERENCE_IMAGE_SIZE_PIXELS),
            )
        )
        enable_crop_mode_raw = os.getenv(
            "DEEPSEEK_OCR2_ENABLE_CROP_MODE",
            "1" if DEFAULT_ENABLE_CROP_MODE else "0",
        )
        enable_crop_mode = enable_crop_mode_raw.strip() not in {"0", "false", "False"}

        return DeepSeekOcr2Settings(
            model_name=model_name,
            model_revision=model_revision,
            markdown_prompt=markdown_prompt,
            base_image_size_pixels=base_image_size_pixels,
            inference_image_size_pixels=inference_image_size_pixels,
            enable_crop_mode=enable_crop_mode,
        )


@dataclass(frozen=True)
class RuntimePaths:
    queue_database_path: Path
    output_directory_path: Path
    merged_markdown_path: Path
    work_directory_path: Path
    per_task_markdown_directory_path: Path

    @staticmethod
    def from_arguments(
        queue_database_path: Path,
        output_directory_path: Path,
        merged_markdown_path: Path,
    ) -> "RuntimePaths":
        work_directory_path = output_directory_path / "work"
        per_task_markdown_directory_path = output_directory_path / "markdown_items"
        return RuntimePaths(
            queue_database_path=queue_database_path,
            output_directory_path=output_directory_path,
            merged_markdown_path=merged_markdown_path,
            work_directory_path=work_directory_path,
            per_task_markdown_directory_path=per_task_markdown_directory_path,
        )


@dataclass(frozen=True)
class MarkdownPostProcessingSettings:
    math_delimiter_style: str

    @staticmethod
    def from_environment() -> "MarkdownPostProcessingSettings":
        raw = os.getenv("OCR_AGENT_MATH_DELIMITER_STYLE", DEFAULT_MATH_DELIMITER_STYLE).strip()
        normalized = raw.lower()
        if normalized in {MATH_DELIMITER_STYLE_LATEX, MATH_DELIMITER_STYLE_DOLLAR}:
            return MarkdownPostProcessingSettings(math_delimiter_style=normalized)

        # Guard: Unknown value should fall back to a safe default.
        return MarkdownPostProcessingSettings(math_delimiter_style=DEFAULT_MATH_DELIMITER_STYLE)

