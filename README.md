# ocr-agent (DeepSeek-OCR-2)

## このリポジトリの責務
- 画像/フォルダ/PDFを投入してタスクキューに積む
- DeepSeek-OCR-2で逐次OCRしてMarkdown化する
- 投入順（PDFはページ順）で1つの大きなMarkdown（`data/output.md`）へ結合する

公式情報:
- DeepSeek-OCR-2モデルカード: `https://huggingface.co/deepseek-ai/DeepSeek-OCR-2`
- Docker Desktop GPU support: `https://docs.docker.com/desktop/features/gpu/`
- WSL GPU compute: `https://learn.microsoft.com/en-us/windows/wsl/tutorials/gpu-compute`

## 前提（Windows想定）
- Docker Desktop（WSL2 backend）
- NVIDIA GPU（WSL2対応ドライバ）

## 最短で動かす（初見向け）
PowerShellで、リポジトリルートにて実行してください。

補足:
- `docker compose run --rm ...` は **都度コンテナを作って実行し、終了後に削除**します。Docker Desktop上でコンテナが「起動時だけ見えて消える」のは正常です（キャッシュは別に残ります）。
- PowerShellの環境によっては `&&` が使えないため、READMEのコマンドは **1行ずつ実行**してください（複数をまとめたい場合は PowerShell 7 などで検討）。

### 1) GPU疎通確認

```powershell
docker run --rm --gpus all nvidia/cuda:11.8.0-base-ubuntu22.04 nvidia-smi
```

### 2) イメージビルド
初回は非常に重いです（PyTorch + flash-attnのビルドあり）。

```powershell
docker compose build
```

### 3) GPUがコンテナから見えるか確認（重要）

```powershell
docker compose run --rm ocr-agent nvidia-smi
```

### 4) 入力を置く
ホストの `./data` がコンテナの `/data` にマウントされています。

- 入力: `data/input/`
- 出力: `data/output.md` と `data/output/`

#### 入力が無い場合（スモークテスト用の画像を生成）
手元に画像が無くても、まず「動くか」だけ確認できます。

```powershell
docker compose run --rm ocr-agent python3 tools/generate_text_image.py `
  --text HELLO_DEEPSEEK_OCR2_12345 `
  --out /data/input/smoke.png
```

### 5) キューに積む → 実行 → 結合Markdownを見る

```powershell
docker compose run --rm ocr-agent python3 -m ocr_agent.cli enqueue /data/input
docker compose run --rm ocr-agent python3 -m ocr_agent.cli run --output-md /data/output.md
```

結果:
- `data/output.md`

## 使い方（CLI）
### status（状態確認）

```powershell
docker compose run --rm ocr-agent python3 -m ocr_agent.cli status
```

### reset（やり直し）
過去の `failed` が残って「0件処理」に見えることがあるので、検証をやり直す時に使います。

```powershell
docker compose run --rm ocr-agent python3 -m ocr_agent.cli reset --yes --delete-outputs
```

## テスト（合成画像→OCR→期待値チェック）
GPU＋モデルが必要なので opt-in です。

```powershell
docker compose run --rm -e RUN_DEEPSEEK_OCR2_INTEGRATION_TESTS=1 ocr-agent bash -lc `
  "python3 -m pip install -r /workspace/requirements.dev.txt && pytest -q"
```

## トラブルシュート（初見で詰まりやすい所）
- **`nvidia-smi` がコンテナ内で失敗する**: まず `docker compose run --rm ocr-agent nvidia-smi` が通る必要があります。Docker Desktop側のGPU設定とWSL2のGPU computeが有効か確認してください。
  - Docker Desktop GPU support: `https://docs.docker.com/desktop/features/gpu/`
  - WSL GPU compute: `https://learn.microsoft.com/en-us/windows/wsl/tutorials/gpu-compute`
- **`enqueue` が `Nothing was enqueued` になる**: `data/input/` に画像/PDFが入っているか、入力パスが正しいか確認してください（対応拡張子: png/jpg/jpeg/webp/bmp/tif/tiff/pdf）。スモークテスト手順で画像生成してから再実行すると切り分けが速いです。
- **毎回Hugging Faceからダウンロードしているように見える**: モデルは `compose.yaml` の `hf-cache` ボリューム（`HF_HOME=/cache/huggingface`）にキャッシュされます。`docker compose run --rm` でコンテナが消えてもキャッシュは残ります。
  - どうしても消したい場合: `docker compose down -v`（ボリューム削除）
  - 「remote codeが更新されて再ダウンロードされる」のを避けたい場合: `.env` で `DEEPSEEK_OCR2_MODEL_REVISION` を固定してください（例は `.env.example`）。

## セキュリティと再現性（重要）
DeepSeek-OCR-2は `trust_remote_code=True` が必要です。初回実行時にモデル側のコードがダウンロードされます。
不安なら **revision固定**を推奨します（例: 特定のcommit hash / tag）。

環境変数:
- `DEEPSEEK_OCR2_MODEL_NAME`（デフォルト: `deepseek-ai/DeepSeek-OCR-2`）
- `DEEPSEEK_OCR2_MODEL_REVISION`（空なら未固定）
