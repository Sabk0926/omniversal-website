//! The command line grammar.
//!
//! Hand-rolled rather than clap, for the same reason `omnia-http` has no
//! dependencies: the grammar is small, it is fully specified here, and the
//! parse is testable without a framework.
//!
//! # Bare words are an intent, not a typo
//!
//! `omni back up my photos nightly` works without the `ask`. That is the whole
//! premise of the system, so it should not need a subcommand. But treating
//! *every* unrecognised word as an intent turns `omni doctro` into a planning
//! request, which is worse than an error.
//!
//! The split is the same "looks like language" rule the shell handler uses:
//! several words become an intent, a single unrecognised word is a mistyped
//! subcommand and gets told so.

/// What the user asked the binary to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// No arguments: what happened, what was built, what needs a decision.
    Inbox,
    Ask(Ask),
    /// What this machine has taught itself.
    Capabilities,
    /// One capability in full, with its plan and its proof.
    Show {
        name: String,
    },
    /// The vetted parts library a plan may draw from.
    Parts,
    Doctor {
        offline: bool,
    },
    Help,
    Version,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Ask {
    pub intent: String,
    pub planner: PlannerChoice,
    pub install: Install,
}

/// Which planner turns the intent into a composition.
///
/// There is deliberately no "whichever is available" mode. Falling back from
/// the model to the stub without being asked would silently change what gets
/// built, and provenance is only worth having if it is not surprising.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerChoice {
    Model,
    Stub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// Install if this process can; otherwise print what to run.
    Auto,
    Always,
    Never,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub json: bool,
    pub verbose: bool,
}

const SUBCOMMANDS: [&str; 8] = [
    "ask",
    "capabilities",
    "caps",
    "show",
    "parts",
    "doctor",
    "help",
    "version",
];

pub fn parse<I>(argv: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = String>,
{
    let mut json = false;
    let mut verbose = false;
    let mut offline = false;
    let mut planner = PlannerChoice::Model;
    let mut install = Install::Auto;
    let mut positional: Vec<String> = Vec::new();
    let mut only_positional = false;

    let mut args = argv.into_iter().peekable();
    while let Some(arg) = args.next() {
        if only_positional {
            positional.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => only_positional = true,
            "--json" => json = true,
            "-v" | "--verbose" => verbose = true,
            "-h" | "--help" => return Ok(invocation(Command::Help, json, verbose)),
            "-V" | "--version" => return Ok(invocation(Command::Version, json, verbose)),
            "--offline" => offline = true,
            "--install" => install = Install::Always,
            "--no-install" => install = Install::Never,
            "--planner" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--planner needs a value: stub or model".to_string())?;
                planner = parse_planner(&value)?;
            }
            other if other.starts_with("--planner=") => {
                planner = parse_planner(&other["--planner=".len()..])?;
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option '{other}' (try: omni help)"));
            }
            _ => positional.push(arg),
        }
    }

    let command = command_from(positional, planner, install, offline)?;
    Ok(invocation(command, json, verbose))
}

fn invocation(command: Command, json: bool, verbose: bool) -> Invocation {
    Invocation {
        command,
        json,
        verbose,
    }
}

fn parse_planner(value: &str) -> Result<PlannerChoice, String> {
    match value {
        "stub" => Ok(PlannerChoice::Stub),
        "model" => Ok(PlannerChoice::Model),
        other => Err(format!(
            "unknown planner '{other}' (expected 'model' or 'stub')"
        )),
    }
}

