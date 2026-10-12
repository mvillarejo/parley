import { useEffect, useState } from "react";
import type {
  AudioDeviceInfo,
  AudioDeviceSelection,
  RecordingSettings,
  StorageInfo,
} from "../types";
import { formatShortcut } from "../lib/shortcut";
import { Icon } from "./Icon";
import { InfoTooltip } from "./InfoTooltip";
import { SettingsPageLayout } from "./SettingsPageLayout";

type ShortcutKeys = "shortcut" | "mark_shortcut" | "note_shortcut";

/** One global shortcut: on/off, and its keys in Tauri accelerator syntax. */
function ShortcutRow({
  label,
  description,
  value,
  enabled,
  placeholder,
  toggleLabel,
  inputLabel,
  onToggle,
  onSave,
}: {
  label: string;
  description: string;
  value: string;
  enabled: boolean;
  placeholder: string;
  toggleLabel: string;
  inputLabel: string;
  onToggle: (enabled: boolean) => void;
  onSave: (value: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <div className="shortcut-row">
      <div className="settings-toggle-row">
        <div>
          <span className="form-label">{label}</span>
          <p className="form-footnote text-secondary">
            {description} Currently{" "}
            <kbd className="kbd">{formatShortcut(value)}</kbd>
          </p>
        </div>
        <label className="toggle-switch">
          <input
            type="checkbox"
            aria-label={toggleLabel}
            checked={enabled}
            onChange={(e) => onToggle(e.target.checked)}
          />
          <span className="toggle-slider" />
        </label>
      </div>
      <div className="inline-field">
        <input
          className="form-input"
          value={draft}
          aria-label={inputLabel}
          disabled={!enabled}
          onChange={(e) => setDraft(e.target.value)}
          placeholder={placeholder}
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="none"
        />
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          disabled={!enabled || draft.trim() === value}
          onClick={() => onSave(draft.trim())}
        >
          Save
        </button>
      </div>
    </div>
  );
}

function selectionKey(selection: AudioDeviceSelection): string {
  if (selection.mode === "specific")
    return `specific:${selection.device_id || selection.device_name}`;
  return selection.mode;
}

export function RecordingSettingsView({
  settings,
  devices,
  storage,
  onSave,
  onChooseStorage,
  onMoveStorage,
}: {
  settings: RecordingSettings;
  devices: AudioDeviceInfo[];
  storage: StorageInfo | null;
  onSave: (settings: RecordingSettings) => Promise<void>;
  onChooseStorage: () => Promise<string | null>;
  onMoveStorage: (dir: string | null) => Promise<void>;
}) {
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [storageError, setStorageError] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);

  useEffect(() => {
    if (!saved) return;
    const t = window.setTimeout(() => setSaved(false), 1500);
    return () => window.clearTimeout(t);
  }, [saved]);

  const save = async (patch: Partial<RecordingSettings>) => {
    setError(null);
    try {
      await onSave({ ...settings, ...patch });
      setSaved(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const options: Array<{
    key: string;
    label: string;
    value: AudioDeviceSelection;
  }> = [
    {
      key: "automatic",
      label: "Automatic (follows macOS)",
      value: { mode: "automatic" },
    },
    {
      key: "system_default",
      label: "System default",
      value: { mode: "system_default" },
    },
    ...devices.map((d) => ({
      key: `specific:${d.id || d.name}`,
      label: d.isDefault ? `${d.name} (default)` : d.name,
      value: {
        mode: "specific",
        device_id: d.id,
        device_name: d.name,
      } as AudioDeviceSelection,
    })),
  ];
  const currentKey = selectionKey(settings.audio_device_selection);
  if (
    !options.some((o) => o.key === currentKey) &&
    settings.audio_device_selection.mode === "specific"
  ) {
    options.push({
      key: currentKey,
      label: `${settings.audio_device_selection.device_name} (not connected)`,
      value: settings.audio_device_selection,
    });
  }

  const move = async (dir: string | null) => {
    setStorageError(null);
    setMoving(true);
    try {
      await onMoveStorage(dir);
    } catch (e) {
      setStorageError(String(e));
    } finally {
      setMoving(false);
    }
  };

  return (
    <SettingsPageLayout
      title="Recording & storage"
      description="Choose the microphone, the call audio and where meetings are kept."
    >
      <div className="settings-section">
        <h3 className="settings-section-title">Audio</h3>
        <div className="form-group">
          <label className="form-label" htmlFor="mic-select">
            <Icon name="mic" size={14} /> Microphone (“Me”)
          </label>
          <select
            id="mic-select"
            className="form-input"
            value={currentKey}
            onChange={(e) => {
              const next = options.find((o) => o.key === e.target.value);
              if (next) void save({ audio_device_selection: next.value });
            }}
          >
            {options.map((o) => (
              <option key={o.key} value={o.key}>
                {o.label}
              </option>
            ))}
          </select>
        </div>
        <div className="settings-toggle-row">
          <div>
            <span className="form-label">
              <Icon name="speaker" size={14} /> Record call audio (“Them”)
              <InfoTooltip label="About call audio">
                <strong>Call audio</strong>
                Everything your Mac plays — Zoom, Meet, Teams, Slack huddles, in
                the browser or the app — is recorded as a second track through
                macOS system audio capture (macOS 14.2+). You keep hearing it
                normally. Use headphones so your mic doesn’t pick the call up
                twice.
              </InfoTooltip>
            </span>
            <p className="form-footnote text-secondary">
              Off: only your microphone is recorded.
            </p>
          </div>
          <label className="toggle-switch">
            <input
              type="checkbox"
              aria-label="Record call audio"
              checked={settings.capture_system_audio}
              onChange={(e) =>
                void save({ capture_system_audio: e.target.checked })
              }
            />
            <span className="toggle-slider" />
          </label>
        </div>
      </div>

      <div className="settings-section">
        <h3 className="settings-section-title">Shortcuts</h3>
        {(
          [
            {
              key: "shortcut",
              label: "Start / stop with a keyboard shortcut",
              description: "Works from any app.",
              placeholder: "CommandOrControl+Shift+Alt+R",
              toggleLabel: "Enable shortcut",
              inputLabel: "Shortcut",
            },
            {
              key: "mark_shortcut",
              label: "Mark this moment",
              description: "Adds a highlight while recording, from any app.",
              placeholder: "CommandOrControl+Shift+Alt+M",
              toggleLabel: "Enable mark shortcut",
              inputLabel: "Mark shortcut",
            },
            {
              key: "note_shortcut",
              label: "Add a note",
              description: "Opens the recorder’s note field while recording.",
              placeholder: "CommandOrControl+Shift+Alt+N",
              toggleLabel: "Enable note shortcut",
              inputLabel: "Note shortcut",
            },
          ] satisfies Array<{
            key: ShortcutKeys;
            label: string;
            description: string;
            placeholder: string;
            toggleLabel: string;
            inputLabel: string;
          }>
        ).map(({ key, ...row }) => (
          <ShortcutRow
            key={key}
            {...row}
            value={settings[key]}
            enabled={settings[`${key}_enabled`]}
            onToggle={(enabled) => void save({ [`${key}_enabled`]: enabled })}
            onSave={(value) => void save({ [key]: value })}
          />
        ))}
        <p className="form-footnote text-secondary">
          Use Tauri accelerator syntax, e.g.{" "}
          <code>CommandOrControl+Shift+Alt+R</code>.
        </p>
      </div>

      {(error || saved) && (
        <p
          className={`form-footnote ${error ? "form-footnote--danger" : "text-secondary"}`}
          role={error ? "alert" : "status"}
        >
          {error ?? "Saved"}
        </p>
      )}

      <div className="settings-section">
        <h3 className="settings-section-title">Storage</h3>
        <p className="form-footnote text-secondary">
          Each meeting gets a folder with <code>mic.wav</code>,{" "}
          <code>system.wav</code>, <code>mixed.wav</code> and{" "}
          <code>transcript.md</code>, next to the search database{" "}
          <code>parley.db</code>.
        </p>
        <div className="path-box">
          <code title={storage?.data_dir}>{storage?.data_dir ?? "…"}</code>
        </div>
        <div className="button-row">
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={moving}
            onClick={async () => {
              const dir = await onChooseStorage();
              if (dir) await move(dir);
            }}
          >
            <Icon name="folder" size={14} />
            {moving ? "Moving…" : "Move to another folder…"}
          </button>
          {storage && !storage.is_default && (
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              disabled={moving}
              onClick={() => void move(null)}
            >
              Move back to the default location
            </button>
          )}
        </div>
        <p className="form-footnote text-secondary">
          The folder must be empty. Existing meetings move with it; the Claude
          connection follows automatically.
        </p>
        {storageError && (
          <p className="form-footnote form-footnote--danger" role="alert">
            {storageError}
          </p>
        )}
      </div>
    </SettingsPageLayout>
  );
}
