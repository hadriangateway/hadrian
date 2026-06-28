import { useEffect, useState } from "react";

interface VideoResultPlayerProps {
  blob: Blob;
}

/**
 * Renders a freshly-generated video from an in-memory blob, managing the object
 * URL lifecycle (created on mount, revoked on unmount) so it can drop straight
 * into a `MultiModelResultGrid` cell.
 */
export function VideoResultPlayer({ blob }: VideoResultPlayerProps) {
  const [url, setUrl] = useState<string>();

  useEffect(() => {
    const objectUrl = URL.createObjectURL(blob);
    setUrl(objectUrl);
    return () => URL.revokeObjectURL(objectUrl);
  }, [blob]);

  if (!url) return null;

  return (
    // eslint-disable-next-line jsx-a11y/media-has-caption
    <video
      src={url}
      controls
      className="aspect-video w-full rounded-xl border border-border bg-black"
    />
  );
}
