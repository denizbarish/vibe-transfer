# Faz 1 elle kabul kontrol listesi

Bu liste gerçek `claude` binary'si ile, iki gerçek Mac üzerinde ya da aynı Mac'teki iki ayrı macOS kullanıcı hesabında elle yürütülür. CI'daki otomatik testler yalnız sentetik veriyle çalışır; bu listenin yerini tutmaz.

Örneklerde kaynak kullanıcı `alice`, hedef kullanıcı `bob`, örnek proje `/Users/alice/proj` (kaynak) ve `/Users/bob/proj` (hedef) yoludur. Her madde bir komut ve beklenen sonuç verir. Sonuç beklenenden farklıysa maddeyi işaretlemeyin, çıktıyı not edin.

`<oturum>` export çıktısındaki `Oturum:` değeridir (dışa aktarma oturumu). `<hedef-oturum>` import çıktısındaki `İçe aktarıldı. Oturum:` değeridir.

## 0. Hazırlık

- [ ] Kaynakta Claude Code girişli ve en az 5 oturum, auto memory'si olan bir proje ve bir git remote'lu proje var.
  Komut: `find ~/.claude/projects -mindepth 2 -maxdepth 2 -name '*.jsonl' | wc -l`
  Beklenen: 5 veya daha büyük bir sayı. Auto memory'li proje (`~/.claude/projects/<slug>/memory/` var) ve git remote'lu proje elle doğrulanır.
- [ ] Hedef hesapta Claude Code kurulu ama girişsiz (temiz hedef).
  Komut: `security find-generic-password -s "Claude Code-credentials" -a bob`
  Beklenen: `The specified item could not be found in the keychain.` ve çıkış kodu 44.
- [ ] Her iki tarafta da aynı `vibe-transfer` sürümü kurulu.
  Komut: `vibe-transfer --version`
  Beklenen: iki tarafta aynı sürüm.
- [ ] Kaynaktaki oturum sayısı not edildi.
  Komut: `find ~/.claude/projects -maxdepth 2 -name '*.jsonl' | wc -l`
  Beklenen: bir sayı (aşağıda `N` olarak anılır). Ayrıca `claude --resume` listesindeki oturum sayısı not edilir.

## 1. Kaynak: tarama ve dışa aktarma

- [ ] Tarama hiçbir şey yazmaz ve sır değeri göstermez.
  Komut: `vibe-transfer scan`
  Beklenen: `Claude Code: /Users/alice/.claude`, kategori satırları, `Projeler:` sayısı; `Düz metin sır deseni bulunan dosyalar:` altında yalnız yollar, hiçbir token değeri yok.
- [ ] Dışa aktarma paket yazar ve parolayı bir kez gösterir.
  Komut: `vibe-transfer export --out ~/Desktop/claude.vtb`
  Beklenen: `Paket parolası (yalnız şimdi gösterilir):`, `uyarı: Parolayı paket dosyasıyla aynı kanaldan gönderme.`, `Paket yazıldı:`, `Oturum: <oturum>`, `Aktarılan: ... 1 sır`. Çıkış kodu 0.
- [ ] Aynı yola ikinci dışa aktarma reddedilir.
  Komut: `vibe-transfer export --out ~/Desktop/claude.vtb`
  Beklenen: `hata: dosya zaten var: ...`, çıkış kodu 1.

Paketi hedefe bir kanaldan, parolayı başka bir kanaldan iletin.

## 2. Hedef: içe aktarma ve doğrulama

- [ ] İçe aktarma tamamlanır ve logout-source komutunu yazar.
  Komut: `vibe-transfer import ~/Desktop/claude.vtb --roots /Users/bob/proj`
  Beklenen: `İçe aktarıldı. Oturum: <hedef-oturum>`, `Doğrulanan: X/X` (iki sayı eşit), `SORUN:` satırı yok, son satırlarda `vibe-transfer logout-source --session <oturum>`. Çıkış kodu 0.
- [ ] Doğrulama temiz.
  Komut: `vibe-transfer verify; echo $?`
  Beklenen: `Oturum <hedef-oturum>: denetlenen X, sapma 0` ve `0`.
- [ ] `claude --resume` listesindeki oturum sayısı kaynakla eşit.
  Komut: `claude --resume` (listeyi sayın) ve `find ~/.claude/projects -maxdepth 2 -name '*.jsonl' | wc -l`
  Beklenen: iki sayı da kaynaktaki değerlerle (`N` ve kaynak `--resume` sayısı) eşit.
- [ ] Rastgele 3 oturum devam ettirilebiliyor.
  Komut: `claude --resume` ile listeden rastgele 3 oturum seçip her birinde bir mesaj gönderin.
  Beklenen: her oturum önceki konuşmayla açılır ve yanıt verir.
