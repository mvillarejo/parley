import { useState } from "react";
import type { PermissionsState, SystemAudioPermission } from "../types";
import { Icon } from "./Icon";
import { SettingsPageLayout } from "./SettingsPageLayout";

export function microphoneText(status: string | undefined): string {
  switch (status) {
    case "authorized":
      return "Granted";
    case "denied":
    case "restricted":
      return "Denied — turn Parley on in System Settings";
    case "not_determined":
      return "Not asked yet";
    default:
      return "Unknown";
  }
}

export function systemAudioText(
  status: SystemAudioPermission | undefined,
): string {
  switch (status) {
    case "authorized":
      return "Granted";
    case "denied":
      return "Denied — the call track would be silent. Turn Parley on in System Settings";
    case "not_determined":
      return "Not asked yet — macOS asks the first time you record";
    case "unsupported":
      return "Needs macOS 14.2 or later";
    default:
      return "Status unavailable — check System Settings if the call track is silent";
  }
}

export function PermissionsView({
  permissions,
  onGrantMicrophone,
  onRequestSystemAudio,
  onOpenSettings,
  onRecheck,
}: {
  permissions: PermissionsState | null;
  onGrantMicrophone: () => void;
  onRequestSystemAudio: () => void;
  onOpenSettings: (panel: "microphone" | "system_audio") => void;
  onRecheck: () => Promise<unknown>;
}) {
  const [checking, setChecking] = useState(false);
  const recheck = async () => {
    setChecking(true);
    try {
      await onRecheck();
    } finally {
      setChecking(false);
    }
  };
  const mic = permissions?.microphone;
  const sys = permissions?.system_audio;
  const micGranted = mic === "authorized";
  const sysGranted = sys === "authorized";
  return (
    <SettingsPageLayout
      title="Permissions"
      description="Parley needs two macOS permissions to record a call. Nothing is recorded until you press Record."
    >
      <div className="permissions-list">
        <div className={`permission-row ${micGranted ? "granted" : "needed"}`}>
          <div className="permission-icon">
            <Icon name="mic" size={16} />
          </div>
          <div className="permission-info">
            <span className="permission-name">Microphone</span>
            <span className="permission-why">Your voice — the “Me” track.</span>
            <span className="permission-status-label">
              {microphoneText(mic)}
            </span>
          </div>
          {!micGranted &&
            (mic === "not_determined" ? (
              <button
                className="btn btn-primary btn-sm"
                onClick={onGrantMicrophone}
              >
                Grant
              </button>
            ) : (
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => onOpenSettings("microphone")}
              >
                Open Settings
              </button>
            ))}
        </div>
        <div className={`permission-row ${sysGranted ? "granted" : "needed"}`}>
          <div className="permission-icon">
            <Icon name="speaker" size={16} />
          </div>
          <div className="permission-info">
            <span className="permission-name">System Audio Recording</span>
            <span className="permission-why">
              The other participants — the “Them” track, from any call app.
            </span>
            <span className="permission-status-label">
              {systemAudioText(sys)}
            </span>
          </div>
          {!sysGranted &&
            sys !== "unsupported" &&
            (sys === "not_determined" ? (
              <button
                className="btn btn-primary btn-sm"
                onClick={onRequestSystemAudio}
              >
                Ask now
              </button>
            ) : (
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => onOpenSettings("system_audio")}
              >
                Open Settings
              </button>
            ))}
        </div>
      </div>
      <div className="notice notice--info">
        <Icon name="shield" size={16} />
        <span>
          Recording a call can require the consent of everyone on it. Parley
          never records on its own, and the menu bar icon turns red while it
          does.
        </span>
      </div>
      <div className="setup-nav setup-nav-center">
        <button
          className="btn btn-secondary"
          onClick={() => void recheck()}
          disabled={checking}
        >
          {checking ? "Checking…" : "Re-check"}
        </button>
      </div>
    </SettingsPageLayout>
  );
}
