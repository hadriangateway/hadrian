"use client";

import { useState } from "react";
import { Check, Copy, Download, ExternalLink, X } from "lucide-react";

type Method = "browser" | "binary" | "docker" | "cargo" | "helm";
type OS = "linux-x86_64" | "linux-arm64" | "macos-arm64" | "windows";
type Profile = "full" | "headless" | "standard" | "minimal" | "tiny";
type Libc = "gnu" | "musl";

const osLabels: Record<OS, string> = {
  "linux-x86_64": "Linux x86_64",
  "linux-arm64": "Linux ARM64",
  "macos-arm64": "macOS ARM64",
  windows: "Windows",
};

const libcLabels: Record<Libc, string> = {
  gnu: "glibc",
  musl: "musl",
};

const profileLabels: Record<Profile, string> = {
  full: "Full",
  headless: "Headless",
  standard: "Standard",
  minimal: "Minimal",
  tiny: "Tiny",
};

function getTarget(os: OS, libc: Libc): string {
  switch (os) {
    case "linux-x86_64":
      return libc === "musl" ? "x86_64-unknown-linux-musl" : "x86_64-unknown-linux-gnu";
    case "linux-arm64":
      return "aarch64-unknown-linux-gnu";
    case "macos-arm64":
      return "aarch64-apple-darwin";
    case "windows":
      return "x86_64-pc-windows-msvc";
  }
}

const profileSummaries: Record<Profile, string> = {
  full: "Everything",
  headless: "Full features, no embedded assets (serve frontend separately)",
  standard: "Production deployment",
  minimal: "Development and embedded use",
  tiny: "Stateless proxy",
};

const allProfiles: Profile[] = ["full", "headless", "standard", "minimal", "tiny"];
const embeddedAssetProfiles: Profile[] = ["minimal", "standard", "full"];

const featureMatrix: { name: string; profiles: Profile[]; href?: string }[] = [
  { name: "OpenAI", profiles: allProfiles, href: "/docs/configuration/providers" },
  {
    name: "Anthropic",
    profiles: ["minimal", "standard", "headless", "full"],
    href: "/docs/configuration/providers",
  },
  {
    name: "AWS Bedrock",
    profiles: ["minimal", "standard", "headless", "full"],
    href: "/docs/configuration/providers",
  },
  {
    name: "Google Vertex AI",
    profiles: ["minimal", "standard", "headless", "full"],
    href: "/docs/configuration/providers",
  },
  {
    name: "Azure OpenAI",
    profiles: ["minimal", "standard", "headless", "full"],
    href: "/docs/configuration/providers",
  },
  {
    name: "SQLite",
    profiles: ["minimal", "standard", "headless", "full"],
    href: "/docs/configuration/database",
  },
  { name: "Embedded UI", profiles: embeddedAssetProfiles, href: "/docs/features/chat-ui" },
  { name: "Model catalog", profiles: embeddedAssetProfiles },
  { name: "Setup wizard", profiles: embeddedAssetProfiles, href: "/docs/configuration/builder" },
  {
    name: "PostgreSQL",
    profiles: ["standard", "headless", "full"],
    href: "/docs/configuration/database",
  },
  {
    name: "Redis caching",
    profiles: ["standard", "headless", "full"],
    href: "/docs/features/caching",
  },
  {
    name: "SSO (OIDC / OAuth)",
    profiles: ["standard", "headless", "full"],
    href: "/docs/features/sso-admin-guide",
  },
  {
    name: "Server-side MCP",
    profiles: ["standard", "headless", "full"],
    href: "/docs/features/mcp",
  },
  {
    name: "CEL RBAC",
    profiles: ["standard", "headless", "full"],
    href: "/docs/features/authorization",
  },
  {
    name: "S3 storage",
    profiles: ["standard", "headless", "full"],
    href: "/docs/configuration/storage",
  },
  {
    name: "Secrets managers",
    profiles: ["standard", "headless", "full"],
    href: "/docs/deployment/advanced#secret-management-with-vault",
  },
  {
    name: "OTLP & Prometheus",
    profiles: ["standard", "headless", "full"],
    href: "/docs/configuration/observability",
  },
  { name: "OpenAPI docs", profiles: ["standard", "headless", "full"], href: "/docs/api" },
  { name: "Embedded docs", profiles: embeddedAssetProfiles },
  {
    name: "Doc extraction",
    profiles: ["standard", "headless", "full"],
    href: "/docs/configuration/features/file-processing#document-extraction",
  },
  {
    name: "Cost forecasting",
    profiles: ["standard", "headless", "full"],
    href: "/docs/features/budgets#time-series-forecasting",
  },
  { name: "CSV export", profiles: ["standard", "headless", "full"] },
  {
    name: "Response validation",
    profiles: ["standard", "headless", "full"],
    href: "/docs/features/guardrails",
  },
  { name: "JSON schema", profiles: ["standard", "headless", "full"] },
  { name: "SAML SSO", profiles: ["headless", "full"], href: "/docs/features/saml" },
  {
    name: "xberg OCR",
    profiles: ["headless", "full"],
    href: "/docs/configuration/features/file-processing#document-extraction",
  },
  {
    name: "ClamAV scanning",
    profiles: ["headless", "full"],
    href: "/docs/configuration/features/file-processing#virus-scanning",
  },
  {
    name: "Microsandbox containers",
    profiles: ["headless", "full"],
    href: "/docs/features/agents#runtimes",
  },
  {
    name: "OpenSandbox containers",
    profiles: ["headless", "full"],
    href: "/docs/features/agents#runtimes",
  },
];

