import { Navigate } from "react-router-dom";
import { useConfig } from "@/config/ConfigProvider";
import { PageNotice } from "@/components/PageNotice/PageNotice";
import type { PagesConfig, AdminPagesConfig, PageConfig } from "@/config/types";

type MainPageKey = keyof Omit<PagesConfig, "admin">;

const mainPageOrder: MainPageKey[] = [
  "chat",
  "studio",
  "projects",
  "teams",
  "knowledge_bases",
  "containers",
  "api_keys",
  "providers",
  "templates",
  "skills",
  "usage",
];

const mainPageRoutes: Record<MainPageKey, string> = {
  chat: "/chat",
  studio: "/studio",
  projects: "/projects",
  teams: "/teams",
  knowledge_bases: "/knowledge-bases",
  containers: "/containers",
  api_keys: "/api-keys",
  providers: "/providers",
  templates: "/templates",
  skills: "/skills",
  usage: "/usage",
};

export function getPageConfig(pages: PagesConfig, key: string): PageConfig {
  if (key.startsWith("admin.")) {
    const adminKey = key.slice(6) as keyof AdminPagesConfig;
    return pages.admin[adminKey] ?? { status: "enabled" };
  }
  return pages[key as MainPageKey] ?? { status: "enabled" };
}

export function getFirstEnabledRoute(pages: PagesConfig): string {
  for (const key of mainPageOrder) {
    if (pages[key].status !== "disabled") {
      return mainPageRoutes[key];
    }
  }
  return "/account";
}

interface PageGuardProps {
  pageKey: string;
  pageTitle: string;
  children: React.ReactNode;
}

export function PageGuard({ pageKey, pageTitle, children }: PageGuardProps) {
  const { config } = useConfig();
  const pageConfig = getPageConfig(config.pages, pageKey);

  if (pageConfig.status === "disabled") {
    return <Navigate to={getFirstEnabledRoute(config.pages)} replace />;
  }

  if (pageConfig.status === "notice") {
    return (
      <PageNotice
        title={pageTitle}
        message={pageConfig.notice_message ?? "This page is currently unavailable."}
      />
    );
  }

  return <>{children}</>;
}
