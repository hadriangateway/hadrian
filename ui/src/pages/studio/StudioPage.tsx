import { useCallback, useMemo, lazy, Suspense } from "react";
import { useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { apiV1ModelsOptions } from "@/api/generated/@tanstack/react-query.gen";
import { StudioTabs } from "@/components/Studio/StudioTabs/StudioTabs";
import { Spinner } from "@/components/Spinner/Spinner";
import type { ModelInfo } from "@/components/ModelPicker/model-utils";
import type { StudioTab } from "./types";

const ImageGenerationPanel = lazy(() =>
  import("@/components/Studio/ImageGeneration/ImageGenerationPanel").then((m) => ({
    default: m.ImageGenerationPanel,
  }))
);
const AudioPanel = lazy(() =>
  import("@/components/Studio/AudioPanel/AudioPanel").then((m) => ({
    default: m.AudioPanel,
  }))
);
const VideoGenerationPanel = lazy(() =>
  import("@/components/Studio/VideoGeneration/VideoGenerationPanel").then((m) => ({
    default: m.VideoGenerationPanel,
  }))
);

const VALID_TABS = new Set<StudioTab>(["images", "audio", "video"]);

function PanelLoader() {
  return (
    <div className="flex h-full items-center justify-center">
      <Spinner size="lg" />
    </div>
  );
}

export default function StudioPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const rawTab = searchParams.get("tab") as StudioTab | null;
  const activeTab: StudioTab = rawTab && VALID_TABS.has(rawTab) ? rawTab : "images";

  // Fetch models from API
  const { data: modelsResponse } = useQuery(apiV1ModelsOptions());
  const availableModels: ModelInfo[] = useMemo(
    () => modelsResponse?.data?.map((m) => m as ModelInfo).filter((m) => m.id) || [],
    [modelsResponse]
  );

  // Filter models by task — models must have explicit task annotations
  // (set via config or catalog) to appear in Studio tabs.
  const imageModels = useMemo(
    () => availableModels.filter((m) => m.tasks?.includes("image_generation")),
    [availableModels]
  );
  const audioModels = useMemo(
    () => availableModels.filter((m) => m.tasks?.includes("tts")),
    [availableModels]
  );
  const videoModels = useMemo(
    () => availableModels.filter((m) => m.tasks?.includes("video_generation")),
    [availableModels]
  );
  const transcriptionModels = useMemo(
    () => availableModels.filter((m) => m.tasks?.includes("transcription")),
    [availableModels]
  );
  const translationModels = useMemo(
    () => availableModels.filter((m) => m.tasks?.includes("translation")),
    [availableModels]
  );
  // Chat-capable models for the text translation step (non-English targets)
  const chatModels = useMemo(
    () =>
      availableModels.filter((m) => !m.tasks || m.tasks.length === 0 || m.tasks.includes("chat")),
    [availableModels]
  );

  const handleTabChange = useCallback(
    (tab: StudioTab) => {
      setSearchParams({ tab }, { replace: true });
    },
    [setSearchParams]
  );

  return (
    <div className="flex h-full flex-col">
      <StudioTabs activeTab={activeTab} onTabChange={handleTabChange} />

      <div
        role="tabpanel"
        id={`studio-panel-${activeTab}`}
        aria-labelledby={`studio-tab-${activeTab}`}
        className="flex-1 overflow-hidden"
      >
        <Suspense fallback={<PanelLoader />}>
          {activeTab === "images" && <ImageGenerationPanel availableModels={imageModels} />}
          {activeTab === "audio" && (
            <AudioPanel
              audioModels={audioModels}
              transcriptionModels={transcriptionModels}
              translationModels={translationModels}
              chatModels={chatModels}
            />
          )}
          {activeTab === "video" && <VideoGenerationPanel availableModels={videoModels} />}
        </Suspense>
      </div>
    </div>
  );
}
