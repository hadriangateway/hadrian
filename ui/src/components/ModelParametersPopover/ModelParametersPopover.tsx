import { useState } from "react";
import {
  Settings2,
  RotateCcw,
  Brain,
  MessageSquareText,
  ChevronDown,
  ChevronRight,
  SlidersHorizontal,
  Tag,
  X,
  Copy,
} from "lucide-react";

import { Button } from "@/components/Button/Button";
import { NumberInput } from "@/components/NumberInput/NumberInput";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/Popover/Popover";
import { Select } from "@/components/Select/Select";
import { Slider } from "@/components/Slider/Slider";
import { Switch } from "@/components/Switch/Switch";
import { cn } from "@/utils/cn";

import type { ModelParameters, ReasoningEffort } from "@/components/chat-types";
import { DEFAULT_REASONING_CONFIG } from "@/components/chat-types";

interface ModelParametersPopoverProps {
  modelName: string;
  parameters: ModelParameters;
  onParametersChange: (params: ModelParameters) => void;
  /** Optional custom label for this instance */
  instanceLabel?: string;
  /** Callback when instance label changes */
  onLabelChange?: (label: string) => void;
  /** Callback to duplicate this instance with its settings */
  onDuplicate?: () => void;
  className?: string;
}

/** Default parameter values - used for display only, not stored */
const DEFAULT_PARAMS: ModelParameters = {
  temperature: 1.0,
  maxTokens: 4096,
  topP: 1.0,
  frequencyPenalty: 0,
  presencePenalty: 0,
  reasoning: DEFAULT_REASONING_CONFIG,
};

const EFFORT_OPTIONS: { value: ReasoningEffort; label: string }[] = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra High" },
  { value: "max", label: "Max" },
];

/** Check if a value differs from its default */
function isNonDefault<K extends keyof ModelParameters>(key: K, value: ModelParameters[K]): boolean {
  // undefined means using default
  if (value === undefined) return false;
  if (key === "reasoning") {
    const reasoning = value as ModelParameters["reasoning"];
    const defaultReasoning = DEFAULT_REASONING_CONFIG;
    return (
      reasoning?.enabled !== defaultReasoning.enabled ||
      reasoning?.effort !== defaultReasoning.effort
    );
  }
  if (key === "systemPrompt") {
    // Empty string or undefined = default (no custom prompt)
    return Boolean(value && (value as string).trim());
  }
  return value !== DEFAULT_PARAMS[key];
}

/** Build a clean parameters object with only non-default values */
function buildCleanParams(params: ModelParameters): ModelParameters {
  const clean: ModelParameters = {};
  for (const key of Object.keys(params) as (keyof ModelParameters)[]) {
    const value = params[key];
    if (value !== undefined && isNonDefault(key, value)) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (clean as any)[key] = value;
    }
  }
  return clean;
}

