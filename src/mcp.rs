//! MCP server (stdio).
use std::path::Path;

use crate::engine::Engine;

pub fn serve(_engine: &Engine, _root: &Path) -> anyhow::Result<()> {
    anyhow::bail!("not yet")
}
