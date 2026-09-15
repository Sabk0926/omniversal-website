//! `omni doctor` — re-establish that what this machine claims it can do, it
//! can still do.
//!
//! Ordinary monitoring tells you the backup unit exited 0. It cannot tell you
//! the backup can be restored. Because every capability keeps the test that
//! proved it, this machine has a growing, specific definition of "healthy", and
//! doctor is what runs it.
//!
//! Re-running a retained test is not a simulation: it is the same `prove` call
//! the forge gates on, against a scratch directory, with the same hostile
//! canary. A capability that passes here is one whose behaviour is unchanged
//! since the day it was kept.

use omnia_core::{paths, Result};
use omnia_forge::{prove, Scratch};
use omnia_model::ModelClient;

use crate::context::Context;
use crate::render;

pub fn run(context: &Context, offline: bool, json: bool) -> Result<i32> {
    let mut report = Report::default();

    configuration(context, &mut report);
    parts(context, &mut report);
    if offline {
        report.section("model", vec![("checked".into(), "no (--offline)".into())]);
    } else {
        model(context, &mut report);
    }
    kernel(&mut report);
    workspace(context, &mut report);
    packages(context, &mut report);
    let failures = capabilities(context, &mut report);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "sections": report.as_json(),
                "capabilities_checked": report.checked,
                "capabilities_failing": failures,
            })
        );
    } else {
        print!("{}", report.render());
        println!("{}", report.verdict(failures));
    }

    // Non-zero when a retained test fails, so this is usable from a cron job
    // or a monitoring check without parsing the output.
    Ok(if failures > 0 { 6 } else { 0 })
}

fn configuration(context: &Context, report: &mut Report) {
    let settings = &context.config.settings;
    let mut rows = vec![
        ("sources".into(), context.config.sources.join(", ")),
        ("profile".into(), settings.general.profile.clone()),
        ("model tier".into(), settings.model.tier.clone()),
    ];
    if context.config.suppressed.is_empty() {
        rows.push(("suppressed".into(), "nothing".into()));
    } else {
        // Silent suppression is how you get a bug report that takes a week to
        // diagnose. This is the whole reason Config records it.
        rows.push((
            "suppressed".into(),
            format!(
                "{} (locked by machine policy, so your setting had no effect)",
                context.config.suppressed.join(", ")
            ),
        ));
    }
    report.section("configuration", rows);
}

fn parts(context: &Context, report: &mut Report) {
    let names = context.catalog.names();
    let mut unusable = Vec::new();
    for name in &names {
        if let Ok(part) = context.catalog.get(name) {
            let missing = context.catalog.missing_binaries(part);
            if !missing.is_empty() {
                unusable.push(format!("{name} needs {}", missing.join(", ")));
            }
        }
    }

    report.section(
        "parts",
        vec![
            (
                "available".into(),
                format!("{} ({} from disk)", names.len(), context.installed_parts),
            ),
            (
                "usable here".into(),
                if unusable.is_empty() {
                    "all of them".into()
                } else {
                    unusable.join("; ")
                },
            ),
        ],
    );
}

fn model(context: &Context, report: &mut Report) {
    let settings = &context.config.settings.model;
    let client = ModelClient::from_config(settings);
    let reachable = client.reachable();

    let status = if !reachable {
        "not answering — omni ask --planner=model will fail".to_string()
    } else {
        match client.loaded_model() {
            Ok(Some(name)) => format!("answering, {name} loaded"),
            Ok(None) => "answering, but no model is loaded yet".into(),
            Err(e) => format!("answering, but /props did not parse: {e}"),
        }
    };

    report.section(
        "model",
        vec![
            ("endpoint".into(), settings.base_url()),
            ("status".into(), status),
            (
                "kv cache".into(),
                if settings.cache_static_prefix {
                    "static prefix reused (this is what keeps planning fast without a GPU)".into()
                } else {
                    "DISABLED — every request re-prefills the whole prompt".into()
                },
            ),
        ],
    );
}

fn kernel(report: &mut Report) {
    let device = std::path::Path::new("/dev/omnia");
    report.section(
        "kernel",
        vec![(
            "/dev/omnia".into(),
            if device.exists() {
                "present".into()
            } else {
                "absent — the event path is not wired up on this machine".into()
            },
        )],
    );
}

/// The forge needs somewhere to stage a package. Finding out it cannot write
/// there at 03:00, mid-build, is worse than finding out here.
fn workspace(context: &Context, report: &mut Report) {
    let dir = context.build_dir();
    let status = match std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(dir.join(".omni-doctor-probe"), b"probe"))
    {
        Ok(()) => {
            let _ = std::fs::remove_file(dir.join(".omni-doctor-probe"));
            "writable".to_string()
        }
        Err(e) => format!("NOT writable: {e}"),
    };
    report.section(
        "workspace",
        vec![
            ("build".into(), format!("{} — {status}", dir.display())),
            (
                "registry".into(),
                paths::registry_dir().display().to_string(),
            ),
        ],
    );
}