export function ModelParametersPopover({
  modelName,
  parameters,
  onParametersChange,
  instanceLabel,
  onLabelChange,
  onDuplicate,
  className,
}: ModelParametersPopoverProps) {
  const [systemPromptExpanded, setSystemPromptExpanded] = useState(false);
  const [parametersExpanded, setParametersExpanded] = useState(false);
  const [reasoningExpanded, setReasoningExpanded] = useState(false);
  const handleReset = () => {
    // Clear all stored parameters (empty object = use defaults)
    onParametersChange({});
    // Also reset the label if handler is provided
    if (onLabelChange) {
      onLabelChange("");
    }
  };

  const handleLabelChange = (value: string) => {
    if (onLabelChange) {
      onLabelChange(value);
    }
  };

  const handleClearLabel = () => {
    if (onLabelChange) {
      onLabelChange("");
    }
  };

  const updateParam = <K extends keyof ModelParameters>(key: K, value: ModelParameters[K]) => {
    // Build new params with the updated value
    const newParams = { ...parameters, [key]: value };
    // Only store values that differ from defaults
    onParametersChange(buildCleanParams(newParams));
  };

  // Get current reasoning config with defaults
  const reasoning = parameters.reasoning ?? DEFAULT_REASONING_CONFIG;

  // Check if custom system prompt is set
  const hasCustomSystemPrompt = Boolean(parameters.systemPrompt?.trim());

  // Check if any generation parameters are non-default
  const hasNonDefaultParams =
    isNonDefault("temperature", parameters.temperature) ||
    isNonDefault("maxTokens", parameters.maxTokens) ||
    isNonDefault("topP", parameters.topP) ||
    isNonDefault("frequencyPenalty", parameters.frequencyPenalty) ||
    isNonDefault("presencePenalty", parameters.presencePenalty);

  // Check if reasoning is non-default
  const hasNonDefaultReasoning = isNonDefault("reasoning", parameters.reasoning);

  // Has changes if any non-default parameters are stored or label is set
  const hasChanges = Object.keys(parameters).length > 0 || Boolean(instanceLabel?.trim());

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          aria-label={`Settings for ${modelName}`}
          className={cn(
            "rounded p-0.5 transition-colors hover:bg-muted-foreground/20",
            hasChanges && "text-primary",
            className
          )}
          onClick={(e) => e.stopPropagation()}
        >
          <Settings2 className="h-3 w-3" />
        </button>
      </PopoverTrigger>
      <PopoverContent className="w-72 p-3" align="start" onClick={(e) => e.stopPropagation()}>
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <h4 className="text-sm font-medium truncate max-w-[140px]" title={modelName}>
              {modelName}
            </h4>
            <div className="flex items-center gap-1">
              {onDuplicate && (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={onDuplicate}
                  className="h-6 px-2 text-xs"
                  title="Duplicate with settings"
                >
                  <Copy className="mr-1 h-3 w-3" />
                  Copy
                </Button>
              )}
              <Button
                variant="ghost"
                size="sm"
                onClick={handleReset}
                disabled={!hasChanges}
                className="h-6 px-2 text-xs"
              >
                <RotateCcw className="mr-1 h-3 w-3" />
                Reset
              </Button>
            </div>
          </div>

          {/* Instance Label */}
          {onLabelChange && (
            <div className="border-b pb-3">
              <div className="flex items-center gap-2 mb-1.5">
                <Tag className="h-3.5 w-3.5 text-muted-foreground" />
                <span className="text-xs text-muted-foreground">Instance Name</span>
              </div>
              <div className="relative">
                <input
                  type="text"
                  value={instanceLabel ?? ""}
                  onChange={(e) => handleLabelChange(e.target.value)}
                  onKeyDown={(e) => e.stopPropagation()}
                  placeholder={modelName}
                  aria-label="Instance name"
                  className="w-full rounded-md border bg-background px-2 py-1.5 pr-7 text-xs placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-ring"
                />
                {instanceLabel && (
                  <button
                    type="button"
                    onClick={handleClearLabel}
                    className="absolute right-1.5 top-1/2 -translate-y-1/2 rounded p-0.5 text-muted-foreground/50 hover:text-muted-foreground"
                  >
                    <X className="h-3 w-3" />
                  </button>
                )}
              </div>
              <p className="text-[10px] text-muted-foreground mt-1">
                Custom name for this instance (e.g., &quot;Creative&quot; or &quot;Precise&quot;)
              </p>
            </div>
          )}

          {/* System Prompt Section */}
          <div className="border-b pb-3">
            <button
              type="button"
              onClick={() => setSystemPromptExpanded(!systemPromptExpanded)}
              className="flex items-center gap-2 w-full text-left hover:bg-muted/50 -mx-1 px-1 py-1 rounded transition-colors"
            >
              {systemPromptExpanded ? (
                <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
              ) : (
                <ChevronRight className="h-3.5 w-3.5 text-muted-foreground" />
              )}
              <MessageSquareText className="h-4 w-4 text-muted-foreground" />
              <span className="text-sm font-medium flex-1">System Prompt</span>
              {hasCustomSystemPrompt && (
                <span className="h-1.5 w-1.5 rounded-full bg-primary" title="Custom prompt set" />
              )}
            </button>
            {systemPromptExpanded && (
              <div className="mt-2">
                <textarea
                  value={parameters.systemPrompt ?? ""}
                  onChange={(e) => updateParam("systemPrompt", e.target.value)}
                  onKeyDown={(e) => e.stopPropagation()}
                  placeholder="Custom instructions for this model..."
                  aria-label="System prompt"
                  className="w-full min-h-[80px] rounded-md border bg-background px-2 py-1.5 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring resize-y"
                />
                <p className="text-[10px] text-muted-foreground mt-1">
                  Overrides the conversation system prompt for this model only.
                </p>
              </div>
            )}
          </div>

          {/* Generation Parameters Section */}
          <div className="border-b pb-3">
            <button
              type="button"
              onClick={() => setParametersExpanded(!parametersExpanded)}
              className="flex items-center gap-2 w-full text-left hover:bg-muted/50 -mx-1 px-1 py-1 rounded transition-colors"
            >
              {parametersExpanded ? (
                <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
              ) : (
                <ChevronRight className="h-3.5 w-3.5 text-muted-foreground" />
              )}
              <SlidersHorizontal className="h-4 w-4 text-muted-foreground" />
              <span className="text-sm font-medium flex-1">Parameters</span>
              {hasNonDefaultParams && (
                <span
                  className="h-1.5 w-1.5 rounded-full bg-primary"
                  title="Custom parameters set"
                />
              )}
            </button>
            {parametersExpanded && (
              <div className="mt-2 space-y-3">
                {/* Temperature */}
                <Slider
                  label="Temperature"
                  showValue
                  min={0}
                  max={2}
                  step={0.1}
                  value={parameters.temperature ?? DEFAULT_PARAMS.temperature!}
                  onChange={(value) => updateParam("temperature", value)}
                  className="text-xs"
                />

                {/* Max Tokens */}
                <NumberInput
                  label="Max Tokens"
                  min={1}
                  max={128000}
                  step={256}
                  value={parameters.maxTokens ?? DEFAULT_PARAMS.maxTokens!}
                  onChange={(value) => updateParam("maxTokens", value)}
                  className="text-xs"
                />

                {/* Top P */}
                <Slider
                  label="Top P"
                  showValue
                  min={0}
                  max={1}
                  step={0.05}
                  value={parameters.topP ?? DEFAULT_PARAMS.topP!}
                  onChange={(value) => updateParam("topP", value)}
                  className="text-xs"
                />

                {/* Frequency Penalty */}
                <Slider
                  label="Freq Penalty"
                  showValue
                  min={-2}
                  max={2}
                  step={0.1}
                  value={parameters.frequencyPenalty ?? DEFAULT_PARAMS.frequencyPenalty!}
                  onChange={(value) => updateParam("frequencyPenalty", value)}
                  className="text-xs"
                />

                {/* Presence Penalty */}
                <Slider
                  label="Pres Penalty"
                  showValue
                  min={-2}
                  max={2}
                  step={0.1}
                  value={parameters.presencePenalty ?? DEFAULT_PARAMS.presencePenalty!}
                  onChange={(value) => updateParam("presencePenalty", value)}
                  className="text-xs"
                />
              </div>
            )}
          </div>

          {/* Reasoning Section */}
          <div>
            <button
              type="button"
              onClick={() => setReasoningExpanded(!reasoningExpanded)}
              className="flex items-center gap-2 w-full text-left hover:bg-muted/50 -mx-1 px-1 py-1 rounded transition-colors"
            >
              {reasoningExpanded ? (
                <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
              ) : (
                <ChevronRight className="h-3.5 w-3.5 text-muted-foreground" />
              )}
              <Brain className="h-4 w-4 text-muted-foreground" />
              <span className="text-sm font-medium flex-1">Reasoning</span>
              {hasNonDefaultReasoning && (
                <span
                  className="h-1.5 w-1.5 rounded-full bg-primary"
                  title="Custom reasoning set"
                />
              )}
            </button>
            {reasoningExpanded && (
              <div className="mt-2 space-y-2">
                {/* Reasoning Toggle */}
                <div className="flex items-center justify-between">
                  <span className="text-xs text-muted-foreground">Enable extended thinking</span>
                  <Switch
                    checked={reasoning.enabled}
                    onChange={(e) =>
                      updateParam("reasoning", { ...reasoning, enabled: e.target.checked })
                    }
                  />
                </div>

                {/* Effort Level */}
                {reasoning.enabled && (
                  <div className="space-y-1">
                    <span className="text-xs text-muted-foreground">Effort level</span>
                    <Select
                      options={EFFORT_OPTIONS}
                      value={reasoning.effort}
                      onChange={(value) =>
                        updateParam("reasoning", { ...reasoning, effort: value as ReasoningEffort })
                      }
                      clearable={false}
                      className="text-xs"
                    />
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}
