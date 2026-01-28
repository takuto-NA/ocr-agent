"""
Responsibility:
- Convert a PDF page into an image file for OCR ingestion.
"""

from __future__ import annotations

from pathlib import Path

import pypdfium2 as pdfium
from PIL import Image

from ocr_agent.config import DEFAULT_PDF_RENDER_DPI


def get_pdf_total_pages(pdf_file_path: Path) -> int:
    if not pdf_file_path.exists():
        # Guard: Missing PDF should be surfaced.
        raise FileNotFoundError(str(pdf_file_path))

    pdf_document = pdfium.PdfDocument(str(pdf_file_path))
    return int(len(pdf_document))


def render_pdf_page_to_image_file(
    pdf_file_path: Path,
    pdf_page_index: int,
    output_image_file_path: Path,
    *,
    render_dots_per_inch: int = DEFAULT_PDF_RENDER_DPI,
) -> Path:
    if not pdf_file_path.exists():
        # Guard: Missing PDF should be surfaced.
        raise FileNotFoundError(str(pdf_file_path))

    if pdf_page_index < 0:
        # Guard: Page index must be non-negative.
        raise ValueError("pdf_page_index must be >= 0")

    output_image_file_path.parent.mkdir(parents=True, exist_ok=True)

    pdf_document = pdfium.PdfDocument(str(pdf_file_path))
    pdf_total_pages = int(len(pdf_document))
    if pdf_page_index >= pdf_total_pages:
        # Guard: Page index must be in range.
        raise ValueError("pdf_page_index is out of range")

    pdf_page = pdf_document[pdf_page_index]
    renderer = pdf_page.render(scale=_dots_per_inch_to_scale(render_dots_per_inch))
    pil_image: Image.Image = renderer.to_pil()
    pil_image.save(output_image_file_path)

    return output_image_file_path


def _dots_per_inch_to_scale(dots_per_inch: int) -> float:
    # pdfium uses 72 DPI as a base.
    base_pdf_dots_per_inch = 72
    return float(dots_per_inch) / float(base_pdf_dots_per_inch)

