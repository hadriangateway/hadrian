import { useEffect, useMemo, useState } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { z } from "zod";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Brain, Trash2, ChevronDown, ChevronRight } from "lucide-react";

import type {
  CreateSkillBody,
  CreateSkillVersionBody,
  SkillFileInput,
  SkillFileManifest,
  SkillOwner,
  SkillResource,
} from "@/api/generated/types.gen";
import { skillCreate, skillCreateVersion, skillGet } from "@/api/generated/sdk.gen";
import { Button } from "@/components/Button/Button";
import { FormField } from "@/components/FormField/FormField";
import { Input } from "@/components/Input/Input";
import { Switch } from "@/components/Switch/Switch";
import {
  Modal,
  ModalClose,
  ModalHeader,
  ModalTitle,
  ModalContent,
  ModalFooter,
} from "@/components/Modal/Modal";

const SKILL_MAIN_FILE = "SKILL.md";

/**
 * Matches the server-side `validate_skill_name` (src/models/skill.rs):
 * 1..=64 chars, lowercase ASCII alphanumeric or hyphen, no leading or
 * trailing hyphen, no consecutive hyphens.
 */
const skillFormSchema = z.object({
  name: z
    .string()
    .min(1, "Name is required")
    .max(64, "Name must be 64 characters or less")
    .regex(/^[a-z0-9-]+$/, "Use lowercase letters, digits, and hyphens only")
    .refine(
      (s) => !s.startsWith("-") && !s.endsWith("-"),
      "Name must not start or end with a hyphen"
    )
    .refine((s) => !s.includes("--"), "Consecutive hyphens are not allowed"),
  description: z
    .string()
    .min(1, "Description is required")
    .max(1024, "Description must be 1024 characters or less"),
  body: z.string().min(1, "SKILL.md body is required"),
  argument_hint: z.string().max(255).optional(),
  allowed_tools_text: z.string().optional(),
  user_invocable: z.enum(["inherit", "true", "false"]),
  disable_model_invocation: z.enum(["inherit", "true", "false"]),
});

type SkillFormValues = z.infer<typeof skillFormSchema>;

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
}

function parseTriState(v: SkillFormValues["user_invocable"]): boolean | undefined {
  return v === "inherit" ? undefined : v === "true";
}

function triStateFromBool(b: boolean | null | undefined): SkillFormValues["user_invocable"] {
  if (b === null || b === undefined) return "inherit";
  return b ? "true" : "false";
}

/** Parse a space- or comma-separated list of allowed-tools strings. */
function parseAllowedToolsText(text: string | undefined): string[] | undefined {
  if (!text) return undefined;
  const trimmed = text.trim();
  if (!trimmed) return undefined;
  return trimmed
    .split(/[\s,]+/)
    .map((t) => t.trim())
    .filter(Boolean);
}

function formatAllowedTools(tools: string[] | null | undefined): string {
  if (!tools || tools.length === 0) return "";
  return tools.join(" ");
}

export interface SkillFormModalProps {
  open: boolean;
  onClose: () => void;
  editingSkill?: SkillResource | null;
  ownerOverride: SkillOwner;
  onSaved?: (skill: SkillResource) => void;
}