function getInstallCommand(method: Method, os: OS, profile: Profile, libc: Libc): string {
  if (method === "docker") {
    return [
      "cat <<'EOF' > hadrian.toml",
      "[server]",
      'host = "0.0.0.0"',
      "port = 8080",
      "",
      "[database]",
      'type = "sqlite"',
      'path = "/app/data/hadrian.db"',
      "",
      "[cache]",
      'type = "memory"',
      "",
      "[ui]",
      "enabled = true",
      "EOF",
      "",
      "docker run -p 8080:8080 \\",
      "  -v ./hadrian.toml:/app/config/hadrian.toml:ro \\",
      "  -v hadrian-data:/app/data \\",
      "  ghcr.io/hadriangateway/hadrian",
    ].join("\n");
  }
  if (method === "cargo") {
    return `cargo install hadrian@${process.env.HADRIAN_VERSION}\nhadrian`;
  }
  if (method === "helm") {
    return [
      "git clone https://github.com/hadriangateway/hadrian.git",
      "cd gateway/helm/hadrian",
      "helm dependency update",
      "helm install my-gateway . -n hadrian --create-namespace",
    ].join("\n");
  }
  const ext = os === "windows" ? "zip" : "tar.gz";
  const target = getTarget(os, libc);
  const filename = `hadrian-${target}-${profile}.${ext}`;
  const url = `https://github.com/hadriangateway/hadrian/releases/latest/download/${filename}`;
  if (os === "windows") {
    return [`curl -LO \\`, `  ${url}`, `tar -xf ${filename}`, `.\\hadrian.exe`].join("\n");
  }
  return [`curl -L \\`, `  ${url} \\`, `  | tar xz`, `./hadrian`].join("\n");
}

function getDownloadUrl(os: OS, profile: Profile, libc: Libc): string {
  const ext = os === "windows" ? "zip" : "tar.gz";
  const target = getTarget(os, libc);
  return `https://github.com/hadriangateway/hadrian/releases/latest/download/hadrian-${target}-${profile}.${ext}`;
}

const dockerSimpleCommand = [
  "docker run -p 8080:8080 \\",
  "  -v hadrian-data:/app/data \\",
  "  ghcr.io/hadriangateway/hadrian",
].join("\n");

const dockerConfigCommand = [
  "cat <<'EOF' > hadrian.toml",
  "[server]",
  'host = "0.0.0.0"',
  "port = 8080",
  "",
  "[database]",
  'type = "sqlite"',
  'path = "/app/data/hadrian.db"',
  "",
  "[cache]",
  'type = "memory"',
  "",
  "[ui]",
  "enabled = true",
  "EOF",
  "",
  "docker run -p 8080:8080 \\",
  "  -v ./hadrian.toml:/app/config/hadrian.toml:ro \\",
  "  -v hadrian-data:/app/data \\",
  "  ghcr.io/hadriangateway/hadrian",
].join("\n");

