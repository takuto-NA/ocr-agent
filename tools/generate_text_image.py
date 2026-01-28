"""
Responsibility:
- Generate a synthetic image containing a given text (for OCR integration testing).
"""

from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


DEFAULT_IMAGE_WIDTH_PIXELS = 1280
DEFAULT_IMAGE_HEIGHT_PIXELS = 720
DEFAULT_FONT_SIZE_PIXELS = 64
DEFAULT_PADDING_PIXELS = 64

# A widely-available font on Ubuntu images.
DEFAULT_FONT_FILE_PATH = Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")


def main() -> None:
    argument_parser = argparse.ArgumentParser(prog="generate-text-image")
    argument_parser.add_argument("--text", required=True, help="Text to embed in the image")
    argument_parser.add_argument("--out", required=True, help="Output image path (png)")
    argument_parser.add_argument(
        "--width",
        type=int,
        default=DEFAULT_IMAGE_WIDTH_PIXELS,
        help="Image width in pixels",
    )
    argument_parser.add_argument(
        "--height",
        type=int,
        default=DEFAULT_IMAGE_HEIGHT_PIXELS,
        help="Image height in pixels",
    )
    argument_parser.add_argument(
        "--font-size",
        type=int,
        default=DEFAULT_FONT_SIZE_PIXELS,
        help="Font size in pixels",
    )
    argument_parser.add_argument(
        "--padding",
        type=int,
        default=DEFAULT_PADDING_PIXELS,
        help="Padding around text in pixels",
    )
    argument_parser.add_argument(
        "--font-file",
        default=str(DEFAULT_FONT_FILE_PATH),
        help="Font file path",
    )

    parsed_arguments = argument_parser.parse_args()

    output_image_path = Path(parsed_arguments.out)
    output_image_path.parent.mkdir(parents=True, exist_ok=True)

    image = Image.new(
        "RGB",
        (int(parsed_arguments.width), int(parsed_arguments.height)),
        color=(255, 255, 255),
    )
    image_draw = ImageDraw.Draw(image)
    font = _load_font(Path(parsed_arguments.font_file), int(parsed_arguments.font_size))

    # Keep it simple: draw at fixed padding position for maximum OCR stability.
    image_draw.text(
        (int(parsed_arguments.padding), int(parsed_arguments.padding)),
        str(parsed_arguments.text),
        fill=(0, 0, 0),
        font=font,
    )
    image.save(output_image_path)


def _load_font(font_file_path: Path, font_size_pixels: int) -> ImageFont.FreeTypeFont:
    if font_file_path.exists():
        return ImageFont.truetype(str(font_file_path), font_size_pixels)

    # Guard: fall back to PIL default if the expected font is missing.
    return ImageFont.load_default()


if __name__ == "__main__":
    main()