- [ ] Bir projede auto memory görünüyor.
  Komut: `cd /Users/bob/proj && claude`, ardından `/memory`
  Beklenen: kaynakta bu proje için kaydedilen auto memory içeriği listelenir.
- [ ] `/rewind` bir oturumda çalışıyor (Task 1 notu).
  Komut: taşınan bir oturumu `claude --resume` ile açın, `/rewind` çalıştırın ve önceki bir noktaya dönün.
  Beklenen: kontrol noktaları listelenir, seçilen noktaya dönülür, hata yok.
- [ ] CLAUDE_REWRITE_JSONL_CWD=false spike: proje yolu farklıyken oturumlar eski `cwd` ile çalışıyor.
  Komut: kaynakta `/Users/alice/proj` olan, hedefte `/Users/bob/proj` altına eşlenen projenin bir oturumunu `cd /Users/bob/proj && claude --resume` ile açın; bir mesaj gönderin ve `/rewind` çalıştırın. Ardından `grep -o '"cwd":"[^"]*"' <oturum dosyası>.jsonl | sort -u`
  Beklenen: oturum listede görünür ve devam eder, `/rewind` çalışır; `cwd` değerleri eski yolu (`/Users/alice/proj`) göstermeye devam eder. Sonucu `docs/spikes/2026-10-29-claude-cwd-rewrite.md` dosyasına not edin.

## 3. Keychain ve giriş (R7)

- [ ] İçe aktarmadan sonra hedefte giriş kaydı var.
  Komut: `security find-generic-password -s "Claude Code-credentials" -a bob`
  Beklenen: kayıt bulunur (değer gösterilmez), çıkış kodu 0.
