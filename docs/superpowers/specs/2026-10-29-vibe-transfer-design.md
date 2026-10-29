# vibe-transfer: Tasarım

Tarih: 2026-10-29
Durum: taslak, kullanıcı onayı bekliyor
Araştırma: `docs/research/research-claude.md`, `docs/research/research-codex.md`, `docs/research/research-transport.md` (ayrıntılı envanter, yol alanları, kaynak URL'ler; bu doküman onlara referans verir)

Not: "(CTO kararı)" ile işaretli maddeler kullanıcının söylediği değil, tasarımın önerdiği varsayılandır; kullanıcı değiştirebilir.

## 1. Amaç ve neden

vibe-transfer, bir bilgisayardaki seçilen AI kodlama aracının yerel "hafızasını" ve aktarılabilir her şeyini (global ayarlar, ajanlar, skill'ler, hook'lar, MCP ayarları, oturum geçmişi, auto memory, sırlar ve proje klasörleri içindeki AI dosyaları) başka bir bilgisayara **sıfır veri kaybıyla** aktaran açık kaynak bir araçtır. Kullanıcı makine değiştirdiğinde ajan kurulumunu ve birikmiş hafızayı elle toplamak zorunda kalmamalı, hiçbir şey sessizce kaybolmamalı.

Güven ilkesi: **iki makine arasına hiçbir sunucu girmez.** Relay, eşleşme (rendezvous) sunucusu, STUN, bulut depolama yok. Veri yalnız kaynak ile hedef arasında, ya doğrudan ağ bağlantısıyla ya da kullanıcının kendi taşıdığı şifreli paket dosyasıyla gider.

## 2. Hedefler, hedef dışı, anti-hedefler

### Hedefler
- Claude Code ve Codex (CLI + masaüstü uygulamasının diskteki verisi) için tam aktarım. Kapsam sonra başka araçlara genişler; mimari araç başına adaptörle genişlemeye açık olur.
- Platform sırası: 1) macOS → macOS, 2) macOS → Windows, 3) Windows → Windows, 4) Windows → macOS. İlk sürüm yalnız 1'i teslim eder; veri modeli ve adaptör arayüzü baştan çapraz platformu taşıyacak şekilde tasarlanır.
- Mutlak yolların hedefte yeniden oluşturulması ve araçlara tanıtılması (kullanıcı adı, ana dizin ve proje konumları farklı olabilir).
- Sırların aktarılması (Claude OAuth, Codex auth, MCP OAuth, config içine gömülü anahtarlar).
- Proje içi AI dosyalarının tamamının aktarılması.
- İki aktarım modu, kullanıcı seçer: **aynı ağ** ve **farklı ağ**. Her ikisi de doğrudan bağlantı.
- Doğrudan bağlantı kurulamazsa **şifreli paket dosyası** yedeği.
- Kanıtlanabilir bütünlük: aktarım sonunda doğrulama raporu.

### Hedef dışı
- **Proje kodunu taşımak.** Araç kod klasörü kopyalamaz, clone etmez. Yalnız AI verisi.
- Sürekli senkronizasyon (iki makineyi canlı eşit tutmak). Tek seferlik aktarım (devam ettirilebilir).
- Bulut yedekleme hizmeti.
- İlk sürümde GUI. CLI, ileride bir GUI'nin kullanabileceği `--json` olay akışı sunar (CTO kararı).

### Anti-hedefler (teknik olarak çalışsa bile başarısızlık sayılır)
- Herhangi bir verinin sessizce düşmesi: atlanan, dönüştürülen veya beklemeye alınan her öğe raporda görünür.
- Hedefte önceden var olan verinin kullanıcıya sorulmadan ezilmesi.
- Sırların herhangi bir aşamada diske düz metin yazılması (staging dahil) veya ağda şifresiz gitmesi.
- Üçüncü bir sunucuya, telemetriye veya dış servise tek bayt gönderilmesi.
- Hedefte Claude/Codex'in veriyi göremediği bir "kopyalandı ama çalışmıyor" durumu (ör. `--resume` listesinin boş görünmesi, Codex thread'lerinin listelenmemesi).
- Aktarım sonrası Claude'un 30 günlük temizliğinin eski oturumları silmesi (mtime kaybı yüzünden).

