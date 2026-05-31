import { useMemo, useState } from "react";
import {
  Brain,
  CheckSquare,
  Download,
  Folder,
  Loader2,
  Plus,
  Search,
  Square,
  Trash2,
} from "lucide-react";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import type { SkillResource } from "@/api/generated/types.gen";
import { skillDelete } from "@/api/generated/sdk.gen";
import { Button } from "@/components/Button/Button";
import {
  Dropdown,
  DropdownContent,
  DropdownItem,
  DropdownTrigger,
} from "@/components/Dropdown/Dropdown";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/Popover/Popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/Tooltip/Tooltip";
import { useConfirm } from "@/components/ConfirmDialog/ConfirmDialog";
import { useToast } from "@/components/Toast/Toast";
import { useUserSkills } from "@/hooks/useUserSkills";
import { useAuth } from "@/auth";
import { useEnabledSkillIds, useChatUIStore } from "@/stores/chatUIStore";
import { SkillFormModal } from "@/components/Admin";
import { SkillImportModal } from "@/components/SkillImportModal/SkillImportModal";
import { SkillOwnerBadge } from "@/components/SkillsButton/SkillOwnerBadge";

export interface SkillsButtonProps {
  /** Whether the button is disabled. */
  disabled?: boolean;
}

export function SkillsButton({ disabled = false }: SkillsButtonProps) {
  const [open, setOpen] = useState(false);
  const [createModalOpen, setCreateModalOpen] = useState(false);
  const [importModalOpen, setImportModalOpen] = useState(false);
  const [importTab, setImportTab] = useState<"github" | "filesystem">("github");
  const [editingSkill, setEditingSkill] = useState<SkillResource | null>(null);

  const { skills, isLoading, hasMore } = useUserSkills();
  const { user } = useAuth();
  const enabledIds = useEnabledSkillIds();
  const toggleSkill = useChatUIStore((s) => s.toggleSkill);
  const setEnabledSkillIds = useChatUIStore((s) => s.setEnabledSkillIds);
  const { toast } = useToast();
  const confirm = useConfirm();
  const queryClient = useQueryClient();

  const [search, setSearch] = useState("");

  const filteredSkills = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return skills;
    return skills.filter(
      (s) => s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q)
    );
  }, [skills, search]);

  // "All enabled" means every skill currently visible in the popover is on.
  // Toggle-all operates on the visible set so a search narrows the scope.
  const allVisibleEnabled =
    filteredSkills.length > 0 && filteredSkills.every((s) => enabledIds.includes(s.id));

  const handleToggleAll = () => {
    if (allVisibleEnabled) {
      // Disable every visible skill, keep others as-is.
      const visibleIds = new Set(filteredSkills.map((s) => s.id));
      setEnabledSkillIds(enabledIds.filter((id) => !visibleIds.has(id)));
    } else {
      // Enable every visible skill (in addition to anything already enabled).
      const merged = new Set(enabledIds);
      for (const s of filteredSkills) merged.add(s.id);
      setEnabledSkillIds([...merged]);
    }
  };

  const deleteMutation = useMutation({
    mutationFn: async (id: string) => {
      const response = await skillDelete({ path: { skill_id: id } });
      if (response.error) throw new Error("Failed to delete skill");
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "skillList" }] });
      toast({ title: "Skill deleted", type: "success" });
    },
    onError: () => {
      toast({ title: "Failed to delete skill", type: "error" });
    },
  });

  const handleDelete = async (e: React.MouseEvent, skill: SkillResource) => {
    e.stopPropagation();
    const confirmed = await confirm({
      title: "Delete Skill",
      message: `Are you sure you want to delete "${skill.name}"? This action cannot be undone.`,
      confirmLabel: "Delete",
      variant: "destructive",
    });
    if (confirmed) {
      deleteMutation.mutate(skill.id);
    }
  };

  const handleRowClick = (skill: SkillResource) => {
    toggleSkill(skill.id);
  };

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <Tooltip>
          <TooltipTrigger asChild>
            <PopoverTrigger asChild>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className="h-8 w-8 shrink-0 rounded-lg text-muted-foreground hover:text-foreground"
                disabled={disabled}
                aria-label="Skills"
              >
                <Brain className="h-4 w-4" />
              </Button>
            </PopoverTrigger>
          </TooltipTrigger>
          <TooltipContent side="top">Skills</TooltipContent>
        </Tooltip>

        <PopoverContent align="start" className="w-80 p-0">
          <div className="flex items-center justify-between border-b px-3 py-2">
            <span className="text-sm font-medium">Skills</span>
            <div className="flex items-center gap-1">
              {skills.length > 0 && (
                <button
                  type="button"
                  onClick={handleToggleAll}
                  className="inline-flex items-center gap-1 rounded-md px-1.5 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
                  aria-label={
                    allVisibleEnabled ? "Disable all visible skills" : "Enable all visible skills"
                  }
                >
                  {allVisibleEnabled ? (
                    <CheckSquare className="h-3.5 w-3.5" />
                  ) : (
                    <Square className="h-3.5 w-3.5" />
                  )}
                  {allVisibleEnabled ? "Disable all" : "Enable all"}
                </button>
              )}
              <Dropdown>
                <DropdownTrigger
                  aria-label="Add or import a skill"
                  variant="ghost"
                  showChevron={false}
                  className="h-7 w-7 p-0 text-muted-foreground hover:text-foreground"
                >
                  <Plus className="h-4 w-4" />
                </DropdownTrigger>
                <DropdownContent align="end">
                  <DropdownItem
                    onClick={() => {
                      setOpen(false);
                      setEditingSkill(null);
                      setCreateModalOpen(true);
                    }}
                  >
                    <Plus className="mr-2 h-4 w-4" />
                    Create new
                  </DropdownItem>
                  <DropdownItem
                    onClick={() => {
                      setOpen(false);
                      setImportTab("github");
                      setImportModalOpen(true);
                    }}
                  >
                    <Download className="mr-2 h-4 w-4" />
                    Import from GitHub
                  </DropdownItem>
                  <DropdownItem
                    onClick={() => {
                      setOpen(false);
                      setImportTab("filesystem");
                      setImportModalOpen(true);
                    }}
                  >
                    <Folder className="mr-2 h-4 w-4" />
                    Import from folder
                  </DropdownItem>
                </DropdownContent>
              </Dropdown>
            </div>
          </div>

          {skills.length > 0 && (
            <div className="border-b px-2 py-1.5">
              <div className="relative">
                <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
                <input
                  type="text"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                  placeholder="Search skills…"
                  aria-label="Search skills"
                  className="w-full rounded-md border bg-transparent py-1 pl-7 pr-2 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                />
              </div>
            </div>
          )}

          <div className="max-h-72 overflow-y-auto scrollbar-thin p-1">
            {isLoading ? (
              <div className="flex items-center justify-center py-6">
                <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
              </div>
            ) : skills.length === 0 ? (
              <div className="px-3 py-6 text-center">
                <Brain className="mx-auto mb-2 h-5 w-5 text-muted-foreground" />
                <p className="mb-3 text-xs text-muted-foreground">No skills yet</p>
                <div className="flex flex-col gap-1">
                  <button
                    type="button"
                    className="inline-flex items-center justify-center gap-1.5 rounded-md border px-2 py-1.5 text-xs hover:bg-accent"
                    onClick={() => {
                      setOpen(false);
                      setImportTab("github");
                      setImportModalOpen(true);
                    }}
                  >
                    <Download className="h-3.5 w-3.5" />
                    Import from GitHub
                  </button>
                  <button
                    type="button"
                    className="inline-flex items-center justify-center gap-1.5 rounded-md border px-2 py-1.5 text-xs hover:bg-accent"
                    onClick={() => {
                      setOpen(false);
                      setImportTab("filesystem");
                      setImportModalOpen(true);
                    }}
                  >
                    <Folder className="h-3.5 w-3.5" />
                    Import from folder
                  </button>
                  <button
                    type="button"
                    className="inline-flex items-center justify-center gap-1.5 rounded-md border px-2 py-1.5 text-xs hover:bg-accent"
                    onClick={() => {
                      setOpen(false);
                      setEditingSkill(null);
                      setCreateModalOpen(true);
                    }}
                  >
                    <Plus className="h-3.5 w-3.5" />
                    Create one
                  </button>
                </div>
              </div>
            ) : (
              <>
                {filteredSkills.length === 0 ? (
                  <div className="px-3 py-6 text-center text-xs text-muted-foreground">
                    No skills match &ldquo;{search}&rdquo;.
                  </div>
                ) : (
                  <ul className="space-y-0.5">
                    {filteredSkills.map((skill) => {
                      const isEnabled = enabledIds.includes(skill.id);
                      const modelInvocable = skill.disable_model_invocation !== true;
                      return (
                        <li key={skill.id} className="group relative">
                          <button
                            type="button"
                            className="flex w-full items-start gap-2 rounded-md px-2 py-1.5 pr-8 text-left text-sm hover:bg-accent/50 transition-colors text-foreground/80"
                            onClick={() => handleRowClick(skill)}
                            aria-pressed={isEnabled}
                            title={
                              isEnabled
                                ? "Disable this skill for the current session"
                                : "Enable this skill — the model may invoke it automatically when relevant"
                            }
                          >
                            <input
                              type="checkbox"
                              checked={isEnabled}
                              onChange={() => handleRowClick(skill)}
                              onClick={(e) => e.stopPropagation()}
                              className="mt-0.5 h-3.5 w-3.5 shrink-0"
                              aria-label={
                                isEnabled ? `Disable ${skill.name}` : `Enable ${skill.name}`
                              }
                            />
                            <div className="min-w-0 flex-1">
                              <div className="flex items-center gap-1.5">
                                <span className="truncate font-mono text-xs">{skill.name}</span>
                                <SkillOwnerBadge skill={skill} currentUserId={user?.id} />
                              </div>
                              <span className="text-xs text-muted-foreground line-clamp-1">
                                {skill.description}
                              </span>
                              {!modelInvocable && (
                                <span className="text-[10px] text-muted-foreground">
                                  Manual invocation only
                                </span>
                              )}
                            </div>
                          </button>
                          {skill.owner_type === "user" && user?.id === skill.owner_id && (
                            <button
                              type="button"
                              className="absolute right-1.5 top-1.5 hidden rounded p-0.5 text-muted-foreground hover:bg-destructive/10 hover:text-destructive group-hover:block"
                              onClick={(e) => handleDelete(e, skill)}
                              aria-label={`Delete skill: ${skill.name}`}
                            >
                              <Trash2 className="h-3 w-3" />
                            </button>
                          )}
                        </li>
                      );
                    })}
                  </ul>
                )}
                {hasMore && (
                  <p className="px-2 py-1.5 text-center text-xs text-muted-foreground">
                    Showing first 50 skills
                  </p>
                )}
              </>
            )}
          </div>
        </PopoverContent>
      </Popover>

      {user?.id && (
        <>
          <SkillFormModal
            open={createModalOpen}
            onClose={() => {
              setCreateModalOpen(false);
              setEditingSkill(null);
            }}
            editingSkill={editingSkill}
            ownerOverride={{ type: "user", user_id: user.id }}
            onSaved={() => {
              toast({
                title: editingSkill ? "Skill updated" : "Skill created",
                type: "success",
              });
            }}
          />
          <SkillImportModal
            open={importModalOpen}
            onClose={() => setImportModalOpen(false)}
            ownerOverride={{ type: "user", user_id: user.id }}
            initialTab={importTab}
          />
        </>
      )}
    </>
  );
}
