import type { ColorPalette } from "./types";

/** Permissive color literal: hex, rgb()/hsl()/oklch()/var(), CSS keyword.
 *  Rejects anything containing CSS control chars (`{`, `}`, `;`, `<`, etc.)
 *  so a misconfigured branding payload can't break out of the rule and
 *  inject arbitrary CSS into the page. Underscore is allowed because custom
 *  property names in var() references may contain it (var(--brand_primary)). */
const COLOR_RE = /^[a-zA-Z0-9#%(),.\s\-/_]+$/;

export function isSafeColor(value: string | undefined): value is string {
  return (
    typeof value === "string" && value.length > 0 && value.length < 200 && COLOR_RE.test(value)
  );
}

/** Builds the effective dark palette. Identity keys (primary,
 *  primary_foreground) inherit from the light palette; surface keys are
 *  mode-scoped and never inherit — dark mode falls back to the built-in
 *  dark theme for them. */
export function mergeDarkPalette(
  colors: ColorPalette,
  colorsDark: ColorPalette | null
): ColorPalette {
  // Explicit undefined checks (not spread order) so a present-but-undefined
  // key in colorsDark can't clobber an inherited identity key.
  const dark: ColorPalette = { ...(colorsDark ?? {}) };
  if (dark.primary === undefined) dark.primary = colors.primary;
  if (dark.primary_foreground === undefined) dark.primary_foreground = colors.primary_foreground;
  return dark;
}

interface ColorCssOptions {
  /** Derive --color-accent-foreground from primary. True in light mode; in
   *  dark mode only when the dark palette explicitly sets primary (an
   *  inherited light primary as text on the stock dark accent surface
   *  risks unreadable contrast). */
  deriveAccentForeground: boolean;
}

/**
 * Generates CSS variable overrides from a color palette
 */
export function generateColorCss(
  colors: ColorPalette,
  selector: string,
  options: ColorCssOptions
): string {
  const rules: string[] = [];

  if (isSafeColor(colors.primary)) {
    rules.push(`--color-primary: ${colors.primary};`);
    rules.push(`--color-ring: ${colors.primary};`);
    if (options.deriveAccentForeground) {
      // Set accent-foreground to primary color for consistent branding on selected items
      rules.push(`--color-accent-foreground: ${colors.primary};`);
    }
  }
  if (isSafeColor(colors.primary_foreground)) {
    rules.push(`--color-primary-foreground: ${colors.primary_foreground};`);
  } else if (isSafeColor(colors.primary)) {
    // Default to white if primary is set but primary_foreground is not
    rules.push(`--color-primary-foreground: #ffffff;`);
  }
  if (isSafeColor(colors.secondary)) {
    rules.push(`--color-secondary: ${colors.secondary};`);
  }
  if (isSafeColor(colors.secondary_foreground)) {
    rules.push(`--color-secondary-foreground: ${colors.secondary_foreground};`);
  }
  if (isSafeColor(colors.accent)) {
    rules.push(`--color-accent: ${colors.accent};`);
  }
  if (isSafeColor(colors.background)) {
    rules.push(`--color-background: ${colors.background};`);
  }
  if (isSafeColor(colors.foreground)) {
    rules.push(`--color-foreground: ${colors.foreground};`);
  }
  if (isSafeColor(colors.muted)) {
    rules.push(`--color-muted: ${colors.muted};`);
  }
  if (isSafeColor(colors.border)) {
    rules.push(`--color-border: ${colors.border};`);
    rules.push(`--color-input: ${colors.border};`);
  }

  if (rules.length === 0) return "";
  return `${selector} { ${rules.join(" ")} }`;
}

/** font-weight for @font-face: a single weight or a variable-font
 *  "min max" range like "100 900". Falls back to 400 so an invalid value
 *  can't break out of the rule. */
const FONT_WEIGHT_RE = /^\d{1,4}(?:\s+\d{1,4})?$/;

export function normalizeFontWeight(weight: string | undefined): string {
  const trimmed = typeof weight === "string" ? weight.trim() : "";
  if (!FONT_WEIGHT_RE.test(trimmed)) return "400";
  return trimmed.replace(/\s+/g, " ");
}

function warnUnsafeColors(palette: ColorPalette | null, section: string): void {
  if (!palette) return;
  for (const [key, value] of Object.entries(palette)) {
    if (typeof value === "string" && !isSafeColor(value)) {
      console.warn(`Ignoring unsafe branding color ${section}.${key}:`, value);
    }
  }
}

/** Full branding stylesheet body: light rule scoped to :root:not(.dark) so
 *  it can't leak into dark mode, dark rule under .dark from the merged dark
 *  palette. Returns "" if nothing passes validation. */
export function buildBrandingColorCss(
  colors: ColorPalette,
  colorsDark: ColorPalette | null
): string {
  warnUnsafeColors(colors, "colors");
  warnUnsafeColors(colorsDark, "colors_dark");
  const lightCss = generateColorCss(colors, ":root:not(.dark)", {
    deriveAccentForeground: true,
  });
  const darkCss = generateColorCss(mergeDarkPalette(colors, colorsDark), ".dark", {
    deriveAccentForeground: isSafeColor(colorsDark?.primary),
  });
  return [lightCss, darkCss].filter(Boolean).join("\n");
}
