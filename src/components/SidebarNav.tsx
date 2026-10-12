import type { ReactNode } from "react";
import { AppLogo } from "./AppLogo";
import { Icon, type IconName } from "./Icon";

export type NavScreen =
  | "library"
  | "vocabulary"
  | "transcription"
  | "settings"
  | "claude"
  | "permissions";

const NAV_ITEMS: Array<{ id: NavScreen; label: string; icon: IconName }> = [
  { id: "library", label: "Library", icon: "library" },
  { id: "transcription", label: "Transcription", icon: "wave" },
  { id: "vocabulary", label: "Vocabulary", icon: "book" },
  { id: "settings", label: "Recording & storage", icon: "settings" },
  { id: "claude", label: "Connect Claude", icon: "plug" },
  { id: "permissions", label: "Permissions", icon: "shield" },
];

export function SidebarNav({
  activeScreen,
  onNavigate,
  footer,
}: {
  activeScreen: NavScreen;
  onNavigate: (screen: NavScreen) => void;
  footer?: ReactNode;
}) {
  return (
    <aside className="sidebar">
      <div className="logo">
        <AppLogo />
      </div>
      <nav className="nav">
        {NAV_ITEMS.map((item) => (
          <button
            key={item.id}
            className={`nav-item ${activeScreen === item.id ? "nav-item-active" : ""}`}
            aria-current={activeScreen === item.id ? "page" : undefined}
            onClick={() => onNavigate(item.id)}
          >
            <Icon name={item.icon} size={18} />
            <span>{item.label}</span>
          </button>
        ))}
      </nav>
      {footer && <div className="sidebar-footer">{footer}</div>}
    </aside>
  );
}
