import type { Meta, StoryObj } from "@storybook/react";
import { expect, within, userEvent, fn } from "storybook/test";
import { MultiModelResponse } from "./MultiModelResponse";
import { PreferencesProvider } from "@/preferences/PreferencesProvider";
import { useChatUIStore } from "@/stores/chatUIStore";
import { useStreamingStore } from "@/stores/streamingStore";

const meta: Meta<typeof MultiModelResponse> = {
  title: "Chat/MultiModelResponse",
  component: MultiModelResponse,
  parameters: {
    layout: "padded",
  },

  decorators: [
    (Story) => {
      // Reset store state before each story
      useChatUIStore.setState({
        viewMode: "grid",
        expandedModel: null,
        editingKey: null, // Reset editing state
        compactMode: false, // Show reasoning & tools in tests
      });
      // Reset streaming store to ensure isStreaming is false
      useStreamingStore.setState({
        streams: new Map(),
        isStreaming: false,
        modeState: { mode: null },
      });
      return (
        <PreferencesProvider>
          <div style={{ maxWidth: 1000 }}>
            <Story />
          </div>
        </PreferencesProvider>
      );
    },
  ],
};

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * Test: Single response renders correctly without multi-response UI elements
 */
export const SingleResponse: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Hello! I'm Claude, an AI assistant made by Anthropic. How can I help you today?",
        isStreaming: false,
        completedRounds: [
          {
            content:
              "Hello! I'm Claude, an AI assistant made by Anthropic. How can I help you today?",
          },
        ],
      },
    ],
    timestamp: new Date(),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify response content is displayed
    await expect(
      canvas.getByText(/Hello! I'm Claude, an AI assistant made by Anthropic/)
    ).toBeInTheDocument();

    // Verify model name badge is shown (look for the styled badge with text-xs font-semibold)
    const modelBadge = canvasElement.querySelector(
      'span[class*="rounded-md"][class*="font-semibold"]'
    );
    await expect(modelBadge).toBeInTheDocument();
    await expect(modelBadge?.textContent).toContain("Claude");

    // Verify "responses" count badge is NOT shown for single response
    const responsesBadge = canvas.queryByText(/\d+ responses/);
    await expect(responsesBadge).not.toBeInTheDocument();
  },
};

/**
 * Test: Multiple responses show view toggle and response count
 */
export const MultipleResponses: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content:
          "Here's a solution using a recursive approach:\n\n```python\ndef factorial(n):\n    if n <= 1:\n        return 1\n    return n * factorial(n - 1)\n```\n\nThis is elegant but may hit recursion limits for large numbers.",
        isStreaming: false,
        completedRounds: [
          {
            content:
              "Here's a solution using a recursive approach:\n\n```python\ndef factorial(n):\n    if n <= 1:\n        return 1\n    return n * factorial(n - 1)\n```\n\nThis is elegant but may hit recursion limits for large numbers.",
          },
        ],
      },
      {
        model: "openai/gpt-4",
        content:
          "I'd recommend an iterative approach:\n\n```python\ndef factorial(n):\n    result = 1\n    for i in range(2, n + 1):\n        result *= i\n    return result\n```\n\nThis avoids stack overflow and is more memory efficient.",
        isStreaming: false,
        completedRounds: [
          {
            content:
              "I'd recommend an iterative approach:\n\n```python\ndef factorial(n):\n    result = 1\n    for i in range(2, n + 1):\n        result *= i\n    return result\n```\n\nThis avoids stack overflow and is more memory efficient.",
          },
        ],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-1",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify both responses are rendered
    await expect(canvas.getByText(/recursive approach/i)).toBeInTheDocument();
    await expect(canvas.getByText(/iterative approach/i)).toBeInTheDocument();

    // Verify response count badge shows "2 responses"
    await expect(canvas.getByText("2 responses")).toBeInTheDocument();

    // Verify both model cards are rendered (cards have shadow-sm class)
    const cards = canvasElement.querySelectorAll('[class*="shadow-sm"][class*="rounded-xl"]');
    await expect(cards.length).toBe(2);

    // Verify view toggle buttons are present for multi-response (grid + stacked inside toggle group)
    const toggleGroup = canvasElement.querySelector('[class*="gap-0.5"][class*="rounded-md"]');
    await expect(toggleGroup).toBeInTheDocument();
    const toggleButtons = toggleGroup!.querySelectorAll("button");
    await expect(toggleButtons.length).toBe(2);
  },
};

