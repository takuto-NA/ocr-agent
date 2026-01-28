/**
 * Responsibility:
 * - Show the currently running input (image / PDF page) as a preview.
 * - Visualize the model's focus areas by overlaying <|det|> rectangles parsed from logs.
 */
import { useEffect, useMemo, useRef, useState } from "react";

type DetectionRectangle = {
  id: string;
  refType: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
};

export type CurrentTaskPreview = {
  task_id: number;
  task_kind: string;
  source_path: string;
  pdf_page_index: number | null;
  pdf_total_pages: number | null;
  preview_image_file_path: string | null;
  deepseek_inference_image_size_pixels: number | null;
};

const MAX_DETECTION_RECTANGLES_TO_SHOW = 24;
const LOG_LINES_TO_SCAN_FOR_DETECTIONS = 500;
const DEFAULT_INFERENCE_IMAGE_SIZE_PIXELS_FALLBACK = 768;

function extractDetectionsFromLogLines(logLines: string[]): DetectionRectangle[] {
  const tail = logLines.slice(Math.max(0, logLines.length - LOG_LINES_TO_SCAN_FOR_DETECTIONS));
  const detections: DetectionRectangle[] = [];

  const detPattern =
    /<\|ref\|>(?<refType>.*?)<\|\/ref\|><\|det\|>\[\[(?<x1>\d+),\s*(?<y1>\d+),\s*(?<x2>\d+),\s*(?<y2>\d+)\]\]<\|\/det\|>/;

  for (let index = 0; index < tail.length; index += 1) {
    const line = tail[index] ?? "";
    const match = line.match(detPattern);
    if (match === null) {
      continue;
    }
    const groups = match.groups;
    if (!groups) {
      continue;
    }
    const x1 = Number.parseInt(groups.x1, 10);
    const y1 = Number.parseInt(groups.y1, 10);
    const x2 = Number.parseInt(groups.x2, 10);
    const y2 = Number.parseInt(groups.y2, 10);
    if (![x1, y1, x2, y2].every(Number.isFinite)) {
      continue;
    }

    detections.push({
      id: `det-${logLines.length - tail.length + index}`,
      refType: groups.refType ?? "ref",
      x1,
      y1,
      x2,
      y2
    });
  }

  if (detections.length <= MAX_DETECTION_RECTANGLES_TO_SHOW) {
    return detections;
  }
  return detections.slice(detections.length - MAX_DETECTION_RECTANGLES_TO_SHOW);
}

function formatTaskLabel(preview: CurrentTaskPreview): string {
  const kind = preview.task_kind;
  if (kind === "pdf_page" && preview.pdf_page_index !== null && preview.pdf_total_pages !== null) {
    return `PDF page ${preview.pdf_page_index + 1}/${preview.pdf_total_pages}`;
  }
  return kind;
}

type PreviewPanelProps = {
  preview: CurrentTaskPreview | null;
  previewImageUrl: string | null;
  backendLogLines: string[];
  fallbackInferenceImageSizePixels: number;
};

export function PreviewPanel(props: PreviewPanelProps) {
  const { preview, previewImageUrl, backendLogLines, fallbackInferenceImageSizePixels } = props;
  const previewContainerRef = useRef<HTMLDivElement | null>(null);
  const [containerSizePixels, setContainerSizePixels] = useState<number>(0);

  useEffect(() => {
    const element = previewContainerRef.current;
    if (element === null) {
      return;
    }

    const observer = new ResizeObserver(() => {
      const width = element.clientWidth;
      const height = element.clientHeight;
      setContainerSizePixels(Math.max(0, Math.min(width, height)));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const detections = useMemo(() => extractDetectionsFromLogLines(backendLogLines), [backendLogLines]);

  const inferenceSizePixels =
    preview?.deepseek_inference_image_size_pixels ?? fallbackInferenceImageSizePixels ?? DEFAULT_INFERENCE_IMAGE_SIZE_PIXELS_FALLBACK;

  const coordinateScale = containerSizePixels > 0 ? containerSizePixels / inferenceSizePixels : 0;

  if (preview === null) {
    return (
      <div className="label">
        No running task yet. Start a job to see the current page/image and detection rectangles.
      </div>
    );
  }

  return (
    <div>
      <div className="row" style={{ justifyContent: "space-between", width: "100%" }}>
        <div className="label">
          Now processing: <b>{formatTaskLabel(preview)}</b> · task_id: <span className="mono">{preview.task_id}</span>
        </div>
        <div className="label">det: <b>{detections.length}</b></div>
      </div>
      <div style={{ height: 10 }} />
      <div className="previewContainer" ref={previewContainerRef}>
        {previewImageUrl === null ? (
          <div className="label">Waiting for preview image…</div>
        ) : (
          <>
            <img className="previewImage" src={previewImageUrl} alt="current task preview" />
            <div className="previewOverlay" aria-hidden="true">
              {coordinateScale > 0
                ? detections.map((det, index) => {
                    const isMostRecent = index === detections.length - 1;
                    const left = det.x1 * coordinateScale;
                    const top = det.y1 * coordinateScale;
                    const width = Math.max(0, (det.x2 - det.x1) * coordinateScale);
                    const height = Math.max(0, (det.y2 - det.y1) * coordinateScale);
                    return (
                      <div
                        key={det.id}
                        className={`previewBox ${isMostRecent ? "previewBoxActive" : ""}`}
                        style={{ left, top, width, height }}
                        title={`${det.refType} [${det.x1},${det.y1},${det.x2},${det.y2}]`}
                      />
                    );
                  })
                : null}
            </div>
          </>
        )}
      </div>
      <div style={{ height: 10 }} />
      <div className="label mono" style={{ opacity: 0.85 }}>
        {preview.preview_image_file_path ?? preview.source_path}
      </div>
      <div className="label" style={{ marginTop: 6 }}>
        Note: detection rectangles are visualized in the model's coordinate space (approx.).
      </div>
    </div>
  );
}

