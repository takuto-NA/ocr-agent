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

## GUI（Tauri）: ジョブ実行（MVP）
このGUIは **既存のDocker+CLIをそのまま使うジョブランナー** です。

できること（MVP）:
- 出力先（=ジョブルート）を選ぶ
- 画像/PDF/フォルダをファイルダイアログで追加（内部的に `input/` にコピー）
- Startで `enqueue → run` を実行
- `queue.sqlite3` を監視して進捗/推定残り時間を表示
- 完了すると `output.md` がジョブルートに生成される

### 前提
- Docker Desktop（WSL2 backend）+ NVIDIA GPU（CLIと同じ）
- Node.js（`npm`）
- Rust toolchain（`cargo` が使えること）
  - Windowsの場合、Tauriのビルドに Visual Studio Build Tools が必要になることがあります

### 起動（開発）
PowerShellで、リポジトリルートから実行してください。

```powershell
# 1) OCR用Dockerイメージ（初回は重い）
docker compose build

# 2) GUI依存を入れる
cd gui
npm install

# 3) GUI起動（Tauri dev）
npm run dev
```

GUIの使い方:
- 「Select output directory」を押して出力先フォルダを選択（ここがジョブルートになります）
- 「Add files」「Add folder」で入力を追加
- 「Start OCR」で実行
- 結果: `output.md`（中間: `output/`, キュー: `queue.sqlite3`, 入力コピー: `input/`）

## 自動化（watch-folder）: 外部連携の受け口（Slack前のベストプラクティス）
Slack連携を作る前に、まず「外部からファイルが入ってきたら自動でOCRする」を成立させるための仕組みです。
GUIを起動したまま **inboxフォルダを監視**し、投入が完了したバンドルを検知してジョブ化します。

### 使い方
GUIで以下を設定します:
- **Select inbox directory**: 監視する受け口フォルダ（例: `C:\ocr-agent-inbox`）
- **Select jobs root (optional)**: ジョブ出力先ルート（未指定なら `inbox/jobs`）
- **Start watch-folder**: 監視開始

### 投入契約（初見が詰まらない最小仕様）
Windowsのファイルコピーは途中状態が見えることがあるため、**`.ready` を「投入完了の合図」**にします。

1) `inbox/<bundle>/` を作り、その中に画像/PDF（またはサブフォルダ）をコピー  
2) 最後に `inbox/<bundle>/.ready` を作成（空ファイルでOK）  
3) GUIが検知するとジョブが作られ、OCRが始まります

生成されるもの（jobs root配下）:
- `jobs/<job_id>/input/`（投入コピー）
- `jobs/<job_id>/output/`（中間生成物）
- `jobs/<job_id>/ocr_output_<timestamp>.md`（結果Markdown）
- `jobs/<job_id>/job_state.json`（ジョブ状態: queued/running/completed/failed）

inbox側のマーカー（bundle内）:
- `.ready`: 投入完了
- `.processing`: 処理中（排他用）
- `.processed`: 受理済み（重複処理防止）
- `.failed`: 受理失敗（エラー内容が書かれる）

### Slackへ移行するとき
将来Slackを実装するときは、Slack側でダウンロードしたファイルをこの `inbox/<bundle>/` に置く（または同等のJobRouterを呼ぶ）だけで移行できます。

### メモ: リポジトリ外から実行したい場合
GUIは `compose.yaml` を使ってDocker実行します。`compose.yaml` の場所が自動推定できない場合は、環境変数で指定できます。

- `OCR_AGENT_REPO_ROOT`: `compose.yaml` があるリポジトリルート（例: `C:\\Users\\owner\\Documents\\git\\ocr-agent`）
