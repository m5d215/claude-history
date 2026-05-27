use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub config_dirs: Option<Vec<String>>,
}

pub fn load() -> Result<Option<Config>> {
    let Some(path) = config_file_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    let parsed: Config = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
    Ok(Some(parsed))
}

pub fn config_file_path() -> Option<PathBuf> {
    let base = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
    Some(base.join("claude-history").join("config.toml"))
}