fn command_from(
    positional: Vec<String>,
    planner: PlannerChoice,
    install: Install,
    offline: bool,
) -> Result<Command, String> {
    let Some(first) = positional.first() else {
        return Ok(Command::Inbox);
    };
    let rest = &positional[1..];

    match first.as_str() {
        "ask" => {
            let intent = rest.join(" ");
            if intent.trim().is_empty() {
                return Err("ask what? e.g. omni ask \"back up my photos nightly\"".into());
            }
            Ok(Command::Ask(Ask {
                intent,
                planner,
                install,
            }))
        }
        // `omni capabilities backup-pictures` is what people type instead of
        // `omni show`, so accept it rather than correcting them.
        "capabilities" | "caps" => match rest.first() {
            Some(name) => Ok(Command::Show { name: name.clone() }),
            None => Ok(Command::Capabilities),
        },
        "show" => match rest.first() {
            Some(name) => Ok(Command::Show { name: name.clone() }),
            None => Err("show which capability? omni capabilities lists them".into()),
        },
        "parts" => Ok(Command::Parts),
        "doctor" => Ok(Command::Doctor { offline }),
        "help" => Ok(Command::Help),
        "version" => Ok(Command::Version),
        _ => {
            // Several words with no recognised subcommand is a request.
            if positional.len() >= 2 {
                return Ok(Command::Ask(Ask {
                    intent: positional.join(" "),
                    planner,
                    install,
                }));
            }
            Err(match nearest_subcommand(first) {
                Some(suggestion) => format!("no command '{first}'. Did you mean '{suggestion}'?"),
                None => format!(
                    "no command '{first}'. To ask for something, use more than one word, \
                     or omni ask \"{first} ...\""
                ),
            })
        }
    }
}

/// Edit distance 1-2 from a real subcommand, same rule as the shell handler.
fn nearest_subcommand(word: &str) -> Option<&'static str> {
    SUBCOMMANDS
        .iter()
        .map(|candidate| (edit_distance(word, candidate), *candidate))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// Levenshtein, two rows. Small inputs, so the allocation does not matter and
/// the clarity does.
fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];

    for (i, l) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, r) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(l != r);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

pub const HELP: &str = "\
omni — ask the machine for what you want.

USAGE
  omni                          what happened, and what needs you
  omni ask \"<what you want>\"    build it if this machine cannot do it yet
  omni <what you want>          the same, without the word 'ask'
  omni capabilities             what this machine has taught itself
  omni show <name>              one capability: its plan, reach and proof
  omni parts                    the vetted parts a plan may be built from
  omni doctor                   re-run every test this machine relies on

OPTIONS
  --planner model|stub   how the intent becomes a plan. 'model' (default) asks
                         the local model; 'stub' is deterministic and needs no
                         model, but only understands backup requests
  --install              install the built package even if that needs root
  --no-install           build the package but leave it uninstalled
  --offline              doctor only: skip anything that touches the model
  --json                 machine-readable output
  -v, --verbose          debug logging, including model cache statistics
  -h, --help             this
  -V, --version          version

EXIT CODES
  0  it worked            5  the model backend is not answering
  1  I/O failure          6  a test did not pass, so nothing was kept
  2  bad usage or config   7  the sandbox refused the access a plan needed
  4  no model at that tier 8  no plan survived validation; nothing was built
