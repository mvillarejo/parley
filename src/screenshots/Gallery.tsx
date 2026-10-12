import React, { useMemo, useState } from "react";
import { SidebarNav, type NavScreen } from "../components/SidebarNav";
import { LibraryView } from "../components/LibraryView";
import type { MeetingActions } from "../components/MeetingDetailView";
import { TranscriptionSettingsView } from "../components/TranscriptionSettingsView";
import { CustomVocabularyView } from "../components/CustomVocabularyView";
import { RecordingSettingsView } from "../components/RecordingSettingsView";
import { ClaudeView } from "../components/ClaudeView";
import { PermissionsView } from "../components/PermissionsView";
import { Widget, type WidgetProps } from "../widget/Widget";
import { DEFAULT_CUSTOM_VOCABULARY } from "../lib/customVocabulary";
import type { ProfilesState } from "../lib/sttProvider";
import {
  IDLE_RECORDER,
  type Marker,
  type Meeting,
  type MeetingDetail,
  type RecorderStatus,
  type Segment,
  type TranscriptionProgress,
  type VocabularySettings,
} from "../types";

const noop = () => {};
const asyncNoop = async () => {};
const GALLERY_NOW = new Date(2026, 9, 8, 16, 30);

const at = (days: number, h: number, m: number) =>
  new Date(2026, 9, 8 - days, h, m).toISOString();

function meeting(
  partial: Partial<Meeting> & Pick<Meeting, "id" | "title">,
): Meeting {
  return {
    started_at: at(0, 10, 0),
    ended_at: null,
    duration_ms: 0,
    status: "ready",
    error: null,
    capture_warning: null,
    profile_name: "Local Ollama",
    provider: "openai_compat",
    language: "en",
    folder: `meetings/${partial.id}`,
    speakers: ["Me", "Them 1", "Them 2"],
    segment_count: 14,
    updated_at: at(0, 11, 0),
    ...partial,
  };
}

const MEETINGS: Meeting[] = [
  meeting({
    id: "m-budget",
    title: "Q4 budget review with Finance",
    started_at: at(0, 14, 2),
    duration_ms: 2_712_000,
    profile_name: "Gladia",
    provider: "gladia",
  }),
  meeting({
    id: "m-vendor",
    title: "Vendor call — observability renewal",
    started_at: at(0, 10, 30),
    duration_ms: 1_380_000,
    speakers: ["Me", "Them"],
  }),
  meeting({
    id: "m-qa",
    title: "QA transformation 1:1",
    started_at: at(1, 16, 0),
    duration_ms: 1_820_000,
    speakers: ["Me", "Them"],
  }),
  meeting({
    id: "m-standup",
    title: "Incident standup — traffic drop",
    started_at: at(1, 9, 15),
    duration_ms: 905_000,
  }),
  meeting({
    id: "m-board",
    title: "Board prep",
    started_at: at(3, 11, 0),
    duration_ms: 3_640_000,
    speakers: ["Me", "Them"],
  }),
];

const seg = (s: number, e: number, speaker: string, text: string): Segment => ({
  start_ms: s * 1000,
  end_ms: e * 1000,
  speaker,
  text,
});

const SEGMENTS: Segment[] = [
  seg(
    2,
    7,
    "Me",
    "Thanks for making time. I'd like to close the Q4 budget today if we can.",
  ),
  seg(
    8,
    15,
    "Them 1",
    "Sure. Finance signed off on the headcount plan, so what's left is infrastructure and licences.",
  ),
  seg(
    16,
    24,
    "Them 1",
    "Cloud spend came in about eight percent over forecast in September.",
  ),
  seg(
    26,
    33,
    "Me",
    "That matches what we saw. Most of it is the data warehouse running real-time loads for marketing.",
  ),
  seg(
    34,
    41,
    "Them 2",
    "Can we cap that, or does it need to grow with the new services?",
  ),
  seg(
    43,
    55,
    "Me",
    "It grows, but we can move the batch jobs to reserved capacity. That should take back about half of the overrun.",
  ),
  seg(
    57,
    62,
    "Them 1",
    "Good. Send me the numbers by Friday and I'll fold them into the forecast.",
  ),
  seg(
    64,
    70,
    "Them 2",
    "And the observability renewal — are we still evaluating alternatives?",
  ),
  seg(
    71,
    82,
    "Me",
    "Yes. The vendor offered twenty percent off for three years this morning. I want one more quote before we commit.",
  ),
  seg(84, 88, "Them 2", "Fine, but the contract ends in November."),
  seg(90, 97, "Me", "Understood. I'll have a recommendation in two weeks."),
  seg(
    99,
    104,
    "Them 1",
    "Last item: the security tooling line. Is the certification training included there?",
  ),
  seg(105, 110, "Me", "No, that sits under people development."),
  seg(112, 116, "Them 1", "Perfect. I think we're done. Thanks, everyone."),
];

