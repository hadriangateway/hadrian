import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import type { UiConfig, ColorPalette, FontsConfig, CustomFont } from "./types";
import { buildBrandingColorCss, normalizeFontWeight } from "./brandingCss";
import { defaultConfig, defaultPagesConfig, getApiBaseUrl } from "./defaults";

interface ConfigContextValue {
  config: UiConfig;
  isLoading: boolean;
  error: Error | null;
  apiBaseUrl: string;
}

const ConfigContext = createContext<ConfigContextValue | null>(null);

const BRANDING_STYLE_ID = "hadrian-branding-colors";
const BRANDING_FONTS_STYLE_ID = "hadrian-branding-fonts";

/** Validate a font-family name. Quotes/braces/semicolons in here would let
 *  an attacker close the `font-family` declaration and inject other rules. */
const FONT_NAME_RE = /^[a-zA-Z0-9 \-_]+$/;

function isSafeFontName(value: string | undefined): value is string {
  return (
    typeof value === "string" && value.length > 0 && value.length < 100 && FONT_NAME_RE.test(value)
  );
}

/** Only accept absolute https/data URLs. Returns the normalized href (never
 *  the raw input) so it can be interpolated into a double-quoted CSS url()
 *  string. Quotes and backslashes could terminate that string; https hrefs
 *  percent-encode them, but data: URLs have opaque paths where they survive
 *  normalization, so reject any that remain. */
function safeFontUrl(value: string | undefined): string | null {
  if (typeof value !== "string" || value.length === 0 || value.length > 2048) return null;
  try {
    const url = new URL(value, window.location.origin);
    if (url.protocol !== "https:" && url.protocol !== "data:") return null;
    if (/["\\\n\r]/.test(url.href)) return null;
    return url.href;
  } catch {
    return null;
  }
}

function isSafeFontUrl(value: string | undefined): value is string {
  return safeFontUrl(value) !== null;
}

/**
 * Injects branding colors as CSS custom properties
 */
function injectBrandingColors(colors: ColorPalette, colorsDark: ColorPalette | null): void {
  // Remove existing branding style if present
  const existing = document.getElementById(BRANDING_STYLE_ID);
  if (existing) {
    existing.remove();
  }

  const css = buildBrandingColorCss(colors, colorsDark);
  if (!css) return;

  const style = document.createElement("style");
  style.id = BRANDING_STYLE_ID;
  style.textContent = css;
  document.head.appendChild(style);
}

/**
 * Generates @font-face rules for custom fonts. Skips entries whose name or URL
 * fails validation; an invalid entry is logged and dropped rather than
 * inlined verbatim into the stylesheet (where it could break out of the rule).
 */
function generateFontFaceRules(customFonts: CustomFont[]): string {
  return customFonts
    .flatMap((font) => {
      const url = safeFontUrl(font.url);
      if (!isSafeFontName(font.name) || url === null) {
        console.warn("Ignoring branded custom font with unsafe name or URL", font);
        return [];
      }
      const weight = normalizeFontWeight(font.weight);
      const style = font.style === "italic" || font.style === "oblique" ? font.style : "normal";
      return [
        `@font-face {
  font-family: "${font.name}";
  src: url("${url}");
  font-weight: ${weight};
  font-style: ${style};
  font-display: swap;
}`,
      ];
    })
    .join("\n\n");
}

/**
 * Generates CSS variable overrides for font families
 */
function generateFontCss(fonts: FontsConfig): string {
  const rules: string[] = [];

  // Build font stacks with fallbacks
  const sansStack =
    'ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif';
  const monoStack =
    'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Monaco, Consolas, "Liberation Mono", monospace';

  if (isSafeFontName(fonts.body)) {
    rules.push(`--font-sans: "${fonts.body}", ${sansStack};`);
  }
  if (isSafeFontName(fonts.heading)) {
    rules.push(`--font-heading: "${fonts.heading}", ${sansStack};`);
  }
  if (isSafeFontName(fonts.mono)) {
    rules.push(`--font-mono: "${fonts.mono}", ${monoStack};`);
  }

  if (rules.length === 0) return "";
  return `:root { ${rules.join(" ")} }`;
}

/**
 * Injects branding fonts as @font-face rules and CSS custom properties
 */
function injectBrandingFonts(fonts: FontsConfig | null): void {
  // Remove existing font style if present
  const existing = document.getElementById(BRANDING_FONTS_STYLE_ID);
  if (existing) {
    existing.remove();
  }

  if (!fonts) return;

  const fontFaceRules = fonts.custom ? generateFontFaceRules(fonts.custom) : "";
  const fontVariables = generateFontCss(fonts);

  const css = [fontFaceRules, fontVariables].filter(Boolean).join("\n\n");
  if (!css) return;

  const style = document.createElement("style");
  style.id = BRANDING_FONTS_STYLE_ID;
  style.textContent = css;
  document.head.appendChild(style);
}

interface ConfigProviderProps {
  children: ReactNode;
}

export function ConfigProvider({ children }: ConfigProviderProps) {
  const [config, setConfig] = useState<UiConfig>(defaultConfig);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const apiBaseUrl = getApiBaseUrl();

  useEffect(() => {
    async function fetchConfig() {
      try {
        const response = await fetch(`${apiBaseUrl}/admin/v1/ui/config`);
        if (response.ok) {
          const data = (await response.json()) as UiConfig;
          // Deep-merge pages so partial server responses fill in defaults
          const mergedPages = {
            ...defaultPagesConfig,
            ...data.pages,
            admin: { ...defaultPagesConfig.admin, ...data.pages?.admin },
          };
          setConfig({ ...data, pages: mergedPages });
        } else {
          // Use defaults if endpoint is not available
          console.warn("UI config endpoint not available, using defaults");
        }
      } catch (err) {
        console.warn("Failed to fetch UI config, using defaults:", err);
        setError(err instanceof Error ? err : new Error("Failed to fetch config"));
      } finally {
        setIsLoading(false);
      }
    }

    fetchConfig();
  }, [apiBaseUrl]);

  // Update document title, favicon, colors, and fonts based on config
  useEffect(() => {
    document.title = config.branding.title;
    if (config.branding.favicon_url && isSafeFontUrl(config.branding.favicon_url)) {
      const favicon = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
      if (favicon) {
        favicon.href = config.branding.favicon_url;
      }
    }
    // Inject branding colors as CSS custom properties
    injectBrandingColors(config.branding.colors, config.branding.colors_dark);
    // Inject branding fonts as @font-face rules and CSS custom properties
    injectBrandingFonts(config.branding.fonts);
  }, [config.branding]);

  return (
    <ConfigContext.Provider value={{ config, isLoading, error, apiBaseUrl }}>
      {children}
    </ConfigContext.Provider>
  );
}

export function useConfig(): ConfigContextValue {
  const context = useContext(ConfigContext);
  if (!context) {
    throw new Error("useConfig must be used within a ConfigProvider");
  }
  return context;
}
