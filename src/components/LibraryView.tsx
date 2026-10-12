import type {
  Meeting,
  MeetingDetail,
  RecorderStatus,
  TranscriptionProgress,
} from "../types";
import type { ProfilesState } from "../lib/sttProvider";
import { MeetingDetailView, type MeetingActions } from "./MeetingDetailView";
import { MeetingList } from "./MeetingList";
import { RecordBar } from "./RecordBar";

export function LibraryView({
  meetings,
  loading,
  query,
  onQueryChange,
  selectedId,
  onSelect,
  detail,
  recorder,
  recorderBusy,
  shortcutLabel,
  progress,
  profiles,
  actions,
  onStart,
  onStop,
  onPause,
  onResume,
  now,
}: {
  meetings: Meeting[];
  loading: boolean;
  query: string;
  onQueryChange: (q: string) => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
  detail: MeetingDetail | null;
  recorder: RecorderStatus;
  recorderBusy: boolean;
  shortcutLabel: string | null;
  progress: Record<string, TranscriptionProgress>;
  profiles: ProfilesState;
  actions: MeetingActions;
  onStart: () => void;
  onStop: () => void;
  onPause: () => void;
  onResume: () => void;
  now?: Date;
}) {
  return (
    <div className="library">
      <RecordBar
        recorder={recorder}
        shortcutLabel={shortcutLabel}
        busy={recorderBusy}
        onStart={onStart}
        onStop={onStop}
        onPause={onPause}
        onResume={onResume}
        onOpenLive={() => recorder.meeting_id && onSelect(recorder.meeting_id)}
      />
      <div className="library-panes">
        <MeetingList
          meetings={meetings}
          loading={loading}
          query={query}
          onQueryChange={onQueryChange}
          selectedId={selectedId}
          onSelect={onSelect}
          progress={progress}
          now={now}
        />
        <div className="library-detail">
          {detail ? (
            <MeetingDetailView
              detail={detail}
              recordingId={recorder.meeting_id}
              progress={progress[detail.meeting.id]}
              profiles={profiles}
              actions={actions}
            />
          ) : (
            <div className="library-placeholder">
              <img
                src="/Images/parley-mark.svg"
                alt=""
                width={56}
                height={56}
              />
              <p>
                {meetings.length === 0
                  ? "Record your first meeting. Parley keeps the audio and a transcript you can search here or ask Claude about."
                  : "Select a meeting to read its transcript."}
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
