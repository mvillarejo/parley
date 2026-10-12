/** Stable colour slot per speaker: Me is the brand teal, Them is amber,
 * numbered remote voices cycle through the remaining slots. */
export const SPEAKER_SLOTS = 6;

export function speakerSlot(speaker: string): number {
  if (speaker === "Me") return 0;
  if (speaker === "Them") return 1;
  const match = /^Them (\d+)$/.exec(speaker);
  if (match) {
    const n = Number(match[1]);
    return 1 + ((n - 1) % (SPEAKER_SLOTS - 1));
  }
  let hash = 0;
  for (const ch of speaker) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  return 1 + (hash % (SPEAKER_SLOTS - 1));
}

export function speakerClass(speaker: string): string {
  return `speaker-${speakerSlot(speaker)}`;
}