- [ ] Claude Code hedefte kaydı izin penceresi olmadan okur.
  Komut: `claude -p "merhaba"`
  Beklenen: yanıt gelir; giriş istenmez ve macOS Keychain izin penceresi çıkmaz. (vibe-transfer'in kendi ilk Keychain erişiminde bir izin penceresi çıkmış olabilir; bu beklenen durumdur.)
- [ ] Claude Code token'ı yenileyebiliyor.
  Komut: erişim token'ının süresi dolduktan sonra (birkaç saat) yeniden `claude -p "merhaba"`, ardından `security find-generic-password -s "Claude Code-credentials" -a bob | grep mdat`
  Beklenen: yanıt gelir, giriş istenmez; `mdat` (değiştirilme zamanı) içe aktarma zamanından sonraya ilerlemiştir.
- [ ] Yerelde derlenmiş (ad hoc imzalı) ve yayın binary'si aynı davranır.
  Komut: yukarıdaki üç maddeyi önce `cargo install --locked --git https://github.com/denizbarish/vibe-transfer vt-cli` ile kurulan binary ile, sonra yayın binary'si ile (`xattr -d com.apple.quarantine ./vibe-transfer` sonrası) ayrı birer temiz hedef hesapta tekrarlayın.
  Beklenen: iki binary ile de kayıt yazılır, Claude Code izin penceresi olmadan okur ve token'ı yeniler.

## 4. Bekleme alanı ve link

- [ ] Hedefte klasörü olmayan bir proje bekleme alanına düşer.
  Komut: içe aktarmadan önce hedefte klonlanmamış bir git remote'lu proje bırakın; içe aktarmadan sonra `vibe-transfer pending`
  Beklenen: proje kimliği, `eski yol: /Users/alice/...`, `öğe: ..., boyut: ... bayt`.
- [ ] Bekleyen proje clone edilip `link --auto` ile bağlanıyor.
  Komut: `git clone <uzak depo adresi> /Users/bob/proj/<ad>` ve `vibe-transfer link --auto --roots /Users/bob/proj`
  Beklenen: `Bağlandı: <proje> -> /Users/bob/proj/<ad>`, `SORUN:` yok; ardından `vibe-transfer pending` çıktısında bu proje yok. Çıkış kodu 0.
- [ ] `pending --orphans` sahipsiz dosyaları listeliyor.
  Komut: bekleyen bir projenin klasörüne sahipsiz bir dosya koyun: `cp /etc/hosts ~/.vibe-transfer/pending/<proje klasörü>/blobs/sahipsiz-test`, sonra `vibe-transfer pending --orphans` ve onay sorusuna `n` yanıtı.
  Beklenen: `Sahipsiz dosyalar: 1` ve dosyanın yolu; `n` sonrası dosya yerinde durur. Komut tekrar çalıştırılıp `y` yanıtı verilince `Silinen: 1`.

## 5. Kaynakta giriş kapatma (logout-source)

- [ ] Kaynakta başka bir hesaba giriş yapılmışsa logout-source reddeder.
  Komut: kaynakta Claude Code'da `/logout`, ardından farklı bir Claude hesabıyla giriş; sonra `vibe-transfer logout-source --session <oturum>`
  Beklenen: `hata: bu makinedeki Claude hesabı (...) dışa aktarılan hesap (...) değil; hiçbir şey silinmedi`, çıkış kodu 1; `security find-generic-password -s "Claude Code-credentials" -a alice` kaydı hâlâ bulur. Sonra kaynakta yeniden asıl hesapla giriş yapın.

  Not: yeniden giriş yeni bir token üretir. Bu maddeyi ayrı bir dışa aktarma ve içe aktarma turunda, aşağıdaki maddeden önce yapın.
- [ ] logout-source sonrası kaynakta Claude giriş istiyor, hedefte çalışıyor.
  Komut: hedefte Claude Code'un çalıştığını doğruladıktan sonra kaynakta `vibe-transfer logout-source --session <oturum>` (yedek parolasını iki kez girin), ardından kaynakta `claude -p "merhaba"` ve hedefte `claude -p "merhaba"`
  Beklenen: `Kaynak makinede Claude girişi kaldırıldı: Claude Code-credentials` ve `Yedek (girdiğiniz parolayla şifreli): .../backups/logout-<oturum>/secrets.age`; kaynakta Claude giriş ister; hedefte yanıt gelir.
- [ ] Aynı oturum için ikinci logout-source yedeğin üzerine yazmaz.
  Komut: `vibe-transfer logout-source --session <oturum>`
  Beklenen: `hata: bu oturum için yedek zaten var, üzerine yazılmaz: ...`, çıkış kodu 1.

## 6. Geri alma (rollback)

- [ ] Temiz hedefte rollback içe aktarılan sırrı kaldırır.
  Komut: temiz bir hedef hesapta içe aktarmadan hemen sonra `vibe-transfer rollback` ve onay sorusuna `y`; ardından `security find-generic-password -s "Claude Code-credentials" -a bob`
  Beklenen: `Geri alındı: <hedef-oturum>`, `İçe aktarılan sır, sır deposundan kaldırıldı.`; `security` çıkış kodu 44 (kayıt yok); `ls ~/.claude/projects` içe aktarma öncesi hâline döner.
- [ ] Token yenilendikten sonra rollback onay ister.
  Komut: içe aktarmadan sonra Claude Code'un token'ı yenilemesini bekleyin (bölüm 3), sonra `vibe-transfer rollback`, ilk onaya `y`, sır sorusuna `n`.
  Beklenen: `Claude Code-credentials kaydı içe aktarmadan sonra değişmiş (ör. token yenilendi). Yine de geri alınsın mı?` sorusu; `n` sonrası `UYARI: sır adımı geri alınmadı, bekliyor: ...` ve çıkış kodu 1, dosyalar geri alınmış. Tekrar `vibe-transfer rollback <hedef-oturum>` ve iki soruya `y`: `İçe aktarılan sır, sır deposundan kaldırıldı.` ve `Not: sır içe aktarmadan sonra değişmişti (ör. token yenilendi); yine de geri alındı.`

## 7. Açık tasarım sorusu: CLAUDE_CONFIG_DIR

- [ ] Kaynakta `CLAUDE_CONFIG_DIR` tanımlı ve eski bir `~/.claude` da duruyorsa ne raporlandığı not edildi.
  Komut: `CLAUDE_CONFIG_DIR=/Users/alice/claude-alt vibe-transfer scan` (kaynakta hem `/Users/alice/claude-alt` hem eski `~/.claude` dolu)
  Beklenen (şimdiki kod): `Claude Code: /Users/alice/claude-alt`; eski `~/.claude` taranmaz ve raporda anılmaz. Bunun doğru davranış olup olmadığı açık bir tasarım sorusudur; gözlenen çıktıyı not edin, karar kullanıcıya aittir.

## 8. Diğer

- [ ] Proje içindeki `.Claude` gibi bir varyant klasör atlanır ve not düşülür.
  Komut: kaynakta Claude Code ile açılmış ama `.claude` klasörü olmayan bir projede `mkdir /Users/alice/proj/.Claude && touch /Users/alice/proj/.Claude/x.md`, ardından `vibe-transfer scan`
  Beklenen: `Taşınmayanlar` altında `büyük/küçük harf veya Unicode varyantı, .claude ile çakışır` gerekçesiyle bu klasör görünür.
- [ ] Ağ bağlantısı yok.
  Komut: dışa ve içe aktarma sırasında ayrı bir terminalde `nettop -p $(pgrep -n vibe-transfer) -L 1`
  Beklenen: `vibe-transfer` için hiçbir ağ bağlantısı görünmez.