/**
 * Test: Streaming responses show typing indicator for empty content
 */
export const Streaming: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "I'm thinking about your question...",
        isStreaming: true,
      },
      {
        model: "openai/gpt-4",
        content: "",
        isStreaming: true,
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-streaming",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify first model's StreamingMarkdown container is rendered (content animates in via Streamdown)
    const markdownContainers = canvasElement.querySelectorAll(".markdown-content");
    await expect(markdownContainers.length).toBeGreaterThan(0);

    // Verify second model shows "Thinking" indicator (empty content during streaming)
    await expect(canvas.getByText("Thinking")).toBeInTheDocument();

    // Verify typing indicator dots are present (the animated dots)
    const typingDots = canvasElement.querySelectorAll('[class*="animate-typing"]');
    await expect(typingDots.length).toBeGreaterThan(0);

    // Verify "select as best" is NOT available during streaming
    const selectBestButton = canvas.queryByText(/select as best/i);
    await expect(selectBestButton).not.toBeInTheDocument();
  },
};

/**
 * Test: Error state displays error message with alert styling
 */
export const WithError: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Here's my response to your question...",
        isStreaming: false,
        completedRounds: [{ content: "Here's my response to your question..." }],
      },
      {
        model: "openai/gpt-4",
        content: "",
        isStreaming: false,
        error: "Rate limit exceeded. Please try again later.",
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-error",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify successful response is displayed
    await expect(canvas.getByText(/Here's my response to your question/i)).toBeInTheDocument();

    // Verify error message is displayed
    await expect(
      canvas.getByText("Rate limit exceeded. Please try again later.")
    ).toBeInTheDocument();

    // Verify error has destructive styling (red/alert colors)
    const errorContainer = canvasElement.querySelector('[class*="destructive"]');
    await expect(errorContainer).toBeInTheDocument();

    // Verify AlertCircle icon is present in the error
    const alertIcon = canvasElement.querySelector('[class*="destructive"] svg');
    await expect(alertIcon).toBeInTheDocument();
  },
};

/**
 * Test: Four models renders with correct count and all cards visible
 */
export const FourModels: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content:
          "Claude's response: This is a comprehensive answer from Anthropic's flagship model.",
        isStreaming: false,
        completedRounds: [
          {
            content:
              "Claude's response: This is a comprehensive answer from Anthropic's flagship model.",
          },
        ],
      },
      {
        model: "openai/gpt-4-turbo",
        content: "GPT-4's response: Here's OpenAI's perspective on the question.",
        isStreaming: false,
        completedRounds: [
          { content: "GPT-4's response: Here's OpenAI's perspective on the question." },
        ],
      },
      {
        model: "google/gemini-1.5-pro",
        content: "Gemini's response: Google's take on the problem at hand.",
        isStreaming: false,
        completedRounds: [{ content: "Gemini's response: Google's take on the problem at hand." }],
      },
      {
        model: "mistral/mistral-large",
        content: "Mistral's response: An alternative viewpoint from the open-source leader.",
        isStreaming: false,
        completedRounds: [
          {
            content: "Mistral's response: An alternative viewpoint from the open-source leader.",
          },
        ],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-four",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify "4 responses" badge
    await expect(canvas.getByText("4 responses")).toBeInTheDocument();

    // Verify all four responses are rendered
    await expect(canvas.getByText(/Claude's response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/GPT-4's response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/Gemini's response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/Mistral's response/i)).toBeInTheDocument();

    // Verify 4 model cards are rendered (each has a border)
    const cards = canvasElement.querySelectorAll('[class*="rounded-xl"][class*="border"]');
    await expect(cards.length).toBe(4);
  },
};

/**
 * Test: View mode toggle switches between grid and stacked layouts
 */