## 3. Kısıtlar

- Araya sunucu girmez (kullanıcının kesin kuralı).
- Dil: **Rust**, tek binary CLI (kullanıcı seçimi, yaklaşım A).
- Hedefte AI verisi varsa araç her seferinde sorar: **birleştir** veya **değiştir**. Değiştir seçilirse hedefteki etkilenecek veri önce yedeklenir.
- Hedefte bulunmayan projelerin AI verisi kayıpsız **bekleme alanında** tutulur; proje sonradan geldiğinde `vibe-transfer link` komutuyla bağlanır. Araç proje klasörünü önceden oluşturmaz.
- Test süiti kullanıcının gerçek `~/.claude` / `~/.codex` dizinlerine dokunmaz; her test sentetik, izole bir ana dizin (`HOME` / `CLAUDE_CONFIG_DIR` / `CODEX_HOME` yönlendirmesi) kullanır.

## 4. Yaklaşımlar (değerlendirilen)

| | Yaklaşım | Artı | Eksi | Sonuç |
|---|---|---|---|---|
| A | Rust tek binary, doğrudan QUIC, PAKE eşleşme | En sağlam kripto kütüphaneleri (RustCrypto `spake2`, resmi BLAKE3, `age`), `rusqlite` gömülü SQLite ile sürümden bağımsız yedekleme, `keyring` ile Keychain + Credential Manager tek arayüz | Kullanıcının ana stack'i değil, geliştirme daha yavaş | **Seçildi** |
| B | Go tek binary | Hızlı geliştirme, kolay cross-compile, `age` referans uygulaması Go'da | PAKE kütüphaneleri daha az denetlenmiş (croc PAKE geçmişte CVE) | Elendi |
| C | Node/TypeScript | Kullanıcının stack'i | iroh JS paketi geride, PAKE/BLAKE3 kütüphaneleri zayıf, native modüllü tek binary ve Windows imzalama kırılgan | Elendi |

İlk öneri iroh + kendi relay/rendezvous sunucusuydu (hole punching için). Kullanıcının "araya hiçbir sunucu girmez" kuralı nedeniyle reddedildi. Sonuç: farklı ağ modu sunucusuz doğrudan bağlantıya dayanır ve her ağda garanti edilemez (bkz. 6.3); bu durumda paket dosyası yedeği devreye girer.

## 5. Mimari

### 5.1 Katmanlar

```
cli            komutlar, etkileşimli sorular, --json olay akışı
plan           tarama, sınıflandırma, seçim, yol eşleme planı (dry-run çıktısı)
adapters/      araç başına: claude, codex (envanter, tutarlı okuma, yol dönüşümü, sır okuma/yazma)
pathmap        proje kimliği (git remote), eşleme, bekleme alanı, link
secrets        platform sır deposu soyutlaması (macOS Keychain, Windows Credential Manager, dosya)
bundle         paket formatı: manifest, segmentler, zstd, BLAKE3/Merkle, şifreleme
transport      LAN keşfi, doğrudan QUIC, UPnP/NAT-PMP/PCP, PAKE eşleşme + SAS
apply          staging, doğrulama, yedek, journal ile dosya bazında atomik yerleştirme, rollback
```

Her katman tek sorumluluklu bir crate veya modül olur (Cargo workspace önerilir, CTO kararı).

### 5.2 Tek format, iki taşıma yolu

Çekirdek bir **paket formatıdır**. Canlı aktarım bu paketin QUIC üzerinden akışıdır; yedek dosya aynı paketin diske yazılmasıdır. Doğrulama, uygulama ve devam ettirme her iki yolda aynı koddan geçer.

### 5.3 Paket formatı

