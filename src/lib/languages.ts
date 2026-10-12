export const EUROPEAN_LANGUAGES = [
  { code: "en", label: "English" },
  { code: "es", label: "Spanish" },
  { code: "fr", label: "French" },
  { code: "de", label: "German" },
  { code: "it", label: "Italian" },
  { code: "pt", label: "Portuguese" },
  { code: "nl", label: "Dutch" },
  { code: "pl", label: "Polish" },
  { code: "cs", label: "Czech" },
  { code: "da", label: "Danish" },
  { code: "sv", label: "Swedish" },
  { code: "no", label: "Norwegian" },
  { code: "fi", label: "Finnish" },
  { code: "ro", label: "Romanian" },
  { code: "hu", label: "Hungarian" },
  { code: "el", label: "Greek" },
  { code: "tr", label: "Turkish" },
  { code: "uk", label: "Ukrainian" },
  { code: "ru", label: "Russian" },
] as const;

export type LanguageOption = (typeof EUROPEAN_LANGUAGES)[number];

export function filterLanguages(search: string): LanguageOption[] {
  const needle = search.trim().toLowerCase();
  return EUROPEAN_LANGUAGES.filter(
    (language) =>
      language.label.toLowerCase().includes(needle) || language.code === needle,
  );
}

/** "Auto-detect", "English, Spanish" or "English, Spanish +2". */
export function languageSummary(codes: string[]): string {
  const labels = EUROPEAN_LANGUAGES.filter((language) =>
    codes.includes(language.code),
  ).map((language) => language.label);
  if (labels.length === 0) return "Auto-detect";
  if (labels.length <= 2) return labels.join(", ");
  return `${labels[0]}, ${labels[1]} +${labels.length - 2}`;
}

/** No language selected means the provider detects it. */
export function toggleLanguage(
  codes: string[],
  code: string,
  checked: boolean,
): string[] {
  if (checked) return codes.includes(code) ? codes : [...codes, code];
  return codes.filter((c) => c !== code);
}
