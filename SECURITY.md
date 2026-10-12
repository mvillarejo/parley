# Security policy

## Reporting a vulnerability

Please report security issues privately to the maintainer (GitHub private vulnerability reporting once the repository is published) rather than in a public issue.

## How Parley handles data

- Recordings, transcripts and the search database stay in the data folder on your Mac (see the README). Files are created with your user's permissions; `config.json` and `parley.db` are mode `0600`.
- API keys for transcription profiles are stored in plain text in `config.json`. Treat the machine as trusted.
- Audio leaves the Mac only when the active profile points to a remote service (Gladia, Groq, OpenAI…). With local Ollama it stays on the machine.
- `parley-mcp` opens the database read-only and only reads; it never modifies or deletes meetings.
- Recording other people may require their consent. Parley never starts recording on its own and shows a red indicator in the menu bar and the recorder while it records.