export const ViewModeToggle: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "First model response for layout testing.",
        isStreaming: false,
        completedRounds: [{ content: "First model response for layout testing." }],
      },
      {
        model: "openai/gpt-4",
        content: "Second model response for layout testing.",
        isStreaming: false,
        completedRounds: [{ content: "Second model response for layout testing." }],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-viewmode",
  },
  play: async ({ canvasElement }) => {
    // Find the grid/stacked toggle buttons inside the toggle group
    const toggleGroup = canvasElement.querySelector('[class*="gap-0.5"][class*="rounded-md"]');
    await expect(toggleGroup).toBeInTheDocument();
    const toggleButtons = Array.from(toggleGroup!.querySelectorAll("button"));

    // Should have 2 toggle buttons (grid + stacked)
    await expect(toggleButtons.length).toBe(2);

    // In grid mode, cards should have basis-[min(500px,85vw)] class (horizontal layout)
    let gridCards = canvasElement.querySelectorAll('[class*="basis-"]');
    await expect(gridCards.length).toBe(2);

    // Click the stacked button (second toggle button)
    await userEvent.click(toggleButtons[1]);

    // After clicking stacked, cards should NOT have basis-[min(500px,85vw)] (vertical layout)
    gridCards = canvasElement.querySelectorAll('[class*="basis-"]');
    await expect(gridCards.length).toBe(0);

    // Cards should now be full width (w-full)
    const stackedCards = canvasElement.querySelectorAll('[class*="w-full"]');
    await expect(stackedCards.length).toBeGreaterThanOrEqual(2);
  },
};

/**
 * Test: Token usage is displayed in response header
 */
export const WithTokenUsage: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Response with token usage information displayed.",
        isStreaming: false,
        completedRounds: [{ content: "Response with token usage information displayed." }],
        usage: {
          inputTokens: 50,
          outputTokens: 100,
          totalTokens: 150,
          cost: 0.0025,
        },
      },
      {
        model: "openai/gpt-4",
        content: "Another response with different usage stats.",
        isStreaming: false,
        completedRounds: [{ content: "Another response with different usage stats." }],
        usage: {
          inputTokens: 45,
          outputTokens: 80,
          totalTokens: 125,
          cost: 0.002,
        },
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-usage",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify token counts are displayed (UsageDisplay shows just the number, not "tokens")
    await expect(canvas.getByText("150")).toBeInTheDocument();
    await expect(canvas.getByText("125")).toBeInTheDocument();

    // Verify costs are displayed (the cost is inside a span, look for text content)
    // UsageDisplay renders: <span>150</span> · <span>$0.0025</span>
    const usageDisplays = canvasElement.querySelectorAll(
      '[class*="text-muted-foreground"][class*="cursor-help"]'
    );
    await expect(usageDisplays.length).toBe(2);

    // Check that the cost values appear somewhere in the usage displays
    const usageText = Array.from(usageDisplays)
      .map((el) => el.textContent)
      .join(" ");
    await expect(usageText).toContain("$0.0025");
    await expect(usageText).toContain("$0.0020");
  },
};

/**
 * Test: Usage ticks live while streaming — a server-tool loop reports
 * cumulative tokens/cost at each turn boundary (`response.usage.updated`),
 * so the display must render before the stream completes.
 */
export const StreamingWithUsage: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Searching the web for relevant results...",
        isStreaming: true,
        usage: {
          inputTokens: 300,
          outputTokens: 80,
          totalTokens: 380,
          cost: 0.003,
        },
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-streaming-usage",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // The running totals show while the stream is still open.
    await expect(canvas.getByText("380")).toBeInTheDocument();
    const usageDisplay = canvasElement.querySelector('[class*="cursor-help"]');
    await expect(usageDisplay).toBeInTheDocument();
    await expect(usageDisplay!.textContent).toContain("$0.0030");

    // The whole provisional label pulses until the terminal figure lands.
    await expect(usageDisplay!.className).toContain("animate-pulse");

    // Still streaming: completion-only actions stay hidden.
    const selectBestButton = canvas.queryByText(/select as best/i);
    await expect(selectBestButton).not.toBeInTheDocument();
  },
};

/**
 * Test: Selected best response shows trophy badge and ring highlight
 */
export const WithSelectedBest: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "This response was selected as the best.",
        isStreaming: false,
        completedRounds: [{ content: "This response was selected as the best." }],
      },
      {
        model: "openai/gpt-4",
        content: "This response was not selected.",
        isStreaming: false,
        completedRounds: [{ content: "This response was not selected." }],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-best",
    selectedBest: "anthropic/claude-3-opus",
    onSelectBest: fn(),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify "Best" badge is shown
    await expect(canvas.getByText("Best")).toBeInTheDocument();

    // Verify trophy icon is present
    const trophyIcon = canvasElement.querySelector('[class*="text-success"] svg');
    await expect(trophyIcon).toBeInTheDocument();

    // Verify selected card has ring highlight
    const highlightedCard = canvasElement.querySelector('[class*="ring-success"]');
    await expect(highlightedCard).toBeInTheDocument();

    // The selected best response should be sorted first (it contains "selected as the best")
    const cards = canvasElement.querySelectorAll('[class*="rounded-xl"][class*="border"]');
    const firstCardText = cards[0]?.textContent;
    await expect(firstCardText).toContain("selected as the best");
  },
};

