import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Brain, Check, Download, Folder, Loader2 } from "lucide-react";

import type { CreateSkillBody, SkillOwner } from "@/api/generated/types.gen";
import { skillCreate } from "@/api/generated/sdk.gen";
import { Button } from "@/components/Button/Button";
import { FormField } from "@/components/FormField/FormField";
import { Input } from "@/components/Input/Input";
import {
  Modal,
  ModalClose,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalTitle,
} from "@/components/Modal/Modal";
import { useToast } from "@/components/Toast/Toast";

import {
  parseGithubUrl,
  walkGithubForSkills,
  type DiscoveredSkill,
  type RateLimitSnapshot,
} from "./githubImport";
import { walkFilesForSkills } from "./filesystemImport";

import { formatApiError } from "@/utils/formatApiError";
type ImportTab = "github" | "filesystem";

export interface SkillImportModalProps {
  open: boolean;
  onClose: () => void;
  ownerOverride: SkillOwner;
  /** Which tab to open initially. Defaults to "github". */
  initialTab?: ImportTab;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
}

export function SkillImportModal({
  open,
  onClose,
  ownerOverride,
  initialTab = "github",
}: SkillImportModalProps) {
  const [tab, setTab] = useState<ImportTab>(initialTab);
  const [githubUrl, setGithubUrl] = useState("");
  const [isScanning, setIsScanning] = useState(false);
  const [scanProgress, setScanProgress] = useState<string>("");
  const [scanError, setScanError] = useState<string | null>(null);
  const [discovered, setDiscovered] = useState<DiscoveredSkill[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [rateLimit, setRateLimit] = useState<RateLimitSnapshot | null>(null);
  const [sourceUrl, setSourceUrl] = useState<string | null>(null);
  const [sourceRef, setSourceRef] = useState<string | null>(null);

  const queryClient = useQueryClient();
  const { toast } = useToast();
  const folderInputRef = useRef<HTMLInputElement | null>(null);

  /**
   * Per-skill import status, indexed by `skillDir`. Populated while the
   * import mutation runs so failed skills show an inline error badge
   * alongside successful ones, without closing the modal.
   */
  const [importStatus, setImportStatus] = useState<
    Record<string, { state: "ok" | "error"; message?: string }>
  >({});

  // Reset state when the modal opens.
  useEffect(() => {
    if (!open) return;
    setTab(initialTab);
    setGithubUrl("");
    setIsScanning(false);
    setScanProgress("");
    setScanError(null);
    setDiscovered([]);
    setSelected(new Set());
    setRateLimit(null);
    setSourceUrl(null);
    setSourceRef(null);
    setImportStatus({});
  }, [open, initialTab]);

  const handleGithubScan = async () => {
    setIsScanning(true);
    setScanError(null);
    setDiscovered([]);
    setSelected(new Set());
    try {
      const ref = parseGithubUrl(githubUrl);
      if (!ref) {
        setScanError("Enter a GitHub URL or owner/repo.");
        return;
      }
      const result = await walkGithubForSkills(ref, (p) => setScanProgress(p));
      setDiscovered(result.skills);
      setRateLimit(result.rateLimit);
      setSourceUrl(
        `https://github.com/${ref.owner}/${ref.repo}` +
          (ref.path ? `/tree/${ref.ref ?? "HEAD"}/${ref.path}` : "")
      );
      setSourceRef(ref.ref ?? null);
      // Pre-select everything valid by default.
      const valid = new Set<string>();
      for (const s of result.skills) {
        if (!s.error) valid.add(s.skillDir);
      }
      setSelected(valid);
    } catch (err) {
      setScanError(err instanceof Error ? err.message : formatApiError(err));
    } finally {
      setIsScanning(false);
      setScanProgress("");
    }
  };

  const handleFilesystemInput = useCallback(async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    setIsScanning(true);
    setScanError(null);
    setDiscovered([]);
    setSelected(new Set());
    setSourceUrl(null);
    setSourceRef(null);
    setRateLimit(null);
    try {
      const skills = await walkFilesForSkills(Array.from(files));
      setDiscovered(skills);
      const valid = new Set<string>();
      for (const s of skills) {
        if (!s.error) valid.add(s.skillDir);
      }
      setSelected(valid);
    } catch (err) {
      setScanError(err instanceof Error ? err.message : formatApiError(err));
    } finally {
      setIsScanning(false);
    }
  }, []);

  const toggle = (dir: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(dir)) next.delete(dir);
      else next.add(dir);
      return next;
    });
  };

  const importMutation = useMutation({
    mutationFn: async (skills: DiscoveredSkill[]) => {
      const results: { name: string; ok: boolean; error?: string }[] = [];
      // Reset per-skill status for this run so re-imports don't carry
      // stale rows forward.
      setImportStatus({});
      for (const s of skills) {
        const payload: CreateSkillBody = {
          owner: ownerOverride,
          name: s.name,
          description: s.description || s.name,
          files: s.files,
          user_invocable: s.frontmatter.user_invocable,
          disable_model_invocation: s.frontmatter.disable_model_invocation,
          allowed_tools: s.frontmatter.allowed_tools,
          argument_hint: s.frontmatter.argument_hint,
          source_url:
            tab === "github"
              ? `${sourceUrl ?? ""}${s.skillDir ? "/" + s.skillDir : ""}` || undefined
              : undefined,
          source_ref: tab === "github" ? (sourceRef ?? undefined) : undefined,
          frontmatter_extra:
            s.frontmatter.extra && Object.keys(s.frontmatter.extra).length > 0
              ? (s.frontmatter.extra as Record<string, unknown>)
              : undefined,
        };
        try {
          const response = await skillCreate({ body: payload });
          if (response.error) {
            const message =
              typeof response.error === "object" && response.error && "message" in response.error
                ? String((response.error as { message: unknown }).message)
                : "Unknown error";
            // 400 (validation: size limit, duplicate path, bad name) and
            // 409 (skill with same name exists) are both "keep going and
            // mark this one failed" cases.
            results.push({ name: s.name, ok: false, error: message });
            setImportStatus((prev) => ({
              ...prev,
              [s.skillDir]: { state: "error", message },
            }));
          } else {
            results.push({ name: s.name, ok: true });
            setImportStatus((prev) => ({
              ...prev,
              [s.skillDir]: { state: "ok" },
            }));
          }
        } catch (err) {
          const message = err instanceof Error ? err.message : formatApiError(err);
          results.push({ name: s.name, ok: false, error: message });
          setImportStatus((prev) => ({
            ...prev,
            [s.skillDir]: { state: "error", message },
          }));
        }
      }
      return results;
    },
    onSuccess: (results) => {
      const ok = results.filter((r) => r.ok).length;
      const failed = results.filter((r) => !r.ok);
      queryClient.invalidateQueries({ queryKey: [{ _id: "skillList" }] });
      if (failed.length === 0) {
        toast({ title: `Imported ${ok} skill${ok === 1 ? "" : "s"}`, type: "success" });
        onClose();
      } else {
        // Per-skill errors are shown inline in the discovered list; the
        // toast is just a summary and doesn't need to enumerate them.
        toast({
          title: `Imported ${ok}, ${failed.length} failed`,
          description: "See the list below for per-skill errors.",
          type: ok > 0 ? "warning" : "error",
        });
      }
    },
  });

  const handleImport = () => {
    const toImport = discovered.filter(
      (s) => selected.has(s.skillDir) && !s.error && importStatus[s.skillDir]?.state !== "ok"
    );
    if (toImport.length === 0) return;
    importMutation.mutate(toImport);
  };

  const selectedCount = useMemo(
    () => discovered.filter((s) => selected.has(s.skillDir)).length,
    [discovered, selected]
  );

  return (
    <Modal open={open} onClose={onClose} className="max-w-2xl">
      <ModalClose onClose={onClose} />
      <ModalHeader>
        <ModalTitle className="flex items-center gap-2">
          <Brain className="h-5 w-5" />
          Import skills
        </ModalTitle>
      </ModalHeader>

      <ModalContent>
        <div className="mb-4 flex gap-2 border-b">
          <button
            type="button"
            className={`flex items-center gap-1 border-b-2 px-3 py-2 text-sm ${
              tab === "github"
                ? "border-primary text-foreground"
                : "border-transparent text-muted-foreground"
            }`}
            onClick={() => setTab("github")}
          >
            <Download className="h-4 w-4" />
            GitHub
          </button>
          <button
            type="button"
            className={`flex items-center gap-1 border-b-2 px-3 py-2 text-sm ${
              tab === "filesystem"
                ? "border-primary text-foreground"
                : "border-transparent text-muted-foreground"
            }`}
            onClick={() => setTab("filesystem")}
          >
            <Folder className="h-4 w-4" />
            Folder
          </button>
        </div>

        {tab === "github" && (
          <div className="space-y-3">
            <FormField
              label="GitHub URL"
              htmlFor="skill-github-url"
              helpText="Any URL like https://github.com/anthropics/skills or owner/repo."
            >
              <div className="flex gap-2">
                <Input
                  id="skill-github-url"
                  value={githubUrl}
                  onChange={(e) => setGithubUrl(e.target.value)}
                  placeholder="https://github.com/anthropics/skills"
                  disabled={isScanning}
                />
                <Button
                  type="button"
                  onClick={handleGithubScan}
                  isLoading={isScanning}
                  disabled={!githubUrl.trim()}
                >
                  Scan
                </Button>
              </div>
            </FormField>
            {rateLimit?.remaining !== null && rateLimit?.remaining !== undefined && (
              <p className="text-xs text-muted-foreground">
                GitHub rate limit: {rateLimit.remaining}/{rateLimit.limit ?? "?"} requests remaining
                {rateLimit.resetAt && rateLimit.remaining < 10
                  ? ` — resets at ${rateLimit.resetAt.toLocaleTimeString()}`
                  : ""}
              </p>
            )}
          </div>
        )}

        {tab === "filesystem" && (
          <div className="space-y-3">
            <FormField
              label="Select a folder"
              htmlFor="skill-folder-input"
              helpText="Pick a directory containing one or more SKILL.md files. Each SKILL.md marks a skill; all files in its folder are bundled."
            >
              <div className="flex items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => folderInputRef.current?.click()}
                  isLoading={isScanning}
                  disabled={isScanning}
                >
                  <Folder className="mr-2 h-4 w-4" />
                  Browse folder…
                </Button>
                <span className="text-xs text-muted-foreground">
                  {discovered.length > 0
                    ? `${discovered.length} skill${discovered.length === 1 ? "" : "s"} found`
                    : "No folder selected yet"}
                </span>
              </div>
              <input
                id="skill-folder-input"
                ref={folderInputRef}
                type="file"
                // @ts-expect-error — non-standard but widely supported; lets the picker select folders.
                webkitdirectory=""
                directory=""
                multiple
                onChange={(e) => {
                  handleFilesystemInput(e.target.files);
                  // Reset so picking the same folder twice re-fires onChange.
                  e.target.value = "";
                }}
                className="hidden"
              />
            </FormField>
          </div>
        )}

        {isScanning && (
          <div className="mt-4 flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {scanProgress ? `Scanning ${scanProgress}…` : "Scanning…"}
          </div>
        )}

        {scanError && (
          <div className="mt-4 rounded-md bg-destructive/10 px-3 py-2 text-sm text-destructive">
            {scanError}
          </div>
        )}

        {discovered.length > 0 && (
          <div className="mt-4">
            <div className="mb-2 flex items-center justify-between">
              <p className="text-sm font-medium">
                Found {discovered.length} skill{discovered.length === 1 ? "" : "s"} —{" "}
                {selectedCount} selected
              </p>
              <button
                type="button"
                className="text-xs text-muted-foreground hover:text-foreground"
                onClick={() => {
                  if (selectedCount === discovered.filter((s) => !s.error).length) {
                    setSelected(new Set());
                  } else {
                    const all = new Set<string>();
                    for (const s of discovered) if (!s.error) all.add(s.skillDir);
                    setSelected(all);
                  }
                }}
              >
                {selectedCount > 0 ? "Clear" : "Select all"}
              </button>
            </div>
            <ul className="max-h-72 space-y-1 overflow-y-auto rounded-md border p-1 scrollbar-thin">
              {discovered.map((s) => {
                const isSelected = selected.has(s.skillDir);
                const status = importStatus[s.skillDir];
                const hasImportError = status?.state === "error";
                const hasImportOk = status?.state === "ok";
                const rowErrored = !!s.error || hasImportError;
                return (
                  <li
                    key={s.skillDir || s.name}
                    className={`flex items-start gap-2 rounded px-2 py-1.5 text-sm ${
                      rowErrored ? "bg-destructive/5" : "hover:bg-accent/40"
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={isSelected}
                      disabled={!!s.error || hasImportOk}
                      onChange={() => toggle(s.skillDir)}
                      className="mt-0.5"
                      aria-label={`Select ${s.name}`}
                    />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="font-mono text-xs">{s.name}</span>
                        <span className="text-xs text-muted-foreground">
                          · {s.files.length} file{s.files.length === 1 ? "" : "s"} ·{" "}
                          {formatBytes(s.total_bytes)}
                        </span>
                      </div>
                      {s.description && (
                        <p className="line-clamp-2 text-xs text-muted-foreground">
                          {s.description}
                        </p>
                      )}
                      {s.error && (
                        <p className="mt-0.5 flex items-center gap-1 text-xs text-destructive">
                          <AlertTriangle className="h-3 w-3" />
                          {s.error}
                        </p>
                      )}
                      {hasImportError && (
                        <p className="mt-0.5 flex items-center gap-1 text-xs text-destructive">
                          <AlertTriangle className="h-3 w-3" />
                          Import failed: {status.message}
                        </p>
                      )}
                    </div>
                    {hasImportOk ? (
                      <span
                        className="mt-0.5 inline-flex items-center gap-1 text-xs text-green-600 dark:text-green-400"
                        aria-label="Imported"
                      >
                        <Check className="h-4 w-4" />
                        Imported
                      </span>
                    ) : (
                      !s.error && isSelected && <Check className="mt-0.5 h-4 w-4 text-primary" />
                    )}
                  </li>
                );
              })}
            </ul>
          </div>
        )}
      </ModalContent>

      <ModalFooter>
        <Button type="button" variant="ghost" onClick={onClose}>
          Cancel
        </Button>
        <Button
          type="button"
          onClick={handleImport}
          disabled={selectedCount === 0 || importMutation.isPending}
          isLoading={importMutation.isPending}
        >
          Import {selectedCount > 0 ? `${selectedCount} ` : ""}
          skill{selectedCount === 1 ? "" : "s"}
        </Button>
      </ModalFooter>
    </Modal>
  );
}
