//! Part manifests: what a part takes, what it may touch, what it runs.

use serde::{Deserialize, Serialize};

use crate::{PartsError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    /// A filesystem path. Validated hard -- see `ParamType::validate`.
    Path,
    Text,
    Integer,
    /// Local time of day, HH:MM.
    Time,
    Bool,
    /// One of `Param::values`.
    Enum,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ParamType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub doc: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub write_paths: Vec<String>,
    #[serde(default)]
    pub network: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecKind {
    /// Runs as a command.
    #[default]
    Command,
    /// Consumed by the packager, which emits a systemd timer.
    Timer,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Exec {
    #[serde(default)]
    pub kind: ExecKind,
    #[serde(default)]
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub name: String,
    pub summary: String,
    pub version: String,
    #[serde(default, rename = "param")]
    pub params: Vec<Param>,
    /// Binaries that must exist for this part to run. Checked before a plan is
    /// accepted, so a capability that could not run is refused at build time
    /// rather than failing on its first scheduled run at 03:00.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub permissions: Permissions,
    pub exec: Exec,
}

impl Part {
    pub fn parse(text: &str, origin: &str) -> Result<Part> {
        let part: Part = toml::from_str(text).map_err(|e| PartsError::Manifest {
            path: origin.into(),
            reason: e.to_string(),
        })?;
        part.check(origin)?;
        Ok(part)
    }

    /// Catch manifest mistakes at load time rather than at build time. A part
    /// whose permission template names a parameter that does not exist would
    /// otherwise fail only when someone happened to use that part.
    fn check(&self, origin: &str) -> Result<()> {
        let bad = |reason: String| PartsError::Manifest {
            path: origin.into(),
            reason,
        };

        for param in &self.params {
            if param.kind == ParamType::Enum && param.values.is_empty() {
                return Err(bad(format!(
                    "parameter '{}' is an enum with no values",
                    param.name
                )));
            }
            if let Some(default) = &param.default {
                if param.required {
                    return Err(bad(format!(
                        "parameter '{}' is required and also has a default",
                        param.name
                    )));
                }
                self.validate_value(param, default)
                    .map_err(|e| bad(format!("default for '{}' is invalid: {e}", param.name)))?;
            }
        }

        for template in self
            .permissions
            .read_paths
            .iter()
            .chain(&self.permissions.write_paths)
            .chain(&self.exec.argv)
        {
            for name in placeholders(template) {
                if !self.params.iter().any(|p| p.name == name) {
                    return Err(bad(format!(
                        "{template:?} refers to unknown parameter '{name}'"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }

    pub fn param_names(&self) -> Vec<String> {
        self.params.iter().map(|p| p.name.clone()).collect()
    }

    pub fn validate_value(&self, param: &Param, value: &str) -> Result<()> {
        let bad = |reason: &str| PartsError::BadValue {
            part: self.name.clone(),
            param: param.name.clone(),
            value: value.to_string(),
            reason: reason.to_string(),
        };

        match param.kind {
            ParamType::Path => validate_path(value).map_err(|reason| bad(&reason)),
            ParamType::Integer => value
                .parse::<i64>()
                .map(|_| ())
                .map_err(|_| bad("not an integer")),
            ParamType::Bool => match value {
                "true" | "false" => Ok(()),
                _ => Err(bad("expected true or false")),
            },
            ParamType::Time => validate_time(value).map_err(|reason| bad(&reason)),
            ParamType::Enum => {
                if param.values.iter().any(|v| v == value) {
                    Ok(())
                } else {
                    Err(bad(&format!(
                        "expected one of: {}",
                        param.values.join(", ")
                    )))
                }
            }
            ParamType::Text => {
                if value.contains('\0') {
                    Err(bad("contains a NUL byte"))
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// Path validation is the security-relevant one.
///
/// Arguments become argv elements and permission entries, so a path that
/// escapes upward or hides a NUL would widen the sandbox past what the
/// manifest promises. Rejecting is cheap; a traversal is not.
fn validate_path(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err("empty path".into());
    }
    if value.contains('\0') {
        return Err("contains a NUL byte".into());
    }
    if !(value.starts_with('/') || value.starts_with("~/")) {
        return Err("must be absolute or start with ~/".into());
    }
    // Reject traversal by segment, so a directory legitimately named "..foo"
    // still works.
    if value.split('/').any(|segment| segment == "..") {
        return Err("contains a '..' segment".into());
    }
    if value.contains("//") {
        return Err("contains an empty path segment".into());
    }
    Ok(())
}

fn validate_time(value: &str) -> std::result::Result<(), String> {
    let Some((hours, minutes)) = value.split_once(':') else {
        return Err("expected HH:MM".into());
    };
    if hours.len() != 2 || minutes.len() != 2 {
        return Err("expected zero-padded HH:MM".into());
    }
    let hours: u32 = hours
        .parse()
        .map_err(|_| "hours are not a number".to_string())?;
    let minutes: u32 = minutes
        .parse()
        .map_err(|_| "minutes are not a number".to_string())?;
    if hours > 23 {
        return Err("hours must be 00-23".into());
    }
    if minutes > 59 {
        return Err("minutes must be 00-59".into());
    }
    Ok(())
}

/// Names inside `{braces}` in a template.
pub(crate) fn placeholders(template: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let name = &rest[open + 1..open + close];
        if !name.is_empty() {
            found.push(name.to_string());
        }
        rest = &rest[open + close + 1..];
    }
    found
}

/// Substitute `{name}` from `args`. Placeholders are replaced as whole values;
/// because the result is an argv element rather than a shell word, nothing
/// needs quoting and nothing can be re-parsed.
pub(crate) fn substitute(template: &str, args: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        out.push_str(&rest[..open]);
        let name = &rest[open + 1..open + close];
        match args(name) {
            Some(value) => out.push_str(&value),
            // An unresolved placeholder is a manifest bug caught at load time,
            // so leaving it literal here is only reachable in tests.
            None => out.push_str(&rest[open..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_and_home_paths_are_accepted() {
        assert!(validate_path("/var/backups").is_ok());
        assert!(validate_path("~/Pictures").is_ok());
    }

    #[test]
    fn traversal_is_rejected() {
        assert!(validate_path("/home/../etc/shadow").is_err());
        assert!(validate_path("~/../../root").is_err());
    }

    #[test]
    fn a_directory_named_with_leading_dots_still_works() {
        // Rejecting on substring rather than segment would break this.
        assert!(validate_path("/srv/..config").is_ok());
    }

    #[test]
    fn relative_paths_and_nuls_are_rejected() {
        assert!(validate_path("Pictures").is_err());
        assert!(validate_path("/tmp/a\0b").is_err());
        assert!(validate_path("").is_err());
    }

    #[test]
    fn times_must_be_real_clock_times() {
        assert!(validate_time("02:00").is_ok());
        assert!(validate_time("23:59").is_ok());
        assert!(validate_time("24:00").is_err());
        assert!(validate_time("02:60").is_err());
        assert!(validate_time("2:00").is_err(), "must be zero padded");
        assert!(validate_time("morning").is_err());
    }

    #[test]
    fn placeholders_are_found() {
        assert_eq!(placeholders("{a}/x/{b}"), vec!["a", "b"]);
        assert_eq!(placeholders("no braces"), Vec::<String>::new());
        assert_eq!(placeholders("{unclosed"), Vec::<String>::new());
    }

    #[test]
    fn substitution_replaces_whole_values() {
        let args = |name: &str| match name {
            "source" => Some("~/Pictures".to_string()),
            _ => None,
        };
        assert_eq!(substitute("{source}/", &args), "~/Pictures/");
    }

    #[test]
    fn a_manifest_naming_an_unknown_parameter_is_rejected_at_load() {
        let text = r#"
            name = "bad"
            summary = "s"
            version = "1.0"
            [[param]]
            name = "source"
            type = "path"
            required = true
            [permissions]
            read_paths = ["{destination}"]
            [exec]
            argv = []
        "#;
        let err = Part::parse(text, "bad.toml").unwrap_err();
        assert!(err.to_string().contains("destination"), "{err}");
    }

    #[test]
    fn a_required_parameter_with_a_default_is_rejected() {
        let text = r#"
            name = "bad"
            summary = "s"
            version = "1.0"
            [[param]]
            name = "x"
            type = "text"
            required = true
            default = "y"
            [exec]
            argv = []
        "#;
        assert!(Part::parse(text, "bad.toml").is_err());
    }
}
