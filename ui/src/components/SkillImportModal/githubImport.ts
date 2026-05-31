import { parseSkillMd, type ParsedFrontmatter } from "./parseFrontmatter";

const API_BASE = "https://api.github.com";
const RAW_BASE = "https://raw.githubusercontent.com";
const FETCH_CONCURRENCY = 8;
const TREE_CACHE_TTL_MS = 10 * 60 * 1000;
const TREE_CACHE_MAX_BYTES = 2 * 1024 * 1024;

const utf8Encoder = new TextEncoder();

/**
 * Byte length the server will see for `content`. The server enforces its
 * size limit using Rust's `String::len()` which counts UTF-8 bytes;
 * `String.prototype.length` in JS counts UTF-16 code units, so we'd
 * misreport non-ASCII content (and surprise the user with a 400).
 */
function utf8ByteLength(content: string): number {
  return utf8Encoder.encode(content).length;
}

/**
 * A skill discovered in a GitHub repository, ready to be sent to
 * `skillCreate`. Fields mirror the server's `CreateSkillBody` minus the
 * owner (owner is injected at import time).
 */
export interface DiscoveredSkill {
  /** Relative path of the SKILL.md inside the repo. Used for keying. */
  skillDir: string;
  name: string;
  description: string;
  /** [{ path, content }] for every file in the skill's directory. */
  files: { path: string; content: string }[];
  total_bytes: number;
  frontmatter: ParsedFrontmatter;
  /** If parsing failed, a human-readable reason; otherwise undefined. */
  error?: string;
}

export interface GithubRepoRef {
  owner: string;
  repo: string;
  ref?: string;
  path?: string;
}

/**
 * Parse a GitHub URL or `owner/repo[/tree/<ref>/<path>]` string. Supports:
 * - `owner/repo`
 * - `https://github.com/owner/repo`
 * - `https://github.com/owner/repo/tree/<ref>/<sub/path>`
 * - raw API form `github.com/owner/repo`
 */
export function parseGithubUrl(input: string): GithubRepoRef | null {
  const trimmed = input.trim().replace(/\/$/, "");
  if (!trimmed) return null;

  // Bare `owner/repo`
  if (/^[\w.-]+\/[\w.-]+$/.test(trimmed)) {
    const [owner, repo] = trimmed.split("/");
    return { owner, repo };
  }

  let urlInput = trimmed;
  if (!urlInput.startsWith("http")) urlInput = "https://" + urlInput;

  let url: URL;
  try {
    url = new URL(urlInput);
  } catch {
    return null;
  }
  if (!/^(www\.)?github\.com$/.test(url.hostname)) return null;

  const parts = url.pathname.split("/").filter(Boolean);
  if (parts.length < 2) return null;

  const [owner, repo, maybeTree, maybeRef, ...rest] = parts;
  if (maybeTree === "tree" && maybeRef) {
    const path = rest.length > 0 ? rest.join("/") : undefined;
    return { owner, repo: repo.replace(/\.git$/, ""), ref: maybeRef, path };
  }
  return { owner, repo: repo.replace(/\.git$/, "") };
}

/**
 * Rate-limit diagnostics surfaced by the scan. With the tree-based walk
 * we only hit the REST API once or twice per scan, so these are mostly
 * informational, but we still surface them in case the user is behind a
 * shared/NAT'd IP that's already close to its budget.
 */
export interface RateLimitSnapshot {
  remaining: number | null;
  limit: number | null;
  resetAt: Date | null;
}

export interface GithubWalkResult {
  skills: DiscoveredSkill[];
  rateLimit: RateLimitSnapshot;
  /**
   * True if GitHub truncated the recursive tree response (>100k entries
   * or >7MB). Some skills may be missing from `skills`.
   */
  truncated?: boolean;
}

interface TreeEntry {
  path: string;
  mode: string;
  type: "blob" | "tree" | "commit";
  sha: string;
  size?: number;
}

interface TreeResponse {
  sha: string;
  tree: TreeEntry[];
  truncated: boolean;
}

