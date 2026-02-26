use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

impl Config {
    pub fn default_profile(&self) -> Option<(&str, &Profile)> {
        self.profiles
            .iter()
            .find(|(_, p)| p.default)
            .map(|(name, profile)| (name.as_str(), profile))
    }

    pub fn get_profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn set_default(&mut self, name: &str) -> bool {
        if !self.profiles.contains_key(name) {
            return false;
        }
        for (_, profile) in self.profiles.iter_mut() {
            profile.default = false;
        }
        if let Some(profile) = self.profiles.get_mut(name) {
            profile.default = true;
        }
        true
    }

    pub fn add_profile(&mut self, name: String, profile: Profile) {
        // If this is the first profile or marked default, ensure only one default
        if profile.default || self.profiles.is_empty() {
            for (_, p) in self.profiles.iter_mut() {
                p.default = false;
            }
            let mut profile = profile;
            profile.default = true;
            self.profiles.insert(name, profile);
        } else {
            self.profiles.insert(name, profile);
        }
    }

    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }

    pub fn remove_profile(&mut self, name: &str) -> bool {
        let was_default = self.profiles.get(name).map(|p| p.default).unwrap_or(false);
        let removed = self.profiles.remove(name).is_some();

        // If we removed the default, promote the first remaining profile
        if removed
            && was_default
            && let Some((_, profile)) = self.profiles.iter_mut().next()
        {
            profile.default = true;
        }
        removed
    }
}

pub fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".switchboard"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("profiles.toml"))
}

pub fn cache_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("cache");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create cache dir: {}", dir.display()))?;
    Ok(dir)
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config: Config =
        toml::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(config)
}

pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config).context("Failed to serialize config")?;
    std::fs::write(&path, contents)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}
