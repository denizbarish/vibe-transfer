//! Command implementations and what they share: the environment and the
//! secret store, and turning flags into a [`Selection`].

pub mod export;
pub mod import;
pub mod scan;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use vt_core::VtEnv;
use vt_core::manifest::{Category, Selection, Tool};
use vt_core::secret_types::SecretBytes;
use vt_secrets::{KeyringStore, MemStore, SecretStore};

use crate::ui::{self, Answers};

/// Test hook: points the whole environment at this directory and swaps the
/// OS secret store for an in-memory one.
const TEST_HOME_VAR: &str = "VT_TEST_HOME";
/// Test hook: JSON file `[{"service", "account", "value"}]` that fills the
/// in-memory secret store. Only valid together with [`TEST_HOME_VAR`].
const TEST_SECRETS_VAR: &str = "VT_TEST_SECRETS";
/// Test hook: JSON answers to interactive questions (see [`Answers`]).
/// Only valid together with [`TEST_HOME_VAR`].
const TEST_ANSWERS_VAR: &str = "VT_TEST_ANSWERS";
/// Test hook: file that receives `[{"service", "account", "blake3"}]` of the
/// in-memory store when the command ends: digests only, never values.
const TEST_SECRETS_DUMP_VAR: &str = "VT_TEST_SECRETS_DUMP";

/// What every command runs against.
pub struct Ctx {
    pub env: VtEnv,
    pub store: Box<dyn SecretStore>,
    /// Set by `VT_TEST_HOME`: synthetic data, never the real home or Keychain.
    pub test_mode: bool,
    /// Test answers (`VT_TEST_ANSWERS`); `None` outside test mode.
    pub answers: Option<Answers>,
    /// The in-memory store in test mode, for the digest dump.
    test_store: Option<Arc<MemStore>>,
}

/// Shares the test `MemStore` between `Ctx::store` and the dump.
struct SharedMem(Arc<MemStore>);

impl SecretStore for SharedMem {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretBytes>> {
        self.0.get(service, account)
    }
    fn set(&self, service: &str, account: &str, value: &SecretBytes) -> Result<()> {
        self.0.set(service, account, value)
    }
    fn delete(&self, service: &str, account: &str) -> Result<()> {
        self.0.delete(service, account)
    }
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
        let test_answers = std::env::var(TEST_ANSWERS_VAR)
            .ok()
            .filter(|v| !v.is_empty());
        let Some(home) = test_home else {
            for (name, set) in [
                (TEST_SECRETS_VAR, test_secrets.is_some()),
                (TEST_ANSWERS_VAR, test_answers.is_some()),
                (
                    TEST_SECRETS_DUMP_VAR,
                    std::env::var_os(TEST_SECRETS_DUMP_VAR).is_some(),
                ),
            ] {
                if set {
                    bail!("{name} yalnız {TEST_HOME_VAR} ile birlikte kullanılabilir");
                }
            }
            return Ok(Self {
                env: VtEnv::from_process()?,
                store: Box::new(KeyringStore::new()),
                test_mode: false,
                answers: None,
                test_store: None,
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
        let store = Arc::new(MemStore::new());
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
        let answers = test_answers.as_deref().map(Answers::parse).transpose()?;
        Ok(Self {
            env: VtEnv::new(home, user, None),
            store: Box::new(SharedMem(Arc::clone(&store))),
            test_mode: true,
            answers,
            test_store: Some(store),
        })
    }

    /// PIDs of a running Claude. In test mode the real process list is
    /// ignored (it belongs to the real machine) and the test answers decide.
    pub fn claude_pids(&self) -> Vec<u32> {
        if self.test_mode {
            return self
                .answers
                .as_ref()
                .map(|a| a.claude_pids.clone())
                .unwrap_or_default();
        }
        vt_core::procs::running(Tool::Claude)
    }

    /// Test mode: writes the digests of the in-memory store to
    /// `VT_TEST_SECRETS_DUMP`, if set.
    pub fn dump_test_secrets(&self) -> Result<()> {
        let (Some(store), Some(path)) = (
            &self.test_store,
            std::env::var_os(TEST_SECRETS_DUMP_VAR).filter(|v| !v.is_empty()),
        ) else {
            return Ok(());
        };
        let mut items = Vec::new();
        for (service, account) in store.keys()? {
            if let Some(value) = store.get(&service, &account)? {
                items.push(serde_json::json!({
                    "service": service,
                    "account": account,
                    "blake3": blake3_hex(value.expose()),
                }));
            }
        }
        std::fs::write(&path, serde_json::to_vec(&items)?)
            .with_context(|| format!("{TEST_SECRETS_DUMP_VAR} yazılamadı"))
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

fn blake3_hex(bytes: &[u8]) -> String {
    vt_core::report::hash_bytes(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
