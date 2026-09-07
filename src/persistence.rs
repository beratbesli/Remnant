use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{RemnantError, Result};
use crate::reducer::{ReductionSession, SessionSummary};

#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn initialize(&self) -> Result<()> {
        fs::create_dir_all(&self.root).map_err(|error| {
            RemnantError::Persistence(format!("create {}: {error}", self.root.display()))
        })
    }

    pub fn save(&self, session: &ReductionSession) -> Result<()> {
        self.initialize()?;
        let path = self.session_path(&session.id);
        let temp_path = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(session)
            .map_err(|error| RemnantError::Persistence(format!("serialize session: {error}")))?;
        fs::write(&temp_path, bytes).map_err(|error| {
            RemnantError::Persistence(format!("write {}: {error}", temp_path.display()))
        })?;
        fs::rename(&temp_path, &path).map_err(|error| {
            RemnantError::Persistence(format!("replace {}: {error}", path.display()))
        })
    }

    pub fn load(&self, session_id: &str) -> Result<ReductionSession> {
        let path = self.session_path(session_id);
        let contents = fs::read_to_string(&path).map_err(|error| {
            RemnantError::Persistence(format!("read {}: {error}", path.display()))
        })?;
        serde_json::from_str(&contents).map_err(|error| {
            RemnantError::Persistence(format!("parse {}: {error}", path.display()))
        })
    }

    pub fn summary(&self, session_id: &str) -> Result<SessionSummary> {
        Ok(self.load(session_id)?.summary())
    }

    pub fn list(&self) -> Result<Vec<String>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|error| {
            RemnantError::Persistence(format!("list {}: {error}", self.root.display()))
        })? {
            let entry = entry.map_err(|error| RemnantError::Persistence(error.to_string()))?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
                && let Some(session_id) = entry.path().file_stem().and_then(|name| name.to_str())
            {
                sessions.push(session_id.to_string());
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.json"))
    }
}
