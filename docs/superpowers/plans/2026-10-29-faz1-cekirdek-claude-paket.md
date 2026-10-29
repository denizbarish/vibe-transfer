# vibe-transfer Faz 1: Çekirdek + Claude + Paket Dosyası (Mac→Mac) Uygulama Planı

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Claude Code verisini macOS'tan macOS'a şifreli paket dosyasıyla (`export` / `import`) sıfır kayıpla taşıyan, yol eşleme, bekleme alanı, birleştir/değiştir, sır taşıma, doğrulama ve geri alma içeren ilk çalışan sürüm.

**Architecture:** Cargo workspace: `vt-core` (format, tarama, eşleme, uygulama), `vt-secrets` (sır deposu soyutlaması), `vt-claude` (Claude adaptörü), `vt-cli` (`vibe-transfer` binary). Canlı aktarım (Faz 3) aynı paket formatını akıtacağı için format ve uygulama motoru bu fazda kesinleşir.

**Tech Stack:** Rust 1.98.1 (edition 2024), clap 4.6, serde/serde_json, blake3 1.8, zstd 0.14, age 0.12 (scrypt passphrase), keyring 4.2, dialoguer 0.12, indicatif 0.18, uuid 1.29, sysinfo 0.40, filetime 0.2, unicode-normalization 0.1, walkdir 2.5, tempfile 3.27, anyhow/thiserror, assert_cmd 2.2 (sürümler 2026-10-29'da crates.io'dan ölçüldü; uygulama anında `cargo add` güncel kararlıyı alır, Cargo.lock commit edilir).

**Spec:** `docs/superpowers/specs/2026-10-29-vibe-transfer-design.md` (araştırma: `docs/research/research-claude.md`)

## Yol haritası (bu plan yalnız Faz 1)

| Faz | İçerik | Çalışan çıktı |
|---|---|---|
| **1 (bu plan)** | Çekirdek format, Claude adaptörü, paket dosyası, eşleme, bekleme alanı, uygulama/rollback, sır taşıma | `export`/`import` ile Claude Mac→Mac |
| 2 | Codex adaptörü: SQLite anlık görüntü, satır düzeyinde birleştirme, `rollout_path`/`cwd` yeniden yazımı, hook hash, `auth.json` + keyring; spike: `thread_history_1` yeniden kurulumu | `export`/`import` ile Codex Mac→Mac |
| 3 | Canlı aktarım, aynı ağ: mDNS, QUIC (quinn), SPAKE2 + SAS, segment düzeyinde devam, delta turu, kaynakta oturum kapatmanın otomatik yapılması | `send`/`receive --lan` |
| 4 | Farklı ağ: UPnP-IGD/NAT-PMP/PCP, IPv6, bağlantı bileti, başarısızlıkta paket önerisi | `send`/`receive --wan` |
| 5 | Mac→Windows, Windows→Windows, Windows→Mac: yol ayracı, Credential Manager / `.credentials.json`, symlink çözme, Firewall tespiti | Çapraz platform |

Her faz kendi planıyla, bir önceki bittikten sonra yazılır.

## Global Constraints

