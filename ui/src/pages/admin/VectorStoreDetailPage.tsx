import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { createColumnHelper, type ColumnDef } from "@tanstack/react-table";
import { useParams, useNavigate } from "react-router-dom";
import {
  ArrowLeft,
  FileText,
  Upload,
  Trash2,
  Database,
  RefreshCw,
  AlertCircle,
  CheckCircle2,
  Clock,
  XCircle,
  Eye,
} from "lucide-react";
import { useState, useRef, useCallback } from "react";

import {
  vectorStoreGetOptions,
  vectorStoreFileListOptions,
  vectorStoreFileDeleteMutation,
  vectorStoreFileCreateMutation,
  fileUploadMutation,
  fileListOptions,
} from "@/api/generated/@tanstack/react-query.gen";
import type {
  VectorStoreFile,
  VectorStoreFileStatus,
  File as ApiFile,
} from "@/api/generated/types.gen";
import { Badge } from "@/components/Badge/Badge";
import { Button } from "@/components/Button/Button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/Card/Card";
import { CodeBadge } from "@/components/CodeBadge/CodeBadge";
import { DataTable } from "@/components/DataTable/DataTable";
import { Modal, ModalHeader, ModalContent, ModalFooter } from "@/components/Modal/Modal";
import { Skeleton } from "@/components/Skeleton/Skeleton";
import { useToast } from "@/components/Toast/Toast";
import { useConfirm } from "@/components/ConfirmDialog/ConfirmDialog";
import { DetailPageHeader, StatCard, StatValue, EMBEDDING_MODELS } from "@/components/Admin";
import { ChunkViewer, SearchPreview } from "@/components/VectorStores";
import { formatDateTime, formatBytes } from "@/utils/formatters";

import { formatApiError } from "@/utils/formatApiError";
const fileColumnHelper = createColumnHelper<VectorStoreFile>();

/** Status badge for file processing status */
function FileStatusBadge({ status }: { status: VectorStoreFileStatus }) {
  const config: Record<
    VectorStoreFileStatus,
    {
      variant: "default" | "secondary" | "destructive" | "outline";
      icon: React.ReactNode;
      label: string;
    }
  > = {
    completed: {
      variant: "default",
      icon: <CheckCircle2 className="h-3 w-3" />,
      label: "Completed",
    },
    in_progress: { variant: "secondary", icon: <Clock className="h-3 w-3" />, label: "Processing" },
    failed: { variant: "destructive", icon: <AlertCircle className="h-3 w-3" />, label: "Failed" },
    cancelled: { variant: "outline", icon: <XCircle className="h-3 w-3" />, label: "Cancelled" },
  };

  const { variant, icon, label } = config[status] || config.failed;

  return (
    <Badge variant={variant} className="gap-1">
      {icon}
      {label}
    </Badge>
  );
}

/** Supported file extensions for vector store uploads — must match backend is_supported_file_type() */
const VALID_FILE_EXTENSIONS = [
  // Plain text
  ".txt",
  ".md",
  ".markdown",
  ".json",
  ".csv",
  ".xml",
  ".html",
  ".htm",
  // Code files
  ".rs",
  ".py",
  ".js",
  ".ts",
  ".jsx",
  ".tsx",
  ".go",
  ".java",
  ".c",
  ".cpp",
  ".h",
  ".hpp",
  ".cs",
  ".rb",
  ".php",
  ".swift",
  ".kt",
  ".scala",
  ".r",
  ".sql",
  ".sh",
  ".bash",
  ".zsh",
  ".ps1",
  ".yaml",
  ".yml",
  ".toml",
  ".ini",
  ".cfg",
  ".conf",
  ".properties",
  ".env",
  ".dockerfile",
  ".makefile",
  // Documentation
  ".rst",
  ".adoc",
  ".tex",
  ".latex",
  // Rich documents (extracted via xberg)
  ".pdf",
  ".docx",
  ".doc",
  ".xlsx",
  ".xls",
  ".pptx",
  ".ppt",
  ".odt",
  ".ods",
  ".odp",
  ".rtf",
  ".epub",
  // Images (OCR extraction via xberg + Tesseract)
  ".png",
  ".jpg",
  ".jpeg",
  ".tiff",
  ".tif",
  ".bmp",
  ".webp",
  ".gif",
];

