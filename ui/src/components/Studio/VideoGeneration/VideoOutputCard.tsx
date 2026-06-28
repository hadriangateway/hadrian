import { useEffect, useState } from "react";
import { Download, Trash2 } from "lucide-react";
import { Button } from "@/components/Button/Button";
import { readVideoFile } from "@/services/opfs/opfsService";
import type { VideoHistoryEntry } from "@/pages/studio/types";

interface VideoOutputCardProps {
  entry: VideoHistoryEntry;
  onRemove: (id: string) => void;
}

/** Render a single stored video: load its blob from OPFS and play/download it. */
export function VideoOutputCard({ entry, onRemove }: VideoOutputCardProps) {
  const [url, setUrl] = useState<string | undefined>();

  useEffect(() => {
    let revoked = false;
    let objectUrl: string | undefined;
    if (entry.videoData) {
      readVideoFile(entry.videoData).then((blob) => {
        if (blob && !revoked) {
          objectUrl = URL.createObjectURL(blob);
          setUrl(objectUrl);
        }
      });
    }
    return () => {
      revoked = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [entry.videoData]);

  const handleDownload = () => {
    if (!url) return;
    const a = document.createElement("a");
    a.href = url;
    a.download = `${entry.jobId}.mp4`;
    document.body.appendChild(a);
    a.click();
    a.remove();
  };

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-card p-3">
      {url ? (
        // eslint-disable-next-line jsx-a11y/media-has-caption
        <video src={url} controls className="aspect-video w-full rounded-md bg-black" />
      ) : (
        <div className="flex aspect-video w-full items-center justify-center rounded-md bg-muted text-sm text-muted-foreground">
          {entry.videoData ? "Loading video…" : "Video unavailable"}
        </div>
      )}
      <p className="line-clamp-2 text-sm text-foreground" title={entry.prompt}>
        {entry.prompt}
      </p>
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span>
          {entry.modelId}
          {entry.options.seconds ? ` · ${entry.options.seconds}s` : ""}
          {entry.options.size ? ` · ${entry.options.size}` : ""}
        </span>
        <div className="flex gap-1">
          <Button
            variant="ghost"
            size="sm"
            onClick={handleDownload}
            disabled={!url}
            aria-label="Download video"
          >
            <Download className="h-4 w-4" />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => onRemove(entry.id)}
            aria-label="Remove video"
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  );
}
