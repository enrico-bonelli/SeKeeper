//! Local, optional conveniences. No secret ever belongs here: this file is not
//! encrypted and holds only the location of the key file, never its content.

use crate::storage::{self, SaveOutcome};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Preferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyfile: Option<PathBuf>,
}

fn path(vault: &Path) -> PathBuf {
    storage::sibling(vault, ".prefs.json")
}

impl Preferences {
    /// A missing or unreadable file only means the defaults apply.
    pub fn load(vault: &Path) -> Self {
        storage::read(&path(vault), 64 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, vault: &Path) -> io::Result<SaveOutcome> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        storage::write_atomic(&path(vault), &bytes, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_roundtrips_and_damaged_files_fall_back_to_defaults() {
        let directory = std::env::temp_dir().join(format!(
            "sekeeper-preferences-test-{:032x}",
            rand::random::<u128>()
        ));
        let vault = directory.join("vault");
        std::fs::create_dir(&directory).unwrap();
        assert_eq!(Preferences::load(&vault), Preferences::default());
        let preferences = Preferences {
            keyfile: Some("/media/usb/sekeeper.key".into()),
        };
        assert!(matches!(
            preferences.save(&vault).unwrap(),
            SaveOutcome::Durable
        ));
        assert_eq!(Preferences::load(&vault), preferences);
        assert!(matches!(
            storage::write_atomic(&path(&vault), b"not json", false).unwrap(),
            SaveOutcome::Durable
        ));
        assert_eq!(Preferences::load(&vault), Preferences::default());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
