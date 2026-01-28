# Responsibility:
# - Provide a reproducible GPU-enabled runtime for DeepSeek-OCR-2 batch OCR.
# - Keep critical ML dependencies pinned for stability.

FROM nvidia/cuda:11.8.0-cudnn8-devel-ubuntu22.04

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    HF_HOME=/cache/huggingface

WORKDIR /workspace

RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 \
      python3-dev \
      python3-pip \
      python3-venv \
      git \
      build-essential \
      ninja-build \
      ca-certificates \
      fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# Keep pip itself modern enough for manylinux wheels.
RUN python3 -m pip install --no-cache-dir --upgrade pip setuptools wheel

COPY requirements.base.txt /workspace/requirements.base.txt
COPY requirements.dev.txt /workspace/requirements.dev.txt

RUN python3 -m pip install --no-cache-dir -r /workspace/requirements.base.txt

# flash-attn is recommended by DeepSeek-OCR-2 but can be sensitive to CUDA/PyTorch combos.
# `--no-build-isolation` follows the model card guidance.
RUN python3 -m pip install --no-cache-dir flash-attn==2.7.3 --no-build-isolation

COPY src /workspace/src
COPY tools /workspace/tools
COPY tests /workspace/tests

ENV PYTHONPATH=/workspace/src

CMD ["python3", "-m", "ocr_agent.cli", "--help"]