- **Manifest:** her öğe için platformdan bağımsız göreli mantıksal yol (`/` ayraçlı, NFC normalize), kök türü (`claude-home`, `claude-json`, `codex-home`, `agents-skills`, `project:<proje-id>`), tür (dosya, dizin, symlink), boyut, mtime, POSIX mod, BLAKE3 hash, sınıf (`session-jsonl`, `sqlite-snapshot`, `secret`, `config`, `memory`, ...), uygulanacak dönüşümler.
- **Segmentler:** dosyalar sıralı çerçeve akışında (başlık + içerik), zstd ile sıkıştırılmış, 8-64 MB segmentlere bölünmüş; her segment bağımsız hash'li. Büyük tek dosyalar kendi segment(ler)inde.
- **Bütünlük:** dosya hash'lerinden Merkle kök; manifest hash'i ve kök, oturum anahtarıyla MAC'lenir.
- **Sır bölümü:** sırlar ayrı bir bölümde, ek bir AEAD katmanıyla şifreli; hedefte yalnız bellekte açılır, doğrudan sır deposuna yazılır.
- **Paket dosyası şifrelemesi:** dosyanın tamamı parola tabanlı şifrelenir (`age` formatı, scrypt alıcısı; CTO kararı, olgun ve denetlenmiş format). Parola araç tarafından güçlü üretilir ve ekranda gösterilir, ya da kullanıcı kendi parolasını verir.

### 5.4 Aktarım akışı (canlı)

1. Hedef: `vibe-transfer receive` dinlemeye başlar, kısa bir kod (ör. 3 kelime + sayı) ve bağlantı bilgisini gösterir.
2. Kaynak: `vibe-transfer send` aracı ve kapsamı seçtirir, hedefi bulur (6. bölüm), kodu ister.
3. PAKE (SPAKE2) ile ortak anahtar; QUIC kanalı bu anahtara bağlanır (kanal bağlama). Her iki ekranda aynı kısa onay kodu (SAS) gösterilir, kullanıcı eşleştiğini onaylar.
4. Hedef kendi durumunu bildirir: platform, kullanıcı adı, ana dizin, mevcut AI verisinin özeti, hedefteki git depolarının remote listesi (proje eşlemesi için; tarama kökleri kullanıcıdan sorulur, varsayılan ana dizin altı sınırlı derinlik, CTO kararı).
5. Kaynak planı çıkarır: ne gidecek, ne dönüşecek, hangi proje nereye eşlendi, ne bekleme alanına düşecek, hedefte çakışan ne var. Plan iki ekranda gösterilir; hedefte **birleştir / değiştir** sorusu burada sorulur.
6. Aktarım: segmentler 2-4 paralel QUIC akışıyla; hedef her segmenti doğrulayıp staging'e yazar. Kesilirse yeniden bağlanınca yalnız eksik segmentler.
7. Son delta turu: kaynakta aktarım sırasında büyüyen append-only JSONL'lerin yalnız yeni kuyruğu gönderilir. Kullanıcıya bu turdan önce kaynakta Claude/Codex'i kapatması önerilir.
8. Uygulama (5.7), ardından doğrulama raporu.

### 5.5 Tutarlı okuma (kaynak)

- **SQLite (Codex'in altı DB'si + masaüstü `codex-dev.db`):** asla dosya kopyası değil. `rusqlite` Online Backup API ile anlık görüntü, `integrity_check`, `journal_mode=DELETE`. `-wal` dosyası olmayan DB'ler `immutable=1` ile ve yalnız Codex kapalıyken. Codex açıkken araç uyarır ve kapatılmasını ister (araştırma: research-codex.md 3).
- **JSONL (Claude transcript, Codex rollout, history):** append-only; manifest anındaki boyut ve hash kaydedilir, yarım kalmış son satır bir sonraki delta turunda tamamlanır.
- **`~/.claude.json`:** atomik yeniden yazılan dosya; okunup JSON olarak ayrıştırılır, geçerli değilse yeniden okunur.
- Kaynakta hiçbir şey değiştirilmez (salt okuma); tek istisna "taşı" politikasıyla hedefte doğrulamadan sonra kaynakta oturum kapatmadır (bkz. 12.1).

### 5.6 Adaptörler

Her adaptör şunları uygular: `discover` (veri nerede, sürüm), `inventory` (öğe + sınıf: taşınır / isteğe bağlı / asla), `snapshot` (tutarlı okuma), `project_refs` (hangi öğe hangi projeye ait, eski yol), `transform` (hedef yollara göre yeniden yazım), `secrets_read` / `secrets_write`, `post_apply_checks`.