function updateRate(res: Response, rate: RateLimitSnapshot): void {
  const remaining = res.headers.get("x-ratelimit-remaining");
  const limit = res.headers.get("x-ratelimit-limit");
  const reset = res.headers.get("x-ratelimit-reset");
  if (remaining) rate.remaining = Number(remaining);
  if (limit) rate.limit = Number(limit);
  if (reset) rate.resetAt = new Date(Number(reset) * 1000);
}

async function githubApi<T>(url: string, rate: RateLimitSnapshot): Promise<T> {
  const res = await fetch(url, { headers: { Accept: "application/vnd.github+json" } });
  updateRate(res, rate);
  if (!res.ok) throw new Error(`GitHub ${res.status}: ${res.statusText}`);
  return (await res.json()) as T;
}

async function resolveRef(ref: GithubRepoRef, rate: RateLimitSnapshot): Promise<string> {
  if (ref.ref) return ref.ref;
  const data = await githubApi<{ default_branch?: string }>(
    `${API_BASE}/repos/${ref.owner}/${ref.repo}`,
    rate
  );
  if (!data.default_branch) throw new Error("Could not determine default branch");
  return data.default_branch;
}

function treeCacheKey(ref: GithubRepoRef, resolvedRef: string): string {
  return `hadrian:gh-tree:${ref.owner}/${ref.repo}@${resolvedRef}`;
}

async function fetchTree(
  ref: GithubRepoRef,
  resolvedRef: string,
  rate: RateLimitSnapshot
): Promise<TreeResponse> {
  const cacheKey = treeCacheKey(ref, resolvedRef);
  try {
    const cached = sessionStorage.getItem(cacheKey);
    if (cached) {
      const { at, data } = JSON.parse(cached) as { at: number; data: TreeResponse };
      if (Date.now() - at < TREE_CACHE_TTL_MS) return data;
    }
  } catch {
    // sessionStorage unavailable — continue without cache
  }

  // The trees endpoint accepts a branch/tag name as well as a tree SHA —
  // GitHub resolves `resolvedRef` → commit → root tree automatically.
  const data = await githubApi<TreeResponse>(
    `${API_BASE}/repos/${ref.owner}/${ref.repo}/git/trees/${encodeURIComponent(
      resolvedRef
    )}?recursive=1`,
    rate
  );

  try {
    const serialized = JSON.stringify({ at: Date.now(), data });
    if (serialized.length < TREE_CACHE_MAX_BYTES) {
      sessionStorage.setItem(cacheKey, serialized);
    }
  } catch {
    // ignore quota errors
  }
  return data;
}

function rawUrl(ref: GithubRepoRef, resolvedRef: string, path: string): string {
  const encodedRef = encodeURIComponent(resolvedRef);
  const encodedPath = path.split("/").map(encodeURIComponent).join("/");
  return `${RAW_BASE}/${ref.owner}/${ref.repo}/${encodedRef}/${encodedPath}`;
}

async function fetchRawFile(url: string): Promise<string> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.text();
}

function isStrictlyNestedUnder(child: string, parent: string): boolean {
  if (parent === "") return child !== "";
  return child.startsWith(parent + "/");
}

function blobBelongsTo(blobPath: string, skillDir: string): boolean {
  if (skillDir === "") return true;
  return blobPath === skillDir || blobPath.startsWith(skillDir + "/");
}

async function fetchSkillFiles(
  ref: GithubRepoRef,
  resolvedRef: string,
  skillDir: string,
  paths: string[]
): Promise<{ path: string; content: string }[]> {
  const results: { path: string; content: string }[] = [];
  const queue = [...paths];
  const worker = async (): Promise<void> => {
    for (let p = queue.shift(); p !== undefined; p = queue.shift()) {
      try {
        const content = await fetchRawFile(rawUrl(ref, resolvedRef, p));
        // Null byte = binary. Mirror the guard in filesystemImport.ts so
        // binary blobs (images, compiled artifacts) don't get decoded as
        // garbled UTF-8 and rejected downstream with a confusing 400.
        if (content.includes("\u0000")) {
          console.debug(`[skill-import] skipped binary file ${p}`);
          continue;
        }
        const relative = skillDir === "" ? p : p.slice(skillDir.length + 1);
        results.push({ path: relative, content });
      } catch (err) {
        console.debug(`[skill-import] skipped unreadable file ${p}:`, err);
      }
    }
  };
  const workerCount = Math.min(FETCH_CONCURRENCY, paths.length);
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  return results;
}

