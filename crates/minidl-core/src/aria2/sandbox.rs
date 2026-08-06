//! Landlock confinement for spawned engine children (Linux ≥ 5.13). Because the
//! setting is opt-in, failure is explicit: a child must never run unconfined
//! after the user asked for confinement.

use std::path::PathBuf;

/// Restrict the *current* process (call inside a child's `pre_exec`) to
/// read/execute anywhere but write/create only under `write_dirs`.
pub fn restrict(write_dirs: &[PathBuf]) -> Result<(), Box<dyn std::error::Error>> {
    use landlock::{
        Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus, ABI,
    };
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
    let status = ruleset.restrict_self()?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(format!("Landlock was not fully enforced: {status:?}").into());
    }
    Ok(())
}
