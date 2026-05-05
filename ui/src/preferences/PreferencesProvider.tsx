import { createContext, useContext, useEffect, useCallback, useState, type ReactNode } from "react";
import { useLocalStorage } from "@/hooks/useLocalStorage";
import { usePrefersDarkMode } from "@/hooks/useMediaQuery";
import type { UserPreferences, Theme } from "./types";
import { defaultPreferences } from "./types";

interface PreferencesContextValue {
  preferences: UserPreferences;
  setPreferences: (prefs: Partial<UserPreferences>) => void;
  setTheme: (theme: Theme) => void;
  resolvedTheme: "light" | "dark";
}

const PreferencesContext = createContext<PreferencesContextValue | null>(null);

const STORAGE_KEY = "hadrian-preferences";

const isTheme = (value: unknown): value is Theme =>
  value === "light" || value === "dark" || value === "system";

function readThemeFromUrl(): Theme | null {
  if (typeof window === "undefined") return null;
  const value = new URLSearchParams(window.location.search).get("theme");
  return isTheme(value) ? value : null;
}

interface PreferencesProviderProps {
  children: ReactNode;
}

export function PreferencesProvider({ children }: PreferencesProviderProps) {
  const [preferences, setStoredPreferences] = useLocalStorage<UserPreferences>(
    STORAGE_KEY,
    defaultPreferences
  );

  // Non-persisting theme override sourced from `?theme=` on load and
  // `postMessage({ type: "hadrian-theme", theme: "light"|"dark"|"system"|null })`
  // from a parent frame. Intentionally never written to localStorage.
  const [themeOverride, setThemeOverride] = useState<Theme | null>(readThemeFromUrl);

  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      const data = event.data;
      if (!data || typeof data !== "object" || data.type !== "hadrian-theme") return;
      if (data.theme === null) {
        setThemeOverride(null);
      } else if (isTheme(data.theme)) {
        setThemeOverride(data.theme);
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  const prefersDark = usePrefersDarkMode();

  const activeTheme = themeOverride ?? preferences.theme;
  const resolvedTheme = activeTheme === "system" ? (prefersDark ? "dark" : "light") : activeTheme;

  // Apply theme to document
  useEffect(() => {
    const root = document.documentElement;
    root.classList.remove("light", "dark");
    root.classList.add(resolvedTheme);
  }, [resolvedTheme]);

  const setPreferences = useCallback(
    (updates: Partial<UserPreferences>) => {
      setStoredPreferences((prev) => ({ ...prev, ...updates }));
    },
    [setStoredPreferences]
  );

  // Explicit in-app toggle clears any active override so the user regains control.
  const setTheme = useCallback(
    (theme: Theme) => {
      setThemeOverride(null);
      setPreferences({ theme });
    },
    [setPreferences]
  );

  return (
    <PreferencesContext.Provider
      value={{
        preferences,
        setPreferences,
        setTheme,
        resolvedTheme,
      }}
    >
      {children}
    </PreferencesContext.Provider>
  );
}

export function usePreferences(): PreferencesContextValue {
  const context = useContext(PreferencesContext);
  if (!context) {
    throw new Error("usePreferences must be used within a PreferencesProvider");
  }
  return context;
}
