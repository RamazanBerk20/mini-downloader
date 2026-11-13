//! Landlock confinement for spawned engine children (Linux ≥ 5.13). Because the
//! setting is opt-in, failure is explicit: a child must never run unconfined
//! after the user asked for confinement.
//!
//! The ruleset is built in the parent ([`prepare`]); the forked child only
//! enforces it ([`enforce`]). Between `fork` and `exec` of a multithreaded
//! parent only async-signal-safe work is allowed, so no allocation there.

use std::path::PathBuf;

use landlock::{
    Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreated, RulesetCreatedAttr, RulesetStatus, ABI,
};

/// Ruleset allowing read/execute anywhere but write/create only under
/// `write_dirs`.
pub fn prepare(write_dirs: &[PathBuf]) -> Result<RulesetCreated, Box<dyn std::error::Error>> {
    let abi = ABI::V1;
    let all = AccessFs::from_all(abi);
    let read = AccessFs::from_read(abi);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all)?
        .create()?;
    // Read + execute across the whole filesystem so the binary, its shared
    // libraries and the CA bundle still load.
    ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new("/")?, read))?;
    // Write/create only under the allowed directories. Every configured path is
    // mandatory; silently skipping one would make behavior depend on timing.
    for directory in write_dirs {
        ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new(directory)?, all))?;
    }
    Ok(ruleset)
}

/// Restrict the current process (the child, inside `pre_exec`). Syscalls only:
/// errors are reported as `EPERM` and never dropped (dropping could `free`).
pub fn enforce(ruleset: Option<RulesetCreated>) -> std::io::Result<()> {
    const EPERM: i32 = 1;
    let refused = || std::io::Error::from_raw_os_error(EPERM);
    let status = ruleset
        .ok_or_else(refused)?
        .restrict_self()
        .map_err(|error| {
            std::mem::forget(error);
            refused()
        })?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(refused());
    }
    Ok(())
}
