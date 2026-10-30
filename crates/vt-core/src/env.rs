//! Process environment, resolved once and passed everywhere as `VtEnv`.
//!
//! Nothing else reads `HOME` or Claude paths directly, so tests can inject
//! a synthetic home.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtEnv {
    pub home: PathBuf,
    pub user: String,
    /// `~/.claude`, or `CLAUDE_CONFIG_DIR` when set.
    pub claude_dir: PathBuf,
    /// `~/.claude.json`, or `<CLAUDE_CONFIG_DIR>/.claude.json` when set.
    pub claude_json: PathBuf,
    /// `~/.agents`
    pub agents_dir: PathBuf,
    /// `~/.vibe-transfer`
    pub vt_home: PathBuf,
    pub claude_config_dir_override: Option<PathBuf>,
}

impl VtEnv {
    /// Builds the environment from explicit values (used by tests).
    pub fn new(home: PathBuf, user: String, claude_config_dir: Option<PathBuf>) -> Self {
        let (claude_dir, claude_json) = match &claude_config_dir {
            Some(dir) => (dir.clone(), dir.join(".claude.json")),
            None => (home.join(".claude"), home.join(".claude.json")),
        };
        Self {
            agents_dir: home.join(".agents"),
            vt_home: home.join(".vibe-transfer"),
            claude_dir,
            claude_json,
            claude_config_dir_override: claude_config_dir,
            user,
            home,
        }
    }

    /// Reads `HOME`, `USER` (falling back to `LOGNAME`) and `CLAUDE_CONFIG_DIR`.
    pub fn from_process() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .context("HOME ortam değişkeni tanımlı değil")?;
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .ok()
            .filter(|u| !u.is_empty());
        let Some(user) = user else {
            bail!("USER ortam değişkeni tanımlı değil");
        };
        let claude_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        Ok(Self::new(PathBuf::from(home), user, claude_config_dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout() {
        let env = VtEnv::new("/home/alice".into(), "alice".into(), None);
        assert_eq!(env.claude_dir, PathBuf::from("/home/alice/.claude"));
        assert_eq!(env.claude_json, PathBuf::from("/home/alice/.claude.json"));
        assert_eq!(env.agents_dir, PathBuf::from("/home/alice/.agents"));
        assert_eq!(env.vt_home, PathBuf::from("/home/alice/.vibe-transfer"));
        assert_eq!(env.claude_config_dir_override, None);
    }

    #[test]
    fn claude_config_dir_override() {
        let env = VtEnv::new(
            "/home/alice".into(),
            "alice".into(),
            Some("/srv/claude".into()),
        );
        assert_eq!(env.claude_dir, PathBuf::from("/srv/claude"));
        assert_eq!(env.claude_json, PathBuf::from("/srv/claude/.claude.json"));
        assert_eq!(env.vt_home, PathBuf::from("/home/alice/.vibe-transfer"));
    }
}