function CommandBlock({ command, label }: { command: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = async () => {
    await navigator.clipboard.writeText(command);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };
  return (
    <div>
      {label && (
        <div className="border-b border-fd-border px-4 py-2">
          <span className="text-xs font-medium text-fd-muted-foreground">{label}</span>
        </div>
      )}
      <div className="relative">
        <pre className="overflow-x-auto whitespace-pre-wrap break-all p-4 pr-12 text-sm">
          <code className="text-fd-foreground">{command}</code>
        </pre>
        <button
          onClick={handleCopy}
          className="absolute right-3 top-3 rounded-md p-1.5 text-fd-muted-foreground transition-colors hover:bg-fd-muted hover:text-fd-foreground"
          aria-label="Copy command"
        >
          {copied ? <Check className="h-4 w-4 text-green-500" /> : <Copy className="h-4 w-4" />}
        </button>
      </div>
    </div>
  );
}

function ToggleGroup<T extends string>({
  options,
  value,
  onChange,
  labels,
  disabled,
}: {
  options: T[];
  value: T;
  onChange: (v: T) => void;
  labels?: Record<T, string>;
  disabled?: Set<T>;
}) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {options.map((opt) => {
        const isDisabled = disabled?.has(opt);
        return (
          <button
            key={opt}
            onClick={() => onChange(opt)}
            disabled={isDisabled}
            className={`rounded-md px-2.5 py-1 text-xs font-medium transition-colors sm:px-3 sm:py-1.5 sm:text-sm ${
              isDisabled
                ? "cursor-not-allowed bg-fd-muted text-fd-muted-foreground/40"
                : value === opt
                  ? "bg-fd-primary text-fd-primary-foreground"
                  : "bg-fd-muted text-fd-muted-foreground hover:bg-fd-muted/80 hover:text-fd-foreground"
            }`}
          >
            {labels ? labels[opt] : opt}
          </button>
        );
      })}
    </div>
  );
}

function getDisabledProfiles(os: OS, libc: Libc): Set<Profile> | undefined {
  // headless and full only built for linux-x86_64-gnu and macos-arm64
  if (os === "windows") return new Set(["full", "headless"]);
  if (os === "linux-arm64") return new Set(["full", "headless"]);
  if (os.startsWith("linux-") && libc === "musl") return new Set(["full", "headless"]);
  return undefined;
}

