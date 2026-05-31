import { skillGet } from "@/api/generated/sdk.gen";

import { getFullSkill, getSkillByName, setFullSkill } from "./skillCache";
import type { ParsedToolCall } from "./toolCallParser";
import type { Artifact, ToolExecutionResult, ToolExecutor } from "./toolExecutors";

import { formatApiError } from "@/utils/formatApiError";
function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
}

function manifestText(skill: { files: { path: string; byte_size: number }[] }): string {
  const others = skill.files.filter((f) => f.path !== "SKILL.md");
  if (others.length === 0) return "";
  const lines = [
    "",
    "---",
    "",
    'Additional files in this skill. Call Skill again with `{command: "<name>", file: "<path>"}` to read any:',
    ...others.map((f) => `- ${f.path} (${formatBytes(f.byte_size)})`),
  ];
  return lines.join("\n");
}

/**
 * Pick a syntax-highlighting language tag from a skill file path. Falls back
 * to "text" so the artifact still renders as a code block.
 */
function languageForPath(path: string): string {
  const lower = path.toLowerCase();
  const ext = lower.includes(".") ? lower.slice(lower.lastIndexOf(".") + 1) : "";
  switch (ext) {
    case "md":
    case "markdown":
      return "markdown";
    case "py":
      return "python";
    case "js":
    case "mjs":
    case "cjs":
      return "javascript";
    case "ts":
    case "tsx":
      return "typescript";
    case "sh":
    case "bash":
      return "bash";
    case "json":
      return "json";
    case "yaml":
    case "yml":
      return "yaml";
    case "toml":
      return "toml";
    case "html":
    case "htm":
      return "html";
    case "css":
      return "css";
    case "rs":
      return "rust";
    default:
      return "text";
  }
}

/**
 * Load a skill's `SKILL.md` body by id, for seeding directly into a request
 * when the user explicitly invokes it via the slash command (rather than
 * relying on the model to call the `Skill` tool). Uses the by-id cache and
 * falls back to a fetch. Returns `null` if the skill or its SKILL.md is gone.
 */
export async function loadSkillSeed(
  skillId: string
): Promise<{ name: string; text: string } | null> {
  let skill = getFullSkill(skillId);
  if (!skill) {
    try {
      const response = await skillGet({ path: { skill_id: skillId } });
      if (response.error || !response.data) return null;
      skill = response.data;
      setFullSkill(skill);
    } catch {
      return null;
    }
  }
  const main = skill.files?.find((f) => f.path === "SKILL.md");
  if (!main) return null;
  return { name: skill.name, text: main.content };
}

interface SkillToolArgs {
  command?: string;
  file?: string | null;
}

function parseArgs(raw: unknown): SkillToolArgs {
  if (raw === null || raw === undefined) return {};
  if (typeof raw === "string") {
    if (!raw.trim()) return {};
    try {
      return JSON.parse(raw) as SkillToolArgs;
    } catch {
      return {};
    }
  }
  return raw as SkillToolArgs;
}

/** Build a single output artifact for a loaded skill file. */
function fileArtifact(
  toolCallId: string,
  index: number,
  title: string,
  language: string,
  code: string
): Artifact {
  return {
    id: `skill-${toolCallId}-${index}`,
    type: "code",
    title,
    role: "output",
    toolCallId,
    data: { language, code },
  };
}

/**
 * Executes the `Skill` function tool registered with the LLM. Two modes:
 *
 * - `{command: "<name>"}`
 *   Returns the skill's SKILL.md body plus a manifest listing every bundled
 *   file by path and size.
 *
 * - `{command: "<name>", file: "<relative-path>"}`
 *   Returns the content of a bundled file (scripts/, references/, assets/)
 *   referenced in the SKILL.md.
 *
 * Matches Claude Code's progressive-disclosure architecture: the first call
 * pulls the main instructions into context; file calls load referenced
 * resources on demand, not eagerly. The loaded content is also returned as
 * a UI artifact so users can see what the model actually received.
 */
export const skillExecutor: ToolExecutor = async (
  toolCall: ParsedToolCall
): Promise<ToolExecutionResult> => {
  const args = parseArgs(toolCall.arguments);
  const command = args.command?.trim();
  // Generate a stable artifact prefix even when the provider drops the
  // tool-call id (mirrors the codeInterpreterExecutor pattern).
  const toolId = toolCall.id || `skill-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;

  if (!command) {
    return {
      success: false,
      error: "Missing required argument `command` (the skill name).",
    };
  }

  const summary = getSkillByName(command);
  if (!summary) {
    const message = `Skill "${command}" is not available. Check the Available skills list for exact names.`;
    return {
      success: true,
      output: message,
      artifacts: [fileArtifact(toolId, 0, `Skill: ${command}`, "text", message)],
    };
  }

  // The tool description's enum is a soft hint to the model; enforce
  // `disable_model_invocation` here as the hard boundary so a model that
  // learns a skill name from prior context can't bypass an admin's flag.
  // (Explicit user slash-invocations don't reach this path — they seed the
  // SKILL.md into the request directly via `loadSkillSeed`.)
  if (summary.disable_model_invocation === true) {
    const message = `Skill "${command}" cannot be invoked by the model.`;
    return {
      success: false,
      error: message,
      artifacts: [fileArtifact(toolId, 0, `Skill: ${command} (blocked)`, "text", message)],
    };
  }

  // Fetch the full skill on first use; subsequent calls hit the cache.
  let skill = getFullSkill(summary.id);
  if (!skill) {
    try {
      const response = await skillGet({ path: { skill_id: summary.id } });
      if (response.error || !response.data) {
        throw new Error(
          typeof response.error === "object" && response.error && "message" in response.error
            ? String((response.error as { message: unknown }).message)
            : "Failed to load skill"
        );
      }
      skill = response.data;
      setFullSkill(skill);
    } catch (err) {
      return {
        success: false,
        error: `Failed to load skill "${command}": ${err instanceof Error ? err.message : formatApiError(err)}`,
      };
    }
  }

  const filePath = args.file?.trim();
  if (filePath) {
    const file = skill.files?.find((f) => f.path === filePath);
    if (!file) {
      const available = (skill.files ?? [])
        .filter((f) => f.path !== "SKILL.md")
        .map((f) => `  - ${f.path}`)
        .join("\n");
      const message =
        `File "${filePath}" not found in skill "${command}".` +
        (available ? `\n\nAvailable files:\n${available}` : "");
      return {
        success: true,
        output: message,
        artifacts: [fileArtifact(toolId, 0, `${command} · ${filePath} (missing)`, "text", message)],
      };
    }
    return {
      success: true,
      output: file.content,
      artifacts: [
        fileArtifact(
          toolId,
          0,
          `${command} · ${file.path}`,
          languageForPath(file.path),
          file.content
        ),
      ],
    };
  }

  const main = skill.files?.find((f) => f.path === "SKILL.md");
  if (!main) {
    return {
      success: false,
      error: `Skill "${command}" is missing its SKILL.md file.`,
    };
  }

  const manifest = skill.files ? manifestText({ files: skill.files }) : "";
  const fullOutput = main.content + manifest;

  // Render the SKILL.md as the primary output artifact, with the manifest
  // (if any) as a smaller secondary artifact so the file list is glanceable.
  const artifacts: Artifact[] = [
    fileArtifact(toolId, 0, `${command} · SKILL.md`, "markdown", main.content),
  ];
  if (manifest) {
    artifacts.push(
      fileArtifact(toolId, 1, `${command} · bundled files`, "markdown", manifest.trim())
    );
  }

  return { success: true, output: fullOutput, artifacts };
};
