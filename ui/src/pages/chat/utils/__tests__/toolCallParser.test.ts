import { describe, it, expect } from "vitest";

import {
  createToolCallTracker,
  parseToolCallFromEvent,
  invalidArgumentsText,
} from "../toolCallParser";
import type { BaseSSEEvent } from "../toolCallParser";

/**
 * Anthropic (and the gateway's other providers) emit `function_call` items
 * whose item `id` (`fc_xxx`) differs from the provider `call_id` (`toolu_xxx`).
 * The streaming argument events only carry `item_id` (== the item `id`), so the
 * tracker must be keyed on the item id, not the call id, or every delta misses.
 */
const ITEM_ID = "fc_abc";
const CALL_ID = "toolu_abc";

const added: BaseSSEEvent = {
  type: "response.output_item.added",
  output_index: 0,
  item: { type: "function_call", id: ITEM_ID, call_id: CALL_ID, name: "web_search" },
};

describe("parseToolCallFromEvent", () => {
  it("matches streaming argument deltas keyed by item_id, not call_id", () => {
    const tracker = createToolCallTracker();
    parseToolCallFromEvent(added, tracker);

    // Deltas carry `item_id` (the item id), never the provider call_id.
    const delta = parseToolCallFromEvent(
      {
        type: "response.function_call_arguments.delta",
        item_id: ITEM_ID,
        output_index: 0,
        delta: '{"query":',
      },
      tracker
    );
    expect(delta.type).toBe("tool_call_arguments_delta");

    const more = parseToolCallFromEvent(
      {
        type: "response.function_call_arguments.delta",
        item_id: ITEM_ID,
        output_index: 0,
        delta: '"rust"}',
      },
      tracker
    );
    expect(more.type).toBe("tool_call_arguments_delta");

    // The buffer accumulated from the deltas (it would be empty if the lookup
    // had keyed on call_id and missed).
    expect(tracker.toolCalls.get(ITEM_ID)?.argumentsBuffer).toBe('{"query":"rust"}');
  });

  it("preserves the provider call_id for building the continuation", () => {
    const tracker = createToolCallTracker();
    const result = parseToolCallFromEvent(added, tracker);
    expect(result.type).toBe("tool_call_added");
    const state = tracker.toolCalls.get(ITEM_ID);
    expect(state?.id).toBe(ITEM_ID);
    expect(state?.callId).toBe(CALL_ID);
  });

  it("marks a call invalid (not dropped) when its arguments fail to parse", () => {
    const tracker = createToolCallTracker();
    parseToolCallFromEvent(added, tracker);

    // Malformed JSON arriving on the done event.
    parseToolCallFromEvent(
      {
        type: "response.function_call_arguments.done",
        item_id: ITEM_ID,
        output_index: 0,
        arguments: '{"query": ',
      },
      tracker
    );

    const done = parseToolCallFromEvent(
      {
        type: "response.output_item.done",
        output_index: 0,
        item: {
          type: "function_call",
          id: ITEM_ID,
          call_id: CALL_ID,
          name: "web_search",
          arguments: '{"query": ',
          status: "completed",
        },
      },
      tracker
    );

    // The call surfaces as complete (so the loop runs) and is flagged invalid
    // rather than producing an `error` result that drops it silently.
    expect(done.type).toBe("tool_call_complete");
    if (done.type === "tool_call_complete") {
      expect(done.toolCall.invalid).toBeTruthy();
      expect(done.toolCall.callId).toBe(CALL_ID);
    }

    // It is included in the completed set so the tool loop can feed back the
    // error instead of ending the turn as a false "completed".
    const completed = tracker.getCompletedToolCalls();
    expect(completed).toHaveLength(1);
    expect(completed[0].invalid).toBeTruthy();
  });

  it("recovers a valid parse from output_item.done after a truncated arguments.done", () => {
    const tracker = createToolCallTracker();
    parseToolCallFromEvent(added, tracker);

    // arguments.done arrives truncated and flags the call invalid.
    parseToolCallFromEvent(
      {
        type: "response.function_call_arguments.done",
        item_id: ITEM_ID,
        output_index: 0,
        arguments: '{"query": ',
      },
      tracker
    );
    expect(tracker.toolCalls.get(ITEM_ID)?.invalid).toBeTruthy();

    // output_item.done carries the complete, valid payload — the call must
    // recover rather than stay permanently flagged invalid.
    const done = parseToolCallFromEvent(
      {
        type: "response.output_item.done",
        output_index: 0,
        item: {
          type: "function_call",
          id: ITEM_ID,
          call_id: CALL_ID,
          name: "web_search",
          arguments: '{"query": "rust"}',
          status: "completed",
        },
      },
      tracker
    );
    expect(done.type).toBe("tool_call_complete");
    if (done.type === "tool_call_complete") {
      expect(done.toolCall.invalid).toBeUndefined();
      expect(done.toolCall.arguments).toEqual({ query: "rust" });
    }
  });

  it("parses well-formed arguments into a clean completed call", () => {
    const tracker = createToolCallTracker();
    parseToolCallFromEvent(added, tracker);
    parseToolCallFromEvent(
      {
        type: "response.function_call_arguments.done",
        item_id: ITEM_ID,
        output_index: 0,
        arguments: '{"query": "rust"}',
      },
      tracker
    );
    const done = parseToolCallFromEvent(
      {
        type: "response.output_item.done",
        output_index: 0,
        item: {
          type: "function_call",
          id: ITEM_ID,
          call_id: CALL_ID,
          name: "web_search",
          arguments: '{"query": "rust"}',
          status: "completed",
        },
      },
      tracker
    );
    expect(done.type).toBe("tool_call_complete");
    if (done.type === "tool_call_complete") {
      expect(done.toolCall.invalid).toBeUndefined();
      expect(done.toolCall.arguments).toEqual({ query: "rust" });
    }
  });
});

describe("invalidArgumentsText", () => {
  it("mirrors the backend's invalid_arguments_text format", () => {
    expect(invalidArgumentsText("web_search", "Unexpected end of JSON input")).toBe(
      "Invalid arguments for tool `web_search`: Unexpected end of JSON input"
    );
  });
});
