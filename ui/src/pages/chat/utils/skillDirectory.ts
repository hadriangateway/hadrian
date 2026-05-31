import type { SkillResource } from "@/api/generated/types.gen";

/**
 * Build the `Skill` tool's description: a directory of every enabled skill
 * so the model can match an incoming request against their descriptions and
 * invoke the right one.
 *
 * Matches Claude Code's architecture (https://code.claude.com/docs/en/skills)
 * where a single `Skill` tool's description carries the catalog. Skills are
 * NOT injected into the system prompt — they live in the tool description.
 */
export function buildSkillToolDescription(skills: SkillResource[]): string {
  if (skills.length === 0) {
    return "Invoke a skill by name.";
  }

  const header = [
    "Invoke a skill by name. A skill is a packaged set of instructions (SKILL.md) plus optional bundled files that guide you through a specific task.",
    "",
    'Call with `{command: "<name>"}` to load the skill\'s SKILL.md and a manifest of bundled files.',
    'Call with `{command: "<name>", file: "<path>"}` to read a bundled file referenced in the SKILL.md.',
    "",
    "Available skills:",
  ];

  const listed = skills.map((s) => `- ${s.name}: ${s.description}`);
  return [...header, ...listed].join("\n");
}
