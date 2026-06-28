import { useCallback } from "react";
import { useIndexedDB } from "@/hooks/useIndexedDB";
import {
  deleteAudioFilesForEntry,
  clearAllAudioFiles,
  deleteVideoFilesForEntry,
  clearAllVideoFiles,
} from "@/services/opfs/opfsService";
import type {
  ImageHistoryEntry,
  AudioHistoryEntry,
  TranscriptionHistoryEntry,
  VideoHistoryEntry,
} from "./types";

const LIMITS = {
  images: 100,
  audio: 50,
  transcriptions: 200,
  videos: 50,
} as const;

function useHistoryArray<T extends { id: string }>(key: string, maxSize: number) {
  const { value, setValue, isLoading } = useIndexedDB<T[]>(key, []);

  /** Add an entry, returning any evicted entries that exceeded maxSize. */
  const addEntry = useCallback(
    (entry: T): T[] => {
      let evicted: T[] = [];
      setValue((prev) => {
        const next = [entry, ...prev];
        if (next.length > maxSize) {
          evicted = next.slice(maxSize);
          return next.slice(0, maxSize);
        }
        return next;
      });
      return evicted;
    },
    [setValue, maxSize]
  );

  const removeEntry = useCallback(
    (id: string) => {
      setValue((prev) => prev.filter((e) => e.id !== id));
    },
    [setValue]
  );

  const clearAll = useCallback(() => {
    setValue([]);
  }, [setValue]);

  return { entries: value, addEntry, removeEntry, clearAll, isLoading };
}

export function useImageHistory() {
  return useHistoryArray<ImageHistoryEntry>("studio-image-history", LIMITS.images);
}

export function useAudioHistory() {
  const {
    entries,
    addEntry: rawAdd,
    removeEntry: rawRemove,
    clearAll: rawClear,
    isLoading,
  } = useHistoryArray<AudioHistoryEntry>("studio-audio-history", LIMITS.audio);

  const addEntry = useCallback(
    (entry: AudioHistoryEntry) => {
      const evicted = rawAdd(entry);
      for (const e of evicted) {
        deleteAudioFilesForEntry(e.id);
      }
    },
    [rawAdd]
  );

  const removeEntry = useCallback(
    (id: string) => {
      rawRemove(id);
      deleteAudioFilesForEntry(id);
    },
    [rawRemove]
  );

  const clearAllEntries = useCallback(() => {
    rawClear();
    clearAllAudioFiles();
  }, [rawClear]);

  return { entries, addEntry, removeEntry, clearAll: clearAllEntries, isLoading };
}

export function useTranscriptionHistory() {
  return useHistoryArray<TranscriptionHistoryEntry>(
    "studio-transcription-history",
    LIMITS.transcriptions
  );
}

export function useVideoHistory() {
  const {
    entries,
    addEntry: rawAdd,
    removeEntry: rawRemove,
    clearAll: rawClear,
    isLoading,
  } = useHistoryArray<VideoHistoryEntry>("studio-video-history", LIMITS.videos);

  const addEntry = useCallback(
    (entry: VideoHistoryEntry) => {
      const evicted = rawAdd(entry);
      for (const e of evicted) {
        deleteVideoFilesForEntry(e.id);
      }
    },
    [rawAdd]
  );

  const removeEntry = useCallback(
    (id: string) => {
      rawRemove(id);
      deleteVideoFilesForEntry(id);
    },
    [rawRemove]
  );

  const clearAllEntries = useCallback(() => {
    rawClear();
    clearAllVideoFiles();
  }, [rawClear]);

  return { entries, addEntry, removeEntry, clearAll: clearAllEntries, isLoading };
}