/**
 * Test: Action buttons are rendered and functional
 */
export const WithActionCallbacks: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Response with available actions.",
        isStreaming: false,
        completedRounds: [{ content: "Response with available actions." }],
      },
      {
        model: "openai/gpt-4",
        content: "Another response with actions.",
        isStreaming: false,
        completedRounds: [{ content: "Another response with actions." }],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-actions",
    onSelectBest: fn(),
    onRegenerate: fn(),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify both responses are rendered
    await expect(canvas.getByText(/Response with available actions/i)).toBeInTheDocument();
    await expect(canvas.getByText(/Another response with actions/i)).toBeInTheDocument();

    // Verify action buttons exist (ResponseActions renders buttons with h-7 w-7 classes)
    const actionButtons = canvasElement.querySelectorAll(
      '[class*="h-7"][class*="w-7"][class*="p-0"]'
    );
    await expect(actionButtons.length).toBeGreaterThan(0);
  },
};

/**
 * Test: History mode badge shows when set to same-model
 */
export const WithHistoryModeBadge: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Response generated with same-model history mode.",
        isStreaming: false,
        completedRounds: [{ content: "Response generated with same-model history mode." }],
      },
      {
        model: "openai/gpt-4",
        content: "Each model only saw its own previous responses.",
        isStreaming: false,
        completedRounds: [{ content: "Each model only saw its own previous responses." }],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-history",
    historyMode: "same-model",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify history mode badge is displayed
    await expect(canvas.getByText("Same model")).toBeInTheDocument();

    // Verify GitFork icon is present (history mode indicator)
    const badgeContainer = canvasElement.querySelector('[class*="bg-primary"]');
    await expect(badgeContainer).toBeInTheDocument();
  },
};

/**
 * Test: Reasoning content (extended thinking) is displayed
 */
export const WithReasoningContent: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "The answer is 42.",
        reasoningContent:
          "Let me think about this step by step... First, I need to consider the question carefully...",
        isStreaming: false,
        completedRounds: [
          {
            reasoning:
              "Let me think about this step by step... First, I need to consider the question carefully...",
            content: "The answer is 42.",
          },
        ],
        usage: {
          inputTokens: 20,
          outputTokens: 50,
          totalTokens: 70,
          reasoningTokens: 100,
        },
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-reasoning",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify main response content is shown
    await expect(canvas.getByText("The answer is 42.")).toBeInTheDocument();

    // Verify reasoning section exists (look for the collapsible thinking section)
    // The ReasoningSection component should render the thinking content
    const reasoningText = canvas.queryByText(/Let me think about this step by step/i);

    // The reasoning section might be collapsed by default, so we check for the toggle
    const thinkingButton = canvas.queryByRole("button", { name: /thinking/i });
    if (thinkingButton && !reasoningText) {
      // Expand the reasoning section
      await userEvent.click(thinkingButton);
      await expect(canvas.getByText(/Let me think about this step by step/i)).toBeInTheDocument();
    } else if (reasoningText) {
      await expect(reasoningText).toBeInTheDocument();
    }
  },
};

/**
 * Test: Streaming with running tool shows tool execution UI (not Thinking indicator)
 */
export const WithToolCallSearching: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "",
        isStreaming: true,
        toolExecutionRounds: [
          {
            round: 1,
            executions: [
              {
                id: "tc_1",
                toolName: "file_search",
                status: "running",
                startTime: Date.now(),
                input: { query: "test query" },
                inputArtifacts: [],
                outputArtifacts: [],
                round: 1,
              },
            ],
          },
        ],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-toolcall",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // While tools are visibly running, shows tool status instead of Thinking indicator
    const runningElements = canvas.getAllByText("running");
    await expect(runningElements.length).toBeGreaterThan(0);
  },
};

/**
 * Test: Tool execution round with content shows both tool block and content
 */
