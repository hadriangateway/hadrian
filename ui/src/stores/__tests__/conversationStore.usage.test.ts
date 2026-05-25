import { beforeEach, describe, expect, it } from "vitest";

import type { ChatMessage, Conversation, MessageUsage } from "@/components/chat-types";
import { useConversationStore } from "@/stores/conversationStore";

/** Build a MessageUsage with sensible defaults. */
function usage(input: number, output: number, cost?: number): MessageUsage {
  return { inputTokens: input, outputTokens: output, totalTokens: input + output, cost };
}

/** Build an assistant ChatMessage carrying usage (and optional mode overhead). */
function assistant(
  id: string,
  model: string,
  u: MessageUsage,
  modeMetadata?: ChatMessage["modeMetadata"]
): ChatMessage {
  return {
    id,
    role: "assistant",
    content: "x",
    model,
    timestamp: new Date(),
    usage: u,
    modeMetadata,
  };
}

function user(id: string): ChatMessage {
  return { id, role: "user", content: "q", timestamp: new Date() };
}

const get = () => useConversationStore.getState();

describe("conversationStore — discarded usage tracking", () => {
  beforeEach(() => {
    // setMessages resets the discard tally; clear currentConversation too.
    useConversationStore.setState({ currentConversation: null });
    get().setMessages([]);
  });

  it("starts with an empty discard tally", () => {
    expect(get().discardedUsage.totalTokens).toBe(0);
    expect(get().discardedResponseCount).toBe(0);
  });

  it("folds responses removed by deleteMessagesAfter into the tally", () => {
    get().setMessages([
      user("u1"),
      assistant("a1", "m", usage(10, 5, 0.01)),
      user("u2"),
      assistant("a2", "m", usage(20, 8, 0.02)),
    ]);

    // Edit-and-rerun on u1: everything after it is discarded.
    get().deleteMessagesAfter("u1");

    expect(get().messages.map((m) => m.id)).toEqual(["u1"]);
    expect(get().discardedResponseCount).toBe(2);
    expect(get().discardedUsage.totalTokens).toBe(10 + 5 + 20 + 8);
    expect(get().discardedUsage.cost).toBeCloseTo(0.03);
  });

  it("moves the overwritten response's usage into the tally on regenerate", () => {
    get().setMessages([user("u1"), assistant("a1", "m", usage(10, 5, 0.01))]);

    get().replaceAssistantMessage("u1", "m", { content: "new", usage: usage(12, 7, 0.02) });

    // Live message now carries the NEW usage; OLD usage went to discarded.
    const live = get().messages.find((m) => m.role === "assistant");
    expect(live?.usage?.totalTokens).toBe(19);
    expect(get().discardedResponseCount).toBe(1);
    expect(get().discardedUsage.totalTokens).toBe(15);
    expect(get().discardedUsage.cost).toBeCloseTo(0.01);
  });

  it("counts mode overhead from discarded responses", () => {
    get().setMessages([
      user("u1"),
      assistant("a1", "m", usage(10, 5), {
        mode: "synthesized",
        routerUsage: usage(3, 1),
        synthesizerUsage: usage(2, 4),
      }),
    ]);

    get().deleteMessagesAfter("u1");

    // 15 (response) + 4 (router) + 6 (synthesizer) = 25
    expect(get().discardedUsage.totalTokens).toBe(25);
    expect(get().discardedResponseCount).toBe(1);
  });

  it("ignores removed messages that carry no billable usage", () => {
    get().setMessages([
      user("u1"),
      {
        id: "err",
        role: "assistant",
        content: "",
        model: "m",
        timestamp: new Date(),
        error: "boom",
      },
    ]);

    get().deleteMessagesAfter("u1");

    expect(get().discardedResponseCount).toBe(0);
    expect(get().discardedUsage.totalTokens).toBe(0);
  });

  it("accumulates across multiple discards", () => {
    get().setMessages([user("u1"), assistant("a1", "m", usage(10, 5))]);
    get().deleteMessagesAfter("u1");
    expect(get().discardedUsage.totalTokens).toBe(15);

    // Append another response after u1, then discard again — the tally adds up.
    useConversationStore.setState((s) => ({
      messages: [...s.messages, assistant("a2", "m", usage(4, 6))],
    }));
    get().deleteMessagesAfter("u1");

    expect(get().discardedUsage.totalTokens).toBe(15 + 10);
    expect(get().discardedResponseCount).toBe(2);
  });

  it("resets the tally on clearMessages, setMessages, and conversation switch", () => {
    get().setMessages([user("u1"), assistant("a1", "m", usage(10, 5))]);
    get().deleteMessagesAfter("u1");
    expect(get().discardedUsage.totalTokens).toBe(15);

    get().clearMessages();
    expect(get().discardedUsage.totalTokens).toBe(0);
    expect(get().discardedResponseCount).toBe(0);

    // Re-dirty, then switching conversations should reset again.
    get().setMessages([user("u2"), assistant("a2", "m", usage(7, 3))]);
    get().deleteMessagesAfter("u2");
    expect(get().discardedUsage.totalTokens).toBe(10);

    const conv = { id: "c1", title: "t", messages: [], models: [] } as unknown as Conversation;
    get().setCurrentConversation(conv);
    expect(get().discardedUsage.totalTokens).toBe(0);
    expect(get().discardedResponseCount).toBe(0);
  });
});
