/** Parley's line icons (24×24, 1.75px stroke, currentColor). */
export type IconName =
  | "library"
  | "book"
  | "wave"
  | "settings"
  | "plug"
  | "shield"
  | "mic"
  | "speaker"
  | "record"
  | "pause"
  | "stop"
  | "play"
  | "search"
  | "copy"
  | "folder"
  | "trash"
  | "refresh"
  | "check"
  | "close"
  | "external"
  | "warning"
  | "star"
  | "note"
  | "pencil";

const PATHS: Record<IconName, string[]> = {
  library: ["M4 5h16", "M4 12h16", "M4 19h10", "M18 16v6", "M15 19h6"],
  book: [
    "M5 4.5A1.5 1.5 0 0 1 6.5 3H19v15H6.5A1.5 1.5 0 0 0 5 19.5z",
    "M5 19.5A1.5 1.5 0 0 0 6.5 21H19",
    "M9 7.5h6",
  ],
  wave: ["M3 12h2", "M7 8v8", "M11 4v16", "M15 7v10", "M19 10v4"],
  settings: [
    "M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z",
    "M19.4 13.5a7.6 7.6 0 0 0 0-3l2-1.6-2-3.4-2.4 1a7.5 7.5 0 0 0-2.6-1.5L14 2.5h-4l-.4 2.5A7.5 7.5 0 0 0 7 6.5l-2.4-1-2 3.4 2 1.6a7.6 7.6 0 0 0 0 3l-2 1.6 2 3.4 2.4-1a7.5 7.5 0 0 0 2.6 1.5l.4 2.5h4l.4-2.5a7.5 7.5 0 0 0 2.6-1.5l2.4 1 2-3.4z",
  ],
  plug: ["M9 3v5", "M15 3v5", "M6 8h12v3a6 6 0 0 1-12 0z", "M12 17v4"],
  shield: [
    "M12 3l7.5 3v5.5c0 4.5-3.2 8.2-7.5 9.5-4.3-1.3-7.5-5-7.5-9.5V6z",
    "M9 12l2 2 4-4",
  ],
  mic: [
    "M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z",
    "M5.5 11a6.5 6.5 0 0 0 13 0",
    "M12 17.5V21",
  ],
  speaker: [
    "M4 9.5h3.5L12 5.5v13l-4.5-4H4z",
    "M15.5 9a4 4 0 0 1 0 6",
    "M18 6.5a7.5 7.5 0 0 1 0 11",
  ],
  record: ["M12 18a6 6 0 1 0 0-12 6 6 0 0 0 0 12z"],
  pause: ["M9 6v12", "M15 6v12"],
  stop: ["M7 7h10v10H7z"],
  play: ["M8 5.5v13l10.5-6.5z"],
  search: [
    "M10.5 17a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13z",
    "M20 20l-4.8-4.8",
  ],
  copy: ["M9 9h11v11H9z", "M5 15H4V4h11v1"],
  folder: [
    "M3.5 6.5A1.5 1.5 0 0 1 5 5h4.5l2 2.5H19a1.5 1.5 0 0 1 1.5 1.5v8.5A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5z",
  ],
  trash: [
    "M4.5 7h15",
    "M9.5 7V4.5h5V7",
    "M6.5 7l1 13h9l1-13",
    "M10.5 11v5",
    "M13.5 11v5",
  ],
  refresh: ["M19.5 12a7.5 7.5 0 1 1-2.2-5.3", "M19.5 4v4.5H15"],
  check: ["M5 12.5l4.5 4.5L19 7.5"],
  close: ["M6 6l12 12", "M18 6L6 18"],
  external: ["M14 4h6v6", "M20 4l-9 9", "M18 14v5.5H4.5V6H10"],
  warning: ["M12 4l9 16H3z", "M12 10v4", "M12 17h.01"],
  star: [
    "M12 3.5l2.6 5.3 5.9.9-4.25 4.1 1 5.85L12 16.9l-5.25 2.75 1-5.85L3.5 9.7l5.9-.9z",
  ],
  note: [
    "M5 4.5h14v10.5l-4.5 4.5H5z",
    "M14.5 19.5V15H19",
    "M8.5 9h7",
    "M8.5 12.5h4",
  ],
  pencil: ["M4.5 19.5l1-4L15.5 5.5l3 3-10 10z", "M13.5 7.5l3 3"],
};

const FILLED: Partial<Record<IconName, true>> = {
  record: true,
  stop: true,
  play: true,
};

export function Icon({
  name,
  size = 20,
  className,
}: {
  name: IconName;
  size?: number;
  className?: string;
}) {
  const filled = FILLED[name];
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth={filled ? 0 : 1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      {PATHS[name].map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
}
