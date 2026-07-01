import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type ColumnDef } from "@tanstack/react-table";
import { Plus } from "lucide-react";

import { templateDeleteMutation } from "@/api/generated/@tanstack/react-query.gen";
import type { Template } from "@/api/generated/types.gen";
import { Button } from "@/components/Button/Button";
import { Card, CardContent } from "@/components/Card/Card";
import { DataTable } from "@/components/DataTable/DataTable";
import { PromptFormModal } from "@/components/PromptFormModal/PromptFormModal";
import { useToast } from "@/components/Toast/Toast";
import { useConfirm } from "@/components/ConfirmDialog/ConfirmDialog";
import { useUserTemplates } from "@/hooks/useUserPrompts";
import { createTemplateColumns } from "@/pages/admin/promptColumns";
import { formatApiError } from "@/utils/formatApiError";

export default function TemplatesPage() {
  const { toast } = useToast();
  const confirm = useConfirm();
  const queryClient = useQueryClient();
  const { templates, isLoading } = useUserTemplates();
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [editingTemplate, setEditingTemplate] = useState<Template | null>(null);

  const invalidateTemplates = () => {
    queryClient.invalidateQueries({ queryKey: [{ _id: "templateListByUser" }] });
    queryClient.invalidateQueries({ queryKey: [{ _id: "templateListByOrg" }] });
  };

  const deleteTemplateMutation = useMutation({
    ...templateDeleteMutation(),
    onSuccess: () => {
      invalidateTemplates();
      toast({ title: "Template deleted", type: "success" });
    },
    onError: (error) => {
      toast({
        title: "Failed to delete template",
        description: formatApiError(error),
        type: "error",
      });
    },
  });

  const handleEdit = (template: Template) => {
    setEditingTemplate(template);
    setIsModalOpen(true);
  };

  const handleDelete = async (template: Template) => {
    const confirmed = await confirm({
      title: "Delete Template",
      message: `Are you sure you want to delete "${template.name}"? This action cannot be undone.`,
      confirmLabel: "Delete",
      variant: "destructive",
    });
    if (confirmed) {
      deleteTemplateMutation.mutate({ path: { id: template.id } });
    }
  };

  const columns = createTemplateColumns(handleEdit, handleDelete);

  return (
    <div className="p-6 max-w-6xl mx-auto">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between mb-6">
        <div>
          <h1 className="text-2xl font-semibold">Templates</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Reusable system-prompt templates you can apply across chats and projects
          </p>
        </div>
        <Button
          onClick={() => {
            setEditingTemplate(null);
            setIsModalOpen(true);
          }}
        >
          <Plus className="mr-2 h-4 w-4" />
          New Template
        </Button>
      </div>

      <Card>
        <CardContent className="p-4">
          <DataTable
            columns={columns as ColumnDef<Template>[]}
            data={templates}
            isLoading={isLoading}
            emptyMessage="No templates yet. Create one to get started."
            searchColumn="name"
            searchPlaceholder="Search templates..."
          />
        </CardContent>
      </Card>

      <PromptFormModal
        open={isModalOpen}
        onClose={() => {
          setIsModalOpen(false);
          setEditingTemplate(null);
        }}
        editingPrompt={editingTemplate}
        onSaved={() => {
          invalidateTemplates();
          toast({
            title: editingTemplate ? "Template updated" : "Template created",
            type: "success",
          });
        }}
      />
    </div>
  );
}
