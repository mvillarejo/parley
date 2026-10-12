import type { McpInfo } from "../types";
import { CopyButton } from "./CopyButton";
import { Icon } from "./Icon";
import { SettingsPageLayout } from "./SettingsPageLayout";

const TOOLS: Array<[string, string]> = [
  ["list_meetings", "browse by date range or text"],
  ["search_transcripts", "full-text search with timestamps"],
  ["get_transcript", "a meeting as Markdown or timestamped segments"],
  ["get_meeting", "title, time, duration, speakers, files"],
];

export function ClaudeView({ info }: { info: McpInfo | null }) {
  return (
    <SettingsPageLayout
      title="Connect Claude"
      description="Parley ships a small MCP server, parley-mcp, that gives Claude read-only access to your meetings."
    >
      {info && !info.binary_exists && (
        <div className="notice notice--warning" role="note">
          <Icon name="warning" size={16} />
          <span>
            <code>parley-mcp</code> isn’t built next to this app yet. In a
            development build run <code>npm run build:mcp</code>; the packaged
            app includes it.
          </span>
        </div>
      )}
      <div className="settings-section">
        <h3 className="settings-section-title">Claude Code</h3>
        <p className="form-footnote text-secondary">Run once in a terminal:</p>
        <div className="code-block">
          <code>{info?.claude_code_command ?? "…"}</code>
          {info && <CopyButton text={info.claude_code_command} />}
        </div>
      </div>
      <div className="settings-section">
        <h3 className="settings-section-title">Claude Desktop</h3>
        <p className="form-footnote text-secondary">
          Add this to{" "}
          <code>
            ~/Library/Application Support/Claude/claude_desktop_config.json
          </code>{" "}
          and restart Claude Desktop:
        </p>
        <div className="code-block code-block--multiline">
          <pre>{info?.desktop_config ?? "…"}</pre>
          {info && <CopyButton text={info.desktop_config} />}
        </div>
      </div>
      <div className="settings-section">
        <h3 className="settings-section-title">What Claude can do</h3>
        <ul className="tool-list">
          {TOOLS.map(([name, what]) => (
            <li key={name}>
              <code>{name}</code>
              <span>{what}</span>
            </li>
          ))}
        </ul>
        <p className="form-footnote text-secondary">
          Each meeting is also a resource at{" "}
          <code>parley://meeting/&lt;id&gt;</code>. Try: “Summarize yesterday’s
          budget review and list the action items for me.”
        </p>
        <p className="form-footnote text-secondary">
          Database: <code>{info?.db_path ?? "…"}</code>
        </p>
      </div>
    </SettingsPageLayout>
  );
}
