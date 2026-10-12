# Contributing to Parley

## Setup

1. Install Node.js 20.19+, Rust stable and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for macOS.
2. `npm ci`, then `npm run tauri:dev`.

`npm run dev` is enough for UI work: open `http://localhost:1420/screenshots.html` for the component gallery with mock data.

## Before you commit

```bash
npm run format
npm test          # cargo test (incl. MCP integration tests), vitest, tsc
npm run build
npm run screenshots   # with `npm run dev` running: captures the gallery and runs UI smoke checks
```

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/) (`.commitlintrc.yml`).

## Testing real capture

Unit tests cover everything except the macOS audio devices themselves. To test capture for real, run `npm run tauri:dev`, grant Microphone and System Audio Recording when asked, play some audio (or join a test call), and record a minute. Check that `mic.wav` and `system.wav` in the meeting folder both have sound.
