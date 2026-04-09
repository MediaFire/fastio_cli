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
/// Download session endpoints.
pub mod download;
/// Audit and activity event endpoints.
pub mod event;
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
/// Organization management endpoints.
pub mod org;
/// File preview endpoints.
pub mod preview;
/// Share link management endpoints.
pub mod share;
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
/// Workflow primitives (tasks, worklogs, approvals, todos).
pub mod workflow;
/// Workspace management endpoints.
pub mod workspace;
