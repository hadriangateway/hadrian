import { useCallback, useEffect, useRef, useState } from "react";
import { Clapperboard, Film } from "lucide-react";
import { cn } from "@/utils/cn";
import { Button } from "@/components/Button/Button";
import { useToast } from "@/components/Toast/Toast";
import { PromptInput } from "@/components/Studio/PromptInput/PromptInput";
import { ModelSelector } from "@/components/ModelSelector/ModelSelector";
import { MultiModelResultGrid } from "@/components/Studio/MultiModelResultGrid/MultiModelResultGrid";
import { useVideoHistory } from "@/pages/studio/useStudioHistory";
import {
  useMultiModelExecution,
  extractCostFromResponse,
} from "@/pages/studio/useMultiModelExecution";
import { usePreferences } from "@/preferences/PreferencesProvider";
import { createDefaultInstance } from "@/components/chat-types";
import type { ModelInstance } from "@/components/chat-types";
import type { ModelInfo } from "@/components/ModelPicker/model-utils";
import {
  apiV1VideosCreate,
  apiV1VideosRetrieve,
  apiV1VideosContent,
} from "@/api/generated/sdk.gen";
import { writeVideoFile } from "@/services/opfs/opfsService";
import { VideoResultPlayer } from "./VideoResultPlayer";
import { VideoOutputCard } from "./VideoOutputCard";

const SECONDS_OPTIONS = ["4", "8", "12"] as const;
const SIZE_OPTIONS = ["720x1280", "1280x720", "1024x1792", "1792x1024"] as const;

type Seconds = (typeof SECONDS_OPTIONS)[number];
type Size = (typeof SIZE_OPTIONS)[number];

const POLL_INTERVAL_MS = 3000;
// Stop polling after this long so a job stuck `queued`/`in_progress` upstream
// can't keep the browser polling forever: the abort signal only fires on a new
// run or clear, not when the panel unmounts.
const MAX_POLL_MS = 10 * 60 * 1000;

interface VideoResult {
  blob: Blob;
  videoId: string;
  seconds?: string | null;
  size?: string | null;
  prompt?: string | null;
}

interface VideoGenerationPanelProps {
  availableModels?: ModelInfo[];
}

