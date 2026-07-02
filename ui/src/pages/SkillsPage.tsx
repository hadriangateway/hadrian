import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type ColumnDef } from "@tanstack/react-table";
import { ChevronDown, Download, Folder, Plus } from "lucide-react";

import { skillDeleteMutation } from "@/api/generated/@tanstack/react-query.gen";
import type { SkillResource } from "@/api/generated/types.gen";
import { useAuth } from "@/auth";
import { Button } from "@/components/Button/Button";
import { Card, CardContent } from "@/components/Card/Card";
import { DataTable } from "@/components/DataTable/DataTable";
import {
  Dropdown,
  DropdownContent,
  DropdownItem,
  DropdownTrigger,
} from "@/components/Dropdown/Dropdown";
import { SkillFormModal } from "@/components/Admin";
import { SkillImportModal } from "@/components/SkillImportModal/SkillImportModal";
import { useToast } from "@/components/Toast/Toast";
import { useConfirm } from "@/components/ConfirmDialog/ConfirmDialog";
import { useUserSkills } from "@/hooks/useUserSkills";
import { createSkillColumns } from "@/pages/admin/skillColumns";
import { formatApiError } from "@/utils/formatApiError";

export default function SkillsPage() {
  const { user } = useAuth();
  const { toast } = useToast();
  const confirm = useConfirm();
  const queryClient = useQueryClient();
  const { skills, isLoading } = useUserSkills();
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [editingSkill, setEditingSkill] = useState<SkillResource | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  const [importTab, setImportTab] = useState<"github" | "filesystem">("github");

  const deleteSkillMutation = useMutation({
    ...skillDeleteMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "skillList" }] });
      toast({ title: "Skill deleted", type: "success" });
    },
    onError: (error) => {
      toast({
        title: "Failed to delete skill",
        description: formatApiError(error),
        type: "error",
      });
    },
  });

  const handleEdit = (skill: SkillResource) => {
    setEditingSkill(skill);
    setIsFormOpen(true);
  };

  const handleDelete = async (skill: SkillResource) => {
    const confirmed = await confirm({
      title: "Delete Skill",
      message: `Are you sure you want to delete "${skill.name}"? This action cannot be undone.`,
      confirmLabel: "Delete",
      variant: "destructive",
    });
    if (confirmed) {
      deleteSkillMutation.mutate({ path: { skill_id: skill.id } });
    }
  };

  const openImport = (tab: "github" | "filesystem") => {
    setImportTab(tab);
    setImportOpen(true);
  };

  const columns = createSkillColumns(handleEdit, handleDelete);

  // Without a user id the server derives the owner from the caller's auth scope.
  const ownerOverride = user?.id ? ({ type: "user", user_id: user.id } as const) : undefined;

  return (
    <div className="p-6 max-w-6xl mx-auto">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between mb-6">
        <div>
          <h1 className="text-2xl font-semibold">Skills</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Reusable, file-based skills the model can invoke across chats and projects
          </p>
        </div>
        <Dropdown>
          <DropdownTrigger asChild>
            <Button>
              <Plus className="mr-2 h-4 w-4" />
              New Skill
              <ChevronDown className="ml-1.5 h-4 w-4" />
            </Button>
          </DropdownTrigger>
          <DropdownContent align="end" className="w-52">
            <DropdownItem
              onClick={() => {
                setEditingSkill(null);
                setIsFormOpen(true);
              }}
            >
              <Plus className="mr-2 h-4 w-4" />
              Create new
            </DropdownItem>
            <DropdownItem onClick={() => openImport("github")}>
              <Download className="mr-2 h-4 w-4" />
              Import from GitHub
            </DropdownItem>
            <DropdownItem onClick={() => openImport("filesystem")}>
              <Folder className="mr-2 h-4 w-4" />
              Import from folder
            </DropdownItem>
          </DropdownContent>
        </Dropdown>
      </div>

      <Card>
        <CardContent className="p-4">
          <DataTable
            columns={columns as ColumnDef<SkillResource>[]}
            data={skills}
            isLoading={isLoading}
            emptyMessage="No skills yet. Create one or import from GitHub or a folder."
            searchColumn="name"
            searchPlaceholder="Search skills..."
          />
        </CardContent>
      </Card>

      <SkillFormModal
        open={isFormOpen}
        onClose={() => {
          setIsFormOpen(false);
          setEditingSkill(null);
        }}
        editingSkill={editingSkill}
        ownerOverride={ownerOverride}
        onSaved={() => {
          toast({
            title: editingSkill ? "Skill updated" : "Skill created",
            type: "success",
          });
        }}
      />
      <SkillImportModal
        open={importOpen}
        onClose={() => setImportOpen(false)}
        ownerOverride={ownerOverride}
        initialTab={importTab}
      />
    </div>
  );
}