/**
 * Scan a GitHub repo (or sub-path) for skills.
 *
 * Strategy: one `/repos/{o}/{r}` call to resolve the default branch (skipped
 * if the user supplied a ref), one `/repos/{o}/{r}/git/trees/{ref}?recursive=1`
 * call for the entire tree, then file content from `raw.githubusercontent.com`
 * which is CDN-served and not counted against the REST API rate limit.
 */
export async function walkGithubForSkills(
  ref: GithubRepoRef,
  onProgress?: (message: string) => void
): Promise<GithubWalkResult> {
  const rate: RateLimitSnapshot = { remaining: null, limit: null, resetAt: null };

  onProgress?.("resolving ref");
  const resolvedRef = await resolveRef(ref, rate);

  onProgress?.("fetching tree");
  const tree = await fetchTree(ref, resolvedRef, rate);

  const startPath = ref.path ?? "";
  const inScope = (p: string): boolean =>
    startPath === "" || p === startPath || p.startsWith(startPath + "/");

  const blobs = tree.tree.filter((e) => e.type === "blob" && inScope(e.path));

  // SKILL.md locations in ascending path order so parent dirs come first.
  const skillDirsAll = blobs
    .filter((b) => b.path === "SKILL.md" || b.path.endsWith("/SKILL.md"))
    .map((b) => (b.path === "SKILL.md" ? "" : b.path.slice(0, -"/SKILL.md".length)))
    .sort();

  // Skills don't nest — if a parent dir has SKILL.md, ignore any child SKILL.md.
  const skillDirs: string[] = [];
  for (const d of skillDirsAll) {
    if (skillDirs.some((p) => isStrictlyNestedUnder(d, p))) continue;
    skillDirs.push(d);
  }

  // Bucket blobs to their owning skill dir. Buckets are disjoint by construction.
  const groups = new Map<string, string[]>();
  for (const d of skillDirs) groups.set(d, []);
  for (const blob of blobs) {
    for (const d of skillDirs) {
      if (blobBelongsTo(blob.path, d)) {
        groups.get(d)?.push(blob.path);
        break;
      }
    }
  }

  const skills: DiscoveredSkill[] = [];
  for (const skillDir of skillDirs) {
    onProgress?.(skillDir || "(repo root)");
    const paths = groups.get(skillDir) ?? [];
    const files = await fetchSkillFiles(ref, resolvedRef, skillDir, paths);
    skills.push(buildSkill(skillDir, files));
  }

  if (tree.truncated) {
    console.warn(
      `[skill-import] GitHub tree truncated for ${ref.owner}/${ref.repo}@${resolvedRef}; some skills may be missing. Narrow the scan with a /tree/<ref>/<subpath> URL.`
    );
  }

  return { skills, rateLimit: rate, truncated: tree.truncated };
}

function buildSkill(skillDir: string, files: { path: string; content: string }[]): DiscoveredSkill {
  const main = files.find((f) => f.path === "SKILL.md");
  const total_bytes = files.reduce((sum, f) => sum + utf8ByteLength(f.content), 0);

  if (!main) {
    return {
      skillDir,
      name: skillDir.split("/").pop() ?? "unknown",
      description: "",
      files,
      total_bytes,
      frontmatter: { extra: {} },
      error: "SKILL.md missing",
    };
  }

  const parsed = parseSkillMd(main.content);
  const fallbackName = skillDir.split("/").pop() ?? "unknown";
  return {
    skillDir,
    name: parsed.frontmatter.name ?? fallbackName,
    description: parsed.frontmatter.description ?? "",
    files,
    total_bytes,
    frontmatter: parsed.frontmatter,
  };
}
