"use client";

import { useEffect, useState } from "react";

interface StoryEmbedProps {
  /** Story ID in format "category-component--story", e.g. "ui-button--primary" */
  storyId: string;
  /** Height of the iframe */
  height?: number | string;
  /** Optional title for accessibility */
  title?: string;
}

/**
 * Embeds a Storybook story in an iframe.
 * Stories are served from /storybook/ (symlinked to ui/storybook-static).
 */
export function StoryEmbed({ storyId, height = 200, title }: StoryEmbedProps) {
  // `null` until the real theme is read on the client. While it's null the iframe
  // gets no `src`, so it loads exactly once — already in the correct theme —
  // rather than loading `theme:light` first and then reloading `theme:dark`. That
  // double load showed up as a light-mode flash, or a stuck-light embed when a
  // tab was switched mid-reload, since the theme comes solely from the URL global.
  const [theme, setTheme] = useState<"light" | "dark" | null>(null);

  // Sync with the Fumadocs theme (a class on <html>) on mount and on every change.
  useEffect(() => {
    const root = document.documentElement;
    const updateTheme = () => {
      setTheme(root.classList.contains("dark") ? "dark" : "light");
    };

    updateTheme();

    const observer = new MutationObserver(updateTheme);
    observer.observe(root, { attributes: true, attributeFilter: ["class"] });

    return () => observer.disconnect();
  }, []);

  const basePath = process.env.DOCS_BASE_PATH || "";
  const src =
    theme === null
      ? undefined
      : `${basePath}/storybook/iframe.html?id=${storyId}&viewMode=story&globals=theme:${theme}`;

  return (
    <iframe
      src={src}
      title={title || `Storybook: ${storyId}`}
      style={{
        width: "100%",
        height: typeof height === "number" ? `${height}px` : height,
        border: "1px solid var(--fd-border)",
        borderRadius: "8px",
        // `--fd-card` adapts to the active theme, so the placeholder box matches
        // before `theme` resolves; then we pin Storybook's own canvas colours.
        background: theme === null ? "var(--fd-card)" : theme === "dark" ? "#09090b" : "#fafafa",
      }}
      loading="lazy"
    />
  );
}
