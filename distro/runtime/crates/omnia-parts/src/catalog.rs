//! The catalogue of vetted parts, and how it is shown to the model.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path as FsPath;

use crate::manifest::Part;
use crate::{PartsError, Result};

/// Parts compiled in, so the runtime works before anything is installed and
/// tests need no filesystem. A deployed system loads `/usr/share/omnia/parts`
/// on top of these.
const BUILTIN: &[(&str, &str)] = &[
    ("snapshot.toml", include_str!("../parts/snapshot.toml")),
    ("schedule.toml", include_str!("../parts/schedule.toml")),
    (
        "verify-restore.toml",
        include_str!("../parts/verify-restore.toml"),
    ),
];

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    parts: BTreeMap<String, Part>,
}

impl Catalog {
    pub fn builtin() -> Result<Catalog> {
        let mut catalog = Catalog::default();
        for (origin, text) in BUILTIN {
            let part = Part::parse(text, origin)?;
            catalog.parts.insert(part.name.clone(), part);
        }
        Ok(catalog)
    }

    /// Load `*.toml` from a directory on top of what is already present.
    ///
    /// A later part with the same name replaces an earlier one, so an operator
    /// can override a shipped part without editing files they do not own.
    pub fn load_dir(&mut self, dir: &FsPath) -> Result<usize> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            // A missing parts directory is normal on a machine that has not
            // installed the package yet; the builtins still work.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => {
                return Err(PartsError::Manifest {
                    path: dir.display().to_string(),
                    reason: e.to_string(),
                })
            }
        };

        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        paths.sort();

        let mut loaded = 0;
        for path in paths {
            let text = fs::read_to_string(&path).map_err(|e| PartsError::Manifest {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;
            let part = Part::parse(&text, &path.display().to_string())?;
            self.parts.insert(part.name.clone(), part);
            loaded += 1;
        }
        Ok(loaded)
    }

    pub fn get(&self, name: &str) -> Result<&Part> {
        self.parts.get(name).ok_or_else(|| PartsError::UnknownPart {
            named: name.to_string(),
            available: self.names(),
        })
    }

    pub fn names(&self) -> Vec<String> {
        self.parts.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Render the catalogue for the model's prompt.
    ///
    /// This belongs in the *stable* half of the prompt: it changes only when
    /// parts are installed, which is roughly never, so it is exactly the kind
    /// of large block the KV cache should be absorbing.
    ///
    /// Output is ordered (BTreeMap) so the text is byte-identical between runs.
    /// Iteration order that wobbled would silently invalidate the cache.
    pub fn render_for_prompt(&self) -> String {
        let mut out = String::new();
        for part in self.parts.values() {
            let _ = writeln!(out, "- {} — {}", part.name, part.summary);
            for param in &part.params {
                let requirement = if param.required {
                    "required".to_string()
                } else if let Some(default) = &param.default {
                    format!("optional, default {default}")
                } else {
                    "optional".to_string()
                };
                let kind = format!("{:?}", param.kind).to_lowercase();
                let values = if param.values.is_empty() {
                    String::new()
                } else {
                    format!(" one of [{}]", param.values.join(", "))
                };
                let _ = writeln!(
                    out,
                    "    {}: {kind}{values} ({requirement}) — {}",
                    param.name, param.doc
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parts_load_and_validate() {
        let catalog = Catalog::builtin().expect("shipped manifests must be valid");
        assert_eq!(
            catalog.names(),
            vec!["schedule", "snapshot", "verify-restore"]
        );
    }

    #[test]
    fn an_unknown_part_lists_the_real_ones() {
        let catalog = Catalog::builtin().unwrap();
        let err = catalog.get("backup-everything").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("backup-everything"), "{text}");
        assert!(
            text.contains("snapshot"),
            "retry prompt needs the real list: {text}"
        );
    }

    #[test]
    fn the_prompt_rendering_is_byte_stable() {
        // Unstable ordering here would silently destroy KV cache reuse, since
        // this block sits in the cacheable prefix.
        let catalog = Catalog::builtin().unwrap();
        assert_eq!(catalog.render_for_prompt(), catalog.render_for_prompt());
    }

    #[test]
    fn the_prompt_rendering_names_parts_and_parameters() {
        let rendered = Catalog::builtin().unwrap().render_for_prompt();
        assert!(rendered.contains("snapshot"));
        assert!(rendered.contains("source: path (required)"), "{rendered}");
        assert!(rendered.contains("default daily"), "{rendered}");
    }

    #[test]
    fn a_missing_parts_directory_is_not_an_error() {
        let mut catalog = Catalog::builtin().unwrap();
        let loaded = catalog
            .load_dir(FsPath::new("/nonexistent/omnia/parts"))
            .unwrap();
        assert_eq!(loaded, 0);
        assert_eq!(catalog.len(), 3, "builtins survive");
    }
}
