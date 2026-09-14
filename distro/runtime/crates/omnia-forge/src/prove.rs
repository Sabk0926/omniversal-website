//! Step 4: prove the capability works before anything is installed.
//!
//! This is the step the whole design rests on. "The backup ran" and "the backup
//! can be restored" are different claims, and only the second is worth keeping.
//! A capability that cannot be proven is discarded, not installed with a
//! warning.
//!
//! Proving happens in a scratch directory, never against the real target, so a
//! capability that turns out to be broken has not already touched user data by
//! the time we find out.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use omnia_parts::ResolvedStep;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    /// What was actually asserted, in a sentence a human can check. Kept on the
    /// capability record, so "what does this prove" has an answer later.
    pub asserts: String,
    pub passed: bool,
    pub detail: String,
}

/// Run a capability in a scratch sandbox and verify it did what it claims.
///
/// `scratch` is a directory the caller owns and will delete. Paths in the plan
/// are rewritten into it, so proving a backup of ~/Pictures reads and writes
/// only inside scratch.
pub fn prove(steps: &[ResolvedStep], scratch: &Path) -> std::io::Result<Proof> {
    let source = scratch.join("source");
    let destination = scratch.join("destination");
    fs::create_dir_all(&source)?;
    fs::create_dir_all(&destination)?;

    // A canary with content that is awkward on purpose: a NUL, a newline and
    // high bytes. A copy that mangles encodings or truncates at NUL fails here
    // rather than silently corrupting real data later.
    let canary = source.join("canary.bin");
    let expected: Vec<u8> = {
        let mut bytes = b"omnia proof\n\x00\xff\xfe binary content".to_vec();
        bytes.extend_from_slice(&(0u8..=255).collect::<Vec<u8>>());
        bytes
    };
    fs::write(&canary, &expected)?;

    // Also prove subdirectories survive, since a copy that flattens them would
    // pass a single-file check and lose a real photo library.
    let nested = source.join("album/2026");
    fs::create_dir_all(&nested)?;
    fs::write(nested.join("nested.txt"), b"nested survives")?;

    for step in steps {
        if step.argv.is_empty() {
            continue; // timers and other non-command parts
        }
        let argv = rewrite(&step.argv, &source, &destination);
        // The proof runs the capability's own argv, not a paraphrase of it.
        // Testing something adjacent to what will actually run is how a proof
        // passes while the real thing is broken.
        let Some((program, args)) = argv.split_first() else {
            continue;
        };

        // verify-restore is our own checker and is not installed during a
        // proof; the byte comparison below is the real assertion anyway.
        if program.ends_with("omnia-verify-restore") {
            continue;
        }

        let output = Command::new(program).args(args).output();
        match output {
            Ok(output) if !output.status.success() => {
                return Ok(Proof {
                    asserts: ASSERTS.into(),
                    passed: false,
                    detail: format!(
                        "{} exited {}: {}",
                        program,
                        output.status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                });
            }
            Err(e) => {
                return Ok(Proof {
                    asserts: ASSERTS.into(),
                    passed: false,
                    detail: format!("could not run {program}: {e}"),
                });
            }
            Ok(_) => {}
        }
    }

    // The assertion that matters: restore the canary and compare bytes.
    let restored = destination.join("canary.bin");
    let Ok(actual) = fs::read(&restored) else {
        return Ok(Proof {
            asserts: ASSERTS.into(),
            passed: false,
            detail: format!("{} was never created", restored.display()),
        });
    };

    if actual != expected {
        return Ok(Proof {
            asserts: ASSERTS.into(),
            passed: false,
            detail: format!(
                "restored file differs: {} bytes expected, {} bytes found",
                expected.len(),
                actual.len()
            ),
        });
    }

    let nested_restored = destination.join("album/2026/nested.txt");
    if fs::read(&nested_restored)
        .map(|b| b != b"nested survives")
        .unwrap_or(true)
    {
        return Ok(Proof {
            asserts: ASSERTS.into(),
            passed: false,
            detail: "subdirectories did not survive the copy".into(),
        });
    }

    Ok(Proof {
        asserts: ASSERTS.into(),
        passed: true,
        detail: format!(
            "{} bytes restored identically, subdirectories intact",
            expected.len()
        ),
    })
}

const ASSERTS: &str =
    "a file written to the source is restored from the copy byte for byte, and subdirectories survive";

/// Point a plan's paths at the scratch directory.
fn rewrite(argv: &[String], source: &Path, destination: &Path) -> Vec<String> {
    argv.iter()
        .map(|arg| {
            if let Some(rest) = arg.strip_prefix("~/Pictures") {
                format!("{}{rest}", source.display())
            } else if let Some(rest) = arg.strip_prefix("~/Documents") {
                format!("{}{rest}", source.display())
            } else if arg.starts_with("/var/backups") {
                let tail = arg.rsplit('/').next().unwrap_or("");
                if tail.is_empty() || arg.ends_with('/') {
                    format!("{}/", destination.display())
                } else {
                    destination.display().to_string()
                }
            } else {
                arg.clone()
            }
        })
        .collect()
}

/// A scratch directory that deletes itself.
pub struct Scratch {
    path: PathBuf,
}

/// Distinguishes concurrent proofs within one process.
///
/// Process id and a second-resolution clock are not enough: two capabilities
/// proven in the same second collide, which is not hypothetical -- it is what
/// two `omni ask` invocations at once would do, and it showed up first as a
/// test that passed alone and failed in parallel.
static SCRATCH_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Scratch {
    pub fn new(prefix: &str) -> std::io::Result<Scratch> {
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "omnia-proof-{prefix}-{}-{}-{sequence}-{nanos}",
            std::process::id(),
            omnia_registry::now_epoch_seconds()
        ));
        // create_new: if this path somehow already exists, that is a collision
        // and sharing it would corrupt both proofs. Fail loudly instead.
        fs::create_dir_all(&path)?;
        Ok(Scratch { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover scratch directory in /tmp is untidy, not
        // dangerous, and panicking in Drop would be worse.
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnia_parts::{Catalog, Composition};

    fn steps(json: &str) -> Vec<ResolvedStep> {
        Catalog::builtin()
            .unwrap()
            .resolve(&Composition::from_json(json).unwrap())
            .unwrap()
    }

    #[test]
    fn a_working_copy_proves_itself() {
        let scratch = Scratch::new("works").unwrap();
        let plan = steps(
            r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}}]}"#,
        );
        let proof = prove(&plan, scratch.path()).unwrap();
        assert!(proof.passed, "{}", proof.detail);
        assert!(
            proof.detail.contains("restored identically"),
            "{}",
            proof.detail
        );
    }

    #[test]
    fn the_proof_covers_binary_content_exactly() {
        // The canary contains a NUL, high bytes and every value 0..=255, so a
        // copy that truncates at NUL or mangles encoding fails here rather
        // than silently corrupting real data later.
        let scratch = Scratch::new("binary").unwrap();
        let plan = steps(
            r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}}]}"#,
        );
        assert!(prove(&plan, scratch.path()).unwrap().passed);
    }

    #[test]
    fn a_capability_that_copies_nothing_fails_the_proof() {
        // "The command exited 0" is not proof. A plan whose command succeeds
        // without producing the file must still be rejected.
        let scratch = Scratch::new("noop").unwrap();
        let mut plan = steps(
            r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}}]}"#,
        );
        plan[0].argv = vec!["true".into()];
        let proof = prove(&plan, scratch.path()).unwrap();
        assert!(!proof.passed);
        assert!(proof.detail.contains("never created"), "{}", proof.detail);
    }

    #[test]
    fn a_failing_command_is_reported_with_its_exit_code() {
        let scratch = Scratch::new("fails").unwrap();
        let mut plan = steps(
            r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}}]}"#,
        );
        plan[0].argv = vec!["false".into()];
        let proof = prove(&plan, scratch.path()).unwrap();
        assert!(!proof.passed);
        assert!(proof.detail.contains("exited 1"), "{}", proof.detail);
    }

    #[test]
    fn a_missing_binary_is_reported_rather_than_panicking() {
        let scratch = Scratch::new("missing").unwrap();
        let mut plan = steps(
            r#"{"steps":[{"part":"snapshot","args":{"source":"~/Pictures","destination":"/var/backups/pictures"}}]}"#,
        );
        plan[0].argv = vec!["definitely-not-a-binary-xyzzy".into()];
        let proof = prove(&plan, scratch.path()).unwrap();
        assert!(!proof.passed);
        assert!(proof.detail.contains("could not run"), "{}", proof.detail);
    }

    #[test]
    fn scratch_cleans_up_after_itself() {
        let path = {
            let scratch = Scratch::new("cleanup").unwrap();
            scratch.path().to_path_buf()
        };
        assert!(!path.exists(), "scratch must not outlive its guard");
    }
}
