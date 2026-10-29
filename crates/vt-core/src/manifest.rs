//! Manifest data types: what a bundle contains and where it came from.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::secret_types::SecretMeta;

/// Current manifest format version.
pub const FORMAT_VERSION: u32 = 1;

/// AI coding tool whose data is transferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Claude,
    Codex,
}

/// Data category (spec 12.2). `Cache` is off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Sessions,
    Memory,
    Settings,
    Secrets,
    ProjectFiles,
    Cache,
}

/// Project identity: a normalized git remote or `path:<old absolute path>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectId(pub String);

/// The root directory or file an entry's `rel` path is relative to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    ClaudeDir,
    ClaudeJson,
    AgentsDir,
    CodexDir,
    Project(ProjectId),
}

impl RootKind {
    /// Stable textual tag used for ordering and Merkle leaves.
    pub fn tag(&self) -> String {
        match self {
            RootKind::ClaudeDir => "claude_dir".into(),
            RootKind::ClaudeJson => "claude_json".into(),
            RootKind::AgentsDir => "agents_dir".into(),
            RootKind::CodexDir => "codex_dir".into(),
            RootKind::Project(id) => format!("project:{}", id.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink { target: String },
}

/// One file system object in a bundle. `rel` is `/`-separated and NFC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub index: u32,
    pub tool: Tool,
    pub category: Category,
    pub root: RootKind,
    pub rel: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// POSIX mode bits; 0 on non-unix hosts.
    pub mode: u32,
    /// BLAKE3 of the content; `None` for dirs and symlinks.
    #[serde(with = "hex_hash")]
    pub hash: Option<[u8; 32]>,
    pub partial_tail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub os: String,
    pub user: String,
    pub home: String,
    pub hostname: String,
    pub tool_versions: BTreeMap<String, String>,
}

/// Transfer class of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Transfer,
    Optional,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    pub id: ProjectId,
    pub old_path: String,
    pub remote: Option<String>,
    pub is_git: bool,
}

/// What the user chose to transfer. Data only; constructors live elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub tools: BTreeSet<Tool>,
    pub categories: BTreeMap<Tool, BTreeSet<Category>>,
}

/// A JSON key whose source and target values differ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonConflict {
    pub path: String,
    pub source: serde_json::Value,
    pub target: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub session_id: String,
    pub created_at: String,
    pub source: SourceInfo,
    pub selection: Selection,
    pub entries: Vec<Entry>,
    pub projects: Vec<ProjectRef>,
    pub secrets: Vec<SecretMeta>,
    pub notes: Vec<String>,
}

/// Serializes `Option<[u8; 32]>` as an optional lowercase hex string.
mod hex_hash {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<[u8; 32]>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(h) => s.serialize_some(&blake3::Hash::from_bytes(*h).to_hex().as_str()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 32]>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| {
                blake3::Hash::from_hex(&s)
                    .map(|h| *h.as_bytes())
                    .map_err(D::Error::custom)
            })
            .transpose()
    }
}
