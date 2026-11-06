//! Choices made in Settings, other than shortcuts, kept in `settings.json`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use mail_core::paths::Paths;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// An email's original loads its remote images as it opens. Loading them
    /// tells the sender you opened it; when this is off, each email has a
    /// "Load images" button instead.
    pub remote_images: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            remote_images: true,
        }
    }
}

impl Preferences {
    pub fn path(paths: &Paths) -> PathBuf {
        paths.root.join("settings.json")
    }

    /// The saved choices. A missing or unreadable file gives the defaults,
    /// and so does any choice it doesn't mention.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(path, serde_json::to_string_pretty(self)? + "\n")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_images_load_unless_turned_off() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        assert!(Preferences::load(&path).remote_images);

        let off = Preferences {
            remote_images: false,
        };
        off.save(&path).unwrap();
        assert_eq!(Preferences::load(&path), off);
    }

    #[test]
    fn unknown_or_missing_choices_keep_their_defaults() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        fs::write(&path, r#"{"from_a_later_version": 3}"#).unwrap();
        assert_eq!(Preferences::load(&path), Preferences::default());
        fs::write(&path, "not json").unwrap();
        assert_eq!(Preferences::load(&path), Preferences::default());
    }
}
