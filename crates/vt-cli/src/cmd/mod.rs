//! Command implementations and what they share: the environment and the
//! secret store, and turning flags into a [`Selection`].

pub mod export;
pub mod scan;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use vt_core::VtEnv;
use vt_core::manifest::{Category, Selection, Tool};
use vt_core::secret_types::SecretBytes;
use vt_secrets::{KeyringStore, MemStore, SecretStore};

use crate::ui;

/// Test hook: points the whole environment at this directory and swaps the
/// OS secret store for an in-memory one.
const TEST_HOME_VAR: &str = "VT_TEST_HOME";
/// Test hook: JSON file `[{"service", "account", "value"}]` that fills the
/// in-memory secret store. Only valid together with [`TEST_HOME_VAR`].
const TEST_SECRETS_VAR: &str = "VT_TEST_SECRETS";

/// What every command runs against.
pub struct Ctx {
    pub env: VtEnv,
    pub store: Box<dyn SecretStore>,
    /// Set by `VT_TEST_HOME`: synthetic data, never the real home or Keychain.
    pub test_mode: bool,
}

#[derive(Deserialize)]
struct TestSecret {
    service: String,
    account: String,
    value: String,
}

impl Ctx {
    pub fn from_process() -> Result<Self> {
        let test_home = std::env::var_os(TEST_HOME_VAR).filter(|v| !v.is_empty());
        let test_secrets = std::env::var_os(TEST_SECRETS_VAR).filter(|v| !v.is_empty());
        let Some(home) = test_home else {
            if test_secrets.is_some() {
                bail!("{TEST_SECRETS_VAR} yalnız {TEST_HOME_VAR} ile birlikte kullanılabilir");
            }
            return Ok(Self {
                env: VtEnv::from_process()?,
                store: Box::new(KeyringStore::new()),
                test_mode: false,
            });
        };

        let home = PathBuf::from(home);
        if !home.is_absolute() {
            bail!("{TEST_HOME_VAR} mutlak bir yol olmalı");
        }
        let user = std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "test".to_owned());
        let store = MemStore::new();
        if let Some(path) = test_secrets {
            let path = PathBuf::from(path);
            let text = std::fs::read(&path)
                .with_context(|| format!("{TEST_SECRETS_VAR} okunamadı: {}", path.display()))?;
            let items: Vec<TestSecret> = serde_json::from_slice(&text)
                .with_context(|| format!("{TEST_SECRETS_VAR} geçerli JSON değil"))?;
            for item in items {
                store.set(
                    &item.service,
                    &item.account,
                    &SecretBytes::new(item.value.into_bytes()),
                )?;
            }
        }
        Ok(Self {
            env: VtEnv::new(home, user, None),
            store: Box::new(store),
            test_mode: true,
        })
    }

    /// One-line warning for every command run in test mode.
    pub fn test_mode_notice(&self) -> Option<String> {
        self.test_mode.then(|| {
            format!(
                "uyarı: test modu ({TEST_HOME_VAR}={}): gerçek ev dizini ve Keychain kullanılmıyor",
                self.env.home.display()
            )
        })
    }
}

/// Phase 1 handles Claude only.
pub fn ensure_supported(tool: Tool) -> Result<()> {
    match tool {
        Tool::Claude => Ok(()),
        Tool::Codex => bail!("Codex desteği henüz yok; şimdilik yalnız claude"),
    }
}

/// Selection flags of `export`.
pub struct SelectionFlags<'a> {
    pub tool: Option<Tool>,
    pub only: &'a [Category],
    pub exclude: &'a [Category],
    pub include_optional: bool,
}

/// The selection from the flags; with no selection flag on a terminal the
/// user picks interactively, otherwise every default category of Claude.
pub fn resolve_selection(flags: &SelectionFlags) -> Result<Selection> {
    let any_flag = flags.tool.is_some()
        || !flags.only.is_empty()
        || !flags.exclude.is_empty()
        || flags.include_optional;
    if !any_flag && ui::interactive() {
        return ui::prompt_selection();
    }
    let tool = flags.tool.unwrap_or(Tool::Claude);
    ensure_supported(tool)?;
    let mut only = flags.only.to_vec();
    if flags.include_optional {
        if only.is_empty() {
            only = Category::ALL.to_vec();
        } else if !only.contains(&Category::Cache) {
            only.push(Category::Cache);
        }
    }
    let sel = Selection::from_flags(&[tool], &only, flags.exclude);
    if sel.categories.get(&tool).is_none_or(|c| c.is_empty()) {
        bail!("hiç kategori seçilmedi");
    }
    Ok(sel)
}
