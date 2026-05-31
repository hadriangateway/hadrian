import type { SkillResource } from "@/api/generated/types.gen";

/**
 * Process-wide cache for skill metadata, indexed by skill name.
 *
 * The chat's `Skill` tool executor needs two things at runtime:
 * 1. A name → id lookup so it can call `skillGet({ id })`.
 * 2. A "have I already fetched this skill's full files?" cache so we don't
 *    refetch on every tool call within the same conversation.
 *
 * Both live here as simple module-scoped maps. `useUserSkills` populates
 * `skillsByName` whenever its result changes (see `setSkillCatalog`),
 * and the executor populates `fullSkillsById` on first fetch.
 *
 * This is intentionally a vanilla JS singleton (not a Zustand store): tool
 * executors run outside React's render tree and need synchronous lookup.
 */
const skillsByName: Map<string, SkillResource> = new Map();
const fullSkillsById: Map<string, SkillResource> = new Map();

/**
 * Replace the in-memory catalog with a fresh listing.
 *
 * Also evicts any entries from `fullSkillsById` whose `default_version`
 * differs from the new catalog (or whose id no longer appears at all).
 * Without this, a long-running session would keep serving stale SKILL.md
 * content from the `Skill` tool executor after a skill was edited (which
 * publishes a new default version) — `useUserSkills` has a 5-min stale
 * time, so the catalog refreshes on its own; this just makes the by-id
 * cache honor that signal too.
 */
export function setSkillCatalog(skills: SkillResource[]): void {
  const seen = new Set<string>();
  skillsByName.clear();
  for (const s of skills) {
    skillsByName.set(s.name, s);
    seen.add(s.id);

    const cached = fullSkillsById.get(s.id);
    if (cached && cached.default_version !== s.default_version) {
      fullSkillsById.delete(s.id);
    }
  }

  // Drop any cached full skills that have disappeared from the catalog.
  for (const id of fullSkillsById.keys()) {
    if (!seen.has(id)) fullSkillsById.delete(id);
  }
}

export function getSkillByName(name: string): SkillResource | undefined {
  return skillsByName.get(name);
}

export function getFullSkill(id: string): SkillResource | undefined {
  return fullSkillsById.get(id);
}

export function setFullSkill(skill: SkillResource): void {
  fullSkillsById.set(skill.id, skill);
}

export function clearSkillCache(): void {
  skillsByName.clear();
  fullSkillsById.clear();
}
