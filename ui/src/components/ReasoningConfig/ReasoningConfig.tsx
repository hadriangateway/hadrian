import { Brain, ChevronDown } from "lucide-react";

import { Button } from "@/components/Button/Button";
import type {
  ReasoningConfig as ReasoningConfigType,
  ReasoningEffort,
} from "@/components/chat-types";
import {
  Dropdown,
  DropdownContent,
  DropdownItem,
  DropdownLabel,
  DropdownSeparator,
  DropdownTrigger,
} from "@/components/Dropdown/Dropdown";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/Tooltip/Tooltip";
import { cn } from "@/utils/cn";

interface ReasoningConfigProps {
  config: ReasoningConfigType;
  onConfigChange: (config: Partial<ReasoningConfigType>) => void;
  disabled?: boolean;
}

const EFFORT_LABELS: Record<ReasoningEffort, { label: string; description: string }> = {
  none: { label: "None", description: "No extended thinking" },
  minimal: { label: "Minimal", description: "Quick, light reasoning" },
  low: { label: "Low", description: "Brief thinking" },
  medium: { label: "Medium", description: "Balanced reasoning" },
  high: { label: "High", description: "Deep, thorough thinking" },
  xhigh: { label: "Extra High", description: "More reasoning than High" },
  max: { label: "Max", description: "Maximum reasoning effort" },
};

export function ReasoningConfig({
  config,
  onConfigChange,
  disabled = false,
}: ReasoningConfigProps) {
  const handleToggle = () => {
    onConfigChange({ enabled: !config.enabled });
  };

  const handleEffortChange = (effort: ReasoningEffort) => {
    if (effort === "none") {
      onConfigChange({ enabled: false, effort: "none" });
    } else {
      onConfigChange({ enabled: true, effort });
    }
  };

  const currentEffortLabel = config.enabled ? EFFORT_LABELS[config.effort].label : "Off";

  return (
    <Dropdown>
      <Tooltip>
        <TooltipTrigger asChild>
          <DropdownTrigger asChild showChevron={false}>
            <Button
              variant="outline"
              size="sm"
              disabled={disabled}
              aria-label="Reasoning settings"
              className={cn(
                "h-8 gap-1.5 px-2 text-xs",
                config.enabled && "border-primary/50 bg-primary/5"
              )}
            >
              <Brain className={cn("h-3.5 w-3.5", config.enabled && "text-primary")} />
              <span className="hidden sm:inline">{currentEffortLabel}</span>
              <ChevronDown className="h-3 w-3 opacity-50" />
            </Button>
          </DropdownTrigger>
        </TooltipTrigger>
        <TooltipContent side="bottom">
          <p>Configure extended thinking</p>
        </TooltipContent>
      </Tooltip>
      <DropdownContent align="start" className="w-48">
        <DropdownLabel>Reasoning</DropdownLabel>
        <DropdownItem
          onClick={handleToggle}
          className={cn(!config.enabled && "text-muted-foreground")}
        >
          <div className="flex items-center justify-between w-full">
            <span>{config.enabled ? "Enabled" : "Disabled"}</span>
            <div
              className={cn(
                "h-4 w-7 rounded-full transition-colors",
                config.enabled ? "bg-primary" : "bg-muted"
              )}
            >
              <div
                className={cn(
                  "h-3 w-3 rounded-full bg-white transition-transform mt-0.5",
                  config.enabled ? "translate-x-3.5 ml-0" : "translate-x-0.5"
                )}
              />
            </div>
          </div>
        </DropdownItem>
        <DropdownSeparator />
        <DropdownLabel className="text-[10px] uppercase tracking-wider">Effort Level</DropdownLabel>
        {(Object.keys(EFFORT_LABELS) as ReasoningEffort[]).map((effort) => (
          <DropdownItem
            key={effort}
            onClick={() => handleEffortChange(effort)}
            className={cn(
              "flex flex-col items-start py-2",
              config.effort === effort && config.enabled && "bg-accent"
            )}
            disabled={effort !== "none" && !config.enabled}
          >
            <span className="font-medium">{EFFORT_LABELS[effort].label}</span>
            <span className="text-[10px] text-muted-foreground">
              {EFFORT_LABELS[effort].description}
            </span>
          </DropdownItem>
        ))}
      </DropdownContent>
    </Dropdown>
  );
}
