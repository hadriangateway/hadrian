import { useEffect, useMemo, useRef } from "react";
import { Brain } from "lucide-react";

import type { SkillResource } from "@/api/generated/types.gen";
import { SkillOwnerBadge } from "@/components/SkillsButton/SkillOwnerBadge";
import { matchSkills } from "@/pages/chat/utils/slashCommandMatcher";

export interface SlashCommandPopoverProps {
  /** All skills the current user can invoke. */
  skills: SkillResource[];
  /** Current query (text after the leading `/`). */
  query: string;
  /** Currently highlighted row (0-indexed). */
  activeIndex: number;
  /** Called when the user picks a skill. */
  onSelect: (skill: SkillResource) => void;
  /** Called with the filtered list length so parent can clamp `activeIndex`. */
  onMatchesChange: (matches: SkillResource[]) => void;
}

export function SlashCommandPopover({
  skills,
  query,
  activeIndex,
  onSelect,
  onMatchesChange,
}: SlashCommandPopoverProps) {
  const matches = useMemo(() => matchSkills(skills, query), [skills, query]);
  const activeRef = useRef<HTMLButtonElement | null>(null);

  // Report matches up so parent can keep activeIndex in bounds.
  useEffect(() => {
    onMatchesChange(matches);
  }, [matches, onMatchesChange]);

  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  if (matches.length === 0) return null;

  return (
    <div
      role="listbox"
      aria-label="Available skills"
      className="absolute bottom-full left-0 z-50 mb-2 w-80 overflow-hidden rounded-lg border bg-popover shadow-lg"
    >
      <div className="border-b px-3 py-1.5 text-xs text-muted-foreground">
        Invoke a skill {query && <span className="font-mono">/{query}</span>}
      </div>
      <ul className="max-h-64 overflow-y-auto scrollbar-thin p-1">
        {matches.map((skill, i) => {
          const isActive = i === activeIndex;
          return (
            <li key={skill.id}>
              <button
                type="button"
                role="option"
                aria-selected={isActive}
                ref={isActive ? activeRef : undefined}
                onMouseDown={(e) => {
                  // Prevent textarea from losing focus before select handler runs.
                  e.preventDefault();
                  onSelect(skill);
                }}
                className={`flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
                  isActive ? "bg-accent" : "hover:bg-accent/50"
                }`}
              >
                <Brain className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate font-mono text-xs">/{skill.name}</span>
                    <SkillOwnerBadge skill={skill} />
                  </div>
                  <span className="line-clamp-1 text-xs text-muted-foreground">
                    {skill.description}
                  </span>
                </div>
              </button>
            </li>
          );
        })}
      </ul>
      <div className="border-t px-3 py-1 text-[10px] text-muted-foreground">
        <kbd>↑</kbd> <kbd>↓</kbd> to navigate · <kbd>Enter</kbd> to select · <kbd>Esc</kbd> to
        dismiss
      </div>
    </div>
  );
}