- Araya sunucu girmez: hiçbir kod ağa çıkmaz (Faz 1'de ağ kodu yok; telemetri, güncelleme kontrolü yok).
- Sır sınıfı öğeler (`secret` kategorisi: Keychain kaydı) diske düz metin yazılmaz: staging dahil. Yalnız bellekte ve şifreli paket içinde.
- Kaynakta hiçbir dosya değiştirilmez; tek istisna `logout-source`.
- Hedefte silme yok: değiştirilen veya kaldırılan her hedef öğe `~/.vibe-transfer/backups/<session>/` altına taşınır.
- mtime ve POSIX mod korunur (Claude 30 günlük temizliği mtime'a bakar).
- Testler gerçek `~/.claude`, `~/.claude.json`, gerçek Keychain'e dokunmaz: tüm yollar `VtEnv` ile enjekte edilir, sır testleri `MemStore` kullanır.
- Uzun tire (U+2014, U+2013) hiçbir kullanıcıya görünen metinde, dokümanda ve commit mesajında kullanılmaz. Kullanıcıya görünen metin Türkçe, tam Türkçe karakterli.
- Commit adımları kullanıcı onayıyla yapılır; push yok (repo henüz GitHub'da değil).
- Lisans: `MIT OR Apache-2.0`.

## Review Focus

1. **Kaynakta Claude açıkken export:** yarım son satırlı jsonl ve yeniden yazılmakta olan `~/.claude.json`. Beklenen: araç uyarır; jsonl yalnız son tam satıra kadar alınır ve raporda "kısmi kuyruk" görünür; `~/.claude.json` geçerli JSON okunana kadar yeniden okunur. Test: Task 10 `export_truncates_partial_jsonl_tail`.
2. **Hedef kullanıcı adı farklı ve Türkçe karakterli yol (NFD):** slug NFC/NFD farkıyla iki klasöre bölünmemeli. Test: Task 8 `slug_rename_merges_nfc_nfd_variants`.
3. **Import ortasında çökme:** journal yarımken `rollback` hedefi tam eski haline getirmeli. Test: Task 9 `rollback_after_partial_apply_restores_exact_tree`.
4. **Yanlış parola veya bozulmuş paket:** hiçbir şey staging dışına yazılmadan net hata. Test: Task 4 `tampered_segment_is_rejected`, `wrong_passphrase_fails_cleanly`.
5. **Hedefte farklı Claude hesabı girişli:** sormadan ezilmemeli. Test: Task 11 `import_prompts_on_different_target_account`.

---

### Task 1: Spike: Claude jsonl `cwd` yeniden yazımı güvenli mi

Throwaway. Kod repoya girmez; yalnız karar dosyası girer.

**Files:**
- Create: `docs/spikes/2026-10-29-claude-cwd-rewrite.md`

**Interfaces:**
- Produces: karar sabiti `CLAUDE_REWRITE_JSONL_CWD: bool` (Task 8 bunu kullanır).

- [ ] **Step 1: Kullanıcıdan onay al.** Spike gerçek Claude hesabıyla iki kısa `claude -p` çağrısı yapar ve gerçek `~/.claude/projects` altında geçici iki klasör oluşturur (sonunda silinir). Onay yoksa Step 5'e geç ve `false` yaz.
- [ ] **Step 2: Hazırlık.** Gerçek bir kısa oturum jsonl'ünü seç. `/private/tmp/vt-spike-a` ve `/private/tmp/vt-spike-b` klasörlerini oluştur. Oturumu yeni UUID ile iki slug klasörüne kopyala: A'da içerik aynen, B'de her satırın `cwd` alanı yeni yola yazılmış (`sessionId` alanları yeni UUID). mtime korunur.
- [ ] **Step 3: Çalıştır.** Her klasörde `claude -p --resume <uuid> "Yalnız OK yaz"`. Kaydet: çıkış kodu, devam eden oturumun önceki bağlamı görüp görmediği (bir önceki mesajı sor), hata metni. `claude --resume` seçici listesinde iki oturumun da göründüğünü kullanıcıya elle kontrol ettir.
- [ ] **Step 4: Temizle.** İki geçici slug klasörünü ve `/private/tmp/vt-spike-*` klasörlerini sil.
- [ ] **Step 5: Karar dosyasını yaz.** Gözlemler ve sonuç: B çalışıyor ve A'dan farklı bir sorun yoksa `CLAUDE_REWRITE_JSONL_CWD = true`; A çalışıp B bozuluyorsa `false`; ikisi de çalışıyorsa `false` (daha az dönüşüm = daha az risk). `/rewind` için Faz 1 kabul testinde elle kontrol notu düş.

### Task 2: Workspace iskeleti, Claude slug kodlaması, git remote normalizasyonu

**Files:**
- Create: `Cargo.toml` (workspace), `rust-toolchain.toml` (`1.98.1`), `.gitignore`, `LICENSE-MIT`, `LICENSE-APACHE`, `crates/vt-core/Cargo.toml`, `crates/vt-core/src/lib.rs`, `crates/vt-core/src/pathcodec.rs`, `crates/vt-core/src/remote.rs`, `crates/vt-core/src/env.rs`, `crates/vt-secrets/`, `crates/vt-claude/`, `crates/vt-cli/` (boş iskeletler, `vt-cli` binary adı `vibe-transfer`)
- Test: `crates/vt-core/tests/pathcodec.rs`, `crates/vt-core/tests/remote.rs`

**Interfaces:**
- Produces:
  - `pub fn claude_slug(path: &str) -> String`
  - `pub fn java_hash(s: &str) -> i32`
  - `pub fn normalize_remote(url: &str) -> Option<String>` (çıktı: `host/owner/repo`, host küçük harf, `.git` ve sondaki `/` atılmış)
  - `pub fn read_origin(repo_root: &Path) -> Option<String>` (`.git/config` içinden `[remote "origin"] url`; `.git` dosyaysa `gitdir:` takip edilir; `git` binary çağrılmaz)
  - `pub struct VtEnv { pub home: PathBuf, pub user: String, pub claude_dir: PathBuf, pub claude_json: PathBuf, pub agents_dir: PathBuf, pub vt_home: PathBuf, pub claude_config_dir_override: Option<PathBuf> }` ve `VtEnv::from_process() -> Result<VtEnv>` (`CLAUDE_CONFIG_DIR` varsa `claude_dir` ona, `claude_json` `<dir>/.claude.json`'a döner; `vt_home` = `~/.vibe-transfer`)

- [ ] **Step 1: Failing testler**

```rust
#[test] fn slug_basic() {
    assert_eq!(claude_slug("/Users/alice/Projects/web/app.example"), "-Users-alice-Projects-web-app-example");
    assert_eq!(claude_slug("/Users/bob/Code/my_app"), "-Users-bob-Code-my-app");
}
#[test] fn slug_utf16_units() {
    assert_eq!(claude_slug("/Users/alice/Projects/Ki\u{015F}isel"), "-Users-alice-Projects-Ki-isel");      // NFC
    assert_eq!(claude_slug("/Users/alice/Projects/Kis\u{0327}isel"), "-Users-alice-Projects-Kis-isel");    // NFD
    assert_eq!(claude_slug("/Users/alice/a\u{1F600}b"), "-Users-alice-a--b");                               // surrogate çifti = 2 tire
}
#[test] fn slug_long_path_hash_suffix() {
    let p = format!("/Users/alice/{}", "x".repeat(210));
    let s = claude_slug(&p);
    assert_eq!(s.len(), 207);
    assert!(s.starts_with("-Users-alice-xxx"));
    assert!(s.ends_with("-ilfi6z"));
}
#[test] fn remote_forms_equal() {
    let a = normalize_remote("git@github.com:Acme/Repo.git");
    assert_eq!(a.as_deref(), Some("github.com/Acme/Repo"));
    assert_eq!(normalize_remote("https://github.com/Acme/Repo"), a);
    assert_eq!(normalize_remote("ssh://git@GitHub.com/Acme/Repo.git/"), a);
    assert_eq!(normalize_remote(""), None);
}
#[test] fn read_origin_from_config_and_worktree_file() { /* tempdir: .git/config ile ve `.git` dosyası (gitdir: ...) ile; ikisi de url döner */ }
```

- [ ] **Step 2: Çalıştır, FAIL gör.** `cargo test -p vt-core --quiet` → derleme hatası (fonksiyonlar yok).
- [ ] **Step 3: Uygula.** `claude_slug`: `path.encode_utf16()` üzerinden her kod birimi ASCII `[A-Za-z0-9]` değilse `-`; sonuç 200 birimden uzunsa `&r[..200] + "-" + base36(|java_hash(path)| as u64)`. `java_hash`: UTF-16 birimleri üzerinde `h = h.wrapping_mul(31).wrapping_add(unit as i32)`. `abs` i32::MIN için `2147483648` vermeli (i64'e çevirip abs). Owner/repo büyük harfi korunur, yalnız host küçültülür.
- [ ] **Step 4: Çalıştır, PASS.** `cargo test -p vt-core --quiet`
- [ ] **Step 5: Commit (onaylı).** `git init` + `git commit -m "feat: workspace, claude slug ve remote normalizasyonu"`

### Task 3: Manifest, tarama, hash ve Merkle kök

**Files:**
- Create: `crates/vt-core/src/manifest.rs`, `crates/vt-core/src/secret_types.rs`, `crates/vt-core/src/scan.rs`, `crates/vt-core/src/merkle.rs`
- Test: `crates/vt-core/tests/manifest.rs`

**Interfaces:**
- Produces:
  - `pub enum Tool { Claude, Codex }`
  - `pub enum Category { Sessions, Memory, Settings, Secrets, ProjectFiles, Cache }` (spec 12.2 kategorileri; `Cache` varsayılan kapalı)
  - `pub enum RootKind { ClaudeDir, ClaudeJson, AgentsDir, CodexDir, Project(ProjectId) }`
  - `pub struct ProjectId(pub String)` (`normalize_remote` sonucu veya `path:<eski mutlak yol>`)
  - `pub enum EntryKind { File, Dir, Symlink { target: String } }`
  - `pub struct Entry { pub index: u32, pub tool: Tool, pub category: Category, pub root: RootKind, pub rel: String, pub kind: EntryKind, pub size: u64, pub mtime_ns: i64, pub mode: u32, pub hash: Option<[u8; 32]>, pub partial_tail: bool }` (`rel` `/` ayraçlı, NFC)
  - `pub struct SourceInfo { pub os: String, pub user: String, pub home: String, pub hostname: String, pub tool_versions: BTreeMap<String, String> }`
  - `pub enum Class { Transfer, Optional, Never }`
  - `pub struct ProjectRef { pub id: ProjectId, pub old_path: String, pub remote: Option<String>, pub is_git: bool }`
  - `pub struct Selection { pub tools: BTreeSet<Tool>, pub categories: BTreeMap<Tool, BTreeSet<Category>> }` (yalnız veri; kurucular Task 10)
  - `pub struct SecretBytes` (zeroize'lı, `Debug` değeri göstermez), `pub enum SecretKind { ClaudeKeychain }` (Faz 2 Codex türlerini ekler), `pub struct SecretMeta { pub tool: Tool, pub kind: SecretKind, pub account_uuid: Option<String>, pub email: Option<String> }`, `pub struct SecretItem { pub meta: SecretMeta, pub service: String, pub account_hint: String, pub value: SecretBytes }` (dosya: `crates/vt-core/src/secret_types.rs`; `vt-secrets` bu tipleri kullanır, döngüsel bağımlılık yok)
  - `pub struct JsonConflict { pub path: String, pub source: serde_json::Value, pub target: serde_json::Value }`
  - `pub struct Manifest { pub format_version: u32, pub session_id: String, pub created_at: String, pub source: SourceInfo, pub selection: Selection, pub entries: Vec<Entry>, pub projects: Vec<ProjectRef>, pub secrets: Vec<SecretMeta>, pub notes: Vec<String> }` (`format_version = 1`)
  - `pub struct RawEntry { pub rel: String, pub kind: EntryKind, pub size: u64, pub mtime_ns: i64, pub mode: u32 }`
  - `pub fn scan_tree(root: &Path, exclude: &dyn Fn(&str) -> bool) -> Result<Vec<RawEntry>>` (symlink takip edilmez, hedefi okunur)
  - `pub fn hash_file(path: &Path) -> Result<[u8; 32]>` (BLAKE3, `update_mmap_rayon` yerine akışlı okuma)
  - `pub fn merkle_root(entries: &[Entry]) -> [u8; 32]`

- [ ] **Step 1: Failing testler:** `secret_item_debug_redacts_value` (`format!("{:?}", item)` değeri içermez), `scan_preserves_symlink_and_mtime` (tempdir: dosya, alt dizin, göreli symlink; dönen RawEntry'de symlink hedefi `../x`, mtime_ns dosyanınkine eşit), `merkle_root_is_order_independent_and_content_sensitive` (entries karıştırılınca kök aynı; bir hash'in tek baytı değişince kök farklı), `manifest_json_roundtrip`.
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-core --quiet`
- [ ] **Step 3: Uygula.** Merkle: yapraklar `blake3(root_kind_tag || rel || kind || hash_or_zero || size_le || mode_le)`, `(root_kind_tag, rel)` sırasına göre sıralı; ikili ağaç, tek kalan yaprak bir üst seviyeye aynen çıkar; boş liste için `blake3(b"vt-empty")`.
- [ ] **Step 4: PASS.** `cargo test -p vt-core --quiet`
- [ ] **Step 5: Commit (onaylı).** `feat: manifest, tarama ve merkle kök`

### Task 4: Paket formatı (yazıcı ve okuyucu)

**Files:**
- Create: `crates/vt-core/src/bundle/mod.rs`, `crates/vt-core/src/bundle/writer.rs`, `crates/vt-core/src/bundle/reader.rs`, `crates/vt-core/src/bundle/passphrase.rs`
- Test: `crates/vt-core/tests/bundle.rs`

**Interfaces:**
- Consumes: `Manifest`, `Entry`, `merkle_root` (Task 3).
- Produces:
  - `pub fn generate_passphrase() -> SecretString` (26 base32 karakter, 4'lü gruplar `-` ile: 130 bit)
  - `pub struct BundleWriter` ile `create(path: &Path, pass: &SecretString) -> Result<Self>`, `add_file(&mut self, entry_index: u32, data: &mut dyn Read) -> Result<[u8; 32]>` (yazılan baytların BLAKE3'ünü döner), `add_secret(&mut self, item: &SecretItem) -> Result<()>`, `finish(self, manifest: &Manifest) -> Result<BundleSummary>`
  - `pub struct BundleSummary { pub bytes: u64, pub segments: u32, pub merkle_root: [u8; 32] }`
  - `pub fn read_bundle(path: &Path, pass: &SecretString, staging: &Path) -> Result<ReadBundle>`; `pub struct ReadBundle { pub manifest: Manifest, pub blobs: PathBuf, pub secrets: Vec<SecretItem> }` (blob'lar `staging/blobs/<entry_index>`; sırlar yalnız bellekte)
  - `pub enum BundleError { WrongPassphrase, Corrupt { what: String }, UnsupportedVersion(u32) }`

Format (spec 5.3'ü kesinleştirir):
```
dış katman: age v1, scrypt passphrase alıcısı, armor yok
iç akış:   b"VTB\x01"
           çerçeve = [kind: u8][len: u32 BE][payload]
           kind 1 Segment: [seq: u32][blake3(zstd_bytes): 32][zstd_bytes]
                     zstd(seviye 3) içeriği = tekrarlanan [entry_index: u32][offset: u64][len: u32][bytes]
                     segment başına sıkıştırılmamış veri 32 MiB'a kadar
           kind 2 Secret:  age(scrypt, aynı parola) ile şifreli JSON SecretItem
           kind 3 Manifest: JSON Manifest + merkle_root(32)
           kind 4 End:     blake3(önceki tüm iç akış baytları)
```

- [ ] **Step 1: Failing testler:** `roundtrip_files_secrets_manifest` (3 dosya, biri 40 MiB rastgele: iki segmente bölünür; 1 sır; okununca blob baytları ve sır aynı, manifest eşit), `wrong_passphrase_fails_cleanly` (hata `BundleError::WrongPassphrase`, staging boş), `tampered_segment_is_rejected` (age'i açmadan bir bayt değişirse age hatası `Corrupt`; ayrıca test yardımcısıyla iç akışta segment hash'i bozulursa `Corrupt { what: "segment 0" }`), `manifest_hash_mismatch_is_rejected` (manifest'teki bir entry hash'i blob'la uyuşmazsa `Corrupt`).
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-core bundle --quiet`
- [ ] **Step 3: Uygula.** Okuyucu akışı tek geçişte okur, blob'ları yazar, sonda manifest gelince her blob'un BLAKE3'ünü manifest'le ve Merkle kökü karşılaştırır; uyuşmazlıkta staging'i siler. `SecretString` için `age::secrecy` kullanılır.
- [ ] **Step 4: PASS.**
- [ ] **Step 5: Commit (onaylı).** `feat: şifreli paket formatı`

### Task 5: Sır deposu soyutlaması ve Claude kimlik kaydı

**Files:**
- Create: `crates/vt-secrets/src/lib.rs`, `crates/vt-secrets/src/keyring_store.rs`, `crates/vt-secrets/src/mem_store.rs`
- Test: `crates/vt-secrets/tests/store.rs`

**Interfaces:**
- Consumes: `SecretBytes`, `SecretItem`, `SecretMeta`, `SecretKind` (Task 3, `vt-core`).
- Produces:
  - `pub trait SecretStore { fn get(&self, service: &str, account: &str) -> Result<Option<SecretBytes>>; fn set(&self, service: &str, account: &str, value: &SecretBytes) -> Result<()>; fn delete(&self, service: &str, account: &str) -> Result<()>; }`
  - `pub struct KeyringStore` (keyring 4.2, macOS Keychain generic password; API docs.rs'den uygulama anında doğrulanır), `pub struct MemStore` (test)
  - `pub fn claude_service_name(claude_config_dir_override: Option<&Path>) -> String` (`"Claude Code-credentials"`, override varsa `+ "-" + hex(sha256(dir))[..8]`)

- [ ] **Step 1: Failing testler:** `mem_store_set_get_delete`, `claude_service_name_default_and_override` (override `/tmp/x` için beklenen sonek testte `sha256("/tmp/x")` ile hesaplanır).
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-secrets --quiet`
- [ ] **Step 3: Uygula.** `KeyringStore` için entegrasyon testi `#[ignore]` ve yalnız `VT_KEYCHAIN_TEST=1` ile, servis adı `vibe-transfer-test-<uuid>` (gerçek Claude kaydına asla dokunmaz).
- [ ] **Step 4: PASS.**
- [ ] **Step 5: Commit (onaylı).** `feat: sır deposu soyutlaması`

### Task 6: Claude adaptörü: envanter, sınıflandırma, proje referansları

**Files:**
- Create: `crates/vt-claude/src/lib.rs`, `crates/vt-claude/src/inventory.rs`, `crates/vt-claude/src/projects.rs`, `crates/vt-claude/src/claude_json.rs`
- Test: `crates/vt-claude/tests/inventory.rs`, `crates/vt-claude/tests/fixtures/` (sentetik `home/` ağacı: `.claude/` alt yapısı, `.claude.json`, `.agents/skills`, iki proje: biri remote'lu, biri remote'suz; içinde sır yok)

**Interfaces:**
- Consumes: `VtEnv`, `scan_tree`, `Entry`, `Category`, `read_origin`, `normalize_remote`.
- Produces:
  - `pub struct ClaudeAdapter { pub env: VtEnv }`
  - `pub fn classify(rel_in_claude_dir: &str) -> Option<(Class, Category)>` (tablo research-claude.md bölüm 6'dan birebir: `projects/**` jsonl Sessions, `projects/*/memory/**` Memory, `history.jsonl`/`file-history`/`paste-cache`/`plans`/`tasks`/`todos` Sessions, `CLAUDE.md`/`settings*.json`/`agents`/`commands`/`skills`/`hooks`/`scripts`/`bin`/`plugins/installed_plugins.json`/`plugins/known_marketplaces.json`/`plugins/data` Settings, `backups`/`cache`/`logs`/`stats-cache.json`/`plugins/cache`/`plugins/marketplaces` Optional+Cache, research listesindeki "asla" öğeleri Never; listede olmayan kök öğe: Optional+Cache ve raporda "sınıflandırılmamış" notu)
  - `pub fn inventory(&self, sel: &Selection) -> Result<Inventory>`; `pub struct Inventory { pub entries: Vec<(Entry, PathBuf)>, pub projects: Vec<ProjectRef>, pub skipped: Vec<(String, String)> }` (ikinci alan mutlak kaynak yolu; `skipped` = rel + neden)
  - `pub fn project_refs(&self) -> Result<Vec<ProjectRef>>`: eski yollar sırasıyla jsonl `cwd` (her slug klasöründe ilk 50 satır taranır), `~/.claude.json` `projects` anahtarları, `githubRepoPaths`; her yol için `read_origin` ile git kökü yukarı doğru aranır.
  - `pub fn project_inner_files(project_root: &Path) -> Vec<String>`: `CLAUDE.md`, `CLAUDE.local.md`, `.mcp.json`, `.claude/**` (`.claude/worktrees/**` hariç) ve kökten 3 seviye derinliğe kadar alt klasörlerdeki `CLAUDE.md`. `AGENTS.md`, `AGENTS.override.md`, `.codex/**` Codex adaptörüne aittir (Faz 2); araç seçimi Claude ise taşınmaz.
  - `pub fn read_claude_json_stable(path: &Path) -> Result<serde_json::Value>` (parse hatasında 100 ms arayla 5 kez yeniden okur)

- [ ] **Step 1: Failing testler:** `classify_table` (her sınıftan en az 2 örnek; `daemon/control.key` Never, `sessions/x.json` Never, `projects/-a/memory/MEMORY.md` Memory), `inventory_respects_selection` (Memory kapalıyken memory dosyası yok, `skipped`'de "kullanıcı seçimiyle atlandı"), `project_refs_from_cwd_and_claude_json` (fixture'daki iki proje bulunur; remote'lu olanın id'si `github.com/acme/app`, diğeri `path:/Users/src/plain`), `inventory_includes_agents_skills_symlink_targets`.
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-claude --quiet`
- [ ] **Step 3: Uygula.**
- [ ] **Step 4: PASS.**
- [ ] **Step 5: Commit (onaylı).** `feat: claude envanteri ve sınıflandırma`

### Task 7: Yol eşleme, hedef indeksi, bekleme alanı deposu

**Files:**
- Create: `crates/vt-core/src/pathmap.rs`, `crates/vt-core/src/pending.rs`
- Test: `crates/vt-core/tests/pathmap.rs`

**Interfaces:**
- Consumes: `normalize_remote`, `read_origin`, `ProjectId`, `ProjectRef` (Task 3).
- Produces:
  - `pub struct TargetIndex { pub by_remote: BTreeMap<String, Vec<PathBuf>> }`
  - `pub fn build_target_index(roots: &[PathBuf], max_depth: usize) -> TargetIndex` (varsayılan kökler: hedef `~`, derinlik 5; `node_modules`, `Library`, `.Trash`, gizli klasörler atlanır)
  - `pub enum Mapping { Auto(PathBuf), Choose(Vec<PathBuf>), Pending }`
  - `pub fn map_project(r: &ProjectRef, idx: &TargetIndex, src_home: &str, dst_home: &Path) -> Mapping`
  - `pub fn rewrite_home(s: &str, src_home: &str, dst_home: &str) -> String` (yalnız `src_home` sonrası karakter `/`, `"`, `'`, boşluk veya dize sonu ise değiştirir)
  - `pub struct PendingStore { pub dir: PathBuf }` ile `put(&self, project: &ProjectRef, items: &[PendingItem]) -> Result<()>`, `list(&self) -> Result<Vec<PendingProject>>`, `take(&self, id: &ProjectId) -> Result<PendingProject>`; `PendingItem { pub entry: Entry, pub blob: PathBuf, pub dest_template: DestTemplate }`; `DestTemplate` hedef yolu proje kökü bilinince hesaplayan enum (`ProjectRel(String)`, `ClaudeProjectSlugDir { rel_in_slug: String }`, `ClaudeJsonProjectsKey`)

Eşleme kuralları (spec 7, sırayla):
1. `remote` var: indekste 1 yol → `Auto`; birden çok → `Choose`; yok → `Pending`.
2. `remote` yok ve `is_git` → `Pending`.
3. Git değil ve `old_path == src_home` veya `src_home/` ile başlıyor → `Auto(rewrite_home)`.
4. Hedefte birebir aynı mutlak yol var → `Auto(old_path)`.
5. Diğer → `Pending`.

- [ ] **Step 1: Failing testler:** her kural için bir test (`map_remote_single_auto`, `map_remote_multiple_choose`, `map_remote_missing_pending`, `map_git_without_remote_pending`, `map_home_non_git_auto_rewrite` (`/Users/src/Desktop/notes` → `<dst_home>/Desktop/notes`), `map_identical_abs_path_auto`, `map_outside_home_pending` (`/private/tmp/x`)), `rewrite_home_boundary` (`/Users/alice2/x` `src_home=/Users/alice` için değişmez; `"bash /Users/alice/.claude/h.sh"` değişir), `pending_put_list_take_roundtrip`.
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-core pathmap --quiet`
- [ ] **Step 3: Uygula.** Bekleme alanı düzeni: `<vt_home>/pending/<blake3(id) ilk 16 hex>/pending.json` + `blobs/<entry_index>`.
- [ ] **Step 4: PASS.**
- [ ] **Step 5: Commit (onaylı).** `feat: yol eşleme ve bekleme alanı`

### Task 8: Claude dönüşümleri

**Files:**
- Create: `crates/vt-claude/src/transform.rs`, `crates/vt-claude/src/merge.rs`
- Test: `crates/vt-claude/tests/transform.rs`

**Interfaces:**
- Consumes: `claude_slug`, `rewrite_home`, `Mapping`, `CLAUDE_REWRITE_JSONL_CWD` (Task 1 kararı, `transform.rs` içinde `const`).
- Produces:
  - `pub fn target_slug_dir(old_slug_dir_cwds: &[String], mapping: &BTreeMap<String, PathBuf>) -> Option<String>` (eski klasördeki cwd'lerin eşlendiği hedef yolun slug'ı; cwd'ler NFC'ye çevrilerek eşlenir, böylece NFC/NFD varyant klasörleri tek hedefte birleşir)
  - `pub fn memory_slug_for(git_root: &Path) -> String`
  - `pub fn rewrite_json_paths(v: &mut serde_json::Value, src_home: &str, dst_home: &str, project_map: &BTreeMap<String, PathBuf>) -> u32` (tüm string değerlerde ve `projects`/`githubRepoPaths` anahtarlarında; önce proje eşlemesi, sonra ana dizin; değiştirilen sayıyı döner). Uygulandığı dosyalar: `~/.claude.json`, `~/.claude/settings.json`, `~/.claude/settings.local.json`, `plugins/installed_plugins.json`, `plugins/known_marketplaces.json`, proje içi `.claude/settings*.json` ve `.mcp.json`. `hooks/`, `scripts/`, `bin/` içindeki metin dosyalarına `rewrite_home` uygulanır (ikili dosyalar ve 1 MiB üstü dokunulmaz); her dönüşüm `TransformRecord` olarak raporlanır.
  - `pub fn merge_claude_json(target: &mut Value, source: &Value, take_oauth_account: bool) -> Vec<JsonConflict>` (`JsonConflict` Task 3; yalnız `projects`, `githubRepoPaths`, `mcpServers`, `take_oauth_account` ise `oauthAccount`; aynı anahtar farklı değer çatışma olarak döner, otomatik çözülmez)
  - `pub fn merge_memory_md(target: &str, source: &str) -> String` (hedef satırları sırasıyla, sonra hedefte birebir bulunmayan kaynak satırları)
  - `pub fn rewrite_jsonl_cwd(line: &str, map: &dyn Fn(&str) -> Option<String>) -> Option<String>` (yalnız `CLAUDE_REWRITE_JSONL_CWD` true ise kullanılır)

- [ ] **Step 1: Failing testler:** `slug_rename_merges_nfc_nfd_variants` (iki eski klasör, cwd'leri `/Users/src/Kişisel` NFC ve NFD; hedef tek slug), `memory_slug_uses_git_root`, `rewrite_json_paths_settings_hooks` (`hooks` içindeki `"bash /Users/src/.claude/hooks/a.sh"` → `/Users/dst/...`), `merge_claude_json_keeps_target_identity` (`userID`, `machineID` hedeften; `projects` birleşik; çatışma listesi 1), `merge_memory_md_dedup`, `rewrite_jsonl_cwd_only_cwd_field` (diğer alanlar bayt bayt aynı).
- [ ] **Step 2: FAIL gör.**
- [ ] **Step 3: Uygula.**
- [ ] **Step 4: PASS.** `cargo test -p vt-claude --quiet`
- [ ] **Step 5: Commit (onaylı).** `feat: claude yol dönüşümleri ve birleştirme`

### Task 9: Uygulama motoru: staging, yedek, journal, rollback

**Files:**
- Create: `crates/vt-core/src/apply/mod.rs`, `crates/vt-core/src/apply/journal.rs`, `crates/vt-core/src/apply/conflict.rs`
- Test: `crates/vt-core/tests/apply.rs`

**Interfaces:**
- Produces:
  - `pub enum Decision { Merge, Replace }`
  - `pub enum Resolution { KeepSource, KeepTarget, KeepBoth }`
  - `pub trait ConflictResolver { fn file(&mut self, dest: &Path) -> Resolution; fn json(&mut self, c: &JsonConflict) -> Resolution; }` (`KeepBoth` json'da geçersiz, `KeepTarget` gibi davranır ve rapora yazılır)
  - `pub struct Placement { pub entry_index: u32, pub blob: PathBuf, pub dest: PathBuf, pub kind: EntryKind, pub mtime_ns: i64, pub mode: u32, pub expected_hash: Option<[u8; 32]> }`
  - `pub struct ReplaceScope { pub dir: PathBuf, pub keep: BTreeSet<PathBuf> }` (Replace kararında dizindeki `keep` dışı hedef dosyaları yedeğe taşınır)
  - `pub struct ApplyPlan { pub session_id: String, pub placements: Vec<Placement>, pub replace_scopes: Vec<ReplaceScope>, pub json_writes: Vec<(PathBuf, Value)> }`
  - `pub struct ApplyReport { pub placed: u32, pub skipped_identical: u32, pub kept_both: Vec<PathBuf>, pub backed_up: Vec<PathBuf>, pub conflicts: Vec<String> }`
  - `pub fn apply(plan: &ApplyPlan, vt_home: &Path, resolver: &mut dyn ConflictResolver) -> Result<ApplyReport>`
  - `pub fn rollback(vt_home: &Path, session_id: &str) -> Result<RollbackReport>`

Kurallar:
- Aynı hedef yolda aynı hash → atla. Farklı içerik → `resolver.file`; `KeepBoth` kaynağı `<ad>.from-<kaynak-hostname><.uzantı>` olarak yazar.
- Her yazımdan önce journal satırı (`<vt_home>/journal/<session>.jsonl`, her satırdan sonra `fsync`): `{"op":"backup","dest":..,"backup":..}`, `{"op":"place","dest":..,"backup":null}`, `{"op":"mkdir","dest":..}`.
- Yerleştirme: blob hedefle aynı dosya sisteminde geçici adla kopyalanır (`.vt-tmp-<uuid>`), mode + mtime ayarlanır (`filetime`), sonra `rename`.
- Rollback journal'ı tersten oynatır: `place` → hedefi sil, `backup` → yedeği geri taşı, `mkdir` → boşsa sil. İdempotent.

- [ ] **Step 1: Failing testler:** `apply_places_and_preserves_mtime_mode`, `identical_file_skipped`, `keep_both_writes_suffix`, `replace_scope_moves_extra_target_files_to_backup` (silinmez: yedekte var), `rollback_after_partial_apply_restores_exact_tree` (resolver 3. dosyada panic eder; `rollback` sonrası hedef ağacının (rel, hash, mtime) listesi apply öncesiyle birebir aynı), `rollback_is_idempotent`.
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-core apply --quiet`
- [ ] **Step 3: Uygula.**
- [ ] **Step 4: PASS.**
- [ ] **Step 5: Commit (onaylı).** `feat: uygulama motoru, journal ve rollback`

### Task 10: `scan` ve `export` komutları

**Files:**
- Create: `crates/vt-core/src/selection.rs`, `crates/vt-core/src/procs.rs`, `crates/vt-cli/src/main.rs`, `crates/vt-cli/src/cmd/scan.rs`, `crates/vt-cli/src/cmd/export.rs`, `crates/vt-cli/src/ui.rs`
- Test: `crates/vt-cli/tests/export.rs`

**Interfaces:**
- Consumes: `ClaudeAdapter::inventory`, `BundleWriter`, `KeyringStore`/`MemStore`, `claude_service_name`.
- Produces:
  - `Selection` (Task 3) kurucuları: `Selection::default_for(tools)` (Cache hariç hepsi), `Selection::from_flags(tools: &[Tool], only: &[Category], exclude: &[Category])`
  - `pub fn running(tool: Tool) -> Vec<u32>` (sysinfo; Claude için süreç adı `claude`)
  - CLI: `vibe-transfer scan [--tool claude] [--json]`, `vibe-transfer export --out <dosya> [--tool claude] [--only <kat,..>] [--exclude <kat,..>] [--include-optional] [--passphrase-stdin] [--json]`
  - Outbox: `<vt_home>/outbox/<session>/meta.json` `{ session_id, created_at, secrets: [SecretMeta], out_path }`
  - Test kancası: `VT_TEST_HOME` ortam değişkeni `VtEnv`'i tamamen o köke yönlendirir ve `MemStore` kullanılır (`VT_TEST_SECRETS=<json dosyası>` ile önceden doldurulur). Release build'de de çalışır ama `scan` çıktısında "test modu" uyarısı basılır.

Akış: seçim (bayrak yoksa `dialoguer` ile araç, sonra kategori çoklu seçim) → `running(Claude)` doluysa uyarı ve onay → envanter → jsonl'ler son `\n`'ye kadar okunur, kalan kısım atılıp `partial_tail = true` → `~/.claude.json` `read_claude_json_stable` → sır okunur (Secrets seçiliyse) → yazım (`indicatif` ilerleme) → parola ekranda gösterilir, "parolayı dosyayla aynı kanaldan gönderme" uyarısı → outbox meta. `scan` aynı envanteri değer göstermeden özetler ve düz metin sır desenleri (`ghp_`, `github_pat_`, `xox[bp]-`, `sk-ant-`, `-----BEGIN .* PRIVATE KEY-----`) bulunan dosyaların yalnız yollarını ve sayısını listeler.

- [ ] **Step 1: Failing testler (assert_cmd, `VT_TEST_HOME` = fixture kopyası):** `export_creates_bundle_and_outbox`, `export_respects_exclude_sessions`, `export_truncates_partial_jsonl_tail` (fixture jsonl'ü yarım satırla biter; paketteki blob son `\n`'de biter, manifest `partial_tail: true`), `scan_lists_secret_patterns_without_values` (stdout sır değerini içermez).
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-cli --quiet`
- [ ] **Step 3: Uygula.**
- [ ] **Step 4: PASS.**
- [ ] **Step 5: Commit (onaylı).** `feat: scan ve export komutları`

### Task 11: `import` komutu

**Files:**
- Create: `crates/vt-cli/src/cmd/import.rs`, `crates/vt-claude/src/plan.rs`, `crates/vt-core/src/report.rs`
- Test: `crates/vt-cli/tests/import.rs`

**Interfaces:**
- Consumes: `read_bundle`, `build_target_index`, `map_project`, `PendingStore`, Claude dönüşümleri (Task 8), `apply`, `SecretStore`.
- Produces:
  - `pub fn build_claude_apply_plan(rb: &ReadBundle, env: &VtEnv, mappings: &BTreeMap<ProjectId, Mapping>, decision: Decision, take_oauth: bool) -> Result<(ApplyPlan, Vec<PendingItem>)>`
  - `pub struct VerifyReport { pub session_id: String, pub expected: u32, pub verified: u32, pub transformed: Vec<TransformRecord>, pub pending: Vec<ProjectId>, pub skipped: Vec<(String, String)>, pub problems: Vec<String> }`; rapor `<vt_home>/reports/<session>.json`. `TransformRecord { pub dest: PathBuf, pub before: [u8; 32], pub after: [u8; 32], pub kind: String }` `vt-core/src/report.rs` içinde tanımlanır; Task 8 dönüşüm fonksiyonları bunu üretmez, `build_claude_apply_plan` dönüşümü uygularken önce/sonra hash'le kaydeder.
  - CLI: `vibe-transfer import <dosya> [--roots <yol,..>] [--passphrase-stdin] [--json]`

Akış:
1. Parola → `read_bundle` → `<vt_home>/staging/<session>/`.
2. Hedef indeksi (`--roots` yoksa `~`), eşleme; `Choose` için kullanıcıya seçim.
3. Hedefte Claude verisi varsa: "Claude için birleştir mi değiştir mi?" (`Decision`); proje içi dosyalarda hedefte farklı içerik varsa proje başına aynı soru. Hiçbir bayrak bu soruları atlatmaz.
4. Hesap kontrolü: hedef `~/.claude.json` `oauthAccount.accountUuid` mevcut ve paketteki `SecretMeta.account_uuid`'den farklıysa sor: "Hedef hesabı koru" / "Kaynak hesabıyla değiştir". Koru → Claude giriş token'ı ve `oauthAccount` aktarılmaz.
5. `running(Claude)` hedefte doluysa kapatılması beklenir.
6. `apply`; bekleme alanına düşenler `PendingStore::put`.
7. Sır: `KeyringStore.set(claude_service_name(..), env.user, value)`; önceki hedef değeri varsa önce `<vt_home>/backups/<session>/secrets.age` dosyasına aynı parolayla şifreli yedeklenir.
8. Doğrulama: yerleştirilen her dosya yeniden hash'lenir (dönüşümsüzler `expected_hash`'e, dönüşümlüler `TransformRecord.after`'a eşit olmalı); Claude kontrolü: hedefteki `projects/*/*.jsonl` sayısı ≥ önceki sayı + aktarılan − bekleyen.
9. Hedef `settings.json`'da `cleanupPeriodDays` yoksa veya en eski aktarılan oturumun yaşından küçükse uyarı ve önerilen değer; yazma yalnız kullanıcı onayıyla.
10. Staging silinir. Özet ekrana; token taşındıysa kaynakta çalıştırılacak komut gösterilir: `vibe-transfer logout-source --session <id>`.

- [ ] **Step 1: Failing testler (iki ayrı `VT_TEST_HOME`: kaynak kullanıcı `src`, hedef kullanıcı `dst`, farklı proje yolları):** `import_into_empty_target_full_roundtrip` (hedefteki tüm dosyalar beklenen hash'lerde, slug klasörleri `dst` yollarına göre, `~/.claude.json` `projects` anahtarları yeni yollar, `MemStore`'da sır `dst` hesabıyla, rapor `problems` boş), `import_merge_keeps_existing_target_sessions`, `import_replace_backs_up_target_only_files`, `import_prompts_on_different_target_account` (test resolver "Hedef hesabı koru" seçer; hedef sır değişmez, `oauthAccount` değişmez), `import_unmatched_project_goes_pending`, `import_warns_cleanup_period`.
  Etkileşimli sorular testte `VT_TEST_ANSWERS=<json>` ile beslenir (`ui.rs` içinde tek giriş noktası).
- [ ] **Step 2: FAIL gör.** `cargo test -p vt-cli import --quiet`
- [ ] **Step 3: Uygula.**
- [ ] **Step 4: PASS.**
- [ ] **Step 5: Commit (onaylı).** `feat: import komutu`

### Task 12: `pending`, `link`, `verify`, `rollback`, `logout-source`

**Files:**
- Create: `crates/vt-cli/src/cmd/{pending,link,verify,rollback,logout_source}.rs`
- Test: `crates/vt-cli/tests/lifecycle.rs`

**Interfaces:**
- Consumes: `PendingStore`, `DestTemplate`, `apply`, `rollback`, `VerifyReport`, outbox meta, `SecretStore`.
- Produces CLI:
  - `vibe-transfer pending [--json]`: id, eski yol, öğe sayısı, boyut.
  - `vibe-transfer link <proje-id|remote|eski-yol> <yol>`: `take` → `DestTemplate`'ler yeni köke göre çözülür → aynı birleştir/değiştir kuralıyla `apply` (yeni oturum id'si, journal'lı) → doğrulama. `vibe-transfer link --auto`: hedef indeksini yeniden kurar, `Auto` eşlenen tüm bekleyenleri bağlar.
  - `vibe-transfer verify [<session>]`: rapordaki her dosyayı yeniden hash'ler, sapmaları listeler; çıkış kodu sapma varsa 1.
  - `vibe-transfer rollback [<session>]`: varsayılan son oturum; onay sorar.
  - `vibe-transfer logout-source --session <id>`: kaynakta çalışır; outbox meta'daki Claude sırrını okur, `<vt_home>/backups/logout-<id>/secrets.age` (parola sorulur) dosyasına yedekler, sonra Keychain'den siler; `claude` çalışıyorsa önce kapatılmasını ister.

- [ ] **Step 1: Failing testler:** `link_moves_pending_into_cloned_project` (import sonrası hedefte proje klasörü oluşturulup `.git/config` yazılır; `link --auto` proje içi dosyaları ve slug klasörünü yerleştirir, `pending` boş), `verify_detects_modified_file` (çıkış 1), `rollback_restores_pre_import_state`, `logout_source_backs_up_then_deletes_secret` (MemStore'da sır yok; yedek dosyası aynı parolayla açılınca değer eşit).
- [ ] **Step 2: FAIL gör.**
- [ ] **Step 3: Uygula.**
- [ ] **Step 4: PASS.** `cargo test --workspace --quiet`
- [ ] **Step 5: Commit (onaylı).** `feat: pending, link, verify, rollback, logout-source`

### Task 13: CI, test container'ı, README ve kabul kontrol listesi

**Files:**
- Create: `Dockerfile.test`, `.github/workflows/ci.yml`, `README.md`, `docs/acceptance.md`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: tüm workspace.

- [ ] **Step 1: `Dockerfile.test`:** `rust:1.98.1` tabanlı; `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace --quiet`. Doğrula: `docker build -f Dockerfile.test .` başarılı (Docker çalışmıyorsa bu adım atlanır ve raporda söylenir).
- [ ] **Step 2: `ci.yml`:** iki iş: `ubuntu-latest` (Dockerfile.test) ve `macos-latest` (`cargo test --workspace`, Keychain testleri `VT_KEYCHAIN_TEST=1` ile). Ağ erişimi yalnız bağımlılık indirme için.
- [ ] **Step 3: README (Türkçe + İngilizce bölüm):** ne yapar, güven modeli (sunucu yok), kurulum (`cargo install --git`, imzasız binary için `xattr -d com.apple.quarantine`), komutlar, sır politikası "taşı" ve `logout-source`, bilinen sınırlar (Faz 1: yalnız Claude ve paket dosyası). Slogan yok.
- [ ] **Step 4: `docs/acceptance.md`:** iki gerçek Mac (veya iki macOS kullanıcı hesabı) ile elle kabul: `claude --resume` listesindeki oturum sayısı kaynakla eşit; rastgele 3 oturum devam ettirilebiliyor; bir projede auto memory görünüyor; `/rewind` bir oturumda çalışıyor (Task 1 notu); `vibe-transfer verify` çıkış 0; bir bekleyen proje clone edilip `link --auto` ile bağlanıyor; `logout-source` sonrası kaynakta Claude giriş istiyor, hedefte çalışıyor.
- [ ] **Step 5: `docs/research/` için `.gitignore` kararı kullanıcıya sorulur** (makineye özgü veri; public öncesi temizlenecek). Varsayılan: commit edilmez.
- [ ] **Step 6: Tüm testler.** `cargo test --workspace --quiet` → PASS; `cargo clippy --workspace -- -D warnings` → temiz.
- [ ] **Step 7: Commit (onaylı).** `chore: ci, test container, readme ve kabul listesi`
