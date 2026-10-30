//! The apply plan of a Claude import: where every bundle entry goes on the
//! target, what is transformed on the way (with before/after hashes), and
//! what waits in the pending store.
//!
//! Every target path is built from a known root (Claude dir, `~/.agents`,
//! a mapped project root, `~/.claude.json`) plus the entry's `rel`, and a
//! `rel` that is absolute, empty where a name is needed, or has `.`/`..`
//! components rejects the whole plan before anything else happens.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use vt_core::VtEnv;
use vt_core::apply::{ApplyPlan, Decision, Placement, ReplaceScope, Resolution};
use vt_core::bundle::ReadBundle;
use vt_core::fsutil::create_private_file;
use vt_core::manifest::{
    Class, Entry, EntryKind, JsonConflict, ProjectId, ProjectRef, RootKind, Tool,
};
use vt_core::pathcodec::claude_slug;
use vt_core::pathmap::{Mapping, rewrite_home};
use vt_core::pending::{DestTemplate, PendingItem};
use vt_core::report::{TransformRecord, hash_bytes, json_bytes};
use vt_core::scan::hash_file;

use crate::claude_json::read_claude_json_stable;
use crate::inventory::classify;
use crate::merge::{merge_claude_json, merge_json_settings, merge_memory_md};
use crate::projects::{first_cwds, is_project_entry_shape};
use crate::transform::{memory_slug_for, rewrite_json_paths, rewrite_path_value, target_slug_dir};

/// Text files larger than this are placed unchanged.
const TEXT_REWRITE_MAX: u64 = 1024 * 1024;
/// Top-level `~/.claude` folders whose text files get the home rewrite.
const TEXT_DIRS: &[&str] = &["hooks", "scripts", "bin"];
/// `~/.claude/plugins` registries holding absolute paths.
const PLUGIN_REGISTRIES: &[&str] = &[
    "plugins/installed_plugins.json",
    "plugins/known_marketplaces.json",
];

pub const KIND_JSON_PATHS: &str = "json_paths";
pub const KIND_HOME_PATHS: &str = "home_paths";
pub const KIND_MEMORY_MERGE: &str = "memory_merge";
pub const KIND_JSON_MERGE: &str = "json_merge";
pub const KIND_CLAUDE_JSON_MERGE: &str = "claude_json_merge";
pub const KIND_SYMLINK_TARGET: &str = "symlink_target";

const KEPT_TARGET: &str = "hedef sürümü korundu (kullanıcı seçimi)";
const KEPT_TARGET_KEY: &str = "hedef değeri korundu (kullanıcı seçimi)";

/// What the caller already decided for the import.
pub struct PlanInput<'a> {
    /// One entry per manifest project; `Choose` must already be resolved
    /// into `Auto` or `Pending`.
    pub mappings: &'a BTreeMap<ProjectId, Mapping>,
    /// Merge or replace for the Claude data as a whole.
    pub decision: Decision,
    /// Whether `oauthAccount` moves with the login token.
    pub take_oauth: bool,
    pub session_id: &'a str,
    /// Owner-only directory (inside staging) for transformed blobs.
    pub work_dir: &'a Path,
}

/// Questions only the user can answer. Each may fail (no terminal, no
/// answer), which aborts the plan before anything is written.
pub trait PlanQuestions {
    /// Merge or replace for the files of one project; asked once, the first
    /// time one of its files differs on the target.
    fn project_decision(&mut self, project: &ProjectRef, root: &Path) -> Result<Decision>;
    /// A differing target file no rule decides.
    fn file(&mut self, dest: &Path) -> Result<Resolution>;
    /// A JSON key whose source and target values differ.
    fn json(&mut self, c: &JsonConflict) -> Result<Resolution>;
}

/// Session file numbers for the post-apply Claude check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStats {
    /// Distinct target paths of placed top-level session jsonl files.
    pub placed: u32,
    /// Those that did not exist on the target when the plan was built.
    pub new_files: u32,
    /// Oldest mtime among placed session jsonl files.
    pub oldest_mtime_ns: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ClaudePlan {
    pub plan: ApplyPlan,
    /// Data of projects without a target path, per project.
    pub pending: Vec<(ProjectRef, Vec<PendingItem>)>,
    /// The answer for every target file that already differs. Dests not in
    /// here were absent or identical at plan time.
    pub resolutions: BTreeMap<PathBuf, Resolution>,
    pub transformed: Vec<TransformRecord>,
    pub skipped: Vec<(String, String)>,
    pub problems: Vec<String>,
    pub sessions: SessionStats,
}

