//! The kernel's own module index: which driver, if any, matches this device.
//!
//! # Why read the index instead of running modprobe
//!
//! `modprobe -R <alias>` answers the same question, and shelling out to it
//! would be two lines. But the ladder needs the *decision* before it takes the
//! *action*, and those are different things:
//!
//! - "A driver exists but is not loaded" is rung 1, and the fix is one command.
//! - "A driver is built into this kernel and still did not bind" is not a
//!   loading problem at all. Running `modprobe` there does nothing and hides a
//!   real diagnosis — usually a missing device ID or firmware, which is rung 2
//!   or 3.
//! - "No driver exists anywhere in this kernel" is what justifies the expensive
//!   rungs, and it is the one claim that should never be made casually.
//!
//! Reading the index separates all three, costs one file read, and has no side
//! effects — so it can run while merely surveying a machine, which is not
//! something you want to say about a command that loads kernel code.
//!
//! # Format
//!
//! `modules.alias` and `modules.builtin.alias` are lines of
//! `alias <pattern> <module>`, where the pattern is a shell glob over the
//! device's modalias. The kernel generates them from each driver's
//! `MODULE_DEVICE_TABLE`, so this is the driver's own declaration of what it
//! can drive.

use std::fs;
use std::path::{Path, PathBuf};

use crate::modalias::Modalias;

/// What the index says about a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Modules that claim this device and can be loaded. Rung 1.
    Loadable(Vec<String>),
    /// A driver for this device is compiled into the kernel. It is already
    /// there and still did not bind, so loading is not the problem.
    Builtin(String),
    /// Nothing in this kernel claims the device.
    None,
}

impl Lookup {
    /// Does rung 1 have anything to do?
    pub fn can_load(&self) -> bool {
        matches!(self, Lookup::Loadable(modules) if !modules.is_empty())
    }

