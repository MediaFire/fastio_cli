//! Core library for the Fast.io CLI and MCP server.
//!
//! Provides the API client, authentication, configuration, error handling,
//! and output formatting shared by both the CLI and MCP interfaces.

/// REST API endpoint abstractions for the Fast.io platform.
pub mod api;
/// Authentication and credential management (OAuth PKCE, API keys, tokens).
pub mod auth;
/// HTTP client with auth headers, retry logic, and API envelope unwrapping.
pub mod client;
/// Profile and credential configuration (XDG-based).
pub mod config;
/// Structured error types with context-aware suggestions.
pub mod error;
/// Output formatting: table, JSON, and CSV renderers with TTY detection.
pub mod output;
