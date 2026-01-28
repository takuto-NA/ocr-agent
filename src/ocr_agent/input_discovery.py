"""
Responsibility:
- Expand user-provided input paths (files/directories) deterministically.
- Preserve the user's input order, and provide stable ordering within directories.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


SUPPORTED_IMAGE_FILE_EXTENSIONS = {
    ".png",
    ".jpg",
    ".jpeg",
    ".webp",
    ".bmp",
    ".tif",
    ".tiff",
}

SUPPORTED_PDF_FILE_EXTENSIONS = {".pdf"}


@dataclass(frozen=True)
class InputDiscoveryReport:
    supported_file_paths_in_enqueue_order: list[Path]
    missing_input_paths: list[Path]
    unsupported_input_file_paths: list[Path]
    directories_with_no_supported_files: list[Path]
    unknown_input_paths: list[Path]


def expand_input_paths_in_enqueue_order(input_paths: Iterable[Path]) -> list[Path]:
    report = discover_input_paths_in_enqueue_order(input_paths)
    return report.supported_file_paths_in_enqueue_order


def split_image_and_pdf_paths(paths: Iterable[Path]) -> tuple[list[Path], list[Path]]:
    image_file_paths: list[Path] = []
    pdf_file_paths: list[Path] = []

    for path in paths:
        if _is_supported_image_file(path):
            image_file_paths.append(path)
            continue
        if _is_supported_pdf_file(path):
            pdf_file_paths.append(path)
            continue

    return image_file_paths, pdf_file_paths


def discover_input_paths_in_enqueue_order(input_paths: Iterable[Path]) -> InputDiscoveryReport:
    supported_file_paths_in_enqueue_order: list[Path] = []
    missing_input_paths: list[Path] = []
    unsupported_input_file_paths: list[Path] = []
    directories_with_no_supported_files: list[Path] = []
    unknown_input_paths: list[Path] = []

    for input_path in input_paths:
        if not input_path.exists():
            # Guard: Surface missing inputs for user diagnostics.
            missing_input_paths.append(input_path)
            continue

        if input_path.is_file():
            if _is_supported_file(input_path):
                supported_file_paths_in_enqueue_order.append(input_path)
                continue

            # Guard: Unsupported file path provided explicitly.
            unsupported_input_file_paths.append(input_path)
            continue

        if input_path.is_dir():
            discovered_file_paths = _list_supported_files_in_directory(input_path)
            if not discovered_file_paths:
                # Guard: Directory exists but contains no supported files.
                directories_with_no_supported_files.append(input_path)
                continue

            supported_file_paths_in_enqueue_order.extend(discovered_file_paths)
            continue

        # Guard: Unknown filesystem entry type (not file/dir).
        unknown_input_paths.append(input_path)

    return InputDiscoveryReport(
        supported_file_paths_in_enqueue_order=supported_file_paths_in_enqueue_order,
        missing_input_paths=missing_input_paths,
        unsupported_input_file_paths=unsupported_input_file_paths,
        directories_with_no_supported_files=directories_with_no_supported_files,
        unknown_input_paths=unknown_input_paths,
    )


def _list_supported_files_in_directory(directory_path: Path) -> list[Path]:
    # Deterministic order for reproducible queue ordering.
    file_paths: list[Path] = []
    for candidate_path in sorted(directory_path.rglob("*")):
        if not candidate_path.is_file():
            continue
        if not _is_supported_file(candidate_path):
            continue
        file_paths.append(candidate_path)
    return file_paths


def _is_supported_file(file_path: Path) -> bool:
    return _is_supported_image_file(file_path) or _is_supported_pdf_file(file_path)


def _is_supported_image_file(file_path: Path) -> bool:
    return file_path.suffix.lower() in SUPPORTED_IMAGE_FILE_EXTENSIONS


def _is_supported_pdf_file(file_path: Path) -> bool:
    return file_path.suffix.lower() in SUPPORTED_PDF_FILE_EXTENSIONS