const FILES = {
  folder: "/Users/me/Library/Application Support/parley/meetings/m-budget",
  mic: "/mic.wav",
  system: "/system.wav",
  mixed: "/mixed.wav",
  transcript: "/transcript.md",
};

const marker = (
  id: number,
  seconds: number,
  kind: Marker["kind"],
  text: string | null = null,
): Marker => ({
  id,
  meeting_id: "m-budget",
  at_ms: seconds * 1000,
  kind,
  text,
  created_at: at(0, 14, 30),
});

const MARKERS: Marker[] = [
  marker(1, 16, "highlight"),
  marker(2, 57, "note", "Send Finance the reserved-capacity numbers by Friday"),
  marker(3, 84, "note", "Get one more observability quote before November"),
];

const detailFor = (m: Meeting): MeetingDetail => ({
  meeting: m,
  segments: m.status === "ready" ? SEGMENTS : [],
  markers: m.status === "ready" ? MARKERS : [],
  files:
    m.status === "recording"
      ? { ...FILES, mixed: null, transcript: null }
      : FILES,
});

const PROFILES: ProfilesState = {
  profiles: [
    {
      id: "local-ollama",
      name: "Local Ollama",
      kind: "openai_compat",
      base_url: "http://localhost:11434/v1",
      model: "gemma4:e4b",
      api_key: "",
    },
    {
      id: "gladia",
      name: "Gladia",
      kind: "gladia",
      api_key: "gl-example-key",
      code_switching: false,
    },
    {
      id: "groq",
      name: "Groq",
      kind: "openai_compat",
      base_url: "https://api.groq.com/openai/v1",
      model: "whisper-large-v3",
      api_key: "gsk-example",
    },
  ],
  active_profile_id: "local-ollama",
};

const LIVE: RecorderStatus = {
  state: "recording",
  meeting_id: "m-live",
  title: "Weekly sync",
  elapsed_ms: 1_234_000,
  me_level: 0.62,
  them_level: 0.41,
  system_audio: true,
  warning: null,
  marker_count: 2,
};

const ACTIONS: MeetingActions = {
  onRename: asyncNoop,
  onDelete: asyncNoop,
  onRetranscribe: asyncNoop,
  onReveal: noop,
  getMarkdown: async () => "# Q4 budget review",
  audioSrc: (path) => path,
  onAddMarker: asyncNoop,
  onUpdateMarker: asyncNoop,
  onDeleteMarker: asyncNoop,
};

function AppShell({
  id,
  activeScreen,
  live = false,
  children,
}: {
  id: string;
  activeScreen: NavScreen;
  live?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div
      id={id}
      data-screenshot={id}
      className="layout"
      style={{ width: 1120, height: 720, overflow: "hidden" }}
    >
      <SidebarNav
        activeScreen={activeScreen}
        onNavigate={noop as never}
        footer={
          live ? (
            <button type="button" className="sidebar-live">
              <span className="rec-dot" aria-hidden="true" />
              Recording
            </button>
          ) : null
        }
      />
      <main className="main-content">
        <section
          className={`content${activeScreen === "library" ? " content--full" : ""}`}
        >
          {children}
        </section>
      </main>
    </div>
  );
}

function Frame({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="screenshot-frame-wrap">
      <p className="screenshot-label">{label}</p>
      {children}
    </div>
  );
}

/** A working library over mock data: selection and search behave like the app. */
function MockLibrary({
  initialId,
  meetings = MEETINGS,
  recorder = IDLE_RECORDER,
  progress = {},
}: {
  initialId: string | null;
  meetings?: Meeting[];
  recorder?: RecorderStatus;
  progress?: Record<string, TranscriptionProgress>;
}) {
  const [selectedId, setSelectedId] = useState<string | null>(initialId);
  const [query, setQuery] = useState("");
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return meetings;
    return meetings.filter(
      (m) =>
        m.title.toLowerCase().includes(q) ||
        (m.id === "m-budget" &&
          SEGMENTS.some((s) => s.text.toLowerCase().includes(q))),
    );
  }, [meetings, query]);
  const selected = meetings.find((m) => m.id === selectedId);
  return (
    <LibraryView
      meetings={visible}
      loading={false}
      query={query}
      onQueryChange={setQuery}
      selectedId={selectedId}
      onSelect={setSelectedId}
      detail={selected ? detailFor(selected) : null}
      recorder={recorder}
      recorderBusy={false}
      shortcutLabel="⌥⇧⌘R"
      progress={progress}
      profiles={PROFILES}
      actions={ACTIONS}
      onStart={noop}
      onStop={noop}
      onPause={noop}
      onResume={noop}
      now={GALLERY_NOW}
    />
  );
}

