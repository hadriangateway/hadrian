import { Routes, Route, Navigate } from "react-router-dom";
import { RequireAuth, RequireAdmin } from "@/auth";
import { AppLayout } from "@/components/AppLayout/AppLayout";
import { AdminLayout } from "@/components/AdminLayout/AdminLayout";
import { PageGuard, getFirstEnabledRoute } from "@/components/PageGuard/PageGuard";
import { useConfig } from "@/config/ConfigProvider";
import { lazy, Suspense } from "react";
import { Spinner } from "@/components/Spinner/Spinner";

const LoginPage = lazy(() => import("@/pages/LoginPage"));
const OAuthAuthorizePage = lazy(() => import("@/pages/OAuthAuthorizePage"));
const AccountPage = lazy(() => import("@/pages/AccountPage"));
const ProjectsPage = lazy(() => import("@/pages/ProjectsPage"));
const TeamsPage = lazy(() => import("@/pages/TeamsPage"));
const KnowledgeBasesPage = lazy(() => import("@/pages/KnowledgeBasesPage"));
const ContainersPage = lazy(() => import("@/pages/ContainersPage"));
const ContainerDetailPage = lazy(() => import("@/pages/ContainerDetailPage"));
const ApiKeysPage = lazy(() => import("@/pages/ApiKeysPage"));
const ApiKeyDetailPage = lazy(() => import("@/pages/ApiKeyDetailPage"));
const MyUsagePage = lazy(() => import("@/pages/MyUsagePage"));
const MyProvidersPage = lazy(() => import("@/pages/MyProvidersPage"));
const TemplatesPage = lazy(() => import("@/pages/TemplatesPage"));
const SkillsPage = lazy(() => import("@/pages/SkillsPage"));
const SelfServiceProjectDetailPage = lazy(() => import("@/pages/project/ProjectDetailPage"));
const StudioPage = lazy(() => import("@/pages/studio/StudioPage"));
const ChatPage = lazy(() => import("@/pages/chat/ChatPage"));
const AdminDashboardPage = lazy(() => import("@/pages/admin/DashboardPage"));
const OrganizationsPage = lazy(() => import("@/pages/admin/OrganizationsPage"));
const OrganizationDetailPage = lazy(() => import("@/pages/admin/OrganizationDetailPage"));
const ProjectDetailPage = lazy(() => import("@/pages/admin/ProjectDetailPage"));
const UsersPage = lazy(() => import("@/pages/admin/UsersPage"));
const UserDetailPage = lazy(() => import("@/pages/admin/UserDetailPage"));
const AdminApiKeysPage = lazy(() => import("@/pages/admin/ApiKeysPage"));
const ProvidersPage = lazy(() => import("@/pages/admin/ProvidersPage"));
const ProviderHealthPage = lazy(() => import("@/pages/admin/ProviderHealthPage"));
const ProviderDetailPage = lazy(() => import("@/pages/admin/ProviderDetailPage"));
const PricingPage = lazy(() => import("@/pages/admin/PricingPage"));
const UsagePage = lazy(() => import("@/pages/admin/UsagePage"));
const AdminProjectsPage = lazy(() => import("@/pages/admin/ProjectsPage"));
const AdminTeamsPage = lazy(() => import("@/pages/admin/TeamsPage"));
const ServiceAccountsPage = lazy(() => import("@/pages/admin/ServiceAccountsPage"));
const TeamDetailPage = lazy(() => import("@/pages/admin/TeamDetailPage"));
const SettingsPage = lazy(() => import("@/pages/admin/SettingsPage"));
const AuditLogsPage = lazy(() => import("@/pages/admin/AuditLogsPage"));
const VectorStoresPage = lazy(() => import("@/pages/admin/VectorStoresPage"));
const VectorStoreDetailPage = lazy(() => import("@/pages/admin/VectorStoreDetailPage"));
const SsoConnectionsPage = lazy(() => import("@/pages/admin/SsoConnectionsPage"));
const SsoGroupMappingsPage = lazy(() => import("@/pages/admin/SsoGroupMappingsPage"));
const OrgSsoConfigPage = lazy(() => import("@/pages/admin/OrgSsoConfigPage"));
const ScimConfigPage = lazy(() => import("@/pages/admin/ScimConfigPage"));
const OrgRbacPoliciesPage = lazy(() => import("@/pages/admin/OrgRbacPoliciesPage"));
const SessionInfoPage = lazy(() => import("@/pages/admin/SessionInfoPage"));

function PageLoader() {
  return (
    <div className="flex h-full items-center justify-center">
      <Spinner size="lg" />
    </div>
  );
}

function RootRedirect() {
  const { config } = useConfig();
  return <Navigate to={getFirstEnabledRoute(config.pages)} replace />;
}