**Claude Code** (ayrıntı: research-claude.md 1, 2, 3, 6):
- Sınıflar research-claude.md 6'daki a/b/c listesine göre. "Asla" sınıfı (`sessions/`, `daemon*`, `ide/`, lock'lar, `.device-keys.json`, `~/.claude.json` kimlik anahtarları vb.) hiçbir zaman taşınmaz.
- Proje klasör adı: `path.replace(/[^a-zA-Z0-9]/g,"-")`, 200 karakter üstünde `slice(0,200)+"-"+base36(abs(javaHash(path)))`. Kayıplı olduğu için gerçek eski yol jsonl `cwd` alanından okunur, yeni ad hedef yoldan hesaplanır. NFC/NFD farkı nedeniyle iki eski klasör aynı yeni ada düşerse dosyalar birleştirilir (çakışan dosya adı UUID olduğundan çakışma beklenmez; olursa ikisi de korunur).
- Auto memory git köküne bağlı: `projects/<enc(git kökü)>/memory/`. Hedefte aynı klasör varsa `MEMORY.md` birleştirilir (satır bazında, tekrarlar atılır).
- `~/.claude.json`: üzerine yazılmaz, birleştirilir. Taşınan anahtarlar: `projects` (yollar yeniden yazılır), `githubRepoPaths`, `mcpServers`; `userID`, `machineID` ve önbellekler hedefte kalır. `oauthAccount` giriş token'ına aittir: token taşınırsa onunla birlikte taşınır, hedef kendi hesabını korursa hedefte kalır (plan sırasında eklendi, CTO kararı).
- mtime korunur. Hedefte `cleanupPeriodDays` yoksa araç uyarır ve en eski aktarılan oturumu koruyacak bir değer önerir (yazmak kullanıcı onayıyla).
- Plugin kayıtları (`installed_plugins.json`, `known_marketplaces.json`) yolları yeniden yazılır; plugin cache Mac→Mac aynı mimaride isteğe bağlı, Windows'a hiçbir zaman.
- `~/.claude/skills` içindeki `~/.agents/skills` symlink'leri: hedef `~/.agents/skills` de taşınır; Windows'ta symlink çözülüp içerik kopyalanır.
- Sır: macOS Keychain generic password, servis `Claude Code-credentials` (`CLAUDE_CONFIG_DIR` varsa `-<sha256(dir)[:8]>` sonekli), hesap = hedefteki `$USER`. Windows'ta `%USERPROFILE%\.claude\.credentials.json`.
- Transcript içindeki `cwd` yeniden yazımının `hash`/`requestDigest` alanlarına ve `/rewind`'a etkisi doğrulanmadı: uygulamadan önce spike ile test edilir; etkiliyorsa yalnız klasör adı değişir, satır içerikleri dokunulmaz.

**Codex** (ayrıntı: research-codex.md 1, 2, 3, 4, 6):
- Kalıcı veri yaklaşık 0,9 GB; plugins, packages, `.tmp`, logs yeniden üretilebilir veya makineye özgü.
- DB'de yeniden yazılacak yapısal alanlar: `state_5.threads.rollout_path` (yazılmazsa thread listeden düşer), `cwd`, `project_roots.path` ve research-codex.md 2.1'deki diğer kolonlar.
- **Rollout jsonl dosyalarına dokunulmaz** (`thread_history_1` bayt ofsetleri tutuyor).
- `config.toml`: `[projects."<yol>"]` trust anahtarları, `notify`, marketplace ve MCP komut yolları yeniden yazılır. Dosya gömülü sır içerebildiği için `secret` sınıfında taşınır.
- Hook onayları: `hooks.state` anahtarı yeni `hooks.json` yoluyla yazılır, `trusted_hash` yeniden hesaplanır; hesaplanamazsa kullanıcıya `/hooks` ile yeniden onay gerektiği raporlanır.
- Sürüm: hedef Codex sürümü kaynağınkinden eskiyse araç uyarır ve önce güncellemeyi ister (eski sürüm "ileride DB" ile çalışsa da risk alınmaz, CTO kararı).
- Sır: `auth.json` (dosya modu) veya keyring (hesap anahtarı `cli|sha256(CODEX_HOME)[:16]`, hedef yola göre yeniden hesaplanır). MCP OAuth: "Codex MCP Credentials" keyring veya `CODEX_HOME/.credentials.json`.

