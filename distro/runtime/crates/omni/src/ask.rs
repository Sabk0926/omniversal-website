//! `omni ask` — the command that makes the machine gain a capability.
//!
//! Everything interesting happens in `omnia-forge`. This module's job is to
//! choose a planner, hand over, and then report the result in a way that makes
//! an unattended build reviewable: what it built, what that can touch, and what
//! it proved before being kept.

use std::path::Path;
use std::process::Command as Process;

use omnia_core::{Error, Result};
use omnia_forge::{Forge, ModelPlanner, Outcome, Planner, StubPlanner};
use omnia_model::{ModelClient, Tier};

use crate::args::{Ask, Install, PlannerChoice};
use crate::context::Context;
use crate::render;

pub fn run(ask: &Ask, context: &mut Context, json: bool) -> Result<i32> {
    let planner = planner_for(ask.planner, context)?;

    // A cheap text hint before spending a planning call. Deliberately advisory:
    // the fingerprint check inside the forge is what actually prevents
    // duplicates, and it needs a plan first.
    if !json {
        for existing in context.registry.similar_intents(&ask.intent) {
            eprintln!(
                "note: this machine already has '{}' — {}",
                existing.name, existing.intent
            );
        }
    }

    let workdir = context.build_dir();
    std::fs::create_dir_all(&workdir)
        .map_err(|e| Error::io(format!("cannot create {}", workdir.display()), e))?;

    let forge_config = context.config.settings.forge.clone();
    let home = context.home.clone();
    let catalog = &context.catalog;
    let outcome = {
        let mut forge = Forge {
            catalog,
            registry: &mut context.registry,
            planner: planner.as_ref(),
            config: &forge_config,
            home,
            workdir: workdir.clone(),
        };
        forge
            .build(&ask.intent)
            .map_err(|e| Error::config(e.to_string()))?
    };

    match outcome {
        Outcome::AlreadyHave(capability) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "outcome": "already-have",
                        "capability": &*capability,
                    })
                );
            } else {
                println!("this machine can already do that. Nothing was built.\n");
                print!("{}", render::capability_detail(&capability));
            }
            Ok(0)
        }

        Outcome::Built {
            capability,
            deb,
            proof,
        } => {
            let installed = install(&deb, ask.install)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "outcome": "built",
                        "capability": &*capability,
                        "package_file": deb.display().to_string(),
                        "proof": {
                            "asserts": proof.asserts,
                            "passed": proof.passed,
                            "detail": proof.detail,
                        },
                        "installed": matches!(installed, Installed::Yes),
                    })
                );
                return Ok(0);
            }

            println!("built {} {}\n", capability.name, capability.version);
            let mut detail = String::new();
            render::field(&mut detail, "proves", &proof.asserts);
            render::field(
                &mut detail,
                "reaches",
                &render::reach(&capability.permissions),
            );
            render::field(&mut detail, "package", &deb.display().to_string());
            print!("{detail}");

            match installed {
                Installed::Yes => println!("\ninstalled. The machine has this now."),
                Installed::Skipped(reason) => println!("\nnot installed: {reason}"),
            }
            Ok(0)
        }

        Outcome::Refused { attempts } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "outcome": "refused",
                        "intent": &ask.intent,
                        "attempts": attempts,
                    })
                );
            }
            // An error rather than a printed message: the exit code is what
            // the shell handler and any calling script branch on.
            Err(Error::Refused {
                intent: ask.intent.clone(),
                attempts,
            })
        }
    }
}

/// Choose the planner, and fail rather than substituting one for another.
///
/// There is no automatic fallback from the model to the stub. The stub has a
/// fixed opinion about where photos live; quietly building that because the
/// model was down would be worse than saying the model is down.
fn planner_for(choice: PlannerChoice, context: &Context) -> Result<Box<dyn Planner>> {
    match choice {
        PlannerChoice::Stub => Ok(Box::new(StubPlanner)),
        PlannerChoice::Model => {
            let settings = &context.config.settings.model;
            let tier: Tier = settings.tier.parse().map_err(Error::config)?;
            let client = ModelClient::from_config(settings);

            if !client.reachable() {
                return Err(Error::BackendUnavailable {
                    message: format!(
                        "nothing is answering at {}. Start omnia-modeld, or use \
                         --planner=stub, which needs no model but only understands \
                         backup requests.",
                        settings.base_url()
                    ),
                });
            }

            // A server that is up with nothing resident is a real state:
            // llama.cpp loads lazily. Distinguishing it from "down" is the
            // difference between waiting and fixing. An error from /props is
            // not conclusive either way, so it is not treated as fatal.
            if matches!(client.loaded_model(), Ok(None)) {
                return Err(Error::NoModel {
                    tier: tier.to_string(),
                    hint: format!("{} is up but has no model loaded", settings.base_url()),
                });
            }

            Ok(Box::new(ModelPlanner::new(
                client,
                tier,
                context.system_facts(),
            )))
        }
    }
}

