import { zodResolver } from "@hookform/resolvers/zod";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useParams, Link } from "react-router-dom";
import {
  ArrowLeft,
  Users,
  Key,
  Server,
  DollarSign,
  BarChart3,
  Pencil,
  ClipboardPenLine,
  Brain,
} from "lucide-react";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { z } from "zod";

import {
  projectGetOptions,
  projectUpdateMutation,
  teamListOptions,
} from "@/api/generated/@tanstack/react-query.gen";
import { Button } from "@/components/Button/Button";
import { Badge } from "@/components/Badge/Badge";
import { FormField } from "@/components/FormField/FormField";
import { Input } from "@/components/Input/Input";
import { Modal, ModalHeader, ModalContent, ModalFooter } from "@/components/Modal/Modal";
import { Skeleton } from "@/components/Skeleton/Skeleton";
import { useToast } from "@/components/Toast/Toast";
import { TabNavigation, TeamSelect, type Tab } from "@/components/Admin";
import { formatDateTime } from "@/utils/formatters";

import { MembersTab } from "./MembersTab";
import { ApiKeysTab } from "./ApiKeysTab";
import { ProvidersTab } from "./ProvidersTab";
import { PricingTab } from "./PricingTab";
import { TemplatesTab } from "./TemplatesTab";
import { SkillsTab } from "./SkillsTab";
import { UsageTab } from "./UsageTab";

import { formatApiError } from "@/utils/formatApiError";
type TabId = "members" | "api-keys" | "providers" | "pricing" | "templates" | "skills" | "usage";

const tabs: Tab<TabId>[] = [
  { id: "members", label: "Members", icon: <Users className="h-4 w-4" /> },
  { id: "api-keys", label: "API Keys", icon: <Key className="h-4 w-4" /> },
  { id: "providers", label: "Providers", icon: <Server className="h-4 w-4" /> },
  { id: "pricing", label: "Pricing", icon: <DollarSign className="h-4 w-4" /> },
  { id: "templates", label: "Templates", icon: <ClipboardPenLine className="h-4 w-4" /> },
  { id: "skills", label: "Skills", icon: <Brain className="h-4 w-4" /> },
  { id: "usage", label: "Usage", icon: <BarChart3 className="h-4 w-4" /> },
];

const editProjectSchema = z.object({
  name: z.string().min(1, "Name is required"),
  team_id: z.string().nullable().optional(),
});

type EditProjectForm = z.infer<typeof editProjectSchema>;

