//! Shared test helpers, for tests that need to know where the repository is.

use std::path::PathBuf;

/// The nearest directory version control answers for, walking up from the
/// crate, so a test finds the repository root however cargo was invoked.
pub(crate) fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        for marker in [".jj", ".git"] {
            if dir.join(marker).exists() {
                return dir;
            }
        }
        if !dir.pop() {
            panic!(
                "could not find the repository root from {}",
                env!("CARGO_MANIFEST_DIR")
            );
        }
    }
}