enum Installed {
    Yes,
    Skipped(String),
}

/// Hand the package to dpkg.
///
/// Building and installing are separated on purpose: the forge produces a file,
/// and putting that file on the system is a privileged act that can be declined,
/// deferred, or done on another machine entirely. Copying the `.deb` to a second
/// box and installing it there is the intended path, not a workaround.
fn install(deb: &Path, choice: Install) -> Result<Installed> {
    if choice == Install::Never {
        return Ok(Installed::Skipped(format!(
            "--no-install was given. To install it: sudo dpkg --install {}",
            deb.display()
        )));
    }

    if choice == Install::Auto && effective_uid() != 0 {
        return Ok(Installed::Skipped(format!(
            "installing needs root. Run: sudo dpkg --install {}",
            deb.display()
        )));
    }

    let output = Process::new("dpkg")
        .arg("--install")
        .arg(deb)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::io(
                    "dpkg is not on PATH, so the package cannot be installed here",
                    e,
                )
            } else {
                Error::io("cannot run dpkg", e)
            }
        })?;

    if output.status.success() {
        return Ok(Installed::Yes);
    }

    // dpkg's own diagnostic is more useful than anything written here, so it
    // is passed through rather than summarised.
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(Error::io(
        format!(
            "dpkg refused to install {}: {}",
            deb.display(),
            if detail.is_empty() {
                "no diagnostic"
            } else {
                detail
            }
        ),
        std::io::Error::other("dpkg exited non-zero"),
    ))
}

/// One call, no arguments, infallible — the same reason `omnia-core::paths`
/// declares `getuid` itself rather than pulling in libc.
fn effective_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_install_explains_how_to_install_it_later() {
        let Installed::Skipped(reason) = install(Path::new("/tmp/x.deb"), Install::Never).unwrap()
        else {
            panic!("--no-install must not install");
        };
        assert!(reason.contains("dpkg --install /tmp/x.deb"), "{reason}");
    }

    #[test]
    fn auto_install_without_root_says_what_to_run_rather_than_failing() {
        // The test suite does not run as root, and neither does a desktop
        // user. Producing a package and telling them how to install it is a
        // success, not an error.
        if effective_uid() == 0 {
            return;
        }
        let Installed::Skipped(reason) = install(Path::new("/tmp/x.deb"), Install::Auto).unwrap()
        else {
            panic!("auto-install must not install without root");
        };
        assert!(reason.contains("sudo dpkg --install"), "{reason}");
    }

    #[test]
    fn the_stub_planner_needs_no_model_and_names_itself() {
        // The point of the stub being a real planner: this must work on a
        // machine with nothing listening.
        let planner = planner_for(PlannerChoice::Stub, &fake_context()).unwrap();
        assert!(
            planner.describe().contains("stub"),
            "{}",
            planner.describe()
        );
    }

    #[test]
    fn the_model_planner_refuses_rather_than_falling_back() {
        // Port 1 has nothing listening, which is the situation this must
        // report instead of silently using the stub.
        let mut context = fake_context();
        context.config.settings.model.port = 1;
        let err = refusal(&context);
        assert_eq!(err.exit_code(), 5, "backend-unavailable exit code");
        let text = err.to_string();
        assert!(
            text.contains("--planner=stub"),
            "offers the way out: {text}"
        );
        assert!(!text.contains("falling back"), "{text}");
    }

    #[test]
    fn an_unknown_tier_is_a_configuration_error_not_a_backend_error() {
        let mut context = fake_context();
        context.config.settings.model.tier = "enormous".into();
        let err = refusal(&context);
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("enormous"), "{err}");
    }

    /// `Box<dyn Planner>` is not `Debug`, so `unwrap_err` is unavailable.
    fn refusal(context: &Context) -> Error {
        match planner_for(PlannerChoice::Model, context) {
            Ok(planner) => panic!("expected a refusal, got {}", planner.describe()),
            Err(e) => e,
        }
    }

    fn fake_context() -> Context {
        Context {
            config: omnia_core::Config::builtin(),
            catalog: omnia_parts::Catalog::builtin().unwrap(),
            registry: omnia_registry::Registry::open(Path::new("/nonexistent/omnia-test-registry"))
                .unwrap(),
            installed_parts: 0,
            home: Some("/home/test".into()),
        }
    }
}
