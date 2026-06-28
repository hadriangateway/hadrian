/**
 * OPFS (Origin Private File System) service for persisting generated media
 * blobs (audio and video). Files live under per-media subdirectories with
 * filenames of the form `{entryId}_{sanitizedInstanceId}.{format}`.
 *
 * All methods catch errors and return null/empty on failure (graceful degradation).
 */

const AUDIO_DIR = "audio";
const VIDEO_DIR = "video";

/** Check whether OPFS is available in this browser. */
export function isAvailable(): boolean {
  return (
    typeof navigator !== "undefined" &&
    "storage" in navigator &&
    "getDirectory" in navigator.storage
  );
}

async function getAudioDir(): Promise<FileSystemDirectoryHandle> {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(AUDIO_DIR, { create: true });
}

function sanitize(s: string): string {
  return s.replace(/[^a-zA-Z0-9_-]/g, "_");
}

function buildFilename(entryId: string, instanceId: string, format: string): string {
  return `${entryId}_${sanitize(instanceId)}.${format}`;
}

/** Write an audio blob to OPFS. Returns the filename on success, or null on failure. */
export async function writeAudioFile(
  entryId: string,
  instanceId: string,
  format: string,
  blob: Blob
): Promise<string | null> {
  try {
    if (!isAvailable()) return null;
    const dir = await getAudioDir();
    const filename = buildFilename(entryId, instanceId, format);
    const fileHandle = await dir.getFileHandle(filename, { create: true });
    const writable = await fileHandle.createWritable();
    await writable.write(blob);
    await writable.close();
    return filename;
  } catch (e) {
    console.warn("OPFS writeAudioFile failed:", e);
    return null;
  }
}

/** Read an audio blob from OPFS by filename. Returns null if unavailable or missing. */
export async function readAudioFile(filename: string): Promise<Blob | null> {
  try {
    if (!isAvailable()) return null;
    const dir = await getAudioDir();
    const fileHandle = await dir.getFileHandle(filename);
    const file = await fileHandle.getFile();
    return file;
  } catch (e) {
    console.warn("OPFS readAudioFile failed:", e);
    return null;
  }
}

/** Delete all OPFS audio files whose filename starts with the given entryId. */
export async function deleteAudioFilesForEntry(entryId: string): Promise<void> {
  try {
    if (!isAvailable()) return;
    const dir = await getAudioDir();
    const prefix = `${entryId}_`;
    for await (const [name] of dir as unknown as AsyncIterable<[string, FileSystemHandle]>) {
      if (name.startsWith(prefix)) {
        await dir.removeEntry(name);
      }
    }
  } catch (e) {
    console.warn("OPFS deleteAudioFilesForEntry failed:", e);
  }
}

/** Remove the entire `audio/` directory from OPFS (no-op if it doesn't exist). */
export async function clearAllAudioFiles(): Promise<void> {
  try {
    if (!isAvailable()) return;
    const root = await navigator.storage.getDirectory();
    // Check if the directory exists before trying to remove it
    await root.getDirectoryHandle(AUDIO_DIR);
    await root.removeEntry(AUDIO_DIR, { recursive: true });
  } catch {
    // NotFoundError is expected when the directory was never created
  }
}

// ---------------------------------------------------------------------------
// Video blobs — same scheme as audio, under a `video/` subdirectory.
// ---------------------------------------------------------------------------

async function getVideoDir(): Promise<FileSystemDirectoryHandle> {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(VIDEO_DIR, { create: true });
}

/** Write a video blob to OPFS. Returns the filename on success, or null on failure. */
export async function writeVideoFile(
  entryId: string,
  instanceId: string,
  format: string,
  blob: Blob
): Promise<string | null> {
  try {
    if (!isAvailable()) return null;
    const dir = await getVideoDir();
    const filename = buildFilename(entryId, instanceId, format);
    const fileHandle = await dir.getFileHandle(filename, { create: true });
    const writable = await fileHandle.createWritable();
    await writable.write(blob);
    await writable.close();
    return filename;
  } catch (e) {
    console.warn("OPFS writeVideoFile failed:", e);
    return null;
  }
}

/** Read a video blob from OPFS by filename. Returns null if unavailable or missing. */
export async function readVideoFile(filename: string): Promise<Blob | null> {
  try {
    if (!isAvailable()) return null;
    const dir = await getVideoDir();
    const fileHandle = await dir.getFileHandle(filename);
    return await fileHandle.getFile();
  } catch (e) {
    console.warn("OPFS readVideoFile failed:", e);
    return null;
  }
}

/** Delete all OPFS video files whose filename starts with the given entryId. */
export async function deleteVideoFilesForEntry(entryId: string): Promise<void> {
  try {
    if (!isAvailable()) return;
    const dir = await getVideoDir();
    const prefix = `${entryId}_`;
    for await (const [name] of dir as unknown as AsyncIterable<[string, FileSystemHandle]>) {
      if (name.startsWith(prefix)) {
        await dir.removeEntry(name);
      }
    }
  } catch (e) {
    console.warn("OPFS deleteVideoFilesForEntry failed:", e);
  }
}

/** Remove the entire `video/` directory from OPFS (no-op if it doesn't exist). */
export async function clearAllVideoFiles(): Promise<void> {
  try {
    if (!isAvailable()) return;
    const root = await navigator.storage.getDirectory();
    await root.getDirectoryHandle(VIDEO_DIR);
    await root.removeEntry(VIDEO_DIR, { recursive: true });
  } catch {
    // NotFoundError is expected when the directory was never created
  }
}

export interface AudioStorageStats {
  fileCount: number;
  totalBytes: number;
}

/** Get the number of files and total size of the OPFS audio directory. */
export async function getAudioStorageStats(): Promise<AudioStorageStats> {
  const stats: AudioStorageStats = { fileCount: 0, totalBytes: 0 };
  try {
    if (!isAvailable()) return stats;
    const dir = await getAudioDir();
    for await (const [, handle] of dir as unknown as AsyncIterable<[string, FileSystemHandle]>) {
      if (handle.kind === "file") {
        const file = await (handle as FileSystemFileHandle).getFile();
        stats.fileCount++;
        stats.totalBytes += file.size;
      }
    }
  } catch (e) {
    console.warn("OPFS getAudioStorageStats failed:", e);
  }
  return stats;
}
