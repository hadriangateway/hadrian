import type { SkillResource } from "@/api/generated/types.gen";

/**
 * Detect whether the caret is inside a skill slash-command token and, if
 * so, return the query (the text between the `/` and the caret). The `/`
 * must be at the start of the textarea or preceded by whitespace — this
 * avoids lighting up the popover for stray slashes inside sentences.
 *
 * Returns `null` when the caret isn't inside a slash-token.
 */
export interface SlashQuery {
  /** Text between `/` and the caret, e.g. "pd" for `/pd|`. */
  query: string;
  /** Index in `text` of the `/` character. */
  start: number;
  /** Caret position (exclusive end of the token). */
  end: number;
}

export function detectSlashQuery(text: string, caret: number): SlashQuery | null {
  if (caret <= 0 || caret > text.length) return null;

  // Walk backwards from the caret until we hit a whitespace character or `/`.
  let i = caret - 1;
  while (i >= 0) {
    const ch = text[i];
    if (ch === "/") break;
    if (/\s/.test(ch)) return null;
    if (!/[a-z0-9-]/.test(ch)) return null; // non-skill-name char: abort
    i--;
  }
  if (i < 0 || text[i] !== "/") return null;

  // Require the `/` to be at the start or preceded by whitespace.
  if (i > 0 && !/\s/.test(text[i - 1])) return null;

  return { query: text.slice(i + 1, caret), start: i, end: caret };
}

/**
 * Filter skills whose name matches `query` as a prefix (preferred) or
 * substring (fallback). Skills marked `user_invocable: false` are excluded
 * since the slash-command UI is a user-facing surface. Results are sorted
 * with prefix matches first, then alphabetical.
 *
 * The result is cached on `(skills array identity, query)` so the keystroke
 * paths in `ChatInput` (input-change handler, key-down Enter/Tab handlers,
 * the popover's own `useMemo`) share work — without this, each keystroke
 * fanned out into 2–3 redundant linear scans of every user skill.
 */
let lastSkillsRef: SkillResource[] | null = null;
let lastQuery: string | null = null;
let lastResult: SkillResource[] = [];

export function matchSkills(skills: SkillResource[], query: string): SkillResource[] {
  if (skills === lastSkillsRef && query === lastQuery) return lastResult;

  const q = query.toLowerCase();
  const invocable = skills.filter((s) => s.user_invocable !== false);
  let result: SkillResource[];
  if (!q) {
    result = invocable.slice(0, 20);
  } else {
    const prefix: SkillResource[] = [];
    const contains: SkillResource[] = [];
    for (const s of invocable) {
      const name = s.name.toLowerCase();
      if (name.startsWith(q)) prefix.push(s);
      else if (name.includes(q)) contains.push(s);
    }
    prefix.sort((a, b) => a.name.localeCompare(b.name));
    contains.sort((a, b) => a.name.localeCompare(b.name));
    result = [...prefix, ...contains].slice(0, 20);
  }

  lastSkillsRef = skills;
  lastQuery = query;
  lastResult = result;
  return result;
}
