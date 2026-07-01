import { Link, NavLink, useLocation, useNavigate } from "react-router-dom";
import {
  BarChart3,
  BookOpen,
  Box,
  Brain,
  ClipboardPenLine,
  FolderOpen,
  Key,
  Menu,
  MessageSquare,
  Palette,
  Server,
  Shield,
  ToolCase,
  WandSparkles,
  UsersRound,
} from "lucide-react";
import { Button } from "@/components/Button/Button";
import {
  Dropdown,
  DropdownContent,
  DropdownItem,
  DropdownTrigger,
} from "@/components/Dropdown/Dropdown";
import { HadrianIcon } from "@/components/HadrianIcon/HadrianIcon";
import { ThemeToggle } from "@/components/ThemeToggle/ThemeToggle";
import { UserMenu } from "@/components/UserMenu/UserMenu";
import { useWasmSetup } from "@/components/WasmSetup/WasmSetupGuard";
import { useConfig } from "@/config/ConfigProvider";
import { getPageConfig, getFirstEnabledRoute } from "@/components/PageGuard/PageGuard";
import { usePreferences } from "@/preferences/PreferencesProvider";
import { useAuth, hasAdminAccess } from "@/auth";
import { cn } from "@/utils/cn";

export interface NavItem {
  to: string;
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  matchPrefix?: string;
  pageKey?: string;
}

/** A set of nav items collapsed under a single top-bar dropdown (e.g. "Resources"). */
export interface NavGroup {
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  items: NavItem[];
}

export type NavEntry = NavItem | NavGroup;

function isNavGroup(entry: NavEntry): entry is NavGroup {
  return "items" in entry;
}

/** Ordered top-bar navigation. Individual links plus grouped dropdowns. */
export const navEntries: NavEntry[] = [
  { to: "/chat", icon: MessageSquare, label: "Chat", pageKey: "chat" },
  { to: "/studio", icon: Palette, label: "Studio", pageKey: "studio" },
  { to: "/projects", icon: FolderOpen, label: "Projects", pageKey: "projects" },
  { to: "/teams", icon: UsersRound, label: "Teams", pageKey: "teams" },
  { to: "/usage", icon: BarChart3, label: "Usage", pageKey: "usage" },
  {
    label: "Resources",
    icon: ToolCase,
    items: [
      { to: "/api-keys", icon: Key, label: "API Keys", pageKey: "api_keys" },
      { to: "/containers", icon: Box, label: "Containers", pageKey: "containers" },
      { to: "/knowledge-bases", icon: BookOpen, label: "Knowledge", pageKey: "knowledge_bases" },
      { to: "/providers", icon: Server, label: "Providers", pageKey: "providers" },
      { to: "/skills", icon: Brain, label: "Skills", pageKey: "skills" },
      { to: "/templates", icon: ClipboardPenLine, label: "Templates", pageKey: "templates" },
    ],
  },
];

/** Flattened list of every destination, consumed by the mobile menu in UserMenu. */
export const navItems: NavItem[] = navEntries.flatMap((entry) =>
  isNavGroup(entry) ? entry.items : [entry]
);

export const adminNavItem: NavItem = {
  to: "/admin",
  icon: Shield,
  label: "Admin",
  matchPrefix: "/admin",
};

interface HeaderProps {
  onMenuClick?: () => void;
  showMenuButton?: boolean;
  className?: string;
}