export default function ProjectDetailPage() {
  const { orgSlug, projectSlug } = useParams<{ orgSlug: string; projectSlug: string }>();
  const { toast } = useToast();
  const queryClient = useQueryClient();

  const [activeTab, setActiveTab] = useState<TabId>("members");
  const [isEditModalOpen, setIsEditModalOpen] = useState(false);

  const editForm = useForm<EditProjectForm>({
    resolver: zodResolver(editProjectSchema),
    defaultValues: { name: "", team_id: null },
  });

  const {
    data: project,
    isLoading,
    error,
  } = useQuery(projectGetOptions({ path: { org_slug: orgSlug!, project_slug: projectSlug! } }));

  const { data: teams } = useQuery({
    ...teamListOptions({ path: { org_slug: orgSlug || "" } }),
    enabled: !!orgSlug && (!!project?.team_id || isEditModalOpen),
  });

  const updateMutation = useMutation({
    ...projectUpdateMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "projectGet" }] });
      setIsEditModalOpen(false);
      toast({ title: "Project updated", type: "success" });
    },
    onError: (error) => {
      toast({
        title: "Failed to update project",
        description: formatApiError(error),
        type: "error",
      });
    },
  });

  const onEditSubmit = (data: EditProjectForm) => {
    updateMutation.mutate({
      path: { org_slug: orgSlug!, project_slug: projectSlug! },
      body: { name: data.name, team_id: data.team_id },
    });
  };

  if (isLoading) {
    return (
      <div className="p-6 max-w-6xl mx-auto space-y-6">
        <Skeleton className="h-4 w-32" />
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-32 w-full" />
      </div>
    );
  }

  if (error || !project) {
    return (
      <div className="p-6 max-w-6xl mx-auto">
        <div className="text-center py-12 text-destructive">
          Project not found or failed to load.
          <br />
          <Link to="/projects" className="mt-4 inline-flex items-center gap-1 text-primary text-sm">
            <ArrowLeft className="h-4 w-4" />
            Back to Projects
          </Link>
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 max-w-6xl mx-auto space-y-6">
      {/* Breadcrumb */}
      <Link
        to="/projects"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-4 w-4" />
        Back to Projects
      </Link>

      {/* Header */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <div className="flex items-center gap-3">
            <h1 className="text-2xl font-semibold">{project.name}</h1>
            <code className="rounded bg-muted px-2 py-1 text-sm">{project.slug}</code>
          </div>
          <p className="text-sm text-muted-foreground mt-1">
            Created {formatDateTime(project.created_at)}
          </p>
        </div>
        <Button
          variant="outline"
          onClick={() => {
            editForm.reset({ name: project.name, team_id: project.team_id ?? null });
            setIsEditModalOpen(true);
          }}
        >
          <Pencil className="mr-2 h-4 w-4" />
          Edit
        </Button>
      </div>

      {/* Team badge */}
      {project.team_id && teams?.data && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <span>Team:</span>
          <Badge variant="secondary">
            {teams.data.find((t) => t.id === project.team_id)?.name ?? "Unknown Team"}
          </Badge>
        </div>
      )}

      {/* Tab navigation */}
      <TabNavigation tabs={tabs} activeTab={activeTab} onTabChange={setActiveTab} />

      {/* Tab content */}
      {activeTab === "members" && <MembersTab orgSlug={orgSlug!} projectSlug={projectSlug!} />}
      {activeTab === "api-keys" && <ApiKeysTab orgSlug={orgSlug!} projectSlug={projectSlug!} />}
      {activeTab === "providers" && (
        <ProvidersTab orgSlug={orgSlug!} projectSlug={projectSlug!} projectId={project.id} />
      )}
      {activeTab === "pricing" && <PricingTab orgSlug={orgSlug!} projectSlug={projectSlug!} />}
      {activeTab === "templates" && (
        <TemplatesTab orgSlug={orgSlug!} projectSlug={projectSlug!} projectId={project.id} />
      )}
      {activeTab === "skills" && <SkillsTab projectId={project.id} />}
      {activeTab === "usage" && (
        <UsageTab orgSlug={orgSlug!} projectSlug={projectSlug!} projectId={project.id} />
      )}

      {/* Edit Modal */}
      <Modal open={isEditModalOpen} onClose={() => setIsEditModalOpen(false)}>
        <form onSubmit={editForm.handleSubmit(onEditSubmit)}>
          <ModalHeader>Edit Project</ModalHeader>
          <ModalContent>
            <div className="space-y-4">
              <FormField
                label="Name"
                htmlFor="name"
                required
                error={editForm.formState.errors.name?.message}
              >
                <Input id="name" {...editForm.register("name")} placeholder="Project Name" />
              </FormField>
              {teams?.data && (
                <TeamSelect
                  teams={teams.data}
                  value={editForm.watch("team_id") ?? null}
                  onChange={(teamId) => editForm.setValue("team_id", teamId)}
                  label="Team"
                  nonePlaceholder="None (Organization-level)"
                />
              )}
            </div>
          </ModalContent>
          <ModalFooter>
            <Button type="button" variant="ghost" onClick={() => setIsEditModalOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" isLoading={updateMutation.isPending}>
              Save
            </Button>
          </ModalFooter>
        </form>
      </Modal>
    </div>
  );
}