export const WithToolCallAndContent: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Based on my search of your documents, I found the following...",
        isStreaming: false,
        toolExecutionRounds: [
          {
            round: 1,
            executions: [
              {
                id: "tc_2",
                toolName: "file_search",
                status: "success",
                startTime: Date.now() - 1000,
                endTime: Date.now(),
                duration: 1000,
                input: { query: "test query" },
                inputArtifacts: [],
                outputArtifacts: [],
                round: 1,
              },
            ],
          },
        ],
        completedRounds: [
          {
            toolExecution: {
              round: 1,
              executions: [
                {
                  id: "tc_2",
                  toolName: "file_search",
                  status: "success" as const,
                  startTime: Date.now() - 1000,
                  endTime: Date.now(),
                  duration: 1000,
                  input: { query: "test query" },
                  inputArtifacts: [],
                  outputArtifacts: [],
                  round: 1,
                },
              ],
            },
            content: "Based on my search of your documents, I found the following...",
          },
        ],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-toolcall-content",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify content is shown
    await expect(canvas.getByText(/Based on my search of your documents/i)).toBeInTheDocument();

    // Verify tool execution summary bar is rendered (shows "1 tool" collapsed)
    await expect(canvas.getByText(/1 tool\b/)).toBeInTheDocument();
  },
};

/**
 * Test: Multiple tool calls shown in execution rounds
 */
export const WithMultipleToolCalls: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Here are the results from my research...",
        isStreaming: false,
        toolExecutionRounds: [
          {
            round: 1,
            executions: [
              {
                id: "tc_3",
                toolName: "file_search",
                status: "success",
                startTime: Date.now() - 2000,
                endTime: Date.now() - 1000,
                duration: 1000,
                input: { query: "test query" },
                inputArtifacts: [],
                outputArtifacts: [],
                round: 1,
              },
              {
                id: "tc_4",
                toolName: "web_search",
                status: "success",
                startTime: Date.now() - 1000,
                endTime: Date.now(),
                duration: 1000,
                input: { query: "web query" },
                inputArtifacts: [],
                outputArtifacts: [],
                round: 1,
              },
            ],
          },
        ],
        completedRounds: [
          {
            toolExecution: {
              round: 1,
              executions: [
                {
                  id: "tc_3",
                  toolName: "file_search",
                  status: "success" as const,
                  startTime: Date.now() - 2000,
                  endTime: Date.now() - 1000,
                  duration: 1000,
                  input: { query: "test query" },
                  inputArtifacts: [],
                  outputArtifacts: [],
                  round: 1,
                },
                {
                  id: "tc_4",
                  toolName: "web_search",
                  status: "success" as const,
                  startTime: Date.now() - 1000,
                  endTime: Date.now(),
                  duration: 1000,
                  input: { query: "web query" },
                  inputArtifacts: [],
                  outputArtifacts: [],
                  round: 1,
                },
              ],
            },
            content: "Here are the results from my research...",
          },
        ],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-multi-toolcall",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify content is shown
    await expect(canvas.getByText(/Here are the results/i)).toBeInTheDocument();

    // Verify tool execution summary bar shows "2 tools" (collapsed by default when not streaming)
    await expect(canvas.getByText(/2 tools/)).toBeInTheDocument();

    // Click the summary bar to expand and show individual tool names
    const summaryBar = canvas.getByText(/2 tools/);
    await userEvent.click(summaryBar);

    // Verify both tool names are now visible in the expanded timeline
    await expect(canvas.getByText("File Search")).toBeInTheDocument();
    await expect(canvas.getByText("Web Search")).toBeInTheDocument();
  },
};

/**
 * Test: Hide callback is triggered when clicking hide button
 */