/// How a file's content is treated on the way to the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    Plain,
    /// JSON with paths (R12): whole paths mapped, embedded ones home-rewritten.
    Json,
    /// Text script: home rewrite of the content.
    Text,
    /// Session jsonl: never rewritten (R1); kept side by side on merge.
    Session,
    /// Auto memory index: merged line by line.
    Memory,
}

/// Who decides merge or replace for a target path.
#[derive(Debug, Clone)]
enum Owner {
    Claude,
    Project(ProjectId, PathBuf),
}

/// Builds the import plan. Also asks `q` everything that needs the user,
/// so `apply` itself never has to.
pub fn build_claude_apply_plan(
    rb: &ReadBundle,
    env: &VtEnv,
    input: &PlanInput,
    q: &mut dyn PlanQuestions,
) -> Result<ClaudePlan> {
    let m = &rb.manifest;
    for e in &m.entries {
        check_rel(e)?;
        if let RootKind::Project(id) = &e.root
            && !is_project_entry_shape(&e.rel, &e.kind)
        {
            bail!(
                "paket, {} projesinde Claude dosyası olmayan bir yol içeriyor: {:?}; içe aktarma reddedildi",
                id.0,
                e.rel
            );
        }
    }
    for (id, mapping) in input.mappings {
        match mapping {
            Mapping::Choose(_) => bail!("proje için hedef seçilmedi: {}", id.0),
            Mapping::Auto(root) => check_root(root)?,
            Mapping::Pending => {}
        }
    }
    let dst_home = env
        .home
        .to_str()
        .context("hedef ev dizini UTF-8 değil")?
        .to_owned();
    let mut b = Builder {
        rb,
        env,
        input,
        src_home: m.source.home.clone(),
        dst_home,
        mapped: BTreeMap::new(),
        projects: m.projects.iter().map(|p| (p.id.clone(), p)).collect(),
        out: ClaudePlan {
            plan: ApplyPlan {
                session_id: input.session_id.to_owned(),
                source_host: m.source.hostname.clone(),
                placements: Vec::new(),
                replace_scopes: Vec::new(),
                json_writes: Vec::new(),
            },
            pending: Vec::new(),
            resolutions: BTreeMap::new(),
            transformed: Vec::new(),
            skipped: Vec::new(),
            problems: Vec::new(),
            sessions: SessionStats::default(),
        },
        pending: BTreeMap::new(),
        project_decisions: BTreeMap::new(),
        claude_scopes: BTreeSet::new(),
        project_scopes: BTreeMap::new(),
        session_dests: BTreeSet::new(),
        blob_seq: 0,
    };
    for p in &m.projects {
        if let Some(Mapping::Auto(root)) = input.mappings.get(&p.id) {
            b.mapped.insert(p.old_path.clone(), root.clone());
        }
    }

    let mut slugs: BTreeMap<&str, Vec<(&Entry, &str)>> = BTreeMap::new();
    let mut claude_json = None;
    for e in &m.entries {
        if e.tool != Tool::Claude {
            b.skip(e, "bu sürümde yalnız Claude verisi aktarılır");
            continue;
        }
        match &e.root {
            RootKind::ClaudeJson => claude_json = Some(e),
            RootKind::ClaudeDir => match split_slug(&e.rel) {
                Some((slug, rest)) => slugs.entry(slug).or_default().push((e, rest)),
                None => {
                    let rule = claude_dir_rule(&e.rel);
                    let base = env.claude_dir.clone();
                    b.entry(&Owner::Claude, &base, e, &e.rel, rule, q)?;
                    if b.input.decision == Decision::Replace {
                        b.claude_scope(e, &base);
                    }
                }
            },
            RootKind::AgentsDir => {
                let base = env.agents_dir.clone();
                b.entry(&Owner::Claude, &base, e, &e.rel, Rule::Plain, q)?;
                if b.input.decision == Decision::Replace
                    && e.kind == EntryKind::Dir
                    && !e.rel.contains('/')
                {
                    b.claude_scopes.insert(base.join(&e.rel));
                }
            }
            RootKind::CodexDir => b.skip(e, "bu sürümde yalnız Claude verisi aktarılır"),
            RootKind::Project(id) => b.project_entry(id, e, q)?,
        }
    }
    for (slug, entries) in slugs {
        b.slug_dir(slug, &entries, q)?;
    }
    if let Some(e) = claude_json {
        b.claude_json(e, q)?;
    }
    b.finish()
}

