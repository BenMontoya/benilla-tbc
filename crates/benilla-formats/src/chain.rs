//! The Burning Crusade 2.4.3 patch chain: base, expansion, locale and patch MPQs.
//! Archives are stored in ascending priority; later archives win.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use benilla_mpq::Archive;

/// One entry from a chain listing: an internal path and its uncompressed size.
pub struct ChainEntry {
    pub name: String,
    pub size: u64,
}

/// A priority-ordered patch chain of MPQ archives (`Send + Sync`; reads are `&self` and lock-free).
pub struct Chain {
    /// Ascending priority: later archives win.
    archives: Vec<Archive>,
}

fn find_file_name(names: &[String], want: &str) -> Option<String> {
    names
        .iter()
        .find(|name| name.eq_ignore_ascii_case(want))
        .cloned()
}

/// Pick the installed locale directory. A normal 2.4.3 install has one; if several are present,
/// prefer enUS, then the first case-folded locale name for deterministic behavior.
fn detect_locale(data_dir: &Path) -> Result<Option<(String, PathBuf)>> {
    let mut locales = Vec::new();
    for entry in
        std::fs::read_dir(data_dir).with_context(|| format!("listing {}", data_dir.display()))?
    {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let locale_mpq = entry.path().join(format!("locale-{name}.MPQ"));
        if locale_mpq.is_file() {
            locales.push((name, entry.path()));
        }
    }
    locales.sort_by_key(|(name, _)| name.to_ascii_lowercase());
    Ok(locales
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("enUS"))
        .cloned()
        .or_else(|| locales.into_iter().next()))
}

/// 2.4.3 mount order, lowest priority first.
///
/// `base-<locale>.MPQ` and `backup-<locale>.MPQ` are updater/support archives, not part of the
/// game-data chain. This follows the TBC extractor's effective priority: locale bases first, then
/// common/expansion, then global patches, then locale patches; later archives win.
fn mount_order(data_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut data_names: Vec<String> = std::fs::read_dir(data_dir)
        .with_context(|| format!("listing {}", data_dir.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            entry.path().is_file().then(|| entry.file_name())
        })
        .filter_map(|name| name.into_string().ok())
        .collect();
    data_names.sort();

    let mut order = Vec::new();
    let locale = detect_locale(data_dir)?;
    if let Some((locale_name, locale_dir)) = &locale {
        let mut locale_names: Vec<String> = std::fs::read_dir(locale_dir)
            .with_context(|| format!("listing {}", locale_dir.display()))?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                entry.path().is_file().then(|| entry.file_name())
            })
            .filter_map(|name| name.into_string().ok())
            .collect();
        locale_names.sort();

        for base in [
            format!("locale-{locale_name}.MPQ"),
            format!("speech-{locale_name}.MPQ"),
            format!("expansion-locale-{locale_name}.MPQ"),
            format!("expansion-speech-{locale_name}.MPQ"),
        ] {
            if let Some(name) = find_file_name(&locale_names, &base) {
                order.push(locale_dir.join(name));
            }
        }
    }

    for base in ["common.MPQ", "expansion.MPQ"] {
        if let Some(name) = find_file_name(&data_names, base) {
            order.push(data_dir.join(name));
        }
    }

    for patch in ["patch.MPQ", "patch-2.MPQ"] {
        if let Some(name) = find_file_name(&data_names, patch) {
            order.push(data_dir.join(name));
        }
    }

    if let Some((locale_name, locale_dir)) = locale {
        let mut locale_names: Vec<String> = std::fs::read_dir(&locale_dir)
            .with_context(|| format!("listing {}", locale_dir.display()))?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                entry.path().is_file().then(|| entry.file_name())
            })
            .filter_map(|name| name.into_string().ok())
            .collect();
        locale_names.sort();

        for patch in [
            format!("patch-{locale_name}.MPQ"),
            format!("patch-{locale_name}-2.MPQ"),
        ] {
            if let Some(name) = find_file_name(&locale_names, &patch) {
                order.push(locale_dir.join(name));
            }
        }
    }

    Ok(order)
}

impl Chain {
    /// Open a 2.4.3 `Data` directory, or one standalone `.MPQ` file.
    pub fn open(path: &Path) -> Result<Self> {
        let mut archives = Vec::new();
        if path.is_dir() {
            for mpq in mount_order(path)? {
                archives.push(
                    Archive::open(&mpq).with_context(|| format!("opening {}", mpq.display()))?,
                );
            }
            if archives.is_empty() {
                bail!("no Burning Crusade 2.4.3 MPQs found in {}", path.display());
            }
        } else {
            archives.push(
                Archive::open(path).with_context(|| format!("opening MPQ {}", path.display()))?,
            );
        }
        Ok(Self { archives })
    }

    /// The highest-priority archive with an entry for `name`, including delete markers.
    fn resolve(&self, name: &str) -> Option<&Archive> {
        self.archives.iter().rev().find(|a| a.contains(name))
    }

    /// Whether `name` (`/` or `\`, any case) is a readable file, not a delete marker.
    pub fn contains(&self, name: &str) -> bool {
        self.resolve(name)
            .is_some_and(|a| !a.is_delete_marker(name))
    }

    /// The path of the archive `name` resolves to, for debugging and extraction.
    pub fn find_file_archive(&self, name: &str) -> Option<&Path> {
        self.resolve(name).map(|a| a.path())
    }

    /// Read a file by internal path from its winning archive.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let archive = self.archive_for(name)?;
        archive
            .read_file(name)
            .with_context(|| format!("reading {name} from {}", archive.path().display()))
    }

    /// The winning archive for `name`.
    pub fn archive_for(&self, name: &str) -> Result<Archive> {
        let archive = self
            .resolve(name)
            .ok_or_else(|| anyhow!("file not in patch chain: {name}"))?;
        if archive.is_delete_marker(name) {
            bail!(
                "file deleted from patch chain: {name} (tombstoned by {})",
                archive.path().display()
            );
        }
        Ok(archive.clone())
    }

    /// `&mut` alias of [`Chain::read`] for existing call sites.
    pub fn read_file(&mut self, name: &str) -> Result<Vec<u8>> {
        self.read(name)
    }

    /// Named files with sizes, for development tooling. Files absent from every listfile remain
    /// readable by hash but cannot appear here.
    pub fn list(&self) -> Result<Vec<ChainEntry>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for archive in &self.archives {
            let Ok(listfile) = archive.read_file("(listfile)") else {
                continue;
            };
            for raw in String::from_utf8_lossy(&listfile).split([';', '\r', '\n']) {
                let name = raw.trim();
                if name.is_empty() || !seen.insert(name.replace('/', "\\").to_ascii_lowercase()) {
                    continue;
                }
                if let Some(a) = self.resolve(name) {
                    if a.is_delete_marker(name) {
                        continue;
                    }
                    out.push(ChainEntry {
                        name: name.to_string(),
                        size: a.file_size(name).unwrap_or(0) as u64,
                    });
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_lookup_is_case_insensitive() {
        let names = vec!["COMMON.mpq".to_string(), "patch-2.MPQ".to_string()];
        assert_eq!(
            find_file_name(&names, "common.MPQ").as_deref(),
            Some("COMMON.mpq")
        );
        assert_eq!(
            find_file_name(&names, "PATCH-2.mpq").as_deref(),
            Some("patch-2.MPQ")
        );
    }
}
