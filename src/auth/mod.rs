//! Authentication module for the Fast.io CLI.
//!
//! Handles credential storage, token resolution, and the PKCE
//! browser-based login flow.

/// Credential storage and retrieval (keyring, file-based fallback).
pub mod credentials;
/// PKCE authorization code flow for browser-based login.
pub mod pkce;
/// Token resolution across the authentication precedence chain.
pub mod token;
