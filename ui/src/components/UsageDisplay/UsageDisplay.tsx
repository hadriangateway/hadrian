import { Coins } from "lucide-react";

import type { MessageUsage } from "@/components/chat-types";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/Tooltip/Tooltip";
import { formatCost, formatDuration, formatTokens, formatTPS } from "@/utils/formatters";

export interface UsageDisplayProps {
  usage: MessageUsage;
  /** Show compact version without icon */
  compact?: boolean;
  /**
   * Running mid-stream total, not the final figure — a server-tool loop is
   * still adding turns. Pulses the whole label in a slightly brighter color
   * and reframes the tooltip as a running total.
   */
  provisional?: boolean;
}

export function UsageDisplay({ usage, compact = false, provisional = false }: UsageDisplayProps) {
  const hasTimingStats =
    usage.firstTokenMs !== undefined ||
    usage.totalDurationMs !== undefined ||
    usage.tokensPerSecond !== undefined;

  const hasMetaInfo =
    usage.finishReason !== undefined || usage.modelId !== undefined || usage.provider !== undefined;

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div
          className={`flex items-center gap-1.5 text-xs cursor-help shrink-0 ${
            // Running total: pulse in a slightly brighter color until the
            // terminal figure lands.
            provisional ? "animate-pulse text-foreground/80" : "text-muted-foreground"
          }`}
        >
          {!compact && <Coins className="h-3 w-3" />}
          <span>{formatTokens(usage.totalTokens)}</span>
          {usage.cost !== undefined && usage.cost > 0 && (
            <>
              <span className="text-muted-foreground/50">·</span>
              <span>{formatCost(usage.cost)}</span>
            </>
          )}
        </div>
      </TooltipTrigger>
      <TooltipContent side="bottom" className="text-xs">
        <div className="space-y-1">
          <div className="font-medium">
            {provisional ? "Token Usage — running total" : "Token Usage"}
          </div>
          <div>Input: {formatTokens(usage.inputTokens)}</div>
          <div>Output: {formatTokens(usage.outputTokens)}</div>
          {usage.cachedTokens !== undefined && usage.cachedTokens > 0 && (
            <div className="text-muted-foreground">Cached: {formatTokens(usage.cachedTokens)}</div>
          )}
          {usage.reasoningTokens !== undefined && usage.reasoningTokens > 0 && (
            <div className="text-muted-foreground">
              Reasoning: {formatTokens(usage.reasoningTokens)}
            </div>
          )}
          {usage.cost !== undefined && usage.cost > 0 && (
            <div className="pt-1 border-t border-border/50 font-medium">
              {provisional ? "Cost so far" : "Cost"}: {formatCost(usage.cost)}
            </div>
          )}
          {provisional && (
            <div className="pt-1 border-t border-border/50 text-muted-foreground">
              Updates as the response runs; final totals on completion.
            </div>
          )}

          {/* Timing Stats */}
          {hasTimingStats && (
            <div className="pt-1 border-t border-border/50 space-y-0.5">
              <div className="font-medium">Performance</div>
              {usage.firstTokenMs !== undefined && (
                <div>Time to first token: {formatDuration(usage.firstTokenMs)}</div>
              )}
              {usage.totalDurationMs !== undefined && (
                <div>Duration: {formatDuration(usage.totalDurationMs)}</div>
              )}
              {usage.tokensPerSecond !== undefined && (
                <div>Speed: {formatTPS(usage.tokensPerSecond)}</div>
              )}
            </div>
          )}

          {/* Response Metadata */}
          {hasMetaInfo && (
            <div className="pt-1 border-t border-border/50 space-y-0.5">
              <div className="font-medium">Response</div>
              {usage.finishReason && <div>Finish reason: {usage.finishReason}</div>}
              {usage.modelId && <div>Model: {usage.modelId}</div>}
              {usage.provider && <div>Provider: {usage.provider}</div>}
            </div>
          )}
        </div>
      </TooltipContent>
    </Tooltip>
  );
}
