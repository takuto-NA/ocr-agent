"""
Responsibility:
- Provide a small, explicit wrapper around DeepSeek-OCR-2 inference.
- Hide model loading details from the rest of the application.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import torch
from transformers import AutoModel, AutoTokenizer

from ocr_agent.config import DeepSeekOcr2Settings


DEFAULT_SAVED_MARKDOWN_FILENAME = "result.mmd"
CUDA_NOT_AVAILABLE_ERROR_MESSAGE = (
    "CUDA GPU is not available inside the container.\n"
    "Verify Docker GPU passthrough first:\n"
    "  docker compose run --rm ocr-agent nvidia-smi\n"
    "If that fails, fix Docker Desktop GPU support / WSL2 GPU drivers."
)


def _select_inference_dtype() -> torch.dtype:
    # Guard: The model card example uses BF16; fall back when unsupported.
    if torch.cuda.is_available() and torch.cuda.is_bf16_supported():
        return torch.bfloat16
    return torch.float16


def _normalize_infer_result_to_markdown(infer_result: Any) -> str:
    if infer_result is None:
        return ""

    if isinstance(infer_result, str):
        return infer_result

    if isinstance(infer_result, dict):
        for candidate_key in ("markdown", "text", "result", "output", "response"):
            candidate_value = infer_result.get(candidate_key)
            if isinstance(candidate_value, str):
                return candidate_value

        return str(infer_result)

    return str(infer_result)

def _read_saved_markdown_if_present(output_directory_path: Path) -> str | None:
    saved_markdown_path = output_directory_path / DEFAULT_SAVED_MARKDOWN_FILENAME
    if not saved_markdown_path.exists():
        return None
    return saved_markdown_path.read_text(encoding="utf-8")


@dataclass
class DeepSeekOcr2Runner:
    settings: DeepSeekOcr2Settings
    _tokenizer: Any | None = None
    _model: Any | None = None

    def _get_tokenizer(self) -> Any:
        if self._tokenizer is not None:
            return self._tokenizer
        self._tokenizer = AutoTokenizer.from_pretrained(
            self.settings.model_name,
            revision=self.settings.model_revision,
            trust_remote_code=True,
        )
        return self._tokenizer

    def _get_model(self) -> Any:
        if self._model is not None:
            return self._model

        if not torch.cuda.is_available():
            # Guard: This project targets GPU execution.
            raise RuntimeError(CUDA_NOT_AVAILABLE_ERROR_MESSAGE)

        inference_dtype = _select_inference_dtype()

        # Prefer flash-attn when available, but do not hard-fail if unavailable.
        # Guard: Not all builds accept this argument; fallback if necessary.
        try:
            model = AutoModel.from_pretrained(
                self.settings.model_name,
                revision=self.settings.model_revision,
                _attn_implementation="flash_attention_2",
                trust_remote_code=True,
                use_safetensors=True,
                torch_dtype=inference_dtype,
            )
        except TypeError:
            model = AutoModel.from_pretrained(
                self.settings.model_name,
                revision=self.settings.model_revision,
                trust_remote_code=True,
                use_safetensors=True,
                torch_dtype=inference_dtype,
            )

        model = model.eval().cuda()
        self._model = model
        return self._model

    def infer_markdown_from_image(
        self,
        image_file_path: Path,
        output_directory_path: Path,
        *,
        save_results: bool,
    ) -> str:
        if not image_file_path.exists():
            # Guard: Explicitly surface missing input.
            raise FileNotFoundError(str(image_file_path))

        output_directory_path.mkdir(parents=True, exist_ok=True)

        tokenizer = self._get_tokenizer()
        model = self._get_model()

        # Guard: DeepSeek-OCR-2 may print OCR results to stdout but return an empty value.
        # To reliably obtain Markdown, always enable saving and read `result.mmd` when present.
        infer_result = model.infer(
            tokenizer,
            prompt=self.settings.markdown_prompt,
            image_file=str(image_file_path),
            output_path=str(output_directory_path),
            base_size=self.settings.base_image_size_pixels,
            image_size=self.settings.inference_image_size_pixels,
            crop_mode=self.settings.enable_crop_mode,
            save_results=True,
        )

        saved_markdown = _read_saved_markdown_if_present(output_directory_path)
        if saved_markdown is not None and saved_markdown.strip() != "":
            return saved_markdown

        # Fallback: Some versions might still return the Markdown directly.
        return _normalize_infer_result_to_markdown(infer_result)

