use std::ops::Deref;
use std::path::Path;

use tempfile::TempDir;

/// A fresh directory under the target's integration-test temp dir, removed
/// when this value drops. Bind it to a local that outlives every use of the
/// path: borrowing it as a `&Path` from a temporary deletes the directory at
/// the end of that statement.
pub struct Scratch(TempDir);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(&format!("doppelscan-{name}-"))
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap();
        Self(dir)
    }
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        self.0.path()
    }
}
