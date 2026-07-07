//! Landlock confinement for spawned engine children (Linux ≥ 5.13). Best-effort:
//! any failure (old kernel, unsupported) leaves the child unconfined rather than
//! killing a legitimate download. Opt-in via `LaunchOptions::sandbox`.

use std::path::PathBuf;

/// Restrict the *current* process (call inside a child's `pre_exec`) to
/// read/execute anywhere but write/create only under `write_dirs`.
pub fn restrict(write_dirs: &[PathBuf]) {
    use landlock::{
        Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI,
    };
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let abi = ABI::V1;
        let all = AccessFs::from_all(abi);
        let read = AccessFs::from_read(abi);
        let mut rs = Ruleset::default().handle_access(all)?.create()?;
        // Read + execute across the whole filesystem so the binary, its shared
        // libraries and the CA bundle still load.
        rs = rs.add_rule(PathBeneath::new(PathFd::new("/")?, read))?;
        // Write/create only under the allowed directories.
        for d in write_dirs {
            if let Ok(fd) = PathFd::new(d) {
                rs = rs.add_rule(PathBeneath::new(fd, all))?;
            }
        }
        rs.restrict_self()?;
        Ok(())
    };
    let _ = run();
}