/** Modal for adding files to vector store */
function AddFileModal({
  open,
  onClose,
  vectorStoreId,
  ownerId,
  ownerType,
}: {
  open: boolean;
  onClose: () => void;
  vectorStoreId: string;
  ownerId: string;
  ownerType: string;
}) {
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [selectedFiles, setSelectedFiles] = useState<File[]>([]);
  const [isUploading, setIsUploading] = useState(false);
  const [uploadProgress, setUploadProgress] = useState<{ current: number; total: number } | null>(
    null
  );
  const [isDragging, setIsDragging] = useState(false);

  // Fetch existing files for this owner
  const { data: existingFiles, isLoading: filesLoading } = useQuery({
    ...fileListOptions({
      query: {
        owner_type: ownerType,
        owner_id: ownerId,
        purpose: "assistants",
      },
    }),
    enabled: open,
  });

  const uploadMutation = useMutation({
    ...fileUploadMutation(),
    onError: (error) => {
      toast({ title: "Failed to upload file", description: formatApiError(error), type: "error" });
    },
  });

  const addFileMutation = useMutation({
    ...vectorStoreFileCreateMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "vectorStoreFileList" }] });
      queryClient.invalidateQueries({ queryKey: [{ _id: "vectorStoreGet" }] });
      toast({ title: "File added to knowledge base", type: "success" });
      handleClose();
    },
    onError: (error) => {
      toast({ title: "Failed to add file", description: formatApiError(error), type: "error" });
    },
  });

  const handleClose = () => {
    setSelectedFiles([]);
    setIsUploading(false);
    setUploadProgress(null);
    onClose();
  };

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files;
    if (files && files.length > 0) {
      setSelectedFiles((prev) => [...prev, ...Array.from(files)]);
    }
    // Reset input so the same file can be selected again
    if (fileInputRef.current) fileInputRef.current.value = "";
  };

  const removeSelectedFile = (index: number) => {
    setSelectedFiles((prev) => prev.filter((_, i) => i !== index));
  };

  const handleDragEnter = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(true);
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // Only set dragging to false if we're leaving the drop zone entirely
    // Check if the related target is still within the drop zone
    if (!e.currentTarget.contains(e.relatedTarget as Node)) {
      setIsDragging(false);
    }
  }, []);

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
  }, []);

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      setIsDragging(false);

      const droppedFiles = Array.from(e.dataTransfer.files);
      const validFiles: File[] = [];
      const invalidFiles: string[] = [];

      for (const file of droppedFiles) {
        const fileExtension = "." + file.name.split(".").pop()?.toLowerCase();
        if (VALID_FILE_EXTENSIONS.includes(fileExtension)) {
          validFiles.push(file);
        } else {
          invalidFiles.push(file.name);
        }
      }

      if (validFiles.length > 0) {
        setSelectedFiles((prev) => [...prev, ...validFiles]);
      }

      if (invalidFiles.length > 0) {
        toast({
          title: "Unsupported file type(s)",
          description: `Skipped: ${invalidFiles.join(", ")}`,
          type: "error",
        });
      }
    },
    [toast]
  );

  const handleUploadAndAdd = async () => {
    if (selectedFiles.length === 0) return;

    setIsUploading(true);
    setUploadProgress({ current: 0, total: selectedFiles.length });

    let successCount = 0;
    let failCount = 0;

    for (let i = 0; i < selectedFiles.length; i++) {
      const file = selectedFiles[i];
      setUploadProgress({ current: i + 1, total: selectedFiles.length });

      try {
        // Upload the file - pass plain object, SDK will convert to FormData
        const uploadedFile = await uploadMutation.mutateAsync({
          body: {
            file: file,
            purpose: "assistants",
            owner_type: ownerType,
            owner_id: ownerId,
          },
        });

        // Add file to vector store
        await addFileMutation.mutateAsync({
          path: { vector_store_id: vectorStoreId },
          body: { file_id: uploadedFile.id },
        });
        successCount++;
      } catch {
        failCount++;
        // Continue with other files
      }
    }

    setIsUploading(false);
    setUploadProgress(null);

    if (successCount > 0) {
      queryClient.invalidateQueries({ queryKey: [{ _id: "vectorStoreFileList" }] });
      queryClient.invalidateQueries({ queryKey: [{ _id: "vectorStoreGet" }] });
      toast({
        title: `${successCount} file${successCount > 1 ? "s" : ""} added`,
        description: failCount > 0 ? `${failCount} failed` : undefined,
        type: failCount > 0 ? "info" : "success",
      });
    }

    handleClose();
  };

  const handleAddExistingFile = async (fileId: string) => {
    addFileMutation.mutate({
      path: { vector_store_id: vectorStoreId },
      body: { file_id: fileId },
    });
  };

  return (
    <Modal open={open} onClose={handleClose}>
      <ModalHeader>Add Files to Knowledge Base</ModalHeader>
      <ModalContent>
        <div className="space-y-6">
          {/* Upload new file section */}
          <div>
            <h3 className="text-sm font-medium mb-2">Upload New Files</h3>
            <div
              className={`border-2 border-dashed rounded-lg p-6 text-center transition-colors ${
                isDragging
                  ? "border-primary bg-primary/5"
                  : "border-muted hover:border-muted-foreground/50"
              }`}
              onDragEnter={handleDragEnter}
              onDragLeave={handleDragLeave}
              onDragOver={handleDragOver}
              onDrop={handleDrop}
            >
              <input
                ref={fileInputRef}
                type="file"
                multiple
                className="hidden"
                onChange={handleFileSelect}
                accept={VALID_FILE_EXTENSIONS.join(",")}
                aria-label="Upload files"
              />
              {selectedFiles.length > 0 ? (
                <div className="space-y-3">
                  <div className="max-h-32 overflow-y-auto space-y-1">
                    {selectedFiles.map((file, index) => (
                      <div
                        key={`${file.name}-${index}`}
                        className="flex items-center justify-between gap-2 px-2 py-1 bg-muted/50 rounded text-sm"
                      >
                        <div className="flex items-center gap-2 min-w-0">
                          <FileText className="h-4 w-4 text-muted-foreground shrink-0" />
                          <span className="truncate">{file.name}</span>
                          <span className="text-xs text-muted-foreground shrink-0">
                            {formatBytes(file.size)}
                          </span>
                        </div>
                        <Button
                          size="sm"
                          variant="ghost"
                          className="h-6 w-6 p-0 shrink-0"
                          onClick={() => removeSelectedFile(index)}
                          disabled={isUploading}
                        >
                          <XCircle className="h-4 w-4" />
                        </Button>
                      </div>
                    ))}
                  </div>
                  <div className="flex justify-center gap-2 pt-2 border-t">
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => fileInputRef.current?.click()}
                      disabled={isUploading}
                    >
                      Add More
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => setSelectedFiles([])}
                      disabled={isUploading}
                    >
                      Clear All
                    </Button>
                    <Button size="sm" onClick={handleUploadAndAdd} isLoading={isUploading}>
                      <Upload className="h-4 w-4 mr-2" />
                      {isUploading && uploadProgress
                        ? `Uploading ${uploadProgress.current}/${uploadProgress.total}...`
                        : `Upload ${selectedFiles.length} File${selectedFiles.length > 1 ? "s" : ""}`}
                    </Button>
                  </div>
                </div>
              ) : isDragging ? (
                <div className="space-y-2">
                  <Upload className="h-8 w-8 mx-auto text-primary animate-bounce" />
                  <p className="text-sm font-medium text-primary">Drop files here</p>
                </div>
              ) : (
                <div className="space-y-2">
                  <Upload className="h-8 w-8 mx-auto text-muted-foreground" />
                  <p className="text-sm text-muted-foreground">
                    Drag and drop files here, or click to browse
                  </p>
                  <Button size="sm" variant="outline" onClick={() => fileInputRef.current?.click()}>
                    Browse Files
                  </Button>
                </div>
              )}
            </div>
            <p className="text-xs text-muted-foreground mt-2">
              Supported: PDF, Office documents, text, code files, images, and more
            </p>
          </div>

          {/* Existing files section */}
          {existingFiles?.data && existingFiles.data.length > 0 && (
            <div>
              <h3 className="text-sm font-medium mb-2">Or Add Existing File</h3>
              <div className="max-h-48 overflow-y-auto border rounded-md">
                {filesLoading ? (
                  <div className="p-4">
                    <Skeleton className="h-8 w-full" />
                  </div>
                ) : (
                  <ul className="divide-y">
                    {existingFiles.data.map((file: ApiFile) => (
                      <li
                        key={file.id}
                        className="flex items-center justify-between p-2 hover:bg-muted/50"
                      >
                        <div className="flex items-center gap-2 min-w-0">
                          <FileText className="h-4 w-4 text-muted-foreground shrink-0" />
                          <span className="text-sm truncate">{file.filename}</span>
                          <span className="text-xs text-muted-foreground shrink-0">
                            {formatBytes(file.bytes)}
                          </span>
                        </div>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => handleAddExistingFile(file.id)}
                          disabled={addFileMutation.isPending}
                        >
                          Add
                        </Button>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </div>
          )}
        </div>
      </ModalContent>
      <ModalFooter>
        <Button variant="ghost" onClick={handleClose}>
          Cancel
        </Button>
      </ModalFooter>
    </Modal>
  );
}

export default function VectorStoreDetailPage() {
  const { vectorStoreId } = useParams<{ vectorStoreId: string }>();
  const navigate = useNavigate();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const confirm = useConfirm();

  const [isAddFileModalOpen, setIsAddFileModalOpen] = useState(false);
  const [selectedFileForChunks, setSelectedFileForChunks] = useState<{
    id: string;
    filename?: string;
  } | null>(null);

  // Fetch vector store details
  const {
    data: vectorStore,
    isLoading: storeLoading,
    error: storeError,
    refetch: refetchStore,
  } = useQuery(vectorStoreGetOptions({ path: { vector_store_id: vectorStoreId! } }));

  // Fetch files in this vector store
  const {
    data: files,
    isLoading: filesLoading,
    refetch: refetchFiles,
  } = useQuery({
    ...vectorStoreFileListOptions({
      path: { vector_store_id: vectorStoreId! },
    }),
    enabled: !!vectorStoreId,
  });

  // Delete file mutation
  const deleteFileMutation = useMutation({
    ...vectorStoreFileDeleteMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: [{ _id: "vectorStoreFileList" }] });
      queryClient.invalidateQueries({ queryKey: [{ _id: "vectorStoreGet" }] });
      toast({ title: "File removed from knowledge base", type: "success" });
    },
    onError: (error) => {
      toast({ title: "Failed to remove file", description: formatApiError(error), type: "error" });
    },
  });

  const handleDeleteFile = async (file: VectorStoreFile) => {
    const confirmed = await confirm({
      title: "Remove File",
      message: `Are you sure you want to remove this file from the knowledge base? The file and its embeddings will be deleted.`,
      confirmLabel: "Remove",
      variant: "destructive",
    });
    if (confirmed) {
      deleteFileMutation.mutate({
        path: { vector_store_id: vectorStoreId!, file_id: file.id },
      });
    }
  };

  const handleRefresh = () => {
    refetchStore();
    refetchFiles();
  };

  // Column definitions for files table
  const fileColumns = [
    fileColumnHelper.accessor("id", {
      header: "File ID",
      cell: (info) => (
        <div className="flex items-center gap-2">
          <FileText className="h-4 w-4 text-muted-foreground" />
          <CodeBadge className="text-xs">{info.getValue().slice(0, 12)}...</CodeBadge>
        </div>
      ),
    }),
    fileColumnHelper.accessor("status", {
      header: "Status",
      cell: (info) => <FileStatusBadge status={info.getValue()} />,
    }),
    fileColumnHelper.accessor("usage_bytes", {
      header: "Size",
      cell: (info) => formatBytes(info.getValue()),
    }),
    fileColumnHelper.accessor("chunking_strategy", {
      header: "Chunking",
      cell: (info) => {
        const strategy = info.getValue();
        if (!strategy) return <span className="text-muted-foreground">Default</span>;
        if (strategy.type === "auto") {
          return <Badge variant="outline">Auto</Badge>;
        }
        // Static strategy - properties are nested under 'static' object
        const maxTokens = strategy.static?.max_chunk_size_tokens ?? 800;
        return <Badge variant="outline">Static ({maxTokens} tokens)</Badge>;
      },
    }),
    fileColumnHelper.accessor("created_at", {
      header: "Added",
      cell: (info) => formatDateTime(info.getValue()),
    }),
    fileColumnHelper.display({
      id: "actions",
      cell: ({ row }) => (
        <div className="flex items-center gap-1">
          <Button
            variant="ghost"
            size="sm"
            title="View chunks"
            onClick={() =>
              setSelectedFileForChunks({
                id: row.original.id,
                filename: row.original.id.slice(0, 12) + "...",
              })
            }
            disabled={row.original.status !== "completed"}
          >
            <Eye className="h-4 w-4" />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="text-destructive"
            title="Remove file"
            onClick={() => handleDeleteFile(row.original)}
            disabled={deleteFileMutation.isPending}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      ),
    }),
  ];

  // Get embedding model info
  const embeddingModelInfo = vectorStore
    ? EMBEDDING_MODELS.find((m) => m.value === vectorStore.embedding_model)
    : null;

  if (storeLoading) {
    return (
      <div className="p-6 space-y-6">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-32 w-full" />
      </div>
    );
  }

  if (storeError || !vectorStore) {
    return (
      <div className="p-6">
        <div className="text-center py-12 text-destructive">
          Knowledge base not found or failed to load.
          <br />
          <Button variant="ghost" onClick={() => navigate("/admin/vector-stores")} className="mt-4">
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Knowledge Bases
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      <DetailPageHeader
        title={vectorStore.name}
        slug={vectorStore.id}
        createdAt={vectorStore.created_at}
        onBack={() => navigate("/admin/vector-stores")}
        onEdit={() => {
          // For now, just show a toast - full edit modal could be added
          toast({ title: "Edit via the main knowledge bases page", type: "info" });
        }}
      />

      {/* Stats Cards */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <StatCard title="Status" icon={<Database className="h-4 w-4" />}>
          <Badge
            variant={
              vectorStore.status === "completed"
                ? "default"
                : vectorStore.status === "in_progress"
                  ? "secondary"
                  : "outline"
            }
          >
            {vectorStore.status === "completed"
              ? "Ready"
              : vectorStore.status === "in_progress"
                ? "Processing"
                : "Expired"}
          </Badge>
        </StatCard>

        <StatCard title="Total Files" icon={<FileText className="h-4 w-4" />}>
          <StatValue value={vectorStore.file_counts.total} />
          {vectorStore.file_counts.in_progress > 0 && (
            <span className="text-xs text-muted-foreground ml-2">
              ({vectorStore.file_counts.in_progress} processing)
            </span>
          )}
        </StatCard>

        <StatCard title="Storage Used" icon={<Database className="h-4 w-4" />}>
          <StatValue value={formatBytes(vectorStore.usage_bytes)} />
        </StatCard>

        <StatCard title="Embedding Model" icon={<Database className="h-4 w-4" />}>
          <div className="text-sm">
            <CodeBadge>{vectorStore.embedding_model}</CodeBadge>
            {embeddingModelInfo && (
              <span className="text-xs text-muted-foreground ml-2">
                ({embeddingModelInfo.dimensions}d)
              </span>
            )}
          </div>
        </StatCard>
      </div>

      {/* Description */}
      {vectorStore.description && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm font-medium">Description</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-sm text-muted-foreground">{vectorStore.description}</p>
          </CardContent>
        </Card>
      )}

      {/* Files Table */}
      <Card>
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle>Files</CardTitle>
          <div className="flex gap-2">
            <Button size="sm" variant="outline" onClick={handleRefresh}>
              <RefreshCw className="h-4 w-4 mr-2" />
              Refresh
            </Button>
            <Button size="sm" onClick={() => setIsAddFileModalOpen(true)}>
              <Upload className="h-4 w-4 mr-2" />
              Add Files
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          <DataTable
            columns={fileColumns as ColumnDef<VectorStoreFile>[]}
            data={files?.data || []}
            isLoading={filesLoading}
            emptyMessage="No files in this knowledge base. Add files to enable RAG search."
            searchColumn="id"
            searchPlaceholder="Search by file ID..."
          />
        </CardContent>
      </Card>

      {/* Search and Chunks Section */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        {/* Search Preview */}
        <SearchPreview
          vectorStoreId={vectorStoreId!}
          onFileClick={(fileId) => {
            // Find the file in the list to get more context
            const file = files?.data?.find((f) => f.id === fileId);
            if (file) {
              setSelectedFileForChunks({
                id: file.id,
                filename: file.id.slice(0, 12) + "...",
              });
            }
          }}
        />

        {/* Chunk Viewer - shown when a file is selected */}
        {selectedFileForChunks ? (
          <ChunkViewer
            vectorStoreId={vectorStoreId!}
            fileId={selectedFileForChunks.id}
            filename={selectedFileForChunks.filename}
          />
        ) : (
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-base">
                <FileText className="h-4 w-4" />
                File Chunks
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-center py-8 text-muted-foreground">
                <Eye className="h-8 w-8 mx-auto mb-2 opacity-50" />
                <p className="text-sm">Select a file to view its chunks.</p>
                <p className="text-xs mt-1">
                  Click the <Eye className="h-3 w-3 inline" /> icon on any completed file above.
                </p>
              </div>
            </CardContent>
          </Card>
        )}
      </div>

      {/* Add File Modal */}
      <AddFileModal
        open={isAddFileModalOpen}
        onClose={() => setIsAddFileModalOpen(false)}
        vectorStoreId={vectorStoreId!}
        ownerId={vectorStore.owner_id}
        ownerType={vectorStore.owner_type}
      />
    </div>
  );
}
