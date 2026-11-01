//! Where Mail keeps its files.
//!
//! Everything lives in one folder, `~/Library/Application Support/org.erlin.mail`
//! by default. The refresh token is not here: it is in the login Keychain.

use std::path::{Path, PathBuf};

use anyhow::Context;
use directories::{ProjectDirs, UserDirs};

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn discover() -> anyhow::Result<Self> {
        let project =
            ProjectDirs::from("org", "erlin", "mail").context("Could not find your home folder")?;
        Ok(Self::under(project.data_dir()))
    }

    /// Keeps all data under `root`, for tests and separate profiles.
    pub fn under(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn database(&self) -> PathBuf {
        self.root.join("mail.sqlite3")
    }

    /// The Google Cloud OAuth client (ID and the not-really-secret secret that
    /// Google issues to desktop apps), copied here by `mail auth`.
    pub fn oauth_client(&self) -> PathBuf {
        self.root.join("oauth-client.json")
    }
}

/// Your Downloads folder, where opened attachments are saved.
pub fn downloads() -> anyhow::Result<PathBuf> {
    UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .context("Could not find your Downloads folder")
}