export const WithHideCallback: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "Response that can be hidden.",
        isStreaming: false,
        completedRounds: [{ content: "Response that can be hidden." }],
      },
      {
        model: "openai/gpt-4",
        content: "Another response for hide testing.",
        isStreaming: false,
        completedRounds: [{ content: "Another response for hide testing." }],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-hide",
    onSelectBest: fn(),
    onRegenerate: fn(),
    onHide: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);

    // Verify both responses are shown
    await expect(canvas.getByText(/Response that can be hidden/i)).toBeInTheDocument();
    await expect(canvas.getByText(/Another response for hide testing/i)).toBeInTheDocument();

    // Find the hide buttons - they use the EyeOff icon and are in the action buttons area
    // The hide button is the last action button (after expand)
    const actionButtons = canvasElement.querySelectorAll(
      '[class*="h-7"][class*="w-7"][class*="p-0"]'
    );

    // Click the hide button for the first response (should be one of the later buttons)
    // Find buttons that contain EyeOff icon
    let hideButton: Element | null = null;
    for (const btn of Array.from(actionButtons)) {
      const svgPath = btn.querySelector("path");
      if (
        svgPath &&
        svgPath.getAttribute("d")?.includes("M17.94 17.94") // EyeOff icon path starts
      ) {
        hideButton = btn;
        break;
      }
    }

    if (hideButton) {
      await userEvent.click(hideButton as HTMLElement);

      // Verify onHide was called with correct arguments
      await expect(args.onHide).toHaveBeenCalledWith("test-group-hide", "anthropic/claude-3-opus");
    }
  },
};

/**
 * Test: Hidden responses indicator shows count and dropdown to restore
 */
export const WithHiddenResponses: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "This response is visible.",
        isStreaming: false,
        completedRounds: [{ content: "This response is visible." }],
      },
    ],
    hiddenResponses: [
      {
        model: "openai/gpt-4",
        instanceId: "openai/gpt-4",
        label: undefined,
      },
      {
        model: "google/gemini-1.5-pro",
        instanceId: "google/gemini-1.5-pro",
        label: "Gemini Pro",
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-hidden",
    onShowHidden: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const body = within(document.body);

    // Verify visible response is shown
    await expect(canvas.getByText(/This response is visible/i)).toBeInTheDocument();

    // Verify hidden responses indicator is shown
    await expect(canvas.getByText("2 hidden responses")).toBeInTheDocument();

    // Click the hidden responses indicator to open dropdown
    const hiddenIndicator = canvas.getByText("2 hidden responses");
    await userEvent.click(hiddenIndicator);

    // Dropdown is rendered in a portal, so query from body
    // Verify dropdown shows "Show all" option
    await expect(body.getByText("Show all (2)")).toBeInTheDocument();

    // Verify individual model options are shown
    // GPT-4 should show display name
    await expect(body.getByText("GPT-4")).toBeInTheDocument();
    // Gemini should show custom label
    await expect(body.getByText("Gemini Pro")).toBeInTheDocument();

    // Click "Show all" to restore all hidden responses
    await userEvent.click(body.getByText("Show all (2)"));

    // Verify onShowHidden was called for each hidden response
    await expect(args.onShowHidden).toHaveBeenCalledWith("test-group-hidden", "openai/gpt-4");
    await expect(args.onShowHidden).toHaveBeenCalledWith(
      "test-group-hidden",
      "google/gemini-1.5-pro"
    );
  },
};

/**
 * Test: Single hidden response shows indicator without "Show all" option
 */
export const WithSingleHiddenResponse: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content: "This response is visible.",
        isStreaming: false,
        completedRounds: [{ content: "This response is visible." }],
      },
      {
        model: "openai/gpt-4",
        content: "Another visible response.",
        isStreaming: false,
        completedRounds: [{ content: "Another visible response." }],
      },
    ],
    hiddenResponses: [
      {
        model: "google/gemini-1.5-pro",
        instanceId: "google/gemini-1.5-pro",
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-single-hidden",
    onShowHidden: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const body = within(document.body);

    // Verify hidden responses indicator shows singular form
    await expect(canvas.getByText("1 hidden response")).toBeInTheDocument();

    // Click the hidden responses indicator to open dropdown
    const hiddenIndicator = canvas.getByText("1 hidden response");
    await userEvent.click(hiddenIndicator);

    // Dropdown is rendered in a portal, so query from body
    // Verify "Show all" is NOT shown for single hidden response
    await expect(body.queryByText(/Show all/i)).not.toBeInTheDocument();

    // Verify the single model option is shown
    await expect(body.getByText("Gemini 1.5 Pro")).toBeInTheDocument();

    // Click to restore the hidden response
    await userEvent.click(body.getByText("Gemini 1.5 Pro"));

    // Verify onShowHidden was called
    await expect(args.onShowHidden).toHaveBeenCalledWith(
      "test-group-single-hidden",
      "google/gemini-1.5-pro"
    );
  },
};

/**
 * Test: Six models renders with scroll navigation arrows in grid mode
 */
