import { createColumnHelper, type ColumnDef } from "@tanstack/react-table";
import { MoreHorizontal, Pencil, Trash2 } from "lucide-react";

import type { SkillResource } from "@/api/generated/types.gen";
import {
  Dropdown,
  DropdownContent,
  DropdownItem,
  DropdownTrigger,
} from "@/components/Dropdown/Dropdown";
import { SkillOwnerBadge } from "@/components/SkillsButton/SkillOwnerBadge";
import { formatDateTime } from "@/utils/formatters";

const columnHelper = createColumnHelper<SkillResource>();

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
}

export function createSkillColumns(
  onEdit: (skill: SkillResource) => void,
  onDelete: (skill: SkillResource) => void
): ColumnDef<SkillResource, unknown>[] {
  return [
    columnHelper.accessor("name", {
      header: "Name",
      cell: (info) => <span className="font-medium">{info.getValue()}</span>,
    }),
    columnHelper.display({
      id: "owner",
      header: "Owner",
      cell: ({ row }) => <SkillOwnerBadge skill={row.original} />,
    }),
    columnHelper.accessor("description", {
      header: "Description",
      cell: (info) => <span className="line-clamp-1">{info.getValue()}</span>,
    }),
    columnHelper.display({
      id: "files",
      header: "Files",
      cell: ({ row }) => {
        const count = row.original.files_manifest?.length ?? row.original.files?.length ?? 0;
        return (
          <span className="text-muted-foreground text-sm">
            {count} file{count === 1 ? "" : "s"}
          </span>
        );
      },
    }),
    columnHelper.accessor("total_bytes", {
      header: "Size",
      cell: (info) => (
        <span className="text-muted-foreground text-sm">{formatBytes(info.getValue() ?? 0)}</span>
      ),
    }),
    columnHelper.accessor("created_at", {
      header: "Created",
      cell: (info) => formatDateTime(new Date(info.getValue() * 1000)),
    }),
    columnHelper.display({
      id: "actions",
      cell: ({ row }) => (
        <Dropdown>
          <DropdownTrigger aria-label="Skill actions" variant="ghost" className="h-8 w-8 p-0">
            <MoreHorizontal className="h-4 w-4" />
          </DropdownTrigger>
          <DropdownContent align="end">
            <DropdownItem onClick={() => onEdit(row.original)}>
              <Pencil className="mr-2 h-4 w-4" />
              Edit
            </DropdownItem>
            <DropdownItem className="text-destructive" onClick={() => onDelete(row.original)}>
              <Trash2 className="mr-2 h-4 w-4" />
              Delete
            </DropdownItem>
          </DropdownContent>
        </Dropdown>
      ),
    }),
  ] as ColumnDef<SkillResource, unknown>[];
}