export function AppRoutes() {
  return (
    <Routes>
      {/* Root redirect */}
      <Route path="/" element={<RootRedirect />} />

      {/* Login route */}
      <Route
        path="/login"
        element={
          <Suspense fallback={<PageLoader />}>
            <LoginPage />
          </Suspense>
        }
      />

      {/* Auth callback route for OIDC */}
      <Route
        path="/auth/callback"
        element={
          <Suspense fallback={<PageLoader />}>
            <LoginPage />
          </Suspense>
        }
      />

      {/* OAuth-style PKCE consent page for external apps requesting an API key.
          Self-gates authentication so it can preserve query string through login. */}
      <Route
        path="/oauth/authorize"
        element={
          <Suspense fallback={<PageLoader />}>
            <OAuthAuthorizePage />
          </Suspense>
        }
      />

      {/* Protected routes with main AppLayout (chat sidebar) */}
      <Route
        element={
          <RequireAuth>
            <AppLayout />
          </RequireAuth>
        }
      >
        {/* Chat routes */}
        <Route
          path="/chat"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="chat" pageTitle="Chat">
                <ChatPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/chat/:conversationId"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="chat" pageTitle="Chat">
                <ChatPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Projects route */}
        <Route
          path="/projects"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="projects" pageTitle="Projects">
                <ProjectsPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Project detail route */}
        <Route
          path="/projects/:orgSlug/:projectSlug"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="projects" pageTitle="Projects">
                <SelfServiceProjectDetailPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Teams route */}
        <Route
          path="/teams"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="teams" pageTitle="Teams">
                <TeamsPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Knowledge Bases route */}
        <Route
          path="/knowledge-bases"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="knowledge_bases" pageTitle="Knowledge Bases">
                <KnowledgeBasesPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Containers routes */}
        <Route
          path="/containers"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="containers" pageTitle="Containers">
                <ContainersPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/containers/:containerId"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="containers" pageTitle="Containers">
                <ContainerDetailPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* API Keys routes */}
        <Route
          path="/api-keys"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="api_keys" pageTitle="API Keys">
                <ApiKeysPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/api-keys/:keyId"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="api_keys" pageTitle="API Keys">
                <ApiKeyDetailPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Providers route (self-service) */}
        <Route
          path="/providers"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="providers" pageTitle="Providers">
                <MyProvidersPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Templates route (self-service) */}
        <Route
          path="/templates"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="templates" pageTitle="Templates">
                <TemplatesPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Skills route (self-service) */}
        <Route
          path="/skills"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="skills" pageTitle="Skills">
                <SkillsPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Usage route (self-service) */}
        <Route
          path="/usage"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="usage" pageTitle="Usage">
                <MyUsagePage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Studio route */}
        <Route
          path="/studio"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="studio" pageTitle="Studio">
                <StudioPage />
              </PageGuard>
            </Suspense>
          }
        />

        {/* Account settings route */}
        <Route
          path="/account"
          element={
            <Suspense fallback={<PageLoader />}>
              <AccountPage />
            </Suspense>
          }
        />

        {/* Session info route (debugging) */}
        <Route
          path="/session"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.session_info" pageTitle="Session Info">
                <SessionInfoPage />
              </PageGuard>
            </Suspense>
          }
        />
      </Route>

      {/* Admin routes with AdminLayout (admin sidebar) */}
      <Route
        element={
          <RequireAdmin>
            <AdminLayout />
          </RequireAdmin>
        }
      >
        <Route
          path="/admin"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.dashboard" pageTitle="Dashboard">
                <AdminDashboardPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.organizations" pageTitle="Organizations">
                <OrganizationsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations/:slug"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.organizations" pageTitle="Organizations">
                <OrganizationDetailPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations/:orgSlug/projects/:projectSlug"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.projects" pageTitle="Projects">
                <ProjectDetailPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/users"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.users" pageTitle="Users">
                <UsersPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/users/:userId"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.users" pageTitle="Users">
                <UserDetailPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/sso"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.sso" pageTitle="SSO">
                <SsoConnectionsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations/:orgSlug/sso-group-mappings"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.sso" pageTitle="SSO">
                <SsoGroupMappingsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations/:orgSlug/sso-config"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.sso" pageTitle="SSO">
                <OrgSsoConfigPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations/:orgSlug/scim-config"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.sso" pageTitle="SSO">
                <ScimConfigPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations/:orgSlug/rbac-policies"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.organizations" pageTitle="Organizations">
                <OrgRbacPoliciesPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/api-keys"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.api_keys" pageTitle="API Keys">
                <AdminApiKeysPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/providers"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.providers" pageTitle="Providers">
                <ProvidersPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/provider-health"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.provider_health" pageTitle="Provider Health">
                <ProviderHealthPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/provider-health/:providerName"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.provider_health" pageTitle="Provider Health">
                <ProviderDetailPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/pricing"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.pricing" pageTitle="Pricing">
                <PricingPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/usage"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.usage" pageTitle="Usage">
                <UsagePage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/projects"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.projects" pageTitle="Projects">
                <AdminProjectsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/teams"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.teams" pageTitle="Teams">
                <AdminTeamsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/service-accounts"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.service_accounts" pageTitle="Service Accounts">
                <ServiceAccountsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/organizations/:orgSlug/teams/:teamSlug"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.teams" pageTitle="Teams">
                <TeamDetailPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/settings"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.settings" pageTitle="Settings">
                <SettingsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/audit-logs"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.audit_logs" pageTitle="Audit Logs">
                <AuditLogsPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/vector-stores"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.knowledge_bases" pageTitle="Knowledge Bases">
                <VectorStoresPage />
              </PageGuard>
            </Suspense>
          }
        />
        <Route
          path="/admin/vector-stores/:vectorStoreId"
          element={
            <Suspense fallback={<PageLoader />}>
              <PageGuard pageKey="admin.knowledge_bases" pageTitle="Knowledge Bases">
                <VectorStoreDetailPage />
              </PageGuard>
            </Suspense>
          }
        />
      </Route>

      {/* Catch all - redirect to first enabled page */}
      <Route path="*" element={<RootRedirect />} />
    </Routes>
  );
}