";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation, String> {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn no_arguments_is_the_inbox() {
        assert_eq!(parse_args(&[]).unwrap().command, Command::Inbox);
    }

    #[test]
    fn ask_joins_the_rest_into_one_intent() {
        let invocation = parse_args(&["ask", "back", "up", "my", "photos"]).unwrap();
        match invocation.command {
            Command::Ask(ask) => assert_eq!(ask.intent, "back up my photos"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn several_bare_words_are_an_intent_without_the_word_ask() {
        let invocation = parse_args(&["back", "up", "my", "photos"]).unwrap();
        match invocation.command {
            Command::Ask(ask) => assert_eq!(ask.intent, "back up my photos"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn one_unrecognised_word_is_a_typo_not_a_request() {
        // The whole point: 'doctro' must not become a planning request.
        let err = parse_args(&["doctro"]).unwrap_err();
        assert!(err.contains("doctor"), "suggests the real command: {err}");
    }

    #[test]
    fn an_unrecognised_word_with_no_near_match_explains_how_to_ask() {
        let err = parse_args(&["quixotic"]).unwrap_err();
        assert!(err.contains("omni ask"), "{err}");
    }

    #[test]
    fn flags_may_follow_the_intent() {
        let invocation = parse_args(&["ask", "back up photos", "--planner=stub"]).unwrap();
        match invocation.command {
            Command::Ask(ask) => {
                assert_eq!(ask.planner, PlannerChoice::Stub);
                assert_eq!(ask.intent, "back up photos");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn planner_accepts_a_separate_value() {
        let invocation = parse_args(&["--planner", "stub", "ask", "back up photos"]).unwrap();
        match invocation.command {
            Command::Ask(ask) => assert_eq!(ask.planner, PlannerChoice::Stub),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_default_planner_is_the_model() {
        // Never silently downgrade: a build that used the stub must have been
        // asked for, because the stub's opinion of where photos live is fixed.
        let invocation = parse_args(&["ask", "back up photos"]).unwrap();
        match invocation.command {
            Command::Ask(ask) => assert_eq!(ask.planner, PlannerChoice::Model),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_double_dash_protects_an_intent_that_looks_like_a_flag() {
        let invocation = parse_args(&["ask", "--", "--json", "everything"]).unwrap();
        match invocation.command {
            Command::Ask(ask) => assert_eq!(ask.intent, "--json everything"),
            other => panic!("{other:?}"),
        }
        assert!(!invocation.json, "the flag after -- was not consumed");
    }

    #[test]
    fn unknown_options_are_refused_rather_than_ignored() {
        let err = parse_args(&["ask", "--plnner=stub", "back up photos"]).unwrap_err();
        assert!(err.contains("--plnner"), "{err}");
    }

    #[test]
    fn an_unknown_planner_lists_the_real_ones() {
        let err = parse_args(&["ask", "x y", "--planner=magic"]).unwrap_err();
        assert!(err.contains("model") && err.contains("stub"), "{err}");
    }

    #[test]
    fn ask_with_nothing_to_ask_says_so() {
        let err = parse_args(&["ask"]).unwrap_err();
        assert!(err.contains("ask what"), "{err}");
    }

    #[test]
    fn capabilities_with_a_name_shows_that_one() {
        assert_eq!(
            parse_args(&["capabilities", "backup-pictures"])
                .unwrap()
                .command,
            Command::Show {
                name: "backup-pictures".into()
            }
        );
        assert_eq!(
            parse_args(&["caps"]).unwrap().command,
            Command::Capabilities
        );
    }

    #[test]
    fn install_defaults_to_auto_and_is_overridable_both_ways() {
        let choice = |args: &[&str]| match parse_args(args).unwrap().command {
            Command::Ask(ask) => ask.install,
            other => panic!("{other:?}"),
        };
        assert_eq!(choice(&["ask", "back up photos"]), Install::Auto);
        assert_eq!(
            choice(&["ask", "back up photos", "--install"]),
            Install::Always
        );
        assert_eq!(
            choice(&["ask", "back up photos", "--no-install"]),
            Install::Never
        );
    }

    #[test]
    fn doctor_takes_offline() {
        assert_eq!(
            parse_args(&["doctor", "--offline"]).unwrap().command,
            Command::Doctor { offline: true }
        );
    }

    #[test]
    fn help_and_version_short_circuit_everything_after_them() {
        // A broken argument list must still be able to reach the help text.
        assert_eq!(
            parse_args(&["--help", "--nonsense"]).unwrap().command,
            Command::Help
        );
        assert_eq!(parse_args(&["-V"]).unwrap().command, Command::Version);
    }

    #[test]
    fn the_help_text_documents_every_subcommand() {
        for subcommand in SUBCOMMANDS {
            if subcommand == "caps" {
                continue; // an alias, deliberately undocumented
            }
            assert!(
                HELP.contains(subcommand),
                "'{subcommand}' is missing from the help text"
            );
        }
    }

    #[test]
    fn edit_distance_is_symmetric_and_counts_single_edits() {
        assert_eq!(edit_distance("doctor", "doctor"), 0);
        assert_eq!(edit_distance("doctro", "doctor"), 2);
        assert_eq!(edit_distance("prts", "parts"), 1);
        assert_eq!(edit_distance("parts", "prts"), 1);
    }
}
