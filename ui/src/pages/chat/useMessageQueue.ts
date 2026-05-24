import { useCallback, useEffect, useState } from "react";

import type { ChatFile, QueuedMessage } from "@/components/chat-types";
import { chatMessageQueue, type SendFn } from "./messageQueue";

/**
 * React wrapper around the {@link chatMessageQueue} singleton. Lets the user
 * keep composing and hitting "send" while a response streams; messages queued
 * mid-turn are dispatched one at a time as each turn completes.
 *
 * The queue is a module-level singleton so its in-flight lock survives the
 * `ChatPage` remount triggered by the first message's `/chat` → `/chat/:id`
 * navigation. The latest `send` closure is pushed in on every render.
 */
export function useMessageQueue(send: SendFn) {
  const [queuedMessages, setQueuedMessages] = useState<QueuedMessage[]>([]);

  // Keep the singleton pointed at the latest send closure. In an effect (not in
  // the render phase) so a render that React starts but discards in concurrent
  // mode can't leave the singleton holding an uncommitted closure. `send`
  // changes whenever model/tool/config changes; the effect commits well before
  // any user-triggered send or queue drain, so dispatch always uses the current
  // context.
  useEffect(() => {
    chatMessageQueue.setSend(send);
  }, [send]);

  useEffect(() => chatMessageQueue.subscribe(setQueuedMessages), []);

  const sendOrQueue = useCallback(
    (content: string, files: ChatFile[]) => chatMessageQueue.sendOrQueue(content, files),
    []
  );

  const removeQueuedMessage = useCallback((id: string) => chatMessageQueue.remove(id), []);

  const clearQueue = useCallback(() => chatMessageQueue.clear(), []);

  return { queuedMessages, sendOrQueue, removeQueuedMessage, clearQueue };
}
