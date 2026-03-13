import { createSignal, onMount, onCleanup } from "solid-js";
import { getThumbnail } from "../../lib/tauri";
import { getFileIcon } from "../../lib/fileIcons";

interface ThumbnailImageProps {
  filePath: string;
  extension: string;
  isDir: boolean;
  size: number;
}

// In-memory cache: path → data URL
const thumbCache = new Map<string, string>();
// Paths that returned null or errored — don't retry
const noThumbPaths = new Set<string>();

// ── Request queue with concurrency limiting ──
// Matches the backend's 12-worker limit so we don't overwhelm IPC
const MAX_CONCURRENT = 8;
let activeCount = 0;
const pendingQueue: Array<{ path: string; resolve: (url: string | null) => void }> = [];

function enqueueRequest(path: string): Promise<string | null> {
  return new Promise((resolve) => {
    pendingQueue.push({ path, resolve });
    drainQueue();
  });
}

function drainQueue() {
  while (activeCount < MAX_CONCURRENT && pendingQueue.length > 0) {
    const item = pendingQueue.shift()!;
    activeCount++;
    getThumbnail(item.path)
      .then((url) => item.resolve(url))
      .catch(() => item.resolve(null))
      .finally(() => {
        activeCount--;
        drainQueue();
      });
  }
}

/** Clear pending requests (e.g. on directory change). */
export function clearThumbnailQueue() {
  // Resolve all pending with null so they don't leak
  while (pendingQueue.length > 0) {
    pendingQueue.shift()!.resolve(null);
  }
}

export function ThumbnailImage(props: ThumbnailImageProps) {
  const [src, setSrc] = createSignal<string | null>(null);
  let cancelled = false;

  onMount(() => {
    if (props.isDir || !props.extension) return;

    // Instant cache hit
    const cached = thumbCache.get(props.filePath);
    if (cached) {
      setSrc(cached);
      return;
    }
    if (noThumbPaths.has(props.filePath)) return;

    enqueueRequest(props.filePath).then((dataUrl) => {
      if (cancelled) return;
      if (dataUrl) {
        thumbCache.set(props.filePath, dataUrl);
        setSrc(dataUrl);
      } else {
        noThumbPaths.add(props.filePath);
      }
    });
  });

  onCleanup(() => {
    cancelled = true;
  });

  const icon = () => getFileIcon(props.extension, props.isDir);
  const iconFontSize = () => Math.max(20, props.size * 0.45);

  return (
    <div
      class="grid-thumbnail"
      style={{
        width: `${props.size}px`,
        height: `${props.size}px`,
        "font-size": `${iconFontSize()}px`,
      }}
    >
      {src() ? (
        <img src={src()!} alt="" draggable={false} />
      ) : (
        <span class="icon" style={{ color: icon().color }}>
          {icon().icon}
        </span>
      )}
    </div>
  );
}
