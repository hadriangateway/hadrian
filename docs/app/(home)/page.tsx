"use client";

import { useState } from "react";
import Link from "next/link";
import { Brain, Code, Eye, Server, Shield, Users, Zap } from "lucide-react";
import { QuickStartSelector } from "@/components/quick-start-selector";
import { GatewayDiagram } from "@/components/gateway-diagram";
import { StoryEmbed } from "@/components/story-embed";

function GitHubIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
      <path d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844a9.59 9.59 0 0 1 2.504.337c1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z" />
    </svg>
  );
}

function OpenRouterIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 512 512"
      fill="currentColor"
      stroke="currentColor"
      className={className}
      aria-hidden="true"
    >
      <path
        d="M3 248.945C18 248.945 76 236 106 219C136 202 136 202 198 158C276.497 102.293 332 120.945 423 120.945"
        strokeWidth="90"
        fill="none"
      />
      <path d="M511 121.5L357.25 210.268L357.25 32.7324L511 121.5Z" stroke="none" />
      <path
        d="M0 249C15 249 73 261.945 103 278.945C133 295.945 133 295.945 195 339.945C273.497 395.652 329 377 420 377"
        strokeWidth="90"
        fill="none"
      />
      <path d="M508 376.445L354.25 287.678L354.25 465.213L508 376.445Z" stroke="none" />
    </svg>
  );
}

// --- See it in Action (Gallery) ---

const demos = [
  {
    id: "chat",
    title: "Multi-Model Chat",
    description:
      "Compare responses from multiple models side-by-side with advanced multi-model modes.",
    storyId: "chat-chatview--multi-model-conversation",
  },
  {
    id: "knowledge-bases",
    title: "Knowledge Bases",
    description: "Search uploaded documents with vector search, citations, and inline references.",
    storyId: "chat-chatview--knowledge-bases",
  },
  {
    id: "execute-code",
    title: "Execute Code",
    description: "Run Python in the browser and display rich visualizations inline.",
    storyId: "chat-chatview--execute-code",
  },
  {
    id: "studio",
    title: "Studio",
    description: "Generate media across providers simultaneously with cost tracking.",
    storyId: "pages-studiopage--images",
  },
  {
    id: "analytics",
    title: "Analytics",
    description: "Track costs per user, team, and project with microcent precision.",
    storyId: "components-usagedashboard--organization",
  },
  {
    id: "usage-logs",
    title: "Usage Logs",
    description: "Inspect individual requests with model, tokens, cost, and latency details.",
    storyId: "admin-usagelogstable--default",
  },
  {
    id: "provider-health",
    title: "Provider Health",
    description: "Monitor provider status, latency, and circuit breakers in real time.",
    storyId: "admin-providerhealthpage--all-healthy",
  },
  {
    id: "rbac-policies",
    title: "RBAC Policies",
    description: "Define fine-grained access control with CEL-based policies per organization.",
    storyId: "admin-orgrbacpoliciespage--with-policies",
  },
  {
    id: "multi-tenancy",
    title: "Multi-Tenancy",
    description: "Manage organizations with teams, projects, members, and scoped resources.",
    storyId: "admin-organizationdetailpage--default",
  },
];

