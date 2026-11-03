//! Terminal settings; fall back to the existing desktop companion configuration.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Used,
    Remaining,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub display: Mode,
    pub refresh_secs: u64,
    pub antigravity_jio: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            display: Mode::Used,
            refresh_secs: 120,
            antigravity_jio: false,
        }
    }
}
impl Config {
    pub fn load() -> Self {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        Self::from_directory(&base)
    }
    fn from_directory(base: &Path) -> Self {
        let own = base.join("ai-top/config.toml");
        let path = if own.exists() {
            own
        } else {
            base.join("ai-quota-bar/config.toml")
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_configuration_defaults_entitlements_off() {
        let c: Config = toml::from_str("display = 'remaining'").unwrap();
        assert_eq!(c.display, Mode::Remaining);
        assert!(!c.antigravity_jio);
    }
    #[test]
    fn standalone_settings_override_the_desktop_fallback() {
        let root = std::env::temp_dir().join(format!(
            "ai-top-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("ai-quota-bar")).unwrap();
        std::fs::write(
            root.join("ai-quota-bar/config.toml"),
            "display='remaining'\nantigravity_jio=true",
        )
        .unwrap();
        let c = Config::from_directory(&root);
        assert_eq!(c.display, Mode::Remaining);
        assert!(c.antigravity_jio);
        std::fs::create_dir_all(root.join("ai-top")).unwrap();
        std::fs::write(
            root.join("ai-top/config.toml"),
            "display='used'\nrefresh_secs=60",
        )
        .unwrap();
        let c = Config::from_directory(&root);
        assert_eq!(c.display, Mode::Used);
        assert_eq!(c.refresh_secs, 60);
        assert!(!c.antigravity_jio);
        std::fs::remove_dir_all(root).unwrap();
    }
}
