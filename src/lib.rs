//! sessionwiki: index, search, and resume every AI coding session on your
//! machine, across Claude Code, Codex, Gemini CLI, OpenCode, the Cline family,
//! gajae-code, and Continue.
//!
//! The binary is a thin CLI over this library. The pieces worth reusing as a
//! dependency are [`adapters`] (parse a session file into a [`model::Session`])
//! and [`index`] (an incremental SQLite FTS5 index over parsed sessions).

pub mod account_link;
pub mod adapters;
pub mod blame;
pub mod commands;
pub mod hook;
pub mod index;
pub mod mcp;
pub mod migrate;
pub mod model;
pub mod redact;
pub mod resume;
pub mod util;
pub mod web;
pub mod window;