export function SkillFormModal({
  open,
  onClose,
  editingSkill,
  ownerOverride,
  onSaved,
}: SkillFormModalProps) {
  const queryClient = useQueryClient();
  const isEditing = !!editingSkill;
  const [showAdvanced, setShowAdvanced] = useState(false);

  /**
   * Bundled files (everything except SKILL.md). Initialized from
   * `editingSkill.files_manifest` and then, after loading the full skill,
   * from `editingSkill.files`. Users can delete individual entries.
   */
  const [bundledFiles, setBundledFiles] = useState<SkillFileInput[]>([]);
  const [bundledManifest, setBundledManifest] = useState<SkillFileManifest[]>([]);
  const [isLoadingFiles, setIsLoadingFiles] = useState(false);

  const form = useForm<SkillFormValues>({
    resolver: zodResolver(skillFormSchema),
    defaultValues: {
      name: "",
      description: "",
      body: "",
      argument_hint: "",
      allowed_tools_text: "",
      user_invocable: "inherit",
      disable_model_invocation: "inherit",
    },
  });

  // Reset form when the modal opens with a new target.
  useEffect(() => {
    if (!open) return;

    if (editingSkill) {
      const mainFile = editingSkill.files?.find((f) => f.path === SKILL_MAIN_FILE);
      form.reset({
        name: editingSkill.name,
        description: editingSkill.description,
        body: mainFile?.content ?? "",
        argument_hint: editingSkill.argument_hint ?? "",
        allowed_tools_text: formatAllowedTools(editingSkill.allowed_tools),
        user_invocable: triStateFromBool(editingSkill.user_invocable),
        disable_model_invocation: triStateFromBool(editingSkill.disable_model_invocation),
      });

      // Files may not be loaded yet if the skill came from a list endpoint.
      setBundledManifest(
        (editingSkill.files_manifest ?? []).filter((f) => f.path !== SKILL_MAIN_FILE)
      );
      setBundledFiles(
        (editingSkill.files ?? [])
          .filter((f) => f.path !== SKILL_MAIN_FILE)
          .map((f) => ({
            path: f.path,
            content: f.content,
            content_type: f.content_type,
          }))
      );

      // If body isn't populated (list response only), fetch full skill.
      if (!mainFile) {
        setIsLoadingFiles(true);
        skillGet({ path: { skill_id: editingSkill.id } })
          .then((res) => {
            if (res.data) {
              const main = res.data.files?.find((f) => f.path === SKILL_MAIN_FILE);
              if (main) {
                form.setValue("body", main.content, { shouldDirty: false });
              }
              setBundledFiles(
                (res.data.files ?? [])
                  .filter((f) => f.path !== SKILL_MAIN_FILE)
                  .map((f) => ({
                    path: f.path,
                    content: f.content,
                    content_type: f.content_type,
                  }))
              );
            }
          })
          .finally(() => setIsLoadingFiles(false));
      }
    } else {
      form.reset({
        name: "",
        description: "",
        body: "",
        argument_hint: "",
        allowed_tools_text: "",
        user_invocable: "inherit",
        disable_model_invocation: "inherit",
      });
      setBundledFiles([]);
      setBundledManifest([]);
    }
    setShowAdvanced(false);
  }, [open, editingSkill, form]);

  const invalidateSkillQueries = () => {
    queryClient.invalidateQueries({ queryKey: [{ _id: "skillList" }] });
    queryClient.invalidateQueries({ queryKey: [{ _id: "skillGet" }] });
  };

  const createMutation = useMutation({
    mutationFn: async (data: CreateSkillBody) => {
      const response = await skillCreate({ body: data });
      if (response.error) {
        throw new Error(
          typeof response.error === "object" && "message" in response.error
            ? String(response.error.message)
            : "Failed to create skill"
        );
      }
      return response.data as SkillResource;
    },
    onSuccess: (skill) => {
      invalidateSkillQueries();
      onSaved?.(skill);
      onClose();
    },
  });

  // Editing an existing skill publishes a new version and points the
  // default pointer at it — there is no in-place update endpoint anymore.
  const updateMutation = useMutation({
    mutationFn: async ({ id, data }: { id: string; data: CreateSkillVersionBody }) => {
      const response = await skillCreateVersion({ path: { skill_id: id }, body: data });
      if (response.error) {
        throw new Error(
          typeof response.error === "object" && "message" in response.error
            ? String(response.error.message)
            : "Failed to save skill"
        );
      }
      // `skillCreateVersion` returns a version, not the skill projection.
      // Re-fetch the skill so `onSaved` sees the new default_version/files
      // rather than the stale pre-edit object.
      const refreshed = await skillGet({ path: { skill_id: id } });
      return (refreshed.data ?? editingSkill) as SkillResource;
    },
    onSuccess: (skill) => {
      invalidateSkillQueries();
      onSaved?.(skill);
      onClose();
    },
  });

  const isLoading = createMutation.isPending || updateMutation.isPending || isLoadingFiles;
  const error = createMutation.error || updateMutation.error;

  const bodySize = useMemo(() => form.watch("body").length, [form]);

  const handleRemoveBundledFile = (path: string) => {
    setBundledFiles((prev) => prev.filter((f) => f.path !== path));
    setBundledManifest((prev) => prev.filter((f) => f.path !== path));
  };

  const handleSubmit = form.handleSubmit((data) => {
    const files: SkillFileInput[] = [
      {
        path: SKILL_MAIN_FILE,
        content: data.body,
        content_type: "text/markdown",
      },
      ...bundledFiles,
    ];

    const allowedTools = parseAllowedToolsText(data.allowed_tools_text);
    const argumentHint = data.argument_hint?.trim() || undefined;

    if (isEditing && editingSkill) {
      // Saving an edit publishes a new default version. The skill's name is
      // immutable, so it isn't part of the version body.
      const payload: CreateSkillVersionBody = {
        description: data.description,
        files,
        default: true,
        user_invocable: parseTriState(data.user_invocable),
        disable_model_invocation: parseTriState(data.disable_model_invocation),
        allowed_tools: allowedTools,
        argument_hint: argumentHint,
      };
      updateMutation.mutate({ id: editingSkill.id, data: payload });
    } else {
      const payload: CreateSkillBody = {
        owner: ownerOverride,
        name: data.name,
        description: data.description,
        files,
        user_invocable: parseTriState(data.user_invocable),
        disable_model_invocation: parseTriState(data.disable_model_invocation),
        allowed_tools: allowedTools,
        argument_hint: argumentHint,
      };
      createMutation.mutate(payload);
    }
  });

  const handleClose = () => {
    if (!isLoading) {
      form.reset();
      createMutation.reset();
      updateMutation.reset();
      onClose();
    }
  };

  // Bundled file paths to render — prefer full file entries (after load)
  // over the lighter manifest.
  const bundledRows =
    bundledFiles.length > 0
      ? bundledFiles.map((f) => ({ path: f.path, byte_size: f.content.length }))
      : bundledManifest.map((f) => ({ path: f.path, byte_size: f.byte_size }));

  return (
    <Modal open={open} onClose={handleClose} className="max-w-2xl">
      <ModalClose onClose={handleClose} />
      <form onSubmit={handleSubmit}>
        <ModalHeader>
          <ModalTitle className="flex items-center gap-2">
            <Brain className="h-5 w-5" />
            {isEditing ? "Edit Skill" : "New Skill"}
          </ModalTitle>
        </ModalHeader>

        <ModalContent>
          <div className="space-y-4">
            {error && (
              <div className="rounded-md bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error.message}
              </div>
            )}

            {isEditing && (
              <div className="rounded-md bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
                Saving publishes a new version of this skill and sets it as the default. Earlier
                versions are kept.
              </div>
            )}

            <FormField
              label="Name"
              htmlFor="skill-name"
              required
              helpText={
                isEditing
                  ? "A skill's name is fixed once created."
                  : "Lowercase letters, digits, and hyphens. 1–64 characters. Invalid characters are stripped automatically."
              }
              error={form.formState.errors.name?.message}
            >
              <Input
                id="skill-name"
                {...form.register("name", {
                  onChange: (e) => {
                    // Force lowercase and drop any char the server would reject.
                    const normalized = e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "");
                    if (e.target.value !== normalized) {
                      form.setValue("name", normalized, {
                        shouldValidate: true,
                        shouldDirty: true,
                      });
                    }
                  },
                })}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
                readOnly={isEditing}
                placeholder="e.g., code-review"
              />
            </FormField>

            <FormField
              label="Description"
              htmlFor="skill-description"
              required
              helpText="Short summary used by the model to decide when to invoke this skill."
              error={form.formState.errors.description?.message}
            >
              <Input
                id="skill-description"
                {...form.register("description")}
                placeholder="e.g., Reviews code for best practices and potential issues"
              />
            </FormField>

            <FormField
              label="SKILL.md"
              htmlFor="skill-body"
              required
              helpText={`Instructions loaded when the skill is invoked. ${formatBytes(bodySize)}.`}
              error={form.formState.errors.body?.message}
            >
              <textarea
                id="skill-body"
                {...form.register("body")}
                placeholder="# My Skill&#10;&#10;When invoked, follow these steps..."
                className="w-full min-h-[200px] rounded-md border bg-background px-3 py-2 font-mono text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 resize-y"
              />
            </FormField>

            {bundledRows.length > 0 && (
              <div className="rounded-md border p-3">
                <div className="mb-2 text-sm font-medium">Bundled files</div>
                <p className="mb-2 text-xs text-muted-foreground">
                  Additional files imported with this skill. Add or edit these via the import flow.
                </p>
                <ul className="space-y-1">
                  {bundledRows.map((f) => (
                    <li
                      key={f.path}
                      className="flex items-center justify-between gap-2 rounded border bg-muted/30 px-2 py-1 text-sm"
                    >
                      <span className="font-mono truncate">{f.path}</span>
                      <span className="flex items-center gap-3">
                        <span className="text-muted-foreground text-xs">
                          {formatBytes(f.byte_size)}
                        </span>
                        <button
                          type="button"
                          aria-label={`Remove ${f.path}`}
                          onClick={() => handleRemoveBundledFile(f.path)}
                          className="text-muted-foreground hover:text-destructive"
                        >
                          <Trash2 className="h-4 w-4" />
                        </button>
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            )}

            <div>
              <button
                type="button"
                className="flex items-center gap-1 text-sm font-medium"
                onClick={() => setShowAdvanced((v) => !v)}
                aria-expanded={showAdvanced}
              >
                {showAdvanced ? (
                  <ChevronDown className="h-4 w-4" />
                ) : (
                  <ChevronRight className="h-4 w-4" />
                )}
                Advanced frontmatter
              </button>

              {showAdvanced && (
                <div className="mt-3 space-y-4 rounded-md border p-3">
                  <FormField
                    label="Argument hint"
                    htmlFor="skill-argument-hint"
                    helpText="Optional hint shown during slash-command autocomplete, e.g. [issue-number]."
                    error={form.formState.errors.argument_hint?.message}
                  >
                    <Input
                      id="skill-argument-hint"
                      {...form.register("argument_hint")}
                      placeholder="[filename]"
                    />
                  </FormField>

                  <FormField
                    label="Allowed tools"
                    htmlFor="skill-allowed-tools"
                    helpText="Space-separated list of pre-approved tools. Example: Bash(git:*) Read Grep"
                  >
                    <Input
                      id="skill-allowed-tools"
                      {...form.register("allowed_tools_text")}
                      placeholder="Bash(git:*) Read"
                    />
                  </FormField>

                  <div className="space-y-2">
                    <div className="text-sm font-medium">User invocable</div>
                    <div className="flex items-center gap-3 text-sm">
                      <label className="flex items-center gap-1">
                        <input type="radio" value="inherit" {...form.register("user_invocable")} />
                        Default (visible)
                      </label>
                      <label className="flex items-center gap-1">
                        <input type="radio" value="true" {...form.register("user_invocable")} />
                        Always show
                      </label>
                      <label className="flex items-center gap-1">
                        <input type="radio" value="false" {...form.register("user_invocable")} />
                        Hide from menu
                      </label>
                    </div>
                  </div>

                  <div className="space-y-2">
                    <div className="text-sm font-medium">Model invocation</div>
                    <div className="flex items-center gap-3 text-sm">
                      <label className="flex items-center gap-1">
                        <input
                          type="radio"
                          value="inherit"
                          {...form.register("disable_model_invocation")}
                        />
                        Default (allowed)
                      </label>
                      <label className="flex items-center gap-1">
                        <input
                          type="radio"
                          value="false"
                          {...form.register("disable_model_invocation")}
                        />
                        Allow
                      </label>
                      <label className="flex items-center gap-1">
                        <input
                          type="radio"
                          value="true"
                          {...form.register("disable_model_invocation")}
                        />
                        Block automatic
                      </label>
                    </div>
                  </div>

                  {isEditing && editingSkill?.source_url && (
                    <FormField
                      label="Imported from"
                      htmlFor="skill-source-url"
                      helpText="Read-only. Set by the import flow."
                    >
                      <Input id="skill-source-url" value={editingSkill.source_url ?? ""} readOnly />
                    </FormField>
                  )}
                </div>
              )}
            </div>
          </div>
        </ModalContent>

        <ModalFooter>
          <Button type="button" variant="ghost" onClick={handleClose} disabled={isLoading}>
            Cancel
          </Button>
          <Button type="submit" isLoading={isLoading}>
            {isEditing ? "Save Changes" : "Create Skill"}
          </Button>
        </ModalFooter>
      </form>
    </Modal>
  );
}

// Re-export Switch to keep the import-graph simple in tests that mock this file.
export { Switch };