export function QuickStartSelector() {
  const [method, setMethod] = useState<Method>("binary");
  const [os, setOs] = useState<OS>("linux-x86_64");
  const [profile, setProfile] = useState<Profile>("standard");
  const [libc, setLibc] = useState<Libc>("musl");
  const isLinux = os === "linux-x86_64" || os === "linux-arm64";
  const disabledProfiles = getDisabledProfiles(os, libc);
  const disabledLibcs = os === "linux-arm64" ? new Set<Libc>(["musl"]) : undefined;

  const handleOsChange = (newOs: OS) => {
    setOs(newOs);
    let newLibc = libc;
    if (!newOs.startsWith("linux-") || newOs === "linux-arm64") {
      newLibc = "gnu";
      setLibc("gnu");
    }
    const disabled = getDisabledProfiles(newOs, newLibc);
    if (disabled?.has(profile)) {
      setProfile(disabled.has("standard") ? "minimal" : "standard");
    }
  };

  const handleLibcChange = (newLibc: Libc) => {
    setLibc(newLibc);
    if (newLibc === "musl" && (profile === "full" || profile === "headless")) {
      setProfile("standard");
    }
  };

  const command = getInstallCommand(method, os, profile, libc);
  const downloadUrl = method === "binary" ? getDownloadUrl(os, profile, libc) : null;

  return (
    <div className="not-prose overflow-hidden rounded-lg border border-fd-border bg-fd-card">
      <div className="space-y-3 border-b border-fd-border bg-fd-muted/50 p-4">
        <div className="flex flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-3">
          <span className="text-sm font-medium text-fd-muted-foreground sm:w-16 sm:shrink-0">
            Method
          </span>
          <ToggleGroup
            options={["binary", "browser", "docker", "helm", "cargo"] as Method[]}
            value={method}
            onChange={setMethod}
            labels={{
              binary: "Binary",
              browser: "Browser",
              docker: "Docker",
              helm: "Helm",
              cargo: "Cargo",
            }}
          />
        </div>
        {method === "binary" && (
          <>
            <div className="flex flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-3">
              <span className="text-sm font-medium text-fd-muted-foreground sm:w-16 sm:shrink-0">
                OS
              </span>
              <ToggleGroup
                options={["linux-x86_64", "linux-arm64", "macos-arm64", "windows"] as OS[]}
                value={os}
                onChange={handleOsChange}
                labels={osLabels}
              />
            </div>
            {isLinux && (
              <div className="flex flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-3">
                <span className="text-sm font-medium text-fd-muted-foreground sm:w-16 sm:shrink-0">
                  Libc
                </span>
                <ToggleGroup
                  options={["gnu", "musl"] as Libc[]}
                  value={libc}
                  onChange={handleLibcChange}
                  labels={libcLabels}
                  disabled={disabledLibcs}
                />
              </div>
            )}
            <div className="flex flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-3">
              <span className="text-sm font-medium text-fd-muted-foreground sm:w-16 sm:shrink-0">
                Features
              </span>
              <ToggleGroup
                options={allProfiles}
                value={profile}
                onChange={setProfile}
                labels={profileLabels}
                disabled={disabledProfiles}
              />
            </div>
          </>
        )}
      </div>

      {/* Feature matrix — shown for binary installs */}
      {method === "binary" && (
        <div className="border-b border-fd-border bg-fd-muted/20 px-4 py-3">
          <p className="mb-2 text-sm font-medium text-fd-foreground">{profileSummaries[profile]}</p>
          <div className="grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-3">
            {featureMatrix.map((f) => {
              const included = f.profiles.includes(profile);
              return (
                <div key={f.name} className="flex items-center gap-2 text-sm">
                  {included ? (
                    <Check className="h-3.5 w-3.5 shrink-0 text-green-500" />
                  ) : (
                    <X className="h-3.5 w-3.5 shrink-0 text-fd-muted-foreground/40" />
                  )}
                  {included && f.href ? (
                    <a
                      href={f.href}
                      className="text-fd-foreground underline decoration-fd-muted-foreground/40 underline-offset-2 transition-colors hover:decoration-fd-foreground"
                    >
                      {f.name}
                    </a>
                  ) : (
                    <span
                      className={
                        included ? "text-fd-foreground" : "text-fd-muted-foreground/50 line-through"
                      }
                    >
                      {f.name}
                    </span>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}

      {method === "browser" ? (
        <div className="flex flex-col">
          <div className="flex items-center justify-between border-b border-fd-border px-4 py-2">
            <p className="text-sm text-fd-muted-foreground">
              Run Hadrian entirely in your browser via WebAssembly. No server or installation
              required.
            </p>
            <a
              href="https://app.hadriangateway.com"
              target="_blank"
              rel="noopener noreferrer"
              className="ml-3 inline-flex shrink-0 items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium text-fd-muted-foreground transition-colors hover:bg-fd-muted hover:text-fd-foreground"
            >
              <ExternalLink className="h-3.5 w-3.5" />
              Open in new tab
            </a>
          </div>
          <iframe
            src="https://app.hadriangateway.com"
            title="Hadrian Browser App"
            className="h-[80vh] w-full border-0"
            allow="clipboard-read; clipboard-write"
          />
        </div>
      ) : method === "docker" ? (
        <>
          <CommandBlock command={dockerSimpleCommand} label="Run" />
          <div className="border-t border-fd-border">
            <CommandBlock command={dockerConfigCommand} label="With configuration" />
          </div>
        </>
      ) : (
        <>
          <CommandBlock command={command} />

          {downloadUrl && (
            <div className="border-t border-fd-border bg-fd-muted/30 px-4 py-3">
              <a
                href={downloadUrl}
                className="inline-flex items-center gap-2 rounded-lg bg-fd-primary px-4 py-2 text-sm font-medium text-fd-primary-foreground transition-colors hover:bg-fd-primary/90"
              >
                <Download className="h-4 w-4" />
                Download binary
              </a>
              <p className="mt-2 break-all text-xs text-fd-muted-foreground">
                <a href={downloadUrl} className="underline">
                  {downloadUrl}
                </a>
              </p>
            </div>
          )}
        </>
      )}
    </div>
  );
}