const WIDGET_HANDLERS = {
  shortcutLabel: "⌥⇧⌘R",
  markShortcutLabel: "⌥⇧⌘M",
  noteShortcutLabel: "⌥⇧⌘N",
  busy: false,
  onStart: noop,
  onStop: noop,
  onPause: noop,
  onResume: noop,
  onRename: noop,
  onOpenLibrary: noop,
  onHide: noop,
};

function WidgetFrame({
  id,
  height = 132,
  ...props
}: { id: string; height?: number } & Pick<
  WidgetProps,
  "recorder" | "statusLine" | "statusAction" | "flash" | "note"
>) {
  return (
    <div
      id={id}
      data-screenshot={id}
      className="widget-frame"
      style={{ width: 340, height }}
    >
      <Widget {...WIDGET_HANDLERS} {...props} />
    </div>
  );
}

export function ScreenshotGallery() {
  const [vocab, setVocab] = useState<VocabularySettings>({
    customVocabulary: [
      ...DEFAULT_CUSTOM_VOCABULARY,
      { value: "Valsea", intensity: 0.6 },
      {
        value: "Dinotech",
        intensity: 0.5,
        pronunciations: ["dino tech", "dyno tech"],
      },
      { value: "Looker", intensity: 0.5 },
      { value: "CISSP", intensity: 0.7, pronunciations: ["sis p"] },
    ],
  });
  const [profiles, setProfiles] = useState(PROFILES);
  const [languages, setLanguages] = useState<string[]>(["en", "es"]);

  const liveMeeting = meeting({
    id: "m-live",
    title: LIVE.title!,
    status: "recording",
    started_at: at(0, 16, 10),
    speakers: [],
    segment_count: 0,
    profile_name: null,
  });
  const transcribing = meeting({
    ...MEETINGS[1],
    status: "transcribing",
    speakers: [],
    segment_count: 0,
  });
  const failed = meeting({
    ...MEETINGS[2],
    status: "failed",
    error: "Can't reach Ollama at http://localhost:11434 — is it running?",
    capture_warning:
      "System audio recording is turned off in System Settings, so only your mic is recorded.",
    speakers: [],
    segment_count: 0,
  });

  return (
    <div className="screenshot-gallery">
      <Frame label="Library — transcript">
        <AppShell id="01-library" activeScreen="library">
          <MockLibrary initialId="m-budget" />
        </AppShell>
      </Frame>
      <Frame label="Library — recording">
        <AppShell id="02-library-recording" activeScreen="library" live>
          <MockLibrary
            initialId="m-live"
            meetings={[liveMeeting, ...MEETINGS]}
            recorder={LIVE}
          />
        </AppShell>
      </Frame>
      <Frame label="Library — transcribing">
        <AppShell id="03-library-transcribing" activeScreen="library">
          <MockLibrary
            initialId="m-vendor"
            meetings={[MEETINGS[0], transcribing, ...MEETINGS.slice(2)]}
            progress={{
              "m-vendor": {
                meeting_id: "m-vendor",
                track: "them",
                done: 7,
                total: 19,
              },
            }}
          />
        </AppShell>
      </Frame>
      <Frame label="Library — failed with retry">
        <AppShell id="04-library-failed" activeScreen="library">
          <MockLibrary
            initialId="m-qa"
            meetings={[MEETINGS[0], MEETINGS[1], failed, ...MEETINGS.slice(3)]}
          />
        </AppShell>
      </Frame>
      <Frame label="Library — empty">
        <AppShell id="05-library-empty" activeScreen="library">
          <MockLibrary initialId={null} meetings={[]} />
        </AppShell>
      </Frame>
      <Frame label="Transcription profiles">
        <AppShell id="06-transcription" activeScreen="transcription">
          <TranscriptionSettingsView
            languages={languages}
            onLanguagesChange={setLanguages}
            profilesState={profiles}
            onProfilesChange={setProfiles}
          />
        </AppShell>
      </Frame>
      <Frame label="Vocabulary">
        <AppShell id="07-vocabulary" activeScreen="vocabulary">
          <CustomVocabularyView settings={vocab} setSettings={setVocab} />
        </AppShell>
      </Frame>
      <Frame label="Recording & storage">
        <AppShell id="08-recording-settings" activeScreen="settings">
          <RecordingSettingsView
            settings={{
              audio_device_selection: { mode: "automatic" },
              capture_system_audio: true,
              shortcut: "CommandOrControl+Shift+Alt+R",
              shortcut_enabled: true,
              mark_shortcut: "CommandOrControl+Shift+Alt+M",
              mark_shortcut_enabled: true,
              note_shortcut: "CommandOrControl+Shift+Alt+N",
              note_shortcut_enabled: false,
            }}
            devices={[
              {
                id: "1",
                name: "MacBook Pro Microphone",
                transport: "built_in",
                isDefault: true,
                isBuiltIn: true,
              },
              {
                id: "2",
                name: "AirPods Pro",
                transport: "bluetooth",
                isDefault: false,
                isBuiltIn: false,
              },
            ]}
            storage={{
              data_dir: "/Users/me/Library/Application Support/parley",
              db_path: "/Users/me/Library/Application Support/parley/parley.db",
              default_dir: "/Users/me/Library/Application Support/parley",
              is_default: true,
            }}
            onSave={asyncNoop}
            onChooseStorage={async () => null}
            onMoveStorage={asyncNoop}
          />
        </AppShell>
      </Frame>
      <Frame label="Connect Claude">
        <AppShell id="09-claude" activeScreen="claude">
          <ClaudeView
            info={{
              binary_path: "/Applications/Parley.app/Contents/MacOS/parley-mcp",
              binary_exists: true,
              db_path: "/Users/me/Library/Application Support/parley/parley.db",
              claude_code_command:
                "claude mcp add parley -- /Applications/Parley.app/Contents/MacOS/parley-mcp",
              desktop_config: JSON.stringify(
                {
                  mcpServers: {
                    parley: {
                      command:
                        "/Applications/Parley.app/Contents/MacOS/parley-mcp",
                      args: [],
                    },
                  },
                },
                null,
                2,
              ),
            }}
          />
        </AppShell>
      </Frame>
      <Frame label="Permissions">
        <AppShell id="10-permissions" activeScreen="permissions">
          <PermissionsView
            permissions={{
              microphone: "authorized",
              system_audio: "not_determined",
            }}
            onGrantMicrophone={noop}
            onRequestSystemAudio={noop}
            onOpenSettings={noop}
            onRecheck={async () => {}}
          />
        </AppShell>
      </Frame>
      <Frame label="Widget — idle">
        <WidgetFrame
          id="11-widget-idle"
          recorder={IDLE_RECORDER}
          statusLine="Transcript ready."
          statusAction={{ label: "Open", onClick: noop }}
        />
      </Frame>
      <Frame label="Widget — recording">
        <WidgetFrame
          id="12-widget-recording"
          recorder={LIVE}
          statusLine={null}
        />
      </Frame>
      <Frame label="Widget — paused, mic only">
        <WidgetFrame
          id="13-widget-paused"
          recorder={{
            ...LIVE,
            state: "paused",
            me_level: 0,
            them_level: 0,
            system_audio: false,
            warning:
              "Call audio isn't recorded: System audio capture needs macOS 14.2 or later",
          }}
          statusLine={null}
        />
      </Frame>
      <Frame label="Widget — moment marked">
        <WidgetFrame
          id="16-widget-marked"
          recorder={{ ...LIVE, marker_count: 3 }}
          statusLine={null}
          flash={{ text: "Marked 20:34", tone: "ok" }}
        />
      </Frame>
      <Frame label="Widget — typing a note">
        <WidgetFrame
          id="17-widget-note"
          height={172}
          recorder={LIVE}
          statusLine={null}
          note={{
            meetingId: "m-live",
            atMs: 1_221_000,
            text: "Ask Finance for the Q3 actuals",
          }}
        />
      </Frame>
      <Frame label="Widget — transcribing">
        <WidgetFrame
          id="14-widget-transcribing"
          recorder={IDLE_RECORDER}
          statusLine="Transcribing the call… 7/19"
        />
      </Frame>
    </div>
  );
}
