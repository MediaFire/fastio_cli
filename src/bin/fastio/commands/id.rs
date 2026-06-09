//! `fastio id info <ID>...` — offline `OpaqueId` classification.
//!
//! Pure, local inspection: no auth, no HTTP client. Each id is classified by
//! [`fastio_cli::opaque_id::classify`] and rendered through the shared
//! [`OutputConfig`], so `--format json|table|csv|markdown`, `--fields`, and
//! `--quiet` all work.
//!
//! Output is **always a JSON array** (one object per id) — even for a single
//! id — so the table/CSV/markdown renderers produce a clean record table rather
//! than the per-key H1 sections the markdown renderer emits for a bare object,
//! and so `jq` consumers see a uniform shape.

use anyhow::Result;
use serde_json::Value;

use crate::cli::IdCommands;
use fastio_cli::opaque_id;
use fastio_cli::output::OutputConfig;

/// Execute an `id` subcommand.
pub fn execute(cmd: &IdCommands, output: &OutputConfig) -> Result<()> {
    match cmd {
        IdCommands::Info { ids } => {
            let rows: Vec<Value> = ids
                .iter()
                .map(|id| opaque_id::to_json(&opaque_id::classify(id)))
                .collect();
            output.render(&Value::Array(rows))?;
            Ok(())
        }
    }
}