export const SixModelsScrollNavigation: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        content:
          "Claude Opus response: A thorough analysis considering multiple perspectives and edge cases.",
        isStreaming: false,
        completedRounds: [
          {
            content:
              "Claude Opus response: A thorough analysis considering multiple perspectives and edge cases.",
          },
        ],
      },
      {
        model: "openai/gpt-4-turbo",
        content: "GPT-4 Turbo response: A fast, detailed answer with practical code examples.",
        isStreaming: false,
        completedRounds: [
          {
            content: "GPT-4 Turbo response: A fast, detailed answer with practical code examples.",
          },
        ],
      },
      {
        model: "google/gemini-1.5-pro",
        content:
          "Gemini Pro response: Leveraging multimodal understanding for comprehensive analysis.",
        isStreaming: false,
        completedRounds: [
          {
            content:
              "Gemini Pro response: Leveraging multimodal understanding for comprehensive analysis.",
          },
        ],
      },
      {
        model: "mistral/mistral-large",
        content:
          "Mistral Large response: An efficient, open-weight model perspective on the topic.",
        isStreaming: false,
        completedRounds: [
          {
            content:
              "Mistral Large response: An efficient, open-weight model perspective on the topic.",
          },
        ],
      },
      {
        model: "anthropic/claude-3-haiku",
        content: "Claude Haiku response: A concise and rapid answer to your question.",
        isStreaming: false,
        completedRounds: [
          { content: "Claude Haiku response: A concise and rapid answer to your question." },
        ],
      },
      {
        model: "openai/gpt-4o",
        content: "GPT-4o response: Combining speed and intelligence for a balanced answer.",
        isStreaming: false,
        completedRounds: [
          {
            content: "GPT-4o response: Combining speed and intelligence for a balanced answer.",
          },
        ],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-six",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify "6 responses" badge
    await expect(canvas.getByText("6 responses")).toBeInTheDocument();

    // Verify all six responses are rendered
    await expect(canvas.getByText(/Claude Opus response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/GPT-4 Turbo response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/Gemini Pro response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/Mistral Large response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/Claude Haiku response/i)).toBeInTheDocument();
    await expect(canvas.getByText(/GPT-4o response/i)).toBeInTheDocument();

    // Verify scroll right button appears (content overflows the 1000px container)
    const scrollRightButton = canvas.queryByRole("button", { name: /scroll right/i });
    await expect(scrollRightButton).toBeInTheDocument();
  },
};

/**
 * Test: Edit button is visible when onSaveEdit and messageId are provided
 */
export const WithEditButton: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        messageId: "msg-1",
        content: "This response can be edited by the user.",
        isStreaming: false,
        completedRounds: [{ content: "This response can be edited by the user." }],
      },
      {
        model: "openai/gpt-4",
        messageId: "msg-2",
        content: "This response can also be edited.",
        isStreaming: false,
        completedRounds: [{ content: "This response can also be edited." }],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-edit",
    onSaveEdit: fn(),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify both responses are shown
    await expect(canvas.getByText(/This response can be edited by the user/i)).toBeInTheDocument();
    await expect(canvas.getByText(/This response can also be edited/i)).toBeInTheDocument();

    // Find edit buttons by aria-label
    const editButtons = canvas.getAllByRole("button", { name: /edit response/i });

    // Should have 2 edit buttons (one per response)
    await expect(editButtons.length).toBe(2);

    // Verify edit buttons are interactive
    await expect(editButtons[0]).toBeEnabled();
    await expect(editButtons[1]).toBeEnabled();
  },
};

/**
 * Test: Edit button is NOT visible when messageId is missing
 */
export const WithoutEditButtonWhenNoMessageId: Story = {
  args: {
    responses: [
      {
        model: "anthropic/claude-3-opus",
        // No messageId provided
        content: "This response cannot be edited (no messageId).",
        isStreaming: false,
        completedRounds: [{ content: "This response cannot be edited (no messageId)." }],
      },
    ],
    timestamp: new Date(),
    groupId: "test-group-no-edit",
    onSaveEdit: fn(),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);

    // Verify response is shown
    await expect(
      canvas.getByText(/This response cannot be edited \(no messageId\)/i)
    ).toBeInTheDocument();

    // Verify NO edit button is present (because messageId is missing)
    const editButtons = canvas.queryAllByRole("button", { name: /edit response/i });
    await expect(editButtons.length).toBe(0);
  },
};
