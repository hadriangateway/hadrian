import { describe, expect, it } from "vitest";

import {
  buildBrandingColorCss,
  isSafeColor,
  mergeDarkPalette,
  normalizeFontWeight,
} from "../brandingCss";

describe("isSafeColor", () => {
  it("accepts common color literals", () => {
    expect(isSafeColor("#aabbcc")).toBe(true);
    expect(isSafeColor("oklch(0.5 0.1 200)")).toBe(true);
    expect(isSafeColor("rgb(1, 2, 3)")).toBe(true);
    expect(isSafeColor("rebeccapurple")).toBe(true);
  });

  it("rejects values that could break out of a CSS rule", () => {
    expect(isSafeColor("red;}body{background:url(x)")).toBe(false);
    expect(isSafeColor("{")).toBe(false);
    expect(isSafeColor("")).toBe(false);
    expect(isSafeColor(undefined)).toBe(false);
    expect(isSafeColor("a".repeat(200))).toBe(false);
  });
});

describe("normalizeFontWeight", () => {
  it("accepts single weights", () => {
    expect(normalizeFontWeight("400")).toBe("400");
    expect(normalizeFontWeight("700")).toBe("700");
  });

  it("accepts variable-font ranges", () => {
    expect(normalizeFontWeight("100 900")).toBe("100 900");
    expect(normalizeFontWeight(" 100   900 ")).toBe("100 900");
  });

  it("falls back to 400 for invalid values", () => {
    expect(normalizeFontWeight("bold")).toBe("400");
    expect(normalizeFontWeight("400; } body { color: red")).toBe("400");
    expect(normalizeFontWeight("")).toBe("400");
    expect(normalizeFontWeight(undefined)).toBe("400");
  });
});

describe("mergeDarkPalette", () => {
  it("inherits identity keys and never surface keys when no dark palette is set", () => {
    const merged = mergeDarkPalette(
      {
        primary: "#112233",
        primary_foreground: "#ffffff",
        background: "#fafafa",
        border: "#e4e4e7",
      },
      null
    );
    expect(merged.primary).toBe("#112233");
    expect(merged.primary_foreground).toBe("#ffffff");
    expect(merged.background).toBeUndefined();
    expect(merged.border).toBeUndefined();
  });

  it("prefers explicit dark values over inherited ones", () => {
    const merged = mergeDarkPalette({ primary: "#111111" }, { primary: "#eeeeee" });
    expect(merged.primary).toBe("#eeeeee");
  });

  it("inherits primary and primary_foreground independently", () => {
    const merged = mergeDarkPalette(
      { primary: "#111111", primary_foreground: "#000000" },
      { primary: "#eeeeee" }
    );
    expect(merged.primary).toBe("#eeeeee");
    expect(merged.primary_foreground).toBe("#000000");
  });

  it("passes dark surface keys through untouched", () => {
    const merged = mergeDarkPalette({ background: "#ffffff" }, { background: "#000000" });
    expect(merged.background).toBe("#000000");
  });
});

describe("buildBrandingColorCss", () => {
  it("brands both modes from primary alone, without dark accent-foreground", () => {
    const css = buildBrandingColorCss({ primary: "#112233" }, null);
    const [light, dark] = css.split("\n");

    expect(light.startsWith(":root:not(.dark) {")).toBe(true);
    expect(light).toContain("--color-primary: #112233;");
    expect(light).toContain("--color-ring: #112233;");
    expect(light).toContain("--color-accent-foreground: #112233;");
    expect(light).toContain("--color-primary-foreground: #ffffff;");

    expect(dark.startsWith(".dark {")).toBe(true);
    expect(dark).toContain("--color-primary: #112233;");
    expect(dark).toContain("--color-ring: #112233;");
    expect(dark).toContain("--color-primary-foreground: #ffffff;");
    expect(dark).not.toContain("--color-accent-foreground");
  });

  it("derives dark accent-foreground when the dark palette sets primary explicitly", () => {
    const css = buildBrandingColorCss({ primary: "#112233" }, { primary: "#abcdef" });
    const dark = css.split("\n")[1];
    expect(dark).toContain("--color-primary: #abcdef;");
    expect(dark).toContain("--color-ring: #abcdef;");
    expect(dark).toContain("--color-accent-foreground: #abcdef;");
  });

  it("keeps inherited primary_foreground over the white default in dark mode", () => {
    const css = buildBrandingColorCss(
      { primary: "#112233", primary_foreground: "#000000" },
      { primary: "#abcdef" }
    );
    const dark = css.split("\n")[1];
    expect(dark).toContain("--color-primary-foreground: #000000;");
    expect(dark).not.toContain("--color-primary-foreground: #ffffff;");
  });

  it("scopes surface keys to light mode and emits no dark rule for them", () => {
    const css = buildBrandingColorCss({ background: "#ffffff", border: "#eeeeee" }, null);
    const rules = css.split("\n");
    expect(rules).toHaveLength(1);
    expect(rules[0].startsWith(":root:not(.dark) {")).toBe(true);
    expect(css).toContain("--color-background: #ffffff;");
    expect(css).toContain("--color-border: #eeeeee;");
    expect(css).toContain("--color-input: #eeeeee;");
  });

  it("derives input from border per mode", () => {
    const css = buildBrandingColorCss({ primary: "#112233" }, { border: "#333333" });
    const dark = css.split("\n")[1];
    expect(dark).toContain("--color-border: #333333;");
    expect(dark).toContain("--color-input: #333333;");
  });

  it("drops unsafe values from both rules", () => {
    expect(buildBrandingColorCss({ primary: "red;} body{background:url(x)" }, null)).toBe("");
  });

  it("suppresses inheritance when the explicit dark primary is invalid", () => {
    const css = buildBrandingColorCss({ primary: "#112233" }, { primary: "bad;value" });
    const rules = css.split("\n");
    expect(rules).toHaveLength(1);
    expect(rules[0].startsWith(":root:not(.dark) {")).toBe(true);
  });

  it("returns an empty string for empty palettes", () => {
    expect(buildBrandingColorCss({}, null)).toBe("");
    expect(buildBrandingColorCss({}, {})).toBe("");
  });
});
