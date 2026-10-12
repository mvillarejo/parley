//! Platform-independent core shared by the Parley app and `parley-mcp`:
//! storage, WAV files, voice activity detection, transcript merging and
//! rendering, and the MCP server.

pub mod markdown;
pub mod mcp;
pub mod merge;
pub mod paths;
pub mod store;
pub mod vad;
pub mod wav;