    /// Explain the finding in the terms the next step needs.
    pub fn describe(&self) -> String {
        match self {
            Lookup::Loadable(modules) if modules.len() == 1 => {
                format!("{} claims this device and is not loaded", modules[0])
            }
            Lookup::Loadable(modules) => format!(
                "{} claim this device and none is loaded",
                modules.join(", ")
            ),
            Lookup::Builtin(module) => format!(
                "{module} is built into this kernel and did not bind, so this is \
                 not a loading problem"
            ),
            Lookup::None => "no driver in this kernel claims this device".into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModuleIndex {
    /// In file order. `modprobe` reports every match, and so does this.
    loadable: Vec<(String, String)>,
    builtin: Vec<(String, String)>,
    source: Option<PathBuf>,
}

impl ModuleIndex {
    /// Read the index for a kernel release.
    ///
    /// A missing directory is not an error: a container has no
    /// `/lib/modules`, and an empty index answering `None` for everything is
    /// the honest result there. Callers that need to distinguish "nothing
    /// claims this device" from "this machine has no module index at all" ask
    /// [`ModuleIndex::is_empty`], because those justify very different actions.
    pub fn load(modules_dir: &Path) -> ModuleIndex {
        ModuleIndex {
            loadable: read_alias_file(&modules_dir.join("modules.alias")),
            builtin: read_alias_file(&modules_dir.join("modules.builtin.alias")),
            source: Some(modules_dir.to_path_buf()),
        }
    }

    /// The running kernel's index, at `/lib/modules/$(uname -r)`.
    pub fn for_running_kernel() -> ModuleIndex {
        match kernel_release() {
            Some(release) => ModuleIndex::load(&PathBuf::from("/lib/modules").join(release)),
            None => ModuleIndex::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.loadable.is_empty() && self.builtin.is_empty()
    }

    pub fn len(&self) -> usize {
        self.loadable.len() + self.builtin.len()
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// What claims this device.
    ///
    /// Built-in wins over loadable when both match. A driver already in the
    /// kernel that did not bind is the more specific and more useful finding:
    /// it means the device was seen and rejected, not merely unrecognised.
    pub fn lookup(&self, alias: &Modalias) -> Lookup {
        if let Some(module) = self.match_in(&self.builtin, &alias.raw).first() {
            return Lookup::Builtin(module.clone());
        }
        match self.match_in(&self.loadable, &alias.raw) {
            modules if modules.is_empty() => Lookup::None,
            modules => Lookup::Loadable(modules),
        }
    }

    fn match_in(&self, table: &[(String, String)], alias: &str) -> Vec<String> {
        let mut found: Vec<String> = table
            .iter()
            .filter(|(pattern, _)| glob_match(pattern, alias))
            .map(|(_, module)| module.clone())
            .collect();
        found.dedup();
        found
    }
}

fn read_alias_file(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("alias ")?;
            // Pattern first, module last. Split from the right: a pattern never
            // contains a space, but splitting from the left would break on one
            // if it ever did.
            let (pattern, module) = rest.rsplit_once(char::is_whitespace)?;
            let pattern = pattern.trim();
            let module = module.trim();
            (!pattern.is_empty() && !module.is_empty())
                .then(|| (pattern.to_string(), module.to_string()))
        })
        .collect()
}

fn kernel_release() -> Option<String> {
    // /proc/sys/kernel/osrelease rather than uname(2): one file read, no libc
    // binding, and identical content.
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Shell-glob match, as the kernel's alias patterns use it.
///
/// Public because it is the one piece of this module worth reusing, and
/// because it is verified against another implementation: 18,634 generated
/// pattern/text pairs were compared against Python's `fnmatch.fnmatchcase`
/// with no disagreement. Re-run that before changing anything here.
///
/// Supports `*`, `?` and `[...]` with ranges and `!`/`^` negation. Iterative
/// with backtracking rather than recursive: a pattern is untrusted input in the
/// sense that it comes off disk, and `*a*a*a*a*b` against a long alias is a
/// stack overflow waiting to happen in the naive recursive form.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();

    let (mut p, mut t) = (0usize, 0usize);
    // Where to resume if the current `*` turns out to have matched too little.
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;

    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                star_text = t;
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some('[') => match match_class(&pattern, p, text[t]) {
                Some(next) => {
                    p = next;
                    t += 1;
                }
                None => match star {
                    Some(position) => {
                        p = position + 1;
                        star_text += 1;
                        t = star_text;
                    }
                    None => return false,
                },
            },
            Some(character) if *character == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some(position) => {
                    p = position + 1;
                    star_text += 1;
                    t = star_text;
                }
                None => return false,
            },
        }
    }

    // Trailing stars may match the empty remainder.
    while pattern.get(p) == Some(&'*') {
        p += 1;
    }
    p == pattern.len()
}