export function Header({ onMenuClick, showMenuButton = false, className }: HeaderProps) {
  const { config } = useConfig();
  const { resolvedTheme } = usePreferences();
  const { user } = useAuth();
  const { isWasm, openSetupWizard } = useWasmSetup();
  const location = useLocation();

  // Determine which logo to use based on theme
  const logoUrl =
    resolvedTheme === "dark" && config?.branding.logo_dark_url
      ? config.branding.logo_dark_url
      : config?.branding.logo_url;

  const isItemVisible = (item: NavItem) => {
    if (!item.pageKey) return true;
    return getPageConfig(config.pages, item.pageKey).status !== "disabled";
  };

  // Filter nav entries by page visibility; drop groups left with no visible items.
  const visibleNavEntries = navEntries.reduce<NavEntry[]>((acc, entry) => {
    if (isNavGroup(entry)) {
      const items = entry.items.filter(isItemVisible);
      if (items.length > 0) acc.push({ ...entry, items });
    } else if (isItemVisible(entry)) {
      acc.push(entry);
    }
    return acc;
  }, []);

  // Only show admin nav if admin is enabled AND user has admin access
  const showAdmin = config?.admin.enabled && hasAdminAccess(user);
  const allNavEntries = showAdmin ? [...visibleNavEntries, adminNavItem] : visibleNavEntries;

  const isActive = (item: NavItem) => {
    if (item.matchPrefix) {
      return location.pathname.startsWith(item.matchPrefix);
    }
    return location.pathname === item.to || location.pathname.startsWith(item.to + "/");
  };

  return (
    <header
      className={cn(
        "sticky top-0 z-40 flex h-14 items-center justify-between border-b bg-background/80 backdrop-blur-sm px-4",
        className
      )}
    >
      {/* Left: Logo + Menu button (mobile) */}
      <div className="flex items-center gap-2">
        {showMenuButton && (
          <Button variant="ghost" size="icon" onClick={onMenuClick} className="lg:hidden">
            <Menu className="h-5 w-5" />
            <span className="sr-only">Toggle menu</span>
          </Button>
        )}
        <Link to={getFirstEnabledRoute(config.pages)} className="flex items-center gap-2.5">
          {logoUrl ? (
            <img
              src={logoUrl}
              alt={config?.branding.title || "Logo"}
              className="h-8 w-8 rounded-lg object-contain"
            />
          ) : (
            <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary/10">
              <HadrianIcon size={24} className="text-primary" />
            </div>
          )}
          <span className="hidden md:inline font-semibold text-foreground tracking-tight">
            {config?.branding.title || "Hadrian"}
          </span>
        </Link>
      </div>

      {/* Center: Navigation tabs */}
      <nav
        className="hidden xl:flex items-center gap-1"
        role="navigation"
        aria-label="Main navigation"
      >
        {allNavEntries.map((entry) => {
          if (isNavGroup(entry)) {
            return <NavGroupMenu key={entry.label} group={entry} isActive={isActive} />;
          }
          const Icon = entry.icon;
          const active = isActive(entry);
          return (
            <NavLink
              key={entry.to}
              to={entry.to}
              className={cn(
                "flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm font-medium transition-colors",
                "hover:bg-muted hover:text-foreground",
                active ? "bg-muted text-foreground" : "text-muted-foreground"
              )}
            >
              <Icon className="h-4 w-4" aria-hidden="true" />
              <span>{entry.label}</span>
            </NavLink>
          );
        })}
      </nav>

      {/* Right: Theme toggle and user menu */}
      <div className="flex items-center gap-2">
        <ThemeToggle />
        {isWasm && (
          <Button
            variant="outline"
            size="sm"
            className="gap-1.5 border-dashed text-muted-foreground hover:text-foreground"
            onClick={openSetupWizard}
            aria-label="Setup Wizard"
          >
            <WandSparkles className="h-3.5 w-3.5" />
            Setup
          </Button>
        )}
        <UserMenu />
      </div>
    </header>
  );
}

/** Top-bar dropdown that collapses a group of nav items (e.g. "Resources"). */
function NavGroupMenu({
  group,
  isActive,
}: {
  group: NavGroup;
  isActive: (item: NavItem) => boolean;
}) {
  const navigate = useNavigate();
  const Icon = group.icon;
  const groupActive = group.items.some(isActive);

  return (
    <Dropdown>
      <DropdownTrigger
        variant="ghost"
        className={cn(
          "gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium",
          "hover:bg-muted hover:text-foreground",
          groupActive ? "bg-muted text-foreground" : "text-muted-foreground"
        )}
      >
        <Icon className="h-4 w-4" aria-hidden="true" />
        <span>{group.label}</span>
      </DropdownTrigger>
      <DropdownContent align="start" className="w-48">
        {group.items.map((item) => {
          const ItemIcon = item.icon;
          return (
            <DropdownItem
              key={item.to}
              onClick={() => navigate(item.to)}
              className={cn(isActive(item) && "bg-accent text-accent-foreground")}
            >
              <ItemIcon className="mr-2 h-4 w-4" />
              {item.label}
            </DropdownItem>
          );
        })}
      </DropdownContent>
    </Dropdown>
  );
}
