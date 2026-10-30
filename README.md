# vibe-transfer

[Türkçe](#türkçe) | [English](#english)

## Türkçe

vibe-transfer, bir AI kodlama aracının hafızasını, ayarlarını ve sırlarını bir makineden diğerine veri kaybı olmadan taşır. Faz 1 yalnız Claude Code verisini ve yalnız şifreli paket dosyası yolunu destekler.

### Ne yapar

- `scan` ile kaynakta neyin taşınacağını gösterir. Hiçbir şey yazmaz; düz metin sır içeren dosyaları değerleri göstermeden listeler.
- `export` ile seçilen verileri tek bir şifreli paket dosyasına yazar: oturum geçmişi, hafıza, ayarlar ve ajanlar, sırlar (Keychain'deki Claude giriş kaydı), proje içi AI dosyaları ve isteğe bağlı önbellek.
- `import` ile paketi hedefe uygular. Projeleri hedefteki klasörleriyle eşler, eşlenemeyenleri bekleme alanına koyar, her yerleştirilen dosyayı doğrular ve bir rapor yazar.
- Hedefte hiçbir şey silinmez: değişen veya kaldırılan her dosya `~/.vibe-transfer/backups/<oturum>/` altına taşınır ve `rollback` ile geri alınabilir.
- Kaynakta hiçbir dosya değiştirilmez. Tek istisna, kullanıcının elle çalıştırdığı `logout-source` komutudur.
- Dosyaların değiştirilme zamanı (mtime) ve POSIX izinleri korunur.

Çalışma verisi `~/.vibe-transfer/` altında tutulur (oturum günlükleri, raporlar, yedekler, bekleme alanı).

### Güven modeli

- Arada sunucu yoktur: aktarım sunucusu, aracı (relay), hesap, telemetri veya güncelleme kontrolü yoktur. Araç ağa çıkmaz. Veri yalnız iki tarafta, kaynak ve hedef makinede bulunur.
- Paket dosyası age ile, bir parolayla şifrelenir (scrypt). `export` varsayılan olarak rastgele bir parola üretir ve yalnız bir kez, terminalde gösterir. `--passphrase-stdin` ile kendi parolanızı verebilirsiniz.
- Paket dosyasını ve parolayı farklı kanallardan gönderin. Örneğin dosyayı bir USB bellekle, parolayı başka bir mesajlaşma kanalıyla iletin.
- Sır sınıfı öğeler (Keychain kaydı) diske hiçbir zaman düz metin yazılmaz; yalnız bellekte ve şifreli paketin içinde bulunur, hedefte doğrudan Keychain'e yazılır.
- Kaynakta zaten düz metin duran ve sır içerebilen dosyalar (oturum kayıtları, geçmiş, `settings.json`) paketin içinde şifreli taşınır ve hedefe kaynaktaki haliyle yazılır. `scan` bu dosyaları listeler.

### Kurulum

Kaynaktan derleme (Rust 1.98.1 veya üstü gerekir):

```sh
cargo install --locked --git https://github.com/denizbarish/vibe-transfer vt-cli
```

Bu, `vibe-transfer` komutunu kurar. Binary imzasızdır. İmzasız bir binary'yi indirip kullanıyorsanız macOS Gatekeeper engelini kaldırmak için:

```sh
xattr -d com.apple.quarantine ./vibe-transfer
```

### Kullanım

Kaynak makinede (örnek kullanıcı `alice`):

```sh
vibe-transfer scan
vibe-transfer export --out ~/Desktop/claude.vtb
```

Komut paket parolasını ve oturum kimliğini yazar. Paketi ve parolayı hedefe ayrı kanallardan iletin.

Hedef makinede (örnek kullanıcı `bob`):

```sh
vibe-transfer import ~/Desktop/claude.vtb --roots /Users/bob/proj
vibe-transfer verify
vibe-transfer pending
vibe-transfer link --auto --roots /Users/bob/proj
```

Giriş kaydı taşındıysa `import`, kaynakta çalıştırılacak komutu yazar. Claude Code'un hedefte çalıştığını doğruladıktan sonra kaynakta çalıştırın:

```sh
vibe-transfer logout-source --session <oturum>
```

### Komutlar

`import`, `link`, `rollback` ve `logout-source` değişiklik yapmadan önce Claude Code'un kapatılmasını bekler. `export`, Claude Code çalışıyorsa uyarır ve terminalde devam için onay ister.

| Komut | Bayraklar | Açıklama |
|---|---|---|
| `scan` | `--tool <araç>`, `--json` | Neyin taşınacağını, proje sayısını ve düz metin sır içeren dosyaları gösterir. Hiçbir şey yazmaz. |
| `export --out <dosya>` | `--tool <araç>`, `--only <kat,..>`, `--exclude <kat,..>`, `--include-optional`, `--passphrase-stdin`, `--json` | Şifreli paketi yazar. Var olan bir dosyanın üzerine yazmaz. Seçim bayrağı yoksa ve bir terminal varsa kategorileri sorar; yoksa önbellek hariç tüm kategorileri alır. |
| `import <dosya>` | `--roots <yol,..>`, `--passphrase-stdin`, `--json` | Paketi uygular. Projeler `--roots` altında aranır (varsayılan: ev dizini). Hedefte Claude verisi varsa birleştirme veya değiştirme sorulur. |
| `pending` | `--orphans`, `--json` | Hedef klasörü bulunamayan projelerin bekleyen verisini listeler. `--orphans` hiçbir kaydın sahiplenmediği dosyaları listeler ve terminalde onayla siler. |
| `link <proje> <yol>` | `--json` | Bekleyen bir projeyi verilen klasöre yerleştirir. `<proje>`: proje kimliği, uzak depo adresi ya da kaynaktaki eski yol. |
| `link --auto` | `--roots <yol,..>`, `--json` | Hedefte klasörü bulunan tüm bekleyen projeleri bağlar. |
| `verify [<oturum>]` | `--json` | Bir oturumda doğrulanan dosyaları yeniden denetler (varsayılan: son rapor). Sapma varsa çıkış kodu 1'dir. |
| `rollback [<oturum>]` | `--passphrase-stdin` | Bir oturumu geri alır (varsayılan: son oturum). Her zaman onay ister; onayı atlayan bayrak yoktur. |
| `logout-source --session <oturum>` | `--passphrase-stdin` | Kaynakta, taşınan Claude giriş kaydını yedekleyip kaldırır. |

Araç (`--tool`): şimdilik yalnız `claude`. Kategoriler (`--only`, `--exclude`): `sessions`, `memory`, `settings`, `secrets`, `project-files`, `cache`.

`--json` özeti komut bitince tek bir JSON belgesi olarak yazar.

### Sırlar: "taşı" politikası ve `logout-source`

Claude Code'un yenileme token'ı tek kullanımlıktır; aynı token iki makinede kullanılırsa bir taraf oturumdan düşer. Bu yüzden politika kopyalamak değil taşımaktır:

1. `export`, Keychain'deki Claude giriş kaydını şifreli paketin içine koyar.
2. `import`, kaydı hedefin Keychain'ine yazar. Hedef başka bir Claude hesabıyla girişliyse hangi hesabın kalacağını sorar; hedef hesabı korursanız giriş kaydı aktarılmaz.
3. Claude Code'un hedefte çalıştığını doğruladıktan sonra kaynakta `logout-source --session <oturum>` çalıştırın. Komut:
   - yalnız o dışa aktarmada taşınan servis ve hesaptaki kaydı ele alır; kaynaktaki Claude hesabı değiştiyse hiçbir şey silmez,
   - kaydı sizin girdiğiniz bir parolayla şifreleyip `~/.vibe-transfer/backups/logout-<oturum>/secrets.age` dosyasına yedekler,
   - yedeği açıp değeri karşılaştırır ve yalnız eşleşirse kaydı Keychain'den siler.

`rollback`, içe aktarmanın Keychain'e yazdığı kaydı da geri alır: hedefte önceden kayıt yoksa içe aktarılanı kaldırır, varsa önceki değeri geri koyar.

### Bilinen sınırlar

- Faz 1 yalnız Claude Code ve yalnız paket dosyası yolunu destekler, yalnız macOS'tan macOS'a. Canlı (ağ üzerinden) aktarım ve Codex desteği sonraki fazlarda gelecek.
- Windows henüz desteklenmiyor.
- Oturum kayıtlarındaki (`.jsonl`) `cwd` değerleri yeniden yazılmaz (`CLAUDE_REWRITE_JSONL_CWD=false`). Kaynak ve hedefte proje yolu farklıysa kayıtlar eski yolu göstermeye devam eder.
- Hedefte Keychain'e ilk erişimde macOS bir kez izin penceresi gösterebilir.
- `rollback`'in sır adımı için: hedefteki önceki değeri geri koymak paket parolasını, içe aktarmadan sonra değişmiş bir kaydı (ör. yenilenmiş token) geri almak ise terminalde onay gerektirir. Bunlar yoksa dosyalar geri alınır, sır adımı bekler ve komut tekrar çalıştırılmalıdır.
- Bir projede `.Claude` gibi `.claude` ile yalnız büyük/küçük harf veya Unicode biçimi farklı olan bir klasör atlanır ve bunun için bir not yazılır.

### Lisans

MIT veya Apache-2.0, tercihinize göre: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).

## English

vibe-transfer moves an AI coding tool's memory, settings and secrets from one machine to another without data loss. Phase 1 supports Claude Code data only, and only the encrypted bundle file path.

### What it does

- `scan` shows what would move from the source. It writes nothing; it lists files that hold plaintext secrets without showing the values.
- `export` writes the selected data into one encrypted bundle file: session history, memory, settings and agents, secrets (the Claude login item in the Keychain), project AI files and an optional cache.
- `import` applies the bundle on the target. It maps projects to their folders on the target, puts unmatched ones in a pending area, verifies every placed file and writes a report.
- Nothing is deleted on the target: every changed or removed file moves to `~/.vibe-transfer/backups/<session>/` and `rollback` can undo the session.
- No file on the source is changed. The one exception is `logout-source`, which you run by hand.
- File modification times (mtime) and POSIX modes are kept.

Working data lives in `~/.vibe-transfer/` (session journals, reports, backups, the pending area).

### Trust model

- There is no server in between: no transfer server, no relay, no account, no telemetry and no update check. The tool makes no network connections. The data exists only on the two sides, the source and the target machine.
- The bundle file is encrypted with age using a passphrase (scrypt). By default `export` generates a random passphrase and shows it once, on the terminal. Use `--passphrase-stdin` to give your own.
- Send the bundle file and the passphrase over different channels. For example, move the file on a USB drive and send the passphrase through another messaging channel.
- Secret items (the Keychain item) are never written to disk in plaintext. They exist only in memory and inside the encrypted bundle, and are written straight to the Keychain on the target.
- Files that already sit on the source in plaintext and may hold secrets (session logs, history, `settings.json`) travel encrypted inside the bundle and are written on the target as they were on the source. `scan` lists these files.

### Install

Build from source (needs Rust 1.98.1 or later):

```sh
cargo install --locked --git https://github.com/denizbarish/vibe-transfer vt-cli
```

This installs the `vibe-transfer` command. The binary is not signed. If you download and use an unsigned binary, remove the macOS Gatekeeper quarantine first:

```sh
xattr -d com.apple.quarantine ./vibe-transfer
```

### Usage

On the source machine (example user `alice`):

```sh
vibe-transfer scan
vibe-transfer export --out ~/Desktop/claude.vtb
```

The command prints the bundle passphrase and the session id. Send the bundle and the passphrase to the target over separate channels.

On the target machine (example user `bob`):

```sh
vibe-transfer import ~/Desktop/claude.vtb --roots /Users/bob/proj
vibe-transfer verify
vibe-transfer pending
vibe-transfer link --auto --roots /Users/bob/proj
```

If the login item moved, `import` prints the command to run on the source. Run it on the source after you confirm that Claude Code works on the target:

```sh
vibe-transfer logout-source --session <session>
```

### Commands

`import`, `link`, `rollback` and `logout-source` wait until Claude Code is closed before they change anything. `export` warns when Claude Code is running and asks on a terminal whether to go on.

| Command | Flags | Description |
|---|---|---|
| `scan` | `--tool <tool>`, `--json` | Shows what would move, the number of projects and the files that hold plaintext secrets. Writes nothing. |
| `export --out <file>` | `--tool <tool>`, `--only <cat,..>`, `--exclude <cat,..>`, `--include-optional`, `--passphrase-stdin`, `--json` | Writes the encrypted bundle. Never overwrites an existing file. With no selection flag on a terminal it asks for the categories; otherwise it takes every category except the cache. |
| `import <file>` | `--roots <path,..>`, `--passphrase-stdin`, `--json` | Applies the bundle. Projects are searched below `--roots` (default: the home folder). If the target already has Claude data, it asks whether to merge or replace. |
| `pending` | `--orphans`, `--json` | Lists the pending data of projects whose folder was not found on the target. `--orphans` lists files no record owns and deletes them after confirmation on a terminal. |
| `link <project> <path>` | `--json` | Places a pending project into the given folder. `<project>`: the project id, the remote repository URL or the old path on the source. |
| `link --auto` | `--roots <path,..>`, `--json` | Links every pending project whose folder is found on the target. |
| `verify [<session>]` | `--json` | Checks again the files a session verified (default: the latest report). Exits with code 1 on any deviation. |
| `rollback [<session>]` | `--passphrase-stdin` | Undoes a session (default: the latest). Always asks for confirmation; no flag skips it. |
| `logout-source --session <session>` | `--passphrase-stdin` | On the source, backs up and removes the Claude login item that was moved. |

Tool (`--tool`): only `claude` for now. Categories (`--only`, `--exclude`): `sessions`, `memory`, `settings`, `secrets`, `project-files`, `cache`.

`--json` writes the summary as one JSON document when the command ends.

### Secrets: the "move" policy and `logout-source`

The Claude Code refresh token is single use; if the same token is used on two machines, one side is logged out. So the policy is to move the login, not copy it:

1. `export` puts the Claude login item from the Keychain into the encrypted bundle.
2. `import` writes the item into the target Keychain. If the target is logged in to another Claude account, it asks which account to keep; if you keep the target account, the login item is not imported.
3. After you confirm that Claude Code works on the target, run `logout-source --session <session>` on the source. The command:
   - handles only the item of the service and account that this export moved; if the Claude account on the source changed, it deletes nothing,
   - encrypts the item with a passphrase you enter and saves it to `~/.vibe-transfer/backups/logout-<session>/secrets.age`,
   - opens the backup, compares the value and deletes the item from the Keychain only if they match.

`rollback` also undoes the Keychain write of an import: if the target had no item before, it removes the imported one; otherwise it restores the previous value.

### Known limits

- Phase 1 supports Claude Code and the bundle file only, from macOS to macOS. Live (network) transfer and Codex support come in later phases.
- Windows is not supported yet.
- Session log (`.jsonl`) `cwd` values are not rewritten (`CLAUDE_REWRITE_JSONL_CWD=false`). If the project path differs between source and target, the logs keep the old path.
- The first Keychain access on the target may show one macOS permission prompt.
- The secret step of `rollback`: restoring a previous value on the target needs the bundle passphrase, and undoing an item that changed after the import (for example a refreshed token) needs confirmation on a terminal. Without them the files are rolled back, the secret step stays pending and the command must be run again.
- A project folder such as `.Claude`, which differs from `.claude` only in case or Unicode form, is skipped and a note is written for it.

### License

MIT or Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