/// Target path of a pending item once its project root is known (R11).
/// `None` for [`DestTemplate::ClaudeJsonProjectsKey`]: that item is a key in
/// `env.claude_json`, not a file of its own.
pub fn resolve_dest_template(
    t: &DestTemplate,
    project_root: &Path,
    env: &VtEnv,
) -> Result<Option<PathBuf>> {
    check_root(project_root)?;
    match t {
        DestTemplate::ProjectRel(rel) => safe_join(project_root, rel, false).map(Some),
        DestTemplate::ClaudeProjectSlugDir { rel_in_slug } => {
            let dir = env
                .claude_dir
                .join("projects")
                .join(memory_slug_for(project_root));
            safe_join(&dir, rel_in_slug, false).map(Some)
        }
        DestTemplate::ClaudeJsonProjectsKey => Ok(None),
    }
}

struct Builder<'a> {
    rb: &'a ReadBundle,
    env: &'a VtEnv,
    input: &'a PlanInput<'a>,
    src_home: String,
    dst_home: String,
    /// Source project path to target path, for mapped projects.
    mapped: BTreeMap<String, PathBuf>,
    projects: BTreeMap<ProjectId, &'a ProjectRef>,
    out: ClaudePlan,
    pending: BTreeMap<ProjectId, Vec<PendingItem>>,
    project_decisions: BTreeMap<ProjectId, Decision>,
    claude_scopes: BTreeSet<PathBuf>,
    /// Replace scope per project: its `.claude` folder.
    project_scopes: BTreeMap<ProjectId, PathBuf>,
    session_dests: BTreeSet<PathBuf>,
    blob_seq: u32,
}

