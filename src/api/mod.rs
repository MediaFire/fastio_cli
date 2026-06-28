//! API endpoint builders for the Fast.io REST API.
//!
//! Each sub-module provides functions that call the HTTP client
//! and return typed responses for a specific API domain.

/// AI chat and prompt endpoints.
pub mod ai;
/// Connected-app management endpoints.
pub mod apps;
/// Asset metadata and transformation endpoints.
pub mod asset;
/// Authentication and token endpoints.
pub mod auth;
/// File and folder comment endpoints.
pub mod comment;
/// Per-workspace Dashboard (actionable card feed) endpoints.
pub mod dashboard;
/// Download session endpoints.
pub mod download;
/// Audit and activity event endpoints.
pub mod event;
/// File Share (durable single-file link) management + consumption endpoints.
pub mod fileshare;
/// How-To (grounded product-guidance) endpoint.
pub mod howto;
/// External storage import endpoints.
pub mod import;
/// Workspace invitation endpoints.
pub mod invitation;
/// File locking endpoints.
pub mod locking;
/// Organization and workspace member endpoints.
pub mod member;
/// Metadata extraction and template management endpoints.
pub mod metadata;
/// Workflow Orchestration (v3.2) durable-runtime endpoints.
///
/// Distinct from the Tasks API in [`workflow`]: this surface ships the
/// workflow profile + runtime, immutable templates, triggers, obligations,
/// extraction schemas, the signed audit chain, outbound webhook
/// subscriptions, concurrency pools, external subjects, the realtime-token
/// mint, and the v3.5b review surface.
pub mod orchestration;
/// Organization management endpoints.
pub mod org;
/// File preview endpoints.
pub mod preview;
/// Unified (grouped-bucket) search endpoints across a workspace or share.
pub mod search;
/// Share link management endpoints.
pub mod share;
/// E-signature (SignEnvelope) endpoints (workspace-parented).
pub mod signing;
/// Low-level storage node endpoints.
pub mod storage;
/// System health and status endpoints.
pub mod system;
/// Shared API response and request types.
pub mod types;
/// Upload session endpoints.
pub mod upload;
/// User profile endpoints.
pub mod user;
/// Tasks API endpoints (task lists, tasks, comments, attachments).
pub mod workflow;
/// Workspace management endpoints.
pub mod workspace;
