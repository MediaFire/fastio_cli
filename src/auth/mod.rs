//! Authentication module for the Fast.io CLI.
//!
//! Handles credential storage, token resolution, and the PKCE
//! browser-based login flow.

/// Credential storage and retrieval (keyring, file-based fallback).
pub mod credentials;
/// RFC 8252 loopback listener that receives the browser sign-in redirect.
pub mod loopback;
/// PKCE authorization code flow for browser-based login.
pub mod pkce;
/// Token resolution across the authentication precedence chain.
pub mod token;