impl Builder<'_> {
    fn skip(&mut self, e: &Entry, reason: &str) {
        self.out
            .skipped
            .push((crate::item_key(&e.root, &e.rel), reason.to_owned()));
    }

    fn blob(&self, e: &Entry) -> PathBuf {
        self.rb.blobs.join(e.index.to_string())
    }

    fn mapping(&self, id: &ProjectId) -> Option<&PathBuf> {
        match self.input.mappings.get(id) {
            Some(Mapping::Auto(root)) => Some(root),
            _ => None,
        }
    }

    /// The project whose old path is `path` or contains it (longest wins).
    fn owner_of(&self, path: &str) -> Option<&ProjectRef> {
        self.projects
            .values()
            .filter(|p| is_below(path, &p.old_path))
            .max_by_key(|p| p.old_path.len())
            .copied()
    }

    /// Writes transformed content to a new private staging blob.
    fn stage(&mut self, bytes: &[u8]) -> Result<(PathBuf, [u8; 32])> {
        self.blob_seq += 1;
        let path = self.input.work_dir.join(format!("x{}", self.blob_seq));
        let mut f = create_private_file(&path)
            .with_context(|| format!("oluşturulamadı: {}", path.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("yazılamadı: {}", path.display()))?;
        Ok((path, hash_bytes(bytes)))
    }

    fn record(&mut self, dest: &Path, before: [u8; 32], after: [u8; 32], kind: &str) {
        self.out.transformed.push(TransformRecord {
            dest: dest.to_path_buf(),
            before,
            after,
            kind: kind.to_owned(),
        });
    }

    /// Merge or replace for paths of `owner`; a project is asked once.
    fn decision(&mut self, owner: &Owner, q: &mut dyn PlanQuestions) -> Result<Decision> {
        let (id, root) = match owner {
            Owner::Claude => return Ok(self.input.decision),
            Owner::Project(id, root) => (id, root),
        };
        if let Some(d) = self.project_decisions.get(id) {
            return Ok(*d);
        }
        let project = self
            .projects
            .get(id)
            .copied()
            .with_context(|| format!("manifestte olmayan proje: {}", id.0))?;
        let d = q.project_decision(project, root)?;
        self.project_decisions.insert(id.clone(), d);
        if d == Decision::Replace {
            self.project_scopes.insert(id.clone(), root.join(".claude"));
        }
        Ok(d)
    }

    fn project_entry(
        &mut self,
        id: &ProjectId,
        e: &Entry,
        q: &mut dyn PlanQuestions,
    ) -> Result<()> {
        match self.mapping(id).cloned() {
            Some(root) => {
                let owner = Owner::Project(id.clone(), root.clone());
                self.entry(&owner, &root, e, &e.rel, project_rule(&e.rel), q)
            }
            None => self.pend(id, e, DestTemplate::ProjectRel(e.rel.clone())),
        }
    }

    /// Queues `e` for the pending store under project `id`. Directories are
    /// implied by their files and not stored; a symlink gets an empty blob
    /// (its target is in the entry).
    fn pend(&mut self, id: &ProjectId, e: &Entry, template: DestTemplate) -> Result<()> {
        let blob = match e.kind {
            EntryKind::Dir => return Ok(()),
            EntryKind::File => self.blob(e),
            EntryKind::Symlink { .. } => self.stage(&[])?.0,
        };
        self.pending
            .entry(id.clone())
            .or_default()
            .push(PendingItem {
                entry: e.clone(),
                blob,
                dest_template: template,
            });
        Ok(())
    }

    /// One `projects/<slug>` folder: placed under the target slug of its
    /// project, or pending with that project.
    fn slug_dir(
        &mut self,
        slug: &str,
        entries: &[(&Entry, &str)],
        q: &mut dyn PlanQuestions,
    ) -> Result<()> {
        let mut cwds = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (e, rest) in entries {
            if e.kind == EntryKind::File && !rest.contains('/') && rest.ends_with(".jsonl") {
                first_cwds(&self.blob(e), &mut cwds, &mut seen)
                    .with_context(|| format!("okunamadı: {}", e.rel))?;
            }
        }
        let target = match self.slug_target(slug, &cwds) {
            SlugTarget::Pending(id) => {
                for (e, rest) in entries {
                    if !rest.is_empty() {
                        let template = DestTemplate::ClaudeProjectSlugDir {
                            rel_in_slug: (*rest).to_owned(),
                        };
                        self.pend(&id, e, template)?;
                    }
                }
                return Ok(());
            }
            SlugTarget::Place(t) => t,
            SlugTarget::Unresolved => {
                self.out.problems.push(format!(
                    "projects/{slug}: hedef proje yolu belirlenemedi, kaynak klasör adıyla yerleştirildi"
                ));
                slug.to_owned()
            }
        };
        let base = self.env.claude_dir.join("projects").join(&target);
        if self.input.decision == Decision::Replace {
            self.claude_scopes.insert(base.clone());
        }
        for (e, rest) in entries {
            let rule = if rest.ends_with(".jsonl") {
                Rule::Session
            } else if *rest == "memory/MEMORY.md" {
                Rule::Memory
            } else {
                Rule::Plain
            };
            self.entry(&Owner::Claude, &base, e, rest, rule, q)?;
            if e.kind == EntryKind::File && rule == Rule::Session && !rest.contains('/') {
                let dest = base.join(rest);
                if self.session_dests.insert(dest.clone()) {
                    self.out.sessions.placed += 1;
                    if fs::symlink_metadata(&dest).is_err() {
                        self.out.sessions.new_files += 1;
                    }
                }
                let oldest = &mut self.out.sessions.oldest_mtime_ns;
                *oldest = Some(oldest.map_or(e.mtime_ns, |o| o.min(e.mtime_ns)));
            }
        }
        Ok(())
    }

    fn slug_target(&self, slug: &str, cwds: &[String]) -> SlugTarget {
        let pending_owner = |p: &ProjectRef| self.mapping(&p.id).is_none().then(|| p.id.clone());
        if cwds.is_empty() {
            // Memory-only folder: named after the git root, i.e. a project.
            if let Some(p) = self
                .projects
                .values()
                .find(|p| claude_slug(&p.old_path) == slug)
            {
                return match self.mapping(&p.id) {
                    Some(root) => SlugTarget::Place(memory_slug_for(root)),
                    None => SlugTarget::Pending(p.id.clone()),
                };
            }
        } else {
            if let Some(t) = target_slug_dir(cwds, &self.mapped) {
                return SlugTarget::Place(t);
            }
            if let Some(id) = self.owner_of(&cwds[0]).and_then(pending_owner) {
                return SlugTarget::Pending(id);
            }
            // Not below a mapped project: the home rewrite, when all agree.
            let targets: BTreeSet<String> = cwds
                .iter()
                .map(|c| rewrite_home(c, &self.src_home, &self.dst_home))
                .collect();
            if targets.len() == 1 && targets.first() != cwds.first() {
                let t = targets.into_iter().next().unwrap_or_default();
                return SlugTarget::Place(claude_slug(&t));
            }
        }
        if slug == claude_slug(&self.src_home) {
            return SlugTarget::Place(claude_slug(&self.dst_home));
        }
        SlugTarget::Unresolved
    }

    /// Plans one entry at `base` + `rel`.
    fn entry(
        &mut self,
        owner: &Owner,
        base: &Path,
        e: &Entry,
        rel: &str,
        rule: Rule,
        q: &mut dyn PlanQuestions,
    ) -> Result<()> {
        // Empty only for a slug folder itself; `check_rel` vetted the rest.
        let dest = safe_join(base, rel, false)?;
        match &e.kind {
            EntryKind::Dir => {
                self.push(e, PathBuf::new(), dest, e.kind.clone(), None);
                Ok(())
            }
            EntryKind::Symlink { target } => {
                let new = rewrite_path_value(target, &self.src_home, &self.dst_home, &self.mapped);
                if new != *target {
                    let (b, a) = (hash_bytes(target.as_bytes()), hash_bytes(new.as_bytes()));
                    self.record(&dest, b, a, KIND_SYMLINK_TARGET);
                }
                let differs = match fs::symlink_metadata(&dest) {
                    Err(_) => false,
                    Ok(md) if md.file_type().is_symlink() => {
                        fs::read_link(&dest).ok().as_deref() != Some(Path::new(&new))
                    }
                    Ok(_) => true,
                };
                if differs {
                    let res = match self.decision(owner, q)? {
                        Decision::Replace => Resolution::KeepSource,
                        Decision::Merge => q.file(&dest)?,
                    };
                    self.resolved(&dest, res);
                }
                self.push(
                    e,
                    PathBuf::new(),
                    dest,
                    EntryKind::Symlink { target: new },
                    None,
                );
                Ok(())
            }
            EntryKind::File => self.file(owner, e, dest, rule, q),
        }
    }

    fn push(
        &mut self,
        e: &Entry,
        blob: PathBuf,
        dest: PathBuf,
        kind: EntryKind,
        hash: Option<[u8; 32]>,
    ) {
        self.out.plan.placements.push(Placement {
            entry_index: e.index,
            blob,
            dest,
            kind,
            mtime_ns: e.mtime_ns,
            mode: e.mode,
            expected_hash: hash,
        });
    }

    fn resolved(&mut self, dest: &Path, res: Resolution) {
        if res == Resolution::KeepTarget {
            self.out
                .skipped
                .push((dest.display().to_string(), KEPT_TARGET.to_owned()));
        }
        self.out.resolutions.insert(dest.to_path_buf(), res);
    }

    fn file(
        &mut self,
        owner: &Owner,
        e: &Entry,
        dest: PathBuf,
        rule: Rule,
        q: &mut dyn PlanQuestions,
    ) -> Result<()> {
        let source_hash = e
            .hash
            .with_context(|| format!("dosya kaydında hash yok: {}", e.rel))?;
        let (mut blob, mut hash, mut json) = (self.blob(e), source_hash, None);
        match rule {
            Rule::Json => {
                if let Some((value, changed)) = self.rewrite_json(e, q)? {
                    if changed {
                        let (b, h) = self.stage(&json_bytes(&value))?;
                        self.record(&dest, source_hash, h, KIND_JSON_PATHS);
                        (blob, hash) = (b, h);
                    }
                    json = Some(value);
                }
            }
            Rule::Text => {
                if let Some(text) = self.rewrite_text(e)? {
                    let (b, h) = self.stage(text.as_bytes())?;
                    self.record(&dest, source_hash, h, KIND_HOME_PATHS);
                    (blob, hash) = (b, h);
                }
            }
            Rule::Plain | Rule::Session | Rule::Memory => {}
        }

        let differs = match fs::symlink_metadata(&dest) {
            Err(_) => false,
            Ok(md) if md.is_file() => hash_file(&dest)? != hash,
            Ok(_) => true,
        };
        if differs {
            let res = match (self.decision(owner, q)?, rule) {
                (Decision::Replace, _) => Resolution::KeepSource,
                (Decision::Merge, Rule::Session) => Resolution::KeepBoth,
                (Decision::Merge, Rule::Memory) => {
                    if let Some((b, h)) = self.merge_memory(&dest, &blob)? {
                        self.record(&dest, source_hash, h, KIND_MEMORY_MERGE);
                        (blob, hash) = (b, h);
                        Resolution::KeepSource
                    } else {
                        q.file(&dest)?
                    }
                }
                (Decision::Merge, Rule::Json) => match (&json, read_json_file(&dest)) {
                    (Some(src), Some(mut target)) => {
                        let mut err = None;
                        let mut kept = Vec::new();
                        let mut ask = |c: &JsonConflict| match q.json(c) {
                            Ok(Resolution::KeepSource) => true,
                            Ok(_) => {
                                kept.push(c.path.clone());
                                false
                            }
                            Err(e) => {
                                err.get_or_insert(e);
                                false
                            }
                        };
                        merge_json_settings(&mut target, src, &mut ask);
                        if let Some(e) = err {
                            return Err(e);
                        }
                        for path in kept {
                            self.out.skipped.push((
                                format!("{}#{path}", dest.display()),
                                KEPT_TARGET_KEY.to_owned(),
                            ));
                        }
                        let after = hash_bytes(&json_bytes(&target));
                        self.record(&dest, source_hash, after, KIND_JSON_MERGE);
                        self.out.plan.json_writes.push((dest, target));
                        return Ok(());
                    }
                    _ => q.file(&dest)?,
                },
                (Decision::Merge, Rule::Plain | Rule::Text) => q.file(&dest)?,
            };
            self.resolved(&dest, res);
        }
        self.push(e, blob, dest, EntryKind::File, Some(hash));
        Ok(())
    }

    /// Source JSON with paths rewritten (R12) and whether anything changed;
    /// `None` (reported) when the file is not JSON.
    fn rewrite_json(
        &mut self,
        e: &Entry,
        q: &mut dyn PlanQuestions,
    ) -> Result<Option<(Value, bool)>> {
        let bytes = fs::read(self.blob(e)).with_context(|| format!("okunamadı: {}", e.rel))?;
        let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
            self.skip(e, "JSON okunamadı, yollar yeniden yazılmadı");
            return Ok(None);
        };
        let outcome = rewrite_json_paths(&mut value, &self.src_home, &self.dst_home, &self.mapped);
        for c in &outcome.conflicts {
            self.collapsed_key(&mut value, c, q)?;
        }
        Ok(Some((value, outcome.count > 0)))
    }

    /// Two source `projects` keys landed on one target key: the user picks.
    fn collapsed_key(
        &mut self,
        doc: &mut Value,
        c: &JsonConflict,
        q: &mut dyn PlanQuestions,
    ) -> Result<()> {
        // `target` here is the entry kept first, `source` the other one.
        if q.json(c)? == Resolution::KeepSource
            && let Some(key) = c.path.strip_prefix("projects.")
            && let Some(Value::Object(projects)) = doc.get_mut("projects")
        {
            projects.insert(key.to_owned(), c.source.clone());
        } else {
            self.out.skipped.push((
                c.path.clone(),
                "aynı hedef yola düşen ikinci proje kaydı alınmadı".into(),
            ));
        }
        Ok(())
    }

    /// Home rewrite of a text script; `None` when unchanged or skipped
    /// (binary, over 1 MiB: reported).
    fn rewrite_text(&mut self, e: &Entry) -> Result<Option<String>> {
        if e.size > TEXT_REWRITE_MAX {
            self.skip(e, "1 MiB üstü, ev dizini yolları yeniden yazılmadı");
            return Ok(None);
        }
        let bytes = fs::read(self.blob(e)).with_context(|| format!("okunamadı: {}", e.rel))?;
        let text = match String::from_utf8(bytes) {
            Ok(t) if !t.contains('\0') => t,
            _ => {
                self.skip(e, "ikili dosya, ev dizini yolları yeniden yazılmadı");
                return Ok(None);
            }
        };
        let new = rewrite_home(&text, &self.src_home, &self.dst_home);
        Ok((new != text).then_some(new))
    }

    /// Target `MEMORY.md` lines plus the source lines it lacks; `None` when
    /// either side is not UTF-8.
    fn merge_memory(&mut self, dest: &Path, blob: &Path) -> Result<Option<(PathBuf, [u8; 32])>> {
        let (Ok(target), Ok(source)) = (fs::read_to_string(dest), fs::read_to_string(blob)) else {
            return Ok(None);
        };
        let merged = merge_memory_md(&target, &source);
        self.stage(merged.as_bytes()).map(Some)
    }

    /// `~/.claude.json`: pending projects' keys go to the pending store, the
    /// rest is path-rewritten and merged into the target document.
    fn claude_json(&mut self, e: &Entry, q: &mut dyn PlanQuestions) -> Result<()> {
        let bytes = fs::read(self.blob(e)).context("~/.claude.json kaydı okunamadı")?;
        let mut src: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(err) => {
                self.out.problems.push(format!(
                    "paketteki ~/.claude.json geçerli JSON değil: {err}"
                ));
                return Ok(());
            }
        };
        self.pend_claude_json_keys(e, &mut src)?;
        let outcome = rewrite_json_paths(&mut src, &self.src_home, &self.dst_home, &self.mapped);
        for c in &outcome.conflicts {
            self.collapsed_key(&mut src, c, q)?;
        }

        let path = self.env.claude_json.clone();
        let original = match fs::symlink_metadata(&path) {
            Ok(_) => read_claude_json_stable(&path)?,
            Err(_) => Value::Null,
        };
        let mut merged = original.clone();
        let conflicts = merge_claude_json(&mut merged, &src, self.input.take_oauth);
        for c in conflicts {
            let res = match self.input.decision {
                Decision::Replace => Resolution::KeepSource,
                Decision::Merge => q.json(&c)?,
            };
            if res == Resolution::KeepSource {
                set_claude_json_value(&mut merged, &c);
            } else {
                self.out.skipped.push((
                    format!("claude_json:{}", c.path),
                    KEPT_TARGET_KEY.to_owned(),
                ));
            }
        }
        if merged != original {
            let after = hash_bytes(&json_bytes(&merged));
            let before = e.hash.unwrap_or(hash_bytes(&bytes));
            self.record(&path, before, after, KIND_CLAUDE_JSON_MERGE);
            self.out.plan.json_writes.push((path, merged));
        }
        Ok(())
    }

    /// Moves `projects` entries of pending projects out of `src` into one
    /// pending item per project: a blob holding `{ old key: value }`.
    fn pend_claude_json_keys(&mut self, e: &Entry, src: &mut Value) -> Result<()> {
        let Some(Value::Object(projects)) = src.get_mut("projects") else {
            return Ok(());
        };
        let mut per_project: BTreeMap<ProjectId, Map<String, Value>> = BTreeMap::new();
        for key in projects.keys().cloned().collect::<Vec<_>>() {
            let Some(owner) = self.owner_of(&key) else {
                continue;
            };
            if self.mapping(&owner.id).is_some() {
                continue;
            }
            let id = owner.id.clone();
            if let Some(v) = projects.remove(&key) {
                per_project.entry(id).or_default().insert(key, v);
            }
        }
        for (id, keys) in per_project {
            let bytes = serde_json::to_vec_pretty(&Value::Object(keys))?;
            let (blob, hash) = self.stage(&bytes)?;
            let mut entry = e.clone();
            entry.size = bytes.len() as u64;
            entry.hash = Some(hash);
            self.pending.entry(id).or_default().push(PendingItem {
                entry,
                blob,
                dest_template: DestTemplate::ClaudeJsonProjectsKey,
            });
        }
        Ok(())
    }

    /// Replace scope for a top-level `~/.claude` folder (or a folder one
    /// level into `plugins`); `projects` folders are scoped per slug.
    fn claude_scope(&mut self, e: &Entry, base: &Path) {
        if e.kind != EntryKind::Dir {
            return;
        }
        let parts: Vec<&str> = e.rel.split('/').collect();
        let scoped = match parts.as_slice() {
            ["projects" | "plugins"] => false,
            [_] => true,
            ["plugins", _] => true,
            _ => false,
        };
        if scoped {
            self.claude_scopes.insert(base.join(&e.rel));
        }
    }

    fn finish(mut self) -> Result<ClaudePlan> {
        let dests: BTreeSet<PathBuf> = self
            .out
            .plan
            .placements
            .iter()
            .map(|p| p.dest.clone())
            .chain(self.out.plan.json_writes.iter().map(|(d, _)| d.clone()))
            .collect();
        let selection = &self.rb.manifest.selection;
        let claude_dir = self.env.claude_dir.clone();
        for dir in std::mem::take(&mut self.claude_scopes) {
            let mut keep = dests_below(&dests, &dir);
            for f in target_files(&dir)? {
                // Machine-specific or unselected data is outside the mirror.
                let protected = f
                    .strip_prefix(&claude_dir)
                    .ok()
                    .and_then(Path::to_str)
                    .and_then(classify)
                    .is_some_and(|(class, category)| {
                        class == Class::Never || !selection.includes(Tool::Claude, category)
                    });
                if protected {
                    keep.insert(f);
                }
            }
            self.out
                .plan
                .replace_scopes
                .push(ReplaceScope { dir, keep });
        }
        for dir in std::mem::take(&mut self.project_scopes).into_values() {
            let mut keep = dests_below(&dests, &dir);
            // `.claude/worktrees` holds code, never Claude data.
            keep.extend(target_files(&dir.join("worktrees"))?);
            self.out
                .plan
                .replace_scopes
                .push(ReplaceScope { dir, keep });
        }
        for (id, items) in std::mem::take(&mut self.pending) {
            let project = self
                .projects
                .get(&id)
                .copied()
                .cloned()
                .with_context(|| format!("manifestte olmayan proje: {}", id.0))?;
            self.out.pending.push((project, items));
        }
        // Pending projects with nothing to store are still reported.
        for p in self.projects.values() {
            let waiting = self.out.pending.iter().any(|(r, _)| r.id == p.id);
            if self.mapping(&p.id).is_none() && !waiting {
                self.out.skipped.push((
                    format!("project:{}", p.id.0),
                    "hedefte bulunamadı; aktarılacak dosyası yok".into(),
                ));
            }
        }
        Ok(self.out)
    }
}

