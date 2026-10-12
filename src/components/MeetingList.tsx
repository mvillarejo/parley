import type { Meeting, TranscriptionProgress } from "../types";
import { humanDuration, timeOfDay } from "../lib/format";
import { groupByDay, STATUS_LABELS } from "../lib/meetings";
import { Icon } from "./Icon";

export function StatusBadge({ status }: { status: Meeting["status"] }) {
  return (
    <span className={`status-badge status-badge--${status}`}>
      {status === "recording" && (
        <span className="rec-dot" aria-hidden="true" />
      )}
      {STATUS_LABELS[status]}
    </span>
  );
}

export function MeetingList({
  meetings,
  loading,
  query,
  onQueryChange,
  selectedId,
  onSelect,
  progress,
  now,
}: {
  meetings: Meeting[];
  loading: boolean;
  query: string;
  onQueryChange: (q: string) => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
  progress: Record<string, TranscriptionProgress>;
  now?: Date;
}) {
  const groups = groupByDay(meetings, now);
  return (
    <section className="meeting-list" aria-label="Meetings">
      <label className="search-field">
        <Icon name="search" size={16} />
        <input
          type="search"
          value={query}
          placeholder="Search titles and transcripts"
          aria-label="Search meetings"
          onChange={(e) => onQueryChange(e.target.value)}
          autoCorrect="off"
          autoCapitalize="none"
          spellCheck={false}
        />
      </label>
      <div className="meeting-list-scroll">
        {loading && meetings.length === 0 ? (
          <div className="meeting-list-skeleton" aria-busy="true">
            {[0, 1, 2].map((i) => (
              <div key={i} className="meeting-item meeting-item--skeleton">
                <span />
                <span />
              </div>
            ))}
          </div>
        ) : meetings.length === 0 ? (
          <div className="meeting-list-empty">
            {query.trim() ? (
              <>
                <strong>No matches</strong>
                <span>
                  Nothing in your titles or transcripts matches “{query.trim()}
                  ”.
                </span>
              </>
            ) : (
              <>
                <strong>No meetings yet</strong>
                <span>
                  Recorded meetings and their transcripts show up here.
                </span>
              </>
            )}
          </div>
        ) : (
          groups.map((group) => (
            <div key={group.label} className="meeting-group">
              <h3 className="meeting-group-label">{group.label}</h3>
              <ul role="list">
                {group.meetings.map((m) => {
                  const p = progress[m.id];
                  return (
                    <li key={m.id}>
                      <button
                        type="button"
                        className={`meeting-item${m.id === selectedId ? " is-selected" : ""}`}
                        aria-current={m.id === selectedId ? "true" : undefined}
                        onClick={() => onSelect(m.id)}
                      >
                        <span className="meeting-item-title">{m.title}</span>
                        <span className="meeting-item-meta">
                          <span>{timeOfDay(m.started_at)}</span>
                          {m.status !== "recording" && (
                            <span>{humanDuration(m.duration_ms)}</span>
                          )}
                          {m.status === "ready" ? (
                            m.speakers.length > 0 && (
                              <span className="meeting-item-speakers">
                                {m.speakers.join(", ")}
                              </span>
                            )
                          ) : (
                            <StatusBadge status={m.status} />
                          )}
                          {m.status === "transcribing" && p && p.total > 0 && (
                            <span className="meeting-item-progress">
                              {p.track === "me" ? "mic" : "call"} {p.done}/
                              {p.total}
                            </span>
                          )}
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </div>
          ))
        )}
      </div>
    </section>
  );
}