function DemoGallery() {
  const [active, setActive] = useState("chat");
  const current = demos.find((d) => d.id === active) ?? demos[0];

  return (
    <div className="mx-auto max-w-screen-2xl px-4">
      <div className="flex flex-col gap-6 lg:flex-row lg:gap-10">
        <ol className="lg:w-72 lg:shrink-0" role="tablist" aria-label="Demo gallery">
          {demos.map((demo, i) => (
            <li key={demo.id}>
              <button
                role="tab"
                aria-selected={active === demo.id}
                aria-controls={`demo-panel-${demo.id}`}
                onClick={() => setActive(demo.id)}
                className={`group flex w-full cursor-pointer items-baseline gap-4 border-b border-fd-border py-3 text-left transition-colors ${
                  active === demo.id
                    ? "text-fd-foreground"
                    : "text-fd-muted-foreground hover:text-fd-foreground"
                }`}
              >
                <span
                  className={`font-mono text-2xl tabular-nums ${
                    active === demo.id ? "text-fd-primary" : "text-fd-muted-foreground/60"
                  }`}
                >
                  {String(i + 1).padStart(2, "0")}
                </span>
                <span className="text-base font-semibold tracking-tight">{demo.title}</span>
                {active === demo.id && (
                  <span aria-hidden="true" className="ml-auto text-fd-primary">
                    ▸
                  </span>
                )}
              </button>
            </li>
          ))}
        </ol>
        <div className="flex-1">
          <p className="mb-4 text-sm text-fd-muted-foreground">{current.description}</p>
          <div
            id={`demo-panel-${current.id}`}
            role="tabpanel"
            aria-label={current.title}
            className="relative h-[600px] overflow-hidden rounded-xl border border-fd-border shadow-lg sm:h-[800px] lg:h-[860px]"
          >
            {demos.map((demo) => (
              <div
                key={demo.id}
                className={active === demo.id ? "h-full" : "invisible absolute inset-0"}
              >
                <StoryEmbed storyId={demo.storyId} height="100%" />
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

// --- Everything Included ---

const featureCategories = [
  {
    icon: Server,
    title: "Infrastructure",
    items: [
      "Single binary, single config file",
      "SQLite, Postgres, or stateless",
      "Redis caching",
      "S3-compatible storage",
      "Provider fallbacks & health checks",
      "Helm chart for Kubernetes",
    ],
  },
  {
    icon: Brain,
    title: "AI Capabilities",
    items: [
      "Multi-model chat",
      "Image generation",
      "TTS & transcription",
      "Knowledge bases / RAG",
      "Web search",
      "Model catalog",
    ],
  },
  {
    icon: Shield,
    title: "Security & Auth",
    items: [
      "API keys & service accounts",
      "OIDC / OAuth / SAML SSO",
      "CEL-based RBAC",
      "Guardrails & content moderation",
      "Rate limiting",
      "Sovereignty enforcement",
    ],
  },
  {
    icon: Users,
    title: "Multi-tenancy",
    items: [
      "Organizations, teams, projects",
      "Dynamic providers",
      "Scoped budgets",
      "Per-tenant SSO",
      "SCIM provisioning",
    ],
  },
  {
    icon: Eye,
    title: "Observability",
    items: [
      "Usage & cost tracking",
      "Cost forecasting",
      "Prometheus metrics",
      "OpenTelemetry tracing",
      "Audit logs & SIEM",
    ],
  },
  {
    icon: Code,
    title: "Developer Experience",
    items: [
      "OpenAI-compatible API",
      "OpenAPI docs & Scalar UI",
      "MCP servers",
      "Frontend tools (Python/JS/SQL/Charts)",
      "Web UI with admin panel",
    ],
  },
];

// --- Page ---

export default function HomePage() {
  return (
    <div className="flex flex-col">
      {/* Hero */}
      <section className="relative overflow-hidden py-16 md:py-24">
        <div className="mx-auto max-w-6xl px-4">
          <div className="text-center">
            <h1 className="mb-6 text-4xl font-bold tracking-tight md:text-6xl">Hadrian Gateway</h1>
            <p className="mx-auto mb-0 max-w-2xl text-lg text-fd-muted-foreground md:text-xl">
              Unified AI gateway with every enterprise feature included.
            </p>
            <p className="mx-auto mb-8 max-w-2xl text-lg text-fd-muted-foreground md:text-xl">
              Completely Open Source and Free.
            </p>
            <p className="mb-8 text-sm text-fd-muted-foreground">
              MIT and Apache-2.0 licensed. No proprietary code, upgrade tiers, or restrictions.
            </p>
            <div className="flex flex-wrap justify-center gap-4">
              <span className="group relative">
                <a
                  href="https://app.hadriangateway.com"
                  className="relative inline-flex items-center gap-2 rounded-lg bg-fd-primary px-6 py-3 font-medium text-fd-primary-foreground transition-colors hover:bg-fd-primary/90 hover:[animation:jiggle_0.6s_ease-in-out] active:scale-95"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  <span className="absolute -right-1.5 -top-1.5 flex h-3.5 w-3.5">
                    <span
                      className="absolute inline-flex h-full w-full animate-ping rounded-full bg-red-500 opacity-75"
                      style={{ animationDuration: "2s" }}
                    />
                    <span className="relative inline-flex h-3.5 w-3.5 rounded-full bg-red-500" />
                  </span>
                  <Zap className="h-4 w-4" />
                  Try in Browser
                </a>
                <span className="pointer-events-none absolute left-1/2 top-full z-10 mt-2 -translate-x-1/2 -translate-y-2 scale-95 rounded-md bg-fd-popover px-3 py-1.5 text-center text-base text-fd-popover-foreground shadow-lg border border-fd-border opacity-0 transition-all duration-300 ease-out group-hover:translate-y-0 group-hover:scale-100 group-hover:opacity-100">
                  <span className="whitespace-nowrap">
                    Connect to <strong>Ollama</strong>, <strong>OpenRouter</strong>, and more!
                  </span>
                  <br />
                  <span className="whitespace-nowrap">
                    Running Hadrian entirely in your web browser.
                  </span>
                </span>
              </span>
              <Link
                href="/docs/getting-started"
                className="inline-flex items-center gap-2 rounded-lg border border-fd-border bg-fd-background px-6 py-3 font-medium transition-colors hover:bg-fd-muted"
              >
                Get Started
              </Link>
              <Link
                href="/docs"
                className="inline-flex items-center gap-2 rounded-lg border border-fd-border bg-fd-background px-6 py-3 font-medium transition-colors hover:bg-fd-muted"
              >
                Documentation
              </Link>
              <a
                href="https://github.com/hadriangateway/hadrian"
                className="inline-flex items-center gap-2 rounded-lg border border-fd-border bg-fd-background px-6 py-3 font-medium transition-colors hover:bg-fd-muted"
                target="_blank"
                rel="noopener noreferrer"
              >
                <GitHubIcon className="h-4 w-4" />
                GitHub
              </a>
              <a
                href="https://openrouter.ai/apps?url=https%3A%2F%2Fhadriangateway.com"
                className="inline-flex items-center gap-2 rounded-lg border border-fd-border bg-fd-background px-6 py-3 font-medium transition-colors hover:bg-fd-muted"
                target="_blank"
                rel="noopener noreferrer"
              >
                <OpenRouterIcon className="h-4 w-4" />
                OpenRouter
              </a>
            </div>
          </div>

          {/* Gateway capabilities diagram */}
          <div className="mt-4">
            <GatewayDiagram />
          </div>

          {/* Quick Start Selector */}
          <div className="mx-auto mt-12 max-w-6xl">
            <h2 className="mb-4 text-lg font-semibold">Get Started</h2>
            <QuickStartSelector />
          </div>
        </div>
      </section>

      {/* See it in Action */}
      <section className="py-16 md:py-24">
        <div className="mx-auto mb-8 max-w-6xl px-4 text-center">
          <h2 className="text-3xl font-bold">See it in Action</h2>
        </div>
        <DemoGallery />
      </section>

      {/* Everything Included */}
      <section className="py-16 md:py-24">
        <div className="mx-auto max-w-6xl px-4">
          <h2 className="mb-4 text-center text-3xl font-bold">Everything Included</h2>
          <p className="mx-auto mb-12 max-w-2xl text-center text-fd-muted-foreground">
            Every feature is included in the open-source release. No asterisks or upgrade walls.
          </p>
          <div className="grid gap-x-12 gap-y-10 md:grid-cols-2 lg:grid-cols-3">
            {featureCategories.map((cat) => (
              <div key={cat.title} className="text-center">
                <cat.icon className="mx-auto mb-3 h-6 w-6 text-fd-primary" />
                <h3 className="mb-4 text-sm font-semibold uppercase tracking-widest text-fd-foreground">
                  {cat.title}
                </h3>
                <ul className="space-y-1.5 text-sm text-fd-muted-foreground">
                  {cat.items.map((item) => (
                    <li key={item}>{item}</li>
                  ))}
                </ul>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* CTA */}
      <section className="py-16 md:py-24">
        <div className="mx-auto max-w-6xl px-4 text-center">
          <h2 className="mb-4 text-3xl font-bold">Ready to Get Started?</h2>
          <p className="mx-auto mb-8 max-w-xl text-fd-muted-foreground">
            Deploy in minutes with a single binary. No external dependencies for basic use.
          </p>
          <div className="flex flex-wrap justify-center gap-4">
            <Link
              href="/docs/getting-started"
              className="inline-flex items-center gap-2 rounded-lg bg-fd-primary px-6 py-3 font-medium text-fd-primary-foreground transition-colors hover:bg-fd-primary/90"
            >
              Quick Start Guide
            </Link>
            <Link
              href="/docs/deployment"
              className="inline-flex items-center gap-2 rounded-lg border border-fd-border bg-fd-background px-6 py-3 font-medium transition-colors hover:bg-fd-muted"
            >
              Deployment Guide
            </Link>
            <a
              href="https://github.com/hadriangateway/hadrian"
              className="inline-flex items-center gap-2 rounded-lg border border-fd-border bg-fd-background px-6 py-3 font-medium transition-colors hover:bg-fd-muted"
              target="_blank"
              rel="noopener noreferrer"
            >
              <GitHubIcon className="h-4 w-4" />
              GitHub
            </a>
          </div>
        </div>
      </section>

      {/* Footer */}
      <footer className="border-t py-8">
        <div className="mx-auto max-w-6xl px-4">
          <div className="flex flex-col items-center justify-between gap-4 text-sm text-fd-muted-foreground md:flex-row">
            <p>Open Source (MIT, Apache-2.0). All enterprise features included.</p>
            <div className="flex gap-6">
              <Link href="/docs" className="hover:text-fd-foreground">
                Documentation
              </Link>
              <a
                href="https://github.com/hadriangateway/hadrian"
                className="hover:text-fd-foreground"
                target="_blank"
                rel="noopener noreferrer"
              >
                GitHub
              </a>
              <a
                href="https://github.com/hadriangateway/hadrian/issues"
                className="hover:text-fd-foreground"
                target="_blank"
                rel="noopener noreferrer"
              >
                Issues
              </a>
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
}