enum SlugTarget {
    Place(String),
    Pending(ProjectId),
    Unresolved,
}

/// `projects/<slug>[/<rest>]` as `(slug, rest)`; `rest` is `""` for the
/// slug folder itself.
fn split_slug(rel: &str) -> Option<(&str, &str)> {
    let rest = rel.strip_prefix("projects/")?;
    Some(rest.split_once('/').unwrap_or((rest, "")))
}

fn claude_dir_rule(rel: &str) -> Rule {
    let first = rel.split('/').next().unwrap_or_default();
    let settings = !rel.contains('/') && first.starts_with("settings") && first.ends_with(".json");
    if settings || PLUGIN_REGISTRIES.contains(&rel) {
        Rule::Json
    } else if TEXT_DIRS.contains(&first) && rel.contains('/') {
        Rule::Text
    } else {
        Rule::Plain
    }
}

fn project_rule(rel: &str) -> Rule {
    let settings = rel
        .strip_prefix(".claude/")
        .is_some_and(|n| !n.contains('/') && n.starts_with("settings") && n.ends_with(".json"));
    if rel == ".mcp.json" || settings {
        Rule::Json
    } else {
        Rule::Plain
    }
}

/// Applies a `merge_claude_json` conflict in favour of the source.
fn set_claude_json_value(doc: &mut Value, c: &JsonConflict) {
    if c.path.is_empty() {
        *doc = c.source.clone();
        return;
    }
    let Value::Object(top) = doc else { return };
    match c.path.split_once('.') {
        None => {
            top.insert(c.path.clone(), c.source.clone());
        }
        Some((key, name)) => {
            if let Some(Value::Object(entries)) = top.get_mut(key) {
                entries.insert(name.to_owned(), c.source.clone());
            }
        }
    }
}

