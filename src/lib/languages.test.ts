import { describe, expect, it } from "vitest";
import { filterLanguages, languageSummary, toggleLanguage } from "./languages";

describe("languages", () => {
  it("summarizes the selection", () => {
    expect(languageSummary([])).toBe("Auto-detect");
    expect(languageSummary(["es", "en"])).toBe("English, Spanish");
    expect(languageSummary(["en", "es", "fr", "de"])).toBe(
      "English, Spanish +2",
    );
    expect(languageSummary(["xx"])).toBe("Auto-detect");
  });

  it("filters by label or exact code", () => {
    expect(filterLanguages("span").map((l) => l.code)).toEqual(["es"]);
    expect(filterLanguages(" DE ").map((l) => l.code)).toEqual(["de"]);
    expect(filterLanguages("").length).toBeGreaterThan(10);
  });

  it("toggles without duplicates and allows an empty selection", () => {
    expect(toggleLanguage(["en"], "en", true)).toEqual(["en"]);
    expect(toggleLanguage(["en"], "fr", true)).toEqual(["en", "fr"]);
    expect(toggleLanguage(["en"], "en", false)).toEqual([]);
  });
});
