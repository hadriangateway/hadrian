import type { Meta, StoryObj } from "@storybook/react";

import type { SkillResource } from "@/api/generated/types.gen";
import type { SkillWithContext } from "@/hooks/useUserSkills";

import { SkillOwnerBadge } from "./SkillOwnerBadge";

const meta: Meta<typeof SkillOwnerBadge> = {
  title: "Skills/SkillOwnerBadge",
  component: SkillOwnerBadge,
  parameters: { layout: "centered" },
};
export default meta;
type Story = StoryObj<typeof SkillOwnerBadge>;

const baseSkill: SkillResource = {
  id: "skill_00000000-0000-0000-0000-000000000001",
  object: "skill",
  owner_type: "user",
  owner_id: "user-1",
  name: "code-review",
  description: "Review code for best practices.",
  default_version: "1",
  latest_version: "1",
  total_bytes: 0,
  files: [],
  files_manifest: [],
  created_at: 1745280000,
};

export const Personal: Story = {
  args: { skill: baseSkill, currentUserId: "user-1" },
};

export const OtherUser: Story = {
  args: { skill: baseSkill, currentUserId: "user-2" },
};

export const Organization: Story = {
  args: {
    skill: { ...baseSkill, owner_type: "organization", org_name: "Acme" } as SkillWithContext,
  },
};

export const Team: Story = {
  args: {
    skill: { ...baseSkill, owner_type: "team", org_name: "Acme" } as SkillWithContext,
  },
};

export const Project: Story = {
  args: {
    skill: { ...baseSkill, owner_type: "project", org_name: "Acme" } as SkillWithContext,
  },
};