fn read_json_file(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// `path` is `base` or below it (string prefix at a `/` boundary).
fn is_below(path: &str, base: &str) -> bool {
    let base = base
        .strip_suffix('/')
        .filter(|b| !b.is_empty())
        .unwrap_or(base);
    path.strip_prefix(base)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Rejects a bundle `rel` that could leave its root (checked for every
/// entry before planning). Only `~/.claude.json` has an empty `rel`.
fn check_rel(e: &Entry) -> Result<()> {
    let allow_empty = e.root == RootKind::ClaudeJson;
    if e.root == RootKind::ClaudeJson && !e.rel.is_empty() {
        bail!("geçersiz kayıt yolu: {:?}", e.rel);
    }
    safe_join(Path::new("/"), &e.rel, !allow_empty)
        .map(|_| ())
        .with_context(|| format!("{} (kök {})", e.rel, e.root.tag()))
}

/// `base` + `rel` where every `/`-separated part of `rel` is a plain name.
/// An empty `rel` gives `base` unless `need_name`.
fn safe_join(base: &Path, rel: &str, need_name: bool) -> Result<PathBuf> {
    if rel.is_empty() {
        if need_name {
            bail!("geçersiz kayıt yolu: boş");
        }
        return Ok(base.to_path_buf());
    }
    let ok = rel.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains('\\')
            && !part.chars().any(char::is_control)
    });
    if !ok {
        bail!("geçersiz kayıt yolu, hedef kökünün dışına çıkabilir: {rel:?}");
    }
    let joined = base.join(rel);
    if !joined.starts_with(base) {
        bail!("geçersiz kayıt yolu, hedef kökünün dışına çıkabilir: {rel:?}");
    }
    Ok(joined)
}

/// A target project root must be absolute and free of `..`.
fn check_root(root: &Path) -> Result<()> {
    if !root.is_absolute()
        || root
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        bail!("geçersiz hedef proje yolu: {}", root.display());
    }
    Ok(())
}

fn dests_below(dests: &BTreeSet<PathBuf>, dir: &Path) -> BTreeSet<PathBuf> {
    dests
        .iter()
        .filter(|d| d.starts_with(dir))
        .cloned()
        .collect()
}

/// Files and symlinks under `dir` on the target (symlinked dirs not
/// entered); empty when `dir` is missing.
fn target_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return Ok(out);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for item in fs::read_dir(&d).with_context(|| format!("okunamadı: {}", d.display()))? {
            let item = item.with_context(|| format!("okunamadı: {}", d.display()))?;
            let ft = item.file_type()?;
            if ft.is_dir() {
                stack.push(item.path());
            } else {
                out.push(item.path());
            }
        }
    }
    Ok(out)
}
