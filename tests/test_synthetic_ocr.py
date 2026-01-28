"""
Responsibility:
- Integration test: generate a synthetic text image, run DeepSeek-OCR-2, and verify output.
"""

from __future__ import annotations

import os
from pathlib import Path
import re

import pytest

from ocr_agent.config import DeepSeekOcr2Settings
from ocr_agent.deepseek_ocr2_runner import DeepSeekOcr2Runner


INTEGRATION_TEST_OPT_IN_ENVIRONMENT_VARIABLE_NAME = "RUN_DEEPSEEK_OCR2_INTEGRATION_TESTS"

# Keep the expected text simple and unambiguous for OCR stability.
EXPECTED_TEXT = "HELLO_DEEPSEEK_OCR2_12345"

SYNTHETIC_IMAGE_WIDTH_PIXELS = 1280
SYNTHETIC_IMAGE_HEIGHT_PIXELS = 720
SYNTHETIC_FONT_SIZE_PIXELS = 64
SYNTHETIC_PADDING_PIXELS = 64
SYNTHETIC_FONT_FILE_PATH = Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")


def _is_integration_test_enabled() -> bool:
    return os.getenv(INTEGRATION_TEST_OPT_IN_ENVIRONMENT_VARIABLE_NAME, "").strip() == "1"


def _normalize_text_for_contains_check(text: str) -> str:
    lowercase_text = text.lower()
    return re.sub(r"[^a-z0-9_]+", "", lowercase_text)


@pytest.mark.skipif(
    not _is_integration_test_enabled(),
    reason=f"Set {INTEGRATION_TEST_OPT_IN_ENVIRONMENT_VARIABLE_NAME}=1 to enable",
)
def test_synthetic_image_ocr_contains_expected_text(tmp_path: Path) -> None:
    synthetic_image_path = tmp_path / "synthetic.png"
    _generate_synthetic_text_image(
        text=EXPECTED_TEXT,
        output_image_path=synthetic_image_path,
    )

    output_directory_path = tmp_path / "model_output"

    settings = DeepSeekOcr2Settings.from_environment()
    runner = DeepSeekOcr2Runner(settings=settings)

    markdown = runner.infer_markdown_from_image(
        image_file_path=synthetic_image_path,
        output_directory_path=output_directory_path,
        save_results=False,
    )

    normalized_output = _normalize_text_for_contains_check(markdown)
    normalized_expected = _normalize_text_for_contains_check(EXPECTED_TEXT)
    assert normalized_expected in normalized_output


def _generate_synthetic_text_image(text: str, output_image_path: Path) -> None:
    from PIL import Image, ImageDraw, ImageFont

    output_image_path.parent.mkdir(parents=True, exist_ok=True)

    image = Image.new(
        "RGB",
        (SYNTHETIC_IMAGE_WIDTH_PIXELS, SYNTHETIC_IMAGE_HEIGHT_PIXELS),
        color=(255, 255, 255),
    )
    draw = ImageDraw.Draw(image)

    if SYNTHETIC_FONT_FILE_PATH.exists():
        font = ImageFont.truetype(str(SYNTHETIC_FONT_FILE_PATH), SYNTHETIC_FONT_SIZE_PIXELS)
    else:
        # Guard: If the expected font is missing, fall back to PIL default.
        font = ImageFont.load_default()

    draw.text((SYNTHETIC_PADDING_PIXELS, SYNTHETIC_PADDING_PIXELS), text, fill=(0, 0, 0), font=font)
    image.save(output_image_path)