/// Does dpkg agree with the registry about what is installed?
///
/// The two can legitimately drift: `omni ask --no-install` records a
/// capability and leaves the package on disk, and `apt remove` takes a package
/// away without telling the registry. Neither is an error, and neither should
/// be silent — "the machine says it can do this, and the thing that does it is
/// not installed" is exactly the state a human needs told.
///
/// Reported, not counted as a test failure. dpkg is the source of truth for
/// what is on disk; this is the layer beneath the registry checking it, which
/// is the same rule as everywhere else: nothing audits itself.
fn packages(context: &Context, report: &mut Report) {
    let capabilities = context.registry.all();
    if capabilities.is_empty() {
        return;
    }

    let mut rows = Vec::new();
    for capability in capabilities {
        rows.push((
            capability.name.clone(),
            match installed_version(&capability.package) {
                Some(version) if version == capability.version => "installed".to_string(),
                Some(version) => format!(
                    "installed as {version}, but the record says {}",
                    capability.version
                ),
                None => format!(
                    "NOT installed — '{}' is not known to dpkg, so nothing will run",
                    capability.package
                ),
            },
        ));
    }
    report.section("packages", rows);
}

/// The installed version of a package, or `None` if dpkg does not have it.
///
/// A missing or unhappy dpkg also yields `None`, which reads as "not
/// installed". That is the safe direction: claiming a package is present when
/// it cannot be confirmed is the failure mode worth avoiding.
fn installed_version(package: &str) -> Option<String> {
    let output = std::process::Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Status} ${Version}"])
        .arg(package)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (status, version) = text.trim().split_once(' ')?;
    (status == "installed").then(|| version.to_string())
}

/// Re-run every retained test. Returns how many failed.
fn capabilities(context: &Context, report: &mut Report) -> usize {
    let capabilities = context.registry.all();
    report.checked = capabilities.len();

    if capabilities.is_empty() {
        report.section(
            "capabilities",
            vec![("checked".into(), "nothing built yet".into())],
        );
        return 0;
    }

    let mut rows = Vec::new();
    let mut failures = 0;
    for capability in capabilities {
        let result = Scratch::new(&format!("doctor-{}", capability.name))
            .and_then(|scratch| prove(&capability.steps, scratch.path()));
        let status = match result {
            Ok(proof) if proof.passed => format!("passes — {}", proof.asserts),
            Ok(proof) => {
                failures += 1;
                format!("FAILS — {}", proof.detail)
            }
            Err(e) => {
                // Could not run the test at all. Counted as a failure: an
                // unverifiable capability is not a working one.
                failures += 1;
                format!("COULD NOT CHECK — {e}")
            }
        };
        rows.push((capability.name.clone(), status));
    }

    report.section("capabilities", rows);
    failures
}

#[derive(Default)]
struct Report {
    sections: Vec<(String, Vec<(String, String)>)>,
    checked: usize,
}

impl Report {
    fn section(&mut self, name: &str, rows: Vec<(String, String)>) {
        self.sections.push((name.to_string(), rows));
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for (name, rows) in &self.sections {
            out.push_str(name);
            out.push('\n');
            let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
            for (label, value) in rows {
                out.push_str(&format!("    {label:<width$}  {value}\n"));
            }
            out.push('\n');
        }
        out
    }

    fn verdict(&self, failures: usize) -> String {
        if self.checked == 0 {
            return "Nothing to verify yet.".into();
        }
        if failures == 0 {
            format!(
                "{} checked, all passing.",
                render::plural(self.checked, "capability", "capabilities")
            )
        } else {
            format!(
                "{} checked, {failures} FAILING. Those capabilities no longer do what \
                 they claimed.",
                render::plural(self.checked, "capability", "capabilities")
            )
        }
    }

    fn as_json(&self) -> serde_json::Value {
        let sections: serde_json::Map<String, serde_json::Value> = self
            .sections
            .iter()
            .map(|(name, rows)| {
                let table: serde_json::Map<String, serde_json::Value> = rows
                    .iter()
                    .map(|(label, value)| (label.clone(), serde_json::Value::String(value.clone())))
                    .collect();
                (name.clone(), serde_json::Value::Object(table))
            })
            .collect();
        serde_json::Value::Object(sections)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_aligns_its_labels() {
        let mut report = Report::default();
        report.section(
            "model",
            vec![
                ("endpoint".into(), "http://127.0.0.1:9111".into()),
                ("kv cache".into(), "on".into()),
            ],
        );
        let text = report.render();
        let endpoint = text.find("http://").unwrap();
        let cache = text.find("on\n").unwrap();
        let column = |index: usize| index - text[..index].rfind('\n').unwrap();
        assert_eq!(
            column(endpoint),
            column(cache),
            "values must line up:\n{text}"
        );
    }

    #[test]
    fn a_package_dpkg_does_not_have_reads_as_not_installed() {
        // Including when dpkg itself is absent: an unconfirmable package must
        // never be reported as present.
        assert_eq!(installed_version("omnia-cap-no-such-thing-9c1f"), None);
    }

    #[test]
    fn the_verdict_distinguishes_nothing_to_check_from_everything_passing() {
        let mut report = Report::default();
        assert!(report.verdict(0).contains("Nothing to verify"));
        report.checked = 2;
        assert!(report.verdict(0).contains("all passing"));
        let failing = report.verdict(1);
        assert!(failing.contains("FAILING"), "{failing}");
        assert!(
            failing.contains("no longer do what they claimed"),
            "says what a failure means: {failing}"
        );
    }
}
