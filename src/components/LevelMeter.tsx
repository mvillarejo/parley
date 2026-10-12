/** Horizontal level bar for one track (value 0..1). */
export function LevelMeter({
  label,
  value,
  tone,
  disabled = false,
}: {
  label: string;
  value: number;
  tone: "me" | "them";
  disabled?: boolean;
}) {
  const pct = Math.round(Math.min(1, Math.max(0, value)) * 100);
  return (
    <div className={`meter meter--${tone}${disabled ? " meter--off" : ""}`}>
      <span className="meter-label">{label}</span>
      <div
        className="meter-track"
        role="meter"
        aria-label={`${label} level`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={disabled ? 0 : pct}
      >
        <div
          className="meter-fill"
          style={{ transform: `scaleX(${disabled ? 0 : pct / 100})` }}
        />
      </div>
    </div>
  );
}
