import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";

import {
  organizationListOptions,
  skillListOptions,
} from "@/api/generated/@tanstack/react-query.gen";
import type { Organization, SkillResource } from "@/api/generated/types.gen";

export interface SkillWithContext extends SkillResource {
  /** Org this skill is accessible through (for org/team/project-owned skills). */
  org_id?: string;
  org_slug?: string;
  org_name?: string;
}

export interface UseUserSkillsResult {
  skills: SkillWithContext[];
  organizations: Organization[];
  isLoading: boolean;
  error: Error | null;
  hasMore: boolean;
}

/**
 * Fetch every skill accessible to the current principal. The `/v1/skills`
 * endpoint, called with no owner filter, returns the full accessible set
 * (the principal's own skills plus any org-, team-, and project-scoped
 * skills they can reach) in a single request. Deduplicated by id.
 *
 * Skills are returned with `files_manifest` populated but file contents
 * omitted — call `skillGet` for the full body.
 */
export function useUserSkills(): UseUserSkillsResult {
  const {
    data: skillsData,
    isLoading: skillsLoading,
    error: skillsError,
  } = useQuery({
    ...skillListOptions({ query: { limit: 50 } }),
    staleTime: 5 * 60 * 1000,
  });

  const {
    data: orgsData,
    isLoading: orgsLoading,
    error: orgsError,
  } = useQuery({
    ...organizationListOptions(),
    staleTime: 5 * 60 * 1000,
  });

  const organizations = useMemo(() => orgsData?.data ?? [], [orgsData?.data]);

  const skills = useMemo(() => {
    // Resolve org context for org-owned skills by matching `owner_id`
    // against the org list. Team/project owners don't carry an org id on
    // the resource, so they render without an org name (still labelled by
    // their owner type via `SkillOwnerBadge`).
    const orgById = new Map(organizations.map((o) => [o.id, o]));

    const seen = new Set<string>();
    const result: SkillWithContext[] = [];

    for (const s of skillsData?.data ?? []) {
      if (seen.has(s.id)) continue;
      seen.add(s.id);

      const org = s.owner_type === "organization" ? orgById.get(s.owner_id) : undefined;
      result.push(org ? { ...s, org_id: org.id, org_slug: org.slug, org_name: org.name } : s);
    }

    result.sort((a, b) => a.name.localeCompare(b.name));
    return result;
  }, [skillsData?.data, organizations]);

  const isLoading = skillsLoading || orgsLoading;
  const error = skillsError ?? orgsError ?? null;
  const hasMore = skillsData?.has_more ?? false;

  return {
    skills,
    organizations,
    isLoading,
    error: error as Error | null,
    hasMore,
  };
}
