use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::{RemnantError, Result};
use crate::reducer::{ReductionSession, SESSION_FORMAT_VERSION, SessionSummary};

/// Session files can contain a full production-like state snapshot. Keep the
/// accepted size bounded before loading and write them with private permissions.
const MAX_SESSION_BYTES: u64 = 64 * 1024 * 1024;

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
        validate_session(session)?;
        self.initialize()?;
        let path = self.session_path(&session.id)?;
        let bytes = serde_json::to_vec_pretty(session)
            .map_err(|error| RemnantError::Persistence(format!("serialize session: {error}")))?;
        if bytes.len() as u64 > MAX_SESSION_BYTES {
            return Err(RemnantError::Persistence(format!(
                "refusing to write {}: session is {} bytes, limit is {MAX_SESSION_BYTES}",
                path.display(),
                bytes.len()
            )));
        }

        let temp_path = self
            .root
            .join(format!(".{}.{}.tmp", session.id, Uuid::new_v4()));
        let write_result = write_and_sync(&temp_path, &bytes);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        fs::rename(&temp_path, &path).map_err(|error| {
            RemnantError::Persistence(format!("replace {}: {error}", path.display()))
        })?;
        sync_directory(&self.root)?;
        Ok(())
    }

    pub fn load(&self, session_id: &str) -> Result<ReductionSession> {
        let path = self.session_path(session_id)?;
        let metadata = fs::metadata(&path).map_err(|error| {
            RemnantError::Persistence(format!("read {}: {error}", path.display()))
        })?;
        if metadata.len() > MAX_SESSION_BYTES {
            return Err(RemnantError::Persistence(format!(
                "refusing to load {}: file is {} bytes, limit is {MAX_SESSION_BYTES}",
                path.display(),
                metadata.len()
            )));
        }
        let contents = fs::read_to_string(&path).map_err(|error| {
            RemnantError::Persistence(format!("read {}: {error}", path.display()))
        })?;
        let session = serde_json::from_str(&contents).map_err(|error| {
            RemnantError::Persistence(format!("parse {}: {error}", path.display()))
        })?;
        validate_session(&session)?;
        Ok(session)
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
                && valid_session_id(session_id)
            {
                sessions.push(session_id.to_string());
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    fn session_path(&self, session_id: &str) -> Result<PathBuf> {
        if !valid_session_id(session_id) {
            return Err(RemnantError::Persistence(format!(
                "invalid session id {session_id:?}"
            )));
        }
        Ok(self.root.join(format!("{session_id}.json")))
    }
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| RemnantError::Persistence(format!("write {}: {error}", path.display())))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| RemnantError::Persistence(format!("write {}: {error}", path.display())))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| RemnantError::Persistence(format!("sync {}: {error}", path.display())))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn validate_session(session: &ReductionSession) -> Result<()> {
    if session.format_version != SESSION_FORMAT_VERSION {
        return Err(RemnantError::Persistence(format!(
            "session format version {} is not supported; expected {SESSION_FORMAT_VERSION}",
            session.format_version
        )));
    }
    if !valid_session_id(&session.id) {
        return Err(RemnantError::Persistence(format!(
            "invalid session id {:?}",
            session.id
        )));
    }
    Ok(())
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::Utc;
    use tempfile::tempdir;

    use super::*;
    use crate::model::{SNAPSHOT_FORMAT_VERSION, Snapshot};
    use crate::oracle::{OracleOutcome, OracleResult};
    use crate::reducer::SessionStatus;

    fn session(id: &str) -> ReductionSession {
        ReductionSession {
            format_version: SESSION_FORMAT_VERSION,
            id: id.to_string(),
            project: "test".to_string(),
            status: SessionStatus::Created,
            strategy: "ddmin".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            baseline: Snapshot {
                format_version: SNAPSHOT_FORMAT_VERSION,
                id: "snapshot-test".to_string(),
                captured_at: Utc::now(),
                sources: BTreeMap::new(),
                fingerprint: "test".to_string(),
            },
            objects: Vec::new(),
            retained_ids: BTreeSet::new(),
            baseline_oracle: OracleResult {
                command: "test".to_string(),
                outcome: OracleOutcome::FailureReproduced,
                exit_code: Some(1),
                duration_ms: 0,
                started_at: Utc::now(),
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
            experiments: Vec::new(),
            result: None,
            last_error: None,
        }
    }

    #[test]
    fn saves_and_replaces_complete_sessions_without_leaving_temp_files() {
        let directory = tempdir().expect("tempdir");
        let store = SessionStore::new(directory.path());
        let mut expected = session("session-good");
        store.save(&expected).expect("save");
        expected.status = SessionStatus::Running;
        store.save(&expected).expect("replace");

        assert_eq!(store.load("session-good").expect("load"), expected);
        assert_eq!(store.list().expect("list"), vec!["session-good"]);
    }

    #[test]
    fn rejects_corrupt_or_path_traversal_session_files() {
        let directory = tempdir().expect("tempdir");
        let store = SessionStore::new(directory.path());
        store.initialize().expect("initialize");
        fs::write(directory.path().join("session-bad.json"), b"{not json")
            .expect("write corrupt session");

        assert!(store.load("session-bad").is_err());
        assert!(store.load("../outside").is_err());
    }

    #[test]
    fn rejects_unsupported_session_format() {
        let directory = tempdir().expect("tempdir");
        let store = SessionStore::new(directory.path());
        let mut unsupported = session("session-version");
        unsupported.format_version = SESSION_FORMAT_VERSION + 1;

        assert!(store.save(&unsupported).is_err());
    }
}