**Proje içi AI dosyaları** (her iki araç): `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md`, `AGENTS.override.md`, `.claude/` (settings, settings.local, agents, commands, skills), `.codex/`, `.mcp.json`. Git'te izlenen dosyalar da taşınır ama hedefte aynı içerik zaten varsa (clone ile gelmişse) atlanır; farklıysa birleştir/değiştir kuralı uygulanır. `.claude/worktrees/` (kod) taşınmaz.

### 5.7 Uygulama (hedef)

1. Staging: `~/.vibe-transfer/staging/<oturum-id>/` (sırlar hariç; sırlar diske düz yazılmaz).
2. Her segment ve dosya BLAKE3 ile doğrulanır; sonunda Merkle kök karşılaştırılır.
3. Hedefte Claude/Codex süreçleri çalışıyorsa uyarır ve bekler.
4. Birleştir/değiştir kararına göre etkilenecek mevcut dosyalar `~/.vibe-transfer/backups/<zaman>/` altına manifest'iyle taşınır.
5. Dosya bazında atomik yerleştirme (aynı dosya sisteminde `rename`; Windows'ta `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`), her adım journal'a yazılır.
6. Hata: journal'dan geri alma. `vibe-transfer rollback [<oturum>]` komutu her zaman son uygulamayı geri alabilir.
7. Sırlar hedef sır deposuna yazılır.
8. Doğrulama: hedef yeniden taranır, beklenen hash'lerle karşılaştırılır; dönüşüm uygulanan dosyalar için "önce/sonra hash + dönüşüm türü" kaydı. Adaptör `post_apply_checks` (ör. Codex thread sayısı, Claude proje klasörü sayısı) çalışır. Rapor `~/.vibe-transfer/reports/<oturum>.json` ve ekranda özet.

### 5.8 Birleştir / değiştir

- Karar araç başına verilir (Claude ve Codex için ayrı sorulur); proje içi dosyalarda proje başına sorulur (CTO kararı).
- **Değiştir:** aracın kapsamındaki hedef, kaynağın birebir aynası olur. Çakışan hedef öğeler ve kaynakta olmayan hedef öğeler silinmez, `~/.vibe-transfer/backups/<zaman>/` altına taşınır; rollback ile tamamı geri gelir (CTO kararı).
- **SQLite birleştirme (Codex):** sıfır kayıp ancak satır düzeyinde birleştirmeyle sağlanır; hedef DB'yi yedekleyip rollout'lardan yeniden kurdurmak pin, isim ve okundu bilgisini kaybettirdiği için reddedildi. Kural: kaynak ve hedef `_sqlx_migrations` sürümü aynı olmalı; farklıysa araç birleştirmeyi başlatmaz ve iki tarafta Codex'in aynı sürüme güncellenmesini ister. Tablo bazında birleştirme anahtarları (thread id, memory id vb.) ve otomatik artan ID'lerin yeniden numaralanması adaptörde tablo tablo tanımlanır; `thread_history_1` projeksiyonu, birleştirilen thread'ler için silinip Codex'in rollout'tan yeniden kurmasına bırakılır (bu tablo türetilmiş veridir, kayıp sayılmaz; spike ile doğrulanır). Bekleme alanındaki projelerin DB satırları `link` anında aynı mekanizmayla eklenir (CTO kararı).
- **Birleştir:** iki taraf da korunur. Kural: farklı adlı dosyalar birlikte durur; aynı ad ve aynı içerik atlanır; aynı ad farklı içerik için JSONL oturumlar (UUID adlı) ikisi de korunur, yapılandırılmış dosyalar (`~/.claude.json`, `settings.json`, `config.toml`) anahtar bazında birleştirilir ve çatışan anahtar kullanıcıya sorulur, diğer dosyalarda kullanıcıya sorulur (kaynak / hedef / ikisi de, `.from-<kaynak>` sonekiyle). Toplu cevap seçeneği vardır ("hepsine kaynak").

## 6. Ağ

### 6.1 Eşleşme
- Kısa kod + SPAKE2 (RustCrypto `spake2`). Kod tek kullanımlık, sınırlı deneme (yanlış koddan sonra oturum düşer).
- PAKE anahtarı QUIC/TLS oturumuna bağlanır (TLS exporter ile kanal bağlama) ve sır bölümünün AEAD anahtarı türetilir.
- SAS: iki ekranda aynı 6 haneli/kelimeli onay kodu; kullanıcı onaylamadan veri akmaz.

### 6.2 Aynı ağ
- Keşif: mDNS/DNS-SD (`_vibe-transfer._udp`). Bulunamazsa hedefin ekranda gösterdiği IP:port elle girilir.
- Windows (ileriki faz): ağ "Public" profilindeyse Firewall keşfi engeller; araç bunu tespit edip açıklar, elle IP yoluna yönlendirir.

### 6.3 Farklı ağ (sunucusuz)
- Hedef dinlemeye başlarken sırayla dener: UPnP-IGD / NAT-PMP / PCP ile modemde port açma ve dış IP öğrenme; açık IPv6 adresi; kullanıcının elle yaptığı port yönlendirmesi. Kullanıcının kendi VPN'i (ör. WireGuard) varsa bu mod aynı ağ gibi çalışır.
- Hedef ekranda bir **bağlantı bileti** gösterir (IP:port listesi, kısa ve kopyalanabilir). Kullanıcı bunu kendi kanalıyla kaynağa iletir. Kimlik doğrulaması bilet ile değil kısa kod + PAKE ile olduğu için bilet gizli değildir.
- STUN, hole punching koordinasyonu veya relay kullanılmaz. Her iki taraf da CGNAT arkasındaysa bağlantı kurulamaz: araç nedenini açıkça söyler ve paket dosyası yoluna geçmeyi önerir. Açılan UPnP port eşlemesi oturum sonunda kapatılır.

### 6.4 Paket dosyası yedeği
- `vibe-transfer export --out <dosya>` kaynakta, `vibe-transfer import <dosya>` hedefte. Hedef bilgisi olmadan üretildiği için yol eşleme ve proje eşlemesi import sırasında hedefte yapılır (plan aşaması hedefte çalışır).
- Paket parolasız açılamaz; parola dosyayla aynı kanaldan taşınmamalı uyarısı gösterilir.

## 7. Yol eşleme ve bekleme alanı

- **Proje kimliği:** normalize git remote URL (`git@github.com:o/r.git` ve `https://github.com/o/r` aynı kimlik). Remote'u olmayan projeler için kimlik = eski yol; bunlar her zaman bekleme alanına gider (otomatik eşleme yok).
- **Kaynaklar:** Claude `~/.claude.json` `githubRepoPaths` ve `projects`, transcript `cwd`; Codex `threads.git_origin_url`, `session_meta.git.repository_url`, `project_roots.path`, `config.toml` `[projects]`.
- **Remote'suz yollar (CTO kararı):** kaynak ana dizininin kendisi ve altındaki git deposu olmayan yollar (ör. `~` altında açılmış oturumlar, `projects/-Users-<ad>/memory`) ana dizin dönüşümüyle otomatik eşlenir, bekleme alanına gitmez. Hedefte birebir aynı mutlak yol varsa (aynı kullanıcı adı) o da otomatik eşlenir. Bekleme alanı yalnız: remote'lu ama hedefte bulunamayan projeler, ana dizin dışındaki remote'suz yollar (ör. `/private/tmp`, harici diskler) ve hedefte artık var olmayan yollar.
- **Ana dizin dönüşümü:** proje dışı mutlak yollar (hook script'leri, plugin yolları, `~/.agents`) kaynak ana dizininden hedef ana dizinine çevrilir.
- **Eşleme sonucu:** hedefte aynı remote'a sahip tek klasör varsa otomatik; birden fazla varsa kullanıcı seçer; hiç yoksa bekleme alanı.
- **Bekleme alanı:** `~/.vibe-transfer/pending/<proje-id>/` (proje içi AI dosyaları + o projeye ait Claude/Codex oturum ve memory verisi + uygulanacak dönüşümler). Bekleyen projelerin Claude/Codex global kayıtları (ör. `~/.claude.json` `projects` girdisi, Codex DB satırları) da link anına kadar bekletilir.
- **Komutlar:** `vibe-transfer pending` (listele), `vibe-transfer link <proje-id|remote> <yol>` (bağla: dosyaları yerleştir, dönüşümleri uygula, doğrula), `vibe-transfer link --auto` (hedefte yeni clone edilmiş projeleri remote ile bulup bağla).
- Kaynakta artık var olmayan eski yollara ait veri (ör. `/private/tmp`, eski disk) de kaybolmaz: bekleme alanına girer, kullanıcı link edebilir veya arşiv olarak tutabilir.

## 8. Komut yüzeyi (ilk sürüm)

```
vibe-transfer scan                  kaynakta ne var, sınıflar, boyutlar, sır uyarıları (dry-run)
vibe-transfer send [--tool claude|codex|all] [--lan|--wan]
vibe-transfer receive [--lan|--wan]
vibe-transfer export --out <dosya> [--tool ...]
vibe-transfer import <dosya>
vibe-transfer pending
vibe-transfer link <proje> <yol> | --auto
vibe-transfer logout-source --session <id>   (paket modunda kaynakta oturumu kapatır)
vibe-transfer verify [<oturum>]
vibe-transfer rollback [<oturum>]
```

Ortak bayraklar: `--json` (olay akışı), `--include-optional`, `--yes` (yalnız plan onayı için; birleştir/değiştir ve sır politikası sorusu hiçbir bayrakla atlanamaz, CTO kararı).

## 9. Sırlar

- Kapsam: Claude OAuth (`claudeAiOauth`, `mcpOAuth`, `pluginSecrets`), Codex `auth.json` / keyring, Codex MCP OAuth, `config.toml` ve `settings.json` içindeki gömülü anahtarlar (dosyanın kendisiyle, şifreli bölümde).
- "Diske düz metin yazılmaz" kuralının kapsamı (CTO kararı): yalnız `secret` sınıfı öğeler (sır deposu kayıtları, `auth.json`, `.credentials.json`). Bunlar staging'e hiç yazılmaz, bellekte tutulup doğrudan hedef sır deposuna yazılır. Kaynakta zaten düz metin duran sır içerikli dosyalar (transcript, history, file-history, `settings.json`, `config.toml`) ağda ve paket dosyasında şifreli gider, hedefte kaynaktaki haliyle (staging dahil) yazılır; araç bu durumu `scan` raporunda belirtir.
- Devam ettirme hem ağ kopmasını hem iki taraftan birinin yeniden başlamasını kapsar. Yeniden başlamada yeni bir kısa kodla yeniden eşleşilir; oturum, manifest hash'i ile tanınır ve yalnız eksik segmentler istenir. Kaynak tarafta anlık görüntü (SQLite yedekleri, manifest) `~/.vibe-transfer/outbox/<oturum>/` altında oturum bitene kadar tutulur. `secret` sınıfı yeniden başlamadan sonra yeniden gönderilir (CTO kararı).
- `scan` düz metin sır gördüğü yerleri (settings, history, transcript, file-history) değerleri göstermeden raporlar; bu dosyalar da paketin tamamı şifreli olduğu için güvenle taşınır.
- Platform çevirisi: macOS Keychain ↔ Windows dosya / Credential Manager (ileriki faz).
- **Refresh token rotasyonu:** Claude ve Codex refresh token'ları tek kullanımlık/dönüşümlü; aynı token iki makinede kullanılırsa biri oturumdan düşer (`refresh_token_reused`). Bu yüzden politika "taşı"dır (bkz. 12.1).

## 10. Test ve doğrulama

- Birim: yol kodlama (Claude slug, 200+ karakter hash, NFC/NFD), git remote normalizasyonu, manifest/Merkle, segment devam ettirme, birleştirme kuralları, journal/rollback.
- Entegrasyon: sentetik `~/.claude` + `~/.codex` fixture'ları (gerçek yapıyı taklit eden, sır içermeyen), iki süreç arası localhost QUIC aktarımı, kesinti ve devam, farklı kullanıcı adı ve farklı proje yolu senaryoları, hedefte mevcut veriyle birleştir/değiştir.
- Kabul (Mac→Mac): hedefte `claude --resume` listesi ve Codex thread listesi kaynakla aynı sayı; auto memory hedef projede görünür; doğrulama raporu sıfır eksik.
- Gerçek kullanıcı dizinine test koşturulmaz.
- Kabul testi gerçek `claude`/`codex` binary'leriyle iki gerçek makinede (veya iki ayrı macOS kullanıcı hesabında) elle yapılır ve kontrol listesi `docs/acceptance.md`'de tutulur. CI otomatik testleri sentetik fixture'larla koşar: Linux (Docker) ve macOS runner (Keychain adaptörü için) (CTO kararı).

## 11. Dağıtım ve proje

- Repo: GitHub'da public, geliştirme açık yapılır.
- Lisans: MIT OR Apache-2.0 (Rust ekosistem geleneği, CTO kararı).
- Dağıtım: GitHub Releases ile macOS universal binary (imzasız, bkz. 12.3) ve `cargo install`; sonra Windows binary; Homebrew tap sonra.
- Docker: kullanıcı politikası her yeni projeyi dockerize etmektir, ancak bu araç masaüstünde Keychain ve kullanıcı ana dizinine dokunan bir CLI olduğu için container'da çalışmaz. Docker yalnız Linux build/test ortamı (CI ve sentetik fixture testleri) için eklenir (CTO kararı, kullanıcı onayına açık).
- `docs/research/` kullanıcının makinesine özgü bilgiler (yollar, proje adları, sayılar) içerir; repo public olmadan önce temizlenmeli veya çıkarılmalıdır.

## 12. Kullanıcı kararları (2026-10-29)

1. **Sır politikası: taşı.** Giriş token'ları hedefe gider; hedefte çalıştığı doğrulandıktan sonra kaynakta ilgili aracın oturumu kapatılır (kaynaktaki sır kaydı silinmeden önce kaynak yedeğine alınır). Hedef zaten başka bir hesapla girişliyse araç sorar ve hedef hesabı koruma seçeneği sunar (o zaman yalnız giriş token'ı aktarılmaz, MCP/diğer sırlar aktarılır). Paket dosyası modunda kaynak aktarımın bittiğini bilemez: import sonunda araç, kaynakta çalıştırılacak `vibe-transfer logout-source --session <id>` komutunu gösterir; kaynak export sırasında oturum kimliğini kaydeder.
2. **Seçim: araç + alt kategoriler.** Önce araç (Claude, Codex, ikisi), sonra her araç için kategoriler: oturum geçmişi, hafıza (auto memory, memories), ayarlar ve ajanlar (CLAUDE.md/AGENTS.md, settings, agents, skills, hooks, plugins kaydı, MCP), sırlar, proje içi AI dosyaları, isteğe bağlı cache. Varsayılan: isteğe bağlı cache hariç hepsi. CLI'da `--only` / `--exclude` bayraklarıyla da seçilebilir. Bir kategori kapatılırsa raporda "kullanıcı seçimiyle atlandı" olarak görünür.
3. **İmza yok.** Kullanıcının Apple Developer ID hesabı yok; proje GitHub'da açık kaynak. İlk sürüm imzasız yayınlanır; README Gatekeeper (`xattr -d com.apple.quarantine`) ve ilk dinlemede çıkan güvenlik duvarı izni için adım adım talimat verir. Kaynaktan derleme (`cargo install`) birinci yol olarak sunulur (CTO kararı).

## 13. Builder'a bırakılanlar

Crate seçimlerinin kesin sürümleri (yazım anında güncel kararlı sürüm doğrulanır), segment boyutu ve zstd seviyesi ölçümle, kısa kod kelime listesi, `--json` olay şeması, hata mesajı metinleri, workspace iç düzeni.