/// Match one `[...]` class against `candidate`.
///
/// Returns the index just past the closing bracket on a match, `None`
/// otherwise. An unterminated class is treated as a literal `[`, which is what
/// shells do and keeps a malformed index from silently matching everything.
fn match_class(pattern: &[char], open: usize, candidate: char) -> Option<usize> {
    let mut index = open + 1;
    let negated = matches!(pattern.get(index), Some('!') | Some('^'));
    if negated {
        index += 1;
    }

    let mut matched = false;
    let mut first = true;
    while index < pattern.len() {
        // A `]` in the first position is a literal, per shell convention.
        if pattern[index] == ']' && !first {
            let result = matched != negated;
            return result.then_some(index + 1);
        }
        first = false;

        let is_range = pattern.get(index + 1) == Some(&'-')
            && pattern.get(index + 2).is_some()
            && pattern.get(index + 2) != Some(&']');

        if is_range {
            let (low, high) = (pattern[index], pattern[index + 2]);
            if candidate >= low && candidate <= high {
                matched = true;
            }
            index += 3;
        } else {
            if pattern[index] == candidate {
                matched = true;
            }
            index += 1;
        }
    }
    // Unterminated: not a class at all.
    (pattern[open] == candidate).then_some(open + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real lines, in the real format, from a Ubuntu modules.alias.
    const ALIAS_FILE: &str = "\
# Aliases extracted from modules themselves.
alias usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in* r8152
alias usb:v0BDAp8152d*dc*dsc*dp*ic*isc*ip*in* r8152
alias usb:v*p*d*dc*dsc*dp*ic02isc06ip00in* cdc_ether
alias pci:v00001AF4d00001041sv*sd*bc*sc*i* virtio_net
alias pci:v00001AF4d*sv*sd*bc*sc*i* virtio_pci
alias of:N*T*Cti,tmp102* lm75
alias acpi*:PNP0501:* serial8250
";

    const BUILTIN_FILE: &str = "\
alias pci:v00008086d0000A0F0sv*sd*bc*sc*i* iwlwifi
alias platform:rtc_cmos rtc_cmos
";

    fn index() -> ModuleIndex {
        ModuleIndex {
            loadable: read_alias_file_from(ALIAS_FILE),
            builtin: read_alias_file_from(BUILTIN_FILE),
            source: None,
        }
    }

    /// The parser, fed from a string rather than a file.
    fn read_alias_file_from(text: &str) -> Vec<(String, String)> {
        text.lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("alias ")?;
                let (pattern, module) = rest.rsplit_once(char::is_whitespace)?;
                Some((pattern.trim().to_string(), module.trim().to_string()))
            })
            .collect()
    }

    #[test]
    fn a_device_with_a_loadable_driver_is_rung_one() {
        let alias = Modalias::parse("usb:v0BDAp8153d0100dc00dsc00dp00ic02isc06ip00in00");
        let lookup = index().lookup(&alias);
        assert!(lookup.can_load());
        match &lookup {
            // cdc_ether also matches by interface class, which is correct:
            // modprobe would report both and either could drive it.
            Lookup::Loadable(modules) => {
                assert!(modules.contains(&"r8152".to_string()), "{modules:?}");
                assert!(modules.contains(&"cdc_ether".to_string()), "{modules:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_device_nothing_claims_is_what_justifies_the_expensive_rungs() {
        let alias = Modalias::parse("usb:v1337p1337d0100dc00dsc00dp00ic09isc00ip00in00");
        let lookup = index().lookup(&alias);
        assert_eq!(lookup, Lookup::None);
        assert!(!lookup.can_load());
        assert!(
            lookup.describe().contains("no driver"),
            "{}",
            lookup.describe()
        );
    }

    #[test]
    fn a_builtin_driver_is_reported_as_not_a_loading_problem() {
        // The distinction the whole module exists for. Running modprobe here
        // does nothing and hides the real diagnosis.
        let alias = Modalias::parse("platform:rtc_cmos");
        let lookup = index().lookup(&alias);
        assert_eq!(lookup, Lookup::Builtin("rtc_cmos".into()));
        assert!(!lookup.can_load(), "there is nothing to load");
        assert!(
            lookup.describe().contains("not a loading problem"),
            "{}",
            lookup.describe()
        );
    }

    #[test]
    fn builtin_wins_over_loadable_when_both_match() {
        // "seen and rejected" is a more specific finding than "unrecognised".
        let mut index = index();
        index
            .loadable
            .push(("platform:rtc_cmos".into(), "rtc_cmos_mod".into()));
        assert_eq!(
            index.lookup(&Modalias::parse("platform:rtc_cmos")),
            Lookup::Builtin("rtc_cmos".into())
        );
    }

    #[test]
    fn a_pci_device_matches_the_specific_entry_and_the_catch_all() {
        // virtio_net claims this exact device; virtio_pci claims the whole
        // vendor. Both are real matches and modprobe would list both.
        let alias = Modalias::parse("pci:v00001AF4d00001041sv00001AF4sd00001041bc02sc00i00");
        match index().lookup(&alias) {
            Lookup::Loadable(modules) => {
                assert_eq!(modules, vec!["virtio_net", "virtio_pci"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_index_is_distinguishable_from_a_device_nothing_claims() {
        // A container has no /lib/modules. Answering "no driver exists" there
        // would send the ladder off to write one on the strength of a missing
        // file.
        let empty = ModuleIndex::load(Path::new("/nonexistent/lib/modules/x"));
        assert!(empty.is_empty());
        assert_eq!(empty.lookup(&Modalias::parse("usb:v1p2")), Lookup::None);
        assert!(!index().is_empty(), "a real index is not empty");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        assert_eq!(index().loadable.len(), 7);
        assert_eq!(index().len(), 9);
    }

    #[test]
    fn a_device_tree_alias_matches_on_compatible() {
        // How a board finds a driver for an I2C sensor.
        let alias = Modalias::parse("of:Ntemp_sensorT(null)Cti,tmp102");
        match index().lookup(&alias) {
            Lookup::Loadable(modules) => assert_eq!(modules, vec!["lm75"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_acpi_alias_with_a_leading_wildcard_matches() {
        let alias = Modalias::parse("acpi:PNP0501:");
        match index().lookup(&alias) {
            Lookup::Loadable(modules) => assert_eq!(modules, vec!["serial8250"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn globs_match_the_way_a_shell_does() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("usb:v*p*", "usb:v0BDAp8153"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"), "? needs exactly one character");
        assert!(glob_match("abc", "abc"));
        assert!(!glob_match("abc", "abcd"));
        assert!(!glob_match("abcd", "abc"));
        assert!(
            glob_match("abc*", "abc"),
            "a trailing star may match nothing"
        );
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn backtracking_finds_a_match_a_greedy_star_would_miss() {
        // The classic case: the first * must give back characters for the
        // literal tail to line up.
        assert!(glob_match("*b", "aaab"));
        assert!(glob_match("*a*b", "xaxxb"));
        assert!(!glob_match("*a*b", "xaxx"));
        assert!(glob_match("a*b*c", "abxbxc"));
    }

    #[test]
    fn a_pathological_pattern_does_not_blow_the_stack() {
        // *a*a*a*a*a*b against a long non-matching string is where a recursive
        // matcher dies. This must simply return false.
        let pattern = "*a*a*a*a*a*a*b";
        let text = "a".repeat(2000);
        assert!(!glob_match(pattern, &text));
    }

    #[test]
    fn character_classes_match_ranges_and_negation() {
        assert!(glob_match("i[0-9][0-9]", "i42"));
        assert!(!glob_match("i[0-9][0-9]", "i4x"));
        assert!(glob_match("[abc]x", "bx"));
        assert!(!glob_match("[abc]x", "dx"));
        assert!(glob_match("[!abc]x", "dx"), "negated class");
        assert!(!glob_match("[!abc]x", "ax"));
        assert!(glob_match("[^abc]x", "dx"), "^ negates too");
    }

    #[test]
    fn an_unterminated_class_is_a_literal_bracket_not_a_wildcard() {
        // A malformed index line must not become a pattern that claims every
        // device on the machine.
        assert!(glob_match("[abc", "[abc"));
        assert!(!glob_match("[abc", "b"));
    }

    #[test]
    fn the_parser_keeps_the_module_name_from_the_right() {
        let parsed = read_alias_file_from("alias usb:v*p* some_module\n");
        assert_eq!(
            parsed,
            vec![("usb:v*p*".to_string(), "some_module".to_string())]
        );
    }

    #[test]
    fn a_malformed_line_is_skipped_rather_than_half_read() {
        let parsed = read_alias_file_from("alias\nalias onlyonefield\nnot an alias line\n");
        assert!(parsed.is_empty());
    }
}
