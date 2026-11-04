//! Project files on disk.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§3.3` and `§3.4`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::export::sanitise;
use crate::model::Project;

/// A storage failure. Carries a path and a reason, never message content.
#[derive(Debug)]
pub enum StoreError {
    /// The name has no valid characters (`§3.3`).
    EmptyName,
    /// A filesystem error.
    Io(io::Error),
    /// The file is not a project; the string is serde's structural reason.
    Parse(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => {
                f.write_str("Project name must contain at least one valid character")
            }
            Self::Io(e) => write!(f, "{e}"),
            Self::Parse(e) => write!(f, "not a valid project file: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The default projects directory, `~/.ultra7/projects` (`§3.1`).
///
/// Reads `HOME`, then `USERPROFILE` (Windows), then falls back to the
/// current directory rather than failing, so the tool still runs in a
/// stripped-down environment.
#[must_use]
pub fn default_projects_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".ultra7").join("projects")
}

/// `<sanitised name>.json`, or an error when nothing valid is left (`§3.3`).
///
/// # Errors
/// [`StoreError::EmptyName`].
pub fn project_filename(name: &str) -> Result<String, StoreError> {
    let safe = sanitise(name);
    if safe.is_empty() {
        return Err(StoreError::EmptyName);
    }
    Ok(format!("{safe}.json"))
}

/// Reads and writes project files under one directory.
///
/// Holds no open handles and no cache, so two stores (or two processes)
/// over one directory see each other's writes immediately.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    /// The projects directory.
    dir: PathBuf,
}

impl ProjectStore {
    /// A store over `dir`; nothing is created until first use.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file for a project name (`§3.3`).
    fn path(&self, name: &str) -> Result<PathBuf, StoreError> {
        Ok(self.dir.join(project_filename(name)?))
    }

    /// Project names on disk, sorted (`§3.4`).
    ///
    /// # Errors
    /// A filesystem error.
    pub fn list_projects(&self) -> Result<Vec<String>, StoreError> {
        fs::create_dir_all(&self.dir)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Load a project by name.
    ///
    /// # Errors
    /// A missing file, or one that is not a project.
    pub fn load(&self, name: &str) -> Result<Project, StoreError> {
        let text = fs::read_to_string(self.path(name)?)?;
        serde_json::from_str(&text).map_err(|e| StoreError::Parse(e.to_string()))
    }

    /// Save atomically: temp file, then rename (`§3.4`).
    ///
    /// The rename means a crash mid-write leaves the previous file intact
    /// rather than a truncated project. The temp file sits in the same
    /// directory because a rename across filesystems is not atomic.
    ///
    /// # Errors
    /// A filesystem or serialisation error.
    pub fn save(&self, project: &Project) -> Result<(), StoreError> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(&project.name)?;
        let tmp = path.with_extension("json.tmp");
        let text =
            serde_json::to_string_pretty(project).map_err(|e| StoreError::Parse(e.to_string()))?;
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Delete a project; a missing one is not an error (`§3.4`).
    ///
    /// # Errors
    /// A filesystem error other than not-found.
    pub fn delete(&self, name: &str) -> Result<(), StoreError> {
        match fs::remove_file(self.path(name)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Whether a project file exists.
    ///
    /// # Errors
    /// [`StoreError::EmptyName`].
    pub fn exists(&self, name: &str) -> Result<bool, StoreError> {
        Ok(self.path(name)?.exists())
    }
}
