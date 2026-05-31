import type { SkillResource } from "@/api/generated/types.gen";
import { Badge } from "@/components/Badge/Badge";

import type { SkillWithContext } from "@/hooks/useUserSkills";

export interface SkillOwnerBadgeProps {
  skill: SkillResource | SkillWithContext;
  /** When set, user-owned skills owned by this user render as "Personal". */
  currentUserId?: string;
}

function isWithContext(s: SkillResource | SkillWithContext): s is SkillWithContext {
  return (s as SkillWithContext).org_name !== undefined;
}

/**
 * Compact badge that tells the user where a skill comes from. Owner is the
 * core distinction (Personal / Org / Team / Project), and for non-personal
 * skills the org name is appended when available so users with many orgs
 * can disambiguate.
 */
export function SkillOwnerBadge({ skill, currentUserId }: SkillOwnerBadgeProps) {
  const orgName = isWithContext(skill) ? skill.org_name : undefined;

  switch (skill.owner_type) {
    case "user":
      if (currentUserId && skill.owner_id === currentUserId) {
        return (
          <Badge variant="outline" className="px-1 py-0 text-[10px] font-normal">
            Personal
          </Badge>
        );
      }
      return (
        <Badge variant="outline" className="px-1 py-0 text-[10px] font-normal">
          User
        </Badge>
      );
    case "organization":
      return (
        <Badge variant="secondary" className="px-1 py-0 text-[10px] font-normal">
          {orgName ? `Org · ${orgName}` : "Org"}
        </Badge>
      );
    case "team":
      return (
        <Badge variant="secondary" className="px-1 py-0 text-[10px] font-normal">
          {orgName ? `Team · ${orgName}` : "Team"}
        </Badge>
      );
    case "project":
      return (
        <Badge variant="outline" className="px-1 py-0 text-[10px] font-normal">
          {orgName ? `Project · ${orgName}` : "Project"}
        </Badge>
      );
    default:
      return null;
  }
}