export function VideoGenerationPanel({ availableModels }: VideoGenerationPanelProps) {
  const [prompt, setPrompt] = useState("");
  const [seconds, setSeconds] = useState<Seconds>("4");
  const [size, setSize] = useState<Size>("720x1280");
  const [instances, setInstances] = useState<ModelInstance[]>([]);

  const { entries, addEntry, removeEntry } = useVideoHistory();
  const { toast } = useToast();
  const { isExecuting, results, execute, clearResults } = useMultiModelExecution<VideoResult>();
  const { preferences } = usePreferences();

  // Initialize instances from saved per-task defaults (once, when models load).
  const hasInitRef = useRef(false);
  useEffect(() => {
    if (hasInitRef.current || !availableModels?.length) return;
    hasInitRef.current = true;
    const defaults = preferences.defaultModels?.video || [];
    const valid = defaults.filter((m) => availableModels.some((am) => am.id === m));
    if (valid.length > 0) {
      setInstances(valid.map((m) => createDefaultInstance(m)));
    }
  }, [availableModels, preferences.defaultModels]);

  // Abort any in-flight generation when the panel unmounts so a job stuck
  // `queued`/`in_progress` can't keep polling (and then download/persist) in
  // the background after the user navigates away.
  useEffect(() => () => clearResults(), [clearResults]);

  const handleSubmit = useCallback(async () => {
    if (!prompt.trim() || isExecuting || instances.length === 0) return;

    // Each instance runs the full async lifecycle inside its call: create the
    // job, poll until terminal, then download the rendered asset. The cell
    // shows a spinner for the whole duration, then the video (same UX as the
    // image panel's per-model results).
    const settled = await execute(instances, async (instance, signal) => {
      const createRes = await apiV1VideosCreate({
        body: { model: instance.modelId, prompt, seconds, size },
      });
      if (createRes.error || !createRes.data) throw new Error("Failed to create video job");
      let job = createRes.data;
      const cost = extractCostFromResponse(createRes.response);

      const deadline = Date.now() + MAX_POLL_MS;
      while (!signal.aborted && job.status !== "completed" && job.status !== "failed") {
        if (Date.now() > deadline) throw new Error("Video generation timed out");
        await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));
        if (signal.aborted) throw new DOMException("Aborted", "AbortError");
        const r = await apiV1VideosRetrieve({ path: { video_id: job.id } });
        if (r.data) job = r.data;
      }
      if (job.status === "failed") {
        throw new Error(job.error?.message ?? "Video generation failed");
      }

      const contentRes = await apiV1VideosContent({ path: { video_id: job.id } });
      if (contentRes.error || !contentRes.data) throw new Error("Failed to download video");
      const blob = contentRes.data as Blob;

      return {
        data: {
          blob,
          videoId: job.id,
          seconds: job.seconds,
          size: job.size,
          prompt: job.prompt,
        },
        costMicrocents: cost,
      };
    });

    // Persist completed results to OPFS + history, then clear the live grid so
    // the finished videos live in the gallery on the right.
    let persisted = false;
    for (const r of settled) {
      if (r.status !== "complete" || !r.data) continue;
      const entryId = crypto.randomUUID();
      const filename = await writeVideoFile(entryId, r.data.videoId, "mp4", r.data.blob);
      addEntry({
        id: entryId,
        jobId: r.data.videoId,
        prompt: r.data.prompt ?? prompt,
        modelId: r.modelId,
        status: "completed",
        options: { seconds: r.data.seconds ?? seconds, size: r.data.size ?? size },
        videoData: filename ?? "",
        costMicrocents: r.costMicrocents,
        createdAt: Date.now(),
      });
      persisted = true;
    }
    if (persisted) clearResults();

    const errors = settled.filter((r) => r.status === "error");
    if (errors.length > 0) {
      toast({
        title: "Some models failed",
        description: errors.map((e) => `${e.modelId}: ${e.error}`).join("; "),
        type: "error",
      });
    }
  }, [prompt, isExecuting, instances, seconds, size, execute, addEntry, clearResults, toast]);

  return (
    <div className="flex h-full flex-col lg:flex-row">
      {/* Left panel: Controls */}
      <div className="flex w-full flex-col gap-4 border-b p-5 lg:w-[380px] lg:overflow-y-auto lg:border-b-0 lg:border-r">
        {/* Model picker */}
        <div>
          <span className="mb-1.5 block text-xs font-medium text-muted-foreground">Models</span>
          <ModelSelector
            selectedInstances={instances}
            onInstancesChange={setInstances}
            availableModels={(availableModels ?? []) as ModelInfo[]}
            task="video"
          />
        </div>

        <PromptInput
          value={prompt}
          onChange={setPrompt}
          onSubmit={handleSubmit}
          placeholder="Describe the video you want to create..."
          disabled={isExecuting}
        />

        <Button
          variant="primary"
          className={cn("w-full gap-2", isExecuting && "motion-safe:animate-pulse")}
          onClick={handleSubmit}
          disabled={!prompt.trim() || isExecuting || instances.length === 0}
          isLoading={isExecuting}
        >
          <Clapperboard className="h-4 w-4" aria-hidden="true" />
          Generate
        </Button>

        {/* Options */}
        <div className="flex flex-col gap-3 rounded-xl border border-border bg-card/50 p-4">
          <label className="flex items-center justify-between text-sm">
            <span className="text-muted-foreground">Duration</span>
            <select
              className="rounded-md border border-border bg-background px-2 py-1 text-sm"
              value={seconds}
              onChange={(e) => setSeconds(e.target.value as Seconds)}
              disabled={isExecuting}
              aria-label="Duration in seconds"
            >
              {SECONDS_OPTIONS.map((s) => (
                <option key={s} value={s}>
                  {s}s
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center justify-between text-sm">
            <span className="text-muted-foreground">Size</span>
            <select
              className="rounded-md border border-border bg-background px-2 py-1 text-sm"
              value={size}
              onChange={(e) => setSize(e.target.value as Size)}
              disabled={isExecuting}
              aria-label="Output resolution"
            >
              {SIZE_OPTIONS.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
        </div>
      </div>

      {/* Right panel: Results + Gallery */}
      <div className="flex-1 space-y-6 overflow-y-auto p-5">
        <MultiModelResultGrid
          results={results}
          renderResult={(r) => (r.data ? <VideoResultPlayer blob={r.data.blob} /> : null)}
        />

        {entries.length > 0 ? (
          <div className="flex flex-col gap-3">
            <h3 className="text-sm font-medium text-muted-foreground">Recent videos</h3>
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {entries.map((entry) => (
                <VideoOutputCard key={entry.id} entry={entry} onRemove={removeEntry} />
              ))}
            </div>
          </div>
        ) : (
          results.size === 0 && (
            <div className="flex flex-1 flex-col items-center justify-center py-16">
              <div className="mb-4 flex h-16 w-16 items-center justify-center rounded-2xl bg-muted/50">
                <Film className="h-8 w-8 text-muted-foreground/50" />
              </div>
              <h3 className="text-base font-medium text-foreground">Create something</h3>
              <p className="mt-1 max-w-xs text-center text-sm text-muted-foreground">
                Describe a scene and generate a short video
              </p>
            </div>
          )
        )}
      </div>
    </div>
  );
}
