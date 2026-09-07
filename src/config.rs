use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{RemnantError, Result};

pub const CONFIG_FILE_NAME: &str = "remnant.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub version: u32,
    pub project: ProjectSettings,
    #[serde(default)]
    pub sources: BTreeMap<String, SourceConfig>,
    pub oracle: OracleConfig,
    #[serde(default)]
    pub reduction: ReductionConfig,
    #[serde(default)]
    pub safety: SafetyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSettings {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfig {
    Postgres {
        url_env: String,
        #[serde(default)]
        schema: Option<String>,
    },
    Redis {
        url_env: String,
        #[serde(default)]
        database: u8,
    },
}

impl SourceConfig {
    pub fn url_env(&self) -> &str {
        match self {
            Self::Postgres { url_env, .. } | Self::Redis { url_env, .. } => url_env,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Postgres { .. } => "postgres",
            Self::Redis { .. } => "redis",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OracleConfig {
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default = "default_failure_exit_code")]
    pub failure_exit_code: i32,
}

impl OracleConfig {
    pub fn timeout_duration(&self) -> Result<Duration> {
        parse_duration(&self.timeout)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReductionConfig {
    #[serde(default = "default_strategy")]
    pub strategy: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default = "default_max_experiments")]
    pub max_experiments: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafetyConfig {
    #[serde(default)]
    pub allow_non_local: bool,
    #[serde(default = "default_require_fingerprint")]
    pub require_fingerprint: bool,
}

impl Default for ReductionConfig {
    fn default() -> Self {
        Self {
            strategy: default_strategy(),
            state_dir: default_state_dir(),
            max_experiments: default_max_experiments(),
        }
    }
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            allow_non_local: false,
            require_fingerprint: true,
        }
    }
}

impl ProjectConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(RemnantError::MissingConfig {
                path: path.to_path_buf(),
            });
        }
        let contents = fs::read_to_string(path)?;
        let config: Self = serde_yaml::from_str(&contents)
            .map_err(|error| RemnantError::ConfigParse(format!("{}: {error}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let yaml = serde_yaml::to_string(self)
            .map_err(|error| RemnantError::ConfigParse(error.to_string()))?;
        fs::write(path, yaml)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(RemnantError::InvalidConfig(format!(
                "version {} is not supported; expected version 1",
                self.version
            )));
        }
        if self.project.name.trim().is_empty() {
            return Err(RemnantError::InvalidConfig(
                "project.name must not be empty".to_string(),
            ));
        }
        if self.sources.is_empty() {
            return Err(RemnantError::InvalidConfig(
                "at least one state source is required".to_string(),
            ));
        }
        for (name, source) in &self.sources {
            if name.trim().is_empty() {
                return Err(RemnantError::InvalidConfig(
                    "source names must not be empty".to_string(),
                ));
            }
            validate_env_name(source.url_env())
                .map_err(|error| RemnantError::InvalidConfig(format!("source {name}: {error}")))?;
        }
        if self.oracle.command.trim().is_empty() {
            return Err(RemnantError::InvalidConfig(
                "oracle.command must not be empty".to_string(),
            ));
        }
        self.oracle
            .timeout_duration()
            .map_err(|error| RemnantError::InvalidConfig(format!("oracle.timeout: {error}")))?;
        if self.reduction.strategy != "hierarchical" && self.reduction.strategy != "ddmin" {
            return Err(RemnantError::InvalidConfig(format!(
                "reduction.strategy must be hierarchical or ddmin, got {}",
                self.reduction.strategy
            )));
        }
        if self.reduction.max_experiments == 0 {
            return Err(RemnantError::InvalidConfig(
                "reduction.max_experiments must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }

    pub fn resolve_state_dir(&self, config_path: impl AsRef<Path>) -> PathBuf {
        let config_dir = config_path
            .as_ref()
            .parent()
            .unwrap_or_else(|| Path::new("."));
        if self.reduction.state_dir.is_absolute() {
            self.reduction.state_dir.clone()
        } else {
            config_dir.join(&self.reduction.state_dir)
        }
    }

    pub fn missing_environment_variables(&self) -> Vec<String> {
        self.sources
            .values()
            .map(SourceConfig::url_env)
            .filter(|name| env::var(name).is_err())
            .map(ToOwned::to_owned)
            .collect()
    }
}

impl Default for ProjectConfig {
    fn default() -> Self {
        let mut sources = BTreeMap::new();
        sources.insert(
            "postgres".to_string(),
            SourceConfig::Postgres {
                url_env: "DATABASE_URL".to_string(),
                schema: None,
            },
        );
        sources.insert(
            "redis".to_string(),
            SourceConfig::Redis {
                url_env: "REDIS_URL".to_string(),
                database: 0,
            },
        );
        Self {
            version: 1,
            project: ProjectSettings {
                name: "my-remnant-project".to_string(),
            },
            sources,
            oracle: OracleConfig {
                command: "./scripts/reproduce.sh".to_string(),
                timeout: default_timeout(),
                failure_exit_code: default_failure_exit_code(),
            },
            reduction: ReductionConfig::default(),
            safety: SafetyConfig::default(),
        }
    }
}

fn validate_env_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("url_env must not be empty".to_string());
    }
    if !name
        .chars()
        .all(|character| character == '_' || character.is_ascii_alphanumeric())
        || name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    {
        return Err(format!(
            "url_env {name:?} is not a valid environment variable name"
        ));
    }
    Ok(())
}

fn parse_duration(value: &str) -> Result<Duration> {
    let value = value.trim();
    let units = [
        ("ms", 1_000_000u64),
        ("s", 1_000_000_000u64),
        ("m", 60_000_000_000u64),
    ];
    for (suffix, multiplier) in units {
        if let Some(number) = value.strip_suffix(suffix) {
            let number: u64 = number
                .trim()
                .parse()
                .map_err(|_| RemnantError::InvalidConfig(format!("invalid duration {value:?}")))?;
            return Ok(Duration::from_nanos(number.saturating_mul(multiplier)));
        }
    }
    Err(RemnantError::InvalidConfig(format!(
        "invalid duration {value:?}; use values such as 30s or 500ms"
    )))
}

fn default_timeout() -> String {
    "30s".to_string()
}

fn default_failure_exit_code() -> i32 {
    1
}

fn default_strategy() -> String {
    "hierarchical".to_string()
}

fn default_state_dir() -> PathBuf {
    PathBuf::from(".remnant")
}

fn default_max_experiments() -> u64 {
    10_000
}

fn default_require_fingerprint() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        ProjectConfig::default()
            .validate()
            .expect("default config validates");
    }

    #[test]
    fn invalid_environment_name_is_rejected() {
        let mut config = ProjectConfig::default();
        config.sources.insert(
            "bad".to_string(),
            SourceConfig::Redis {
                url_env: "not-valid".to_string(),
                database: 0,
            },
        );
        let error = config.validate().expect_err("invalid env name must fail");
        assert!(error.to_string().contains("not-valid"));
    }

    #[test]
    fn round_trip_preserves_configuration() {
        let config = ProjectConfig::default();
        let yaml = serde_yaml::to_string(&config).expect("serialize");
        let parsed: ProjectConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(config, parsed);
    }

    #[test]
    fn durations_are_parsed_without_float_rounding() {
        let oracle = OracleConfig {
            command: "true".to_string(),
            timeout: "500ms".to_string(),
            failure_exit_code: 1,
        };
        assert_eq!(
            oracle.timeout_duration().expect("duration"),
            Duration::from_millis(500)
        );
    }
}
