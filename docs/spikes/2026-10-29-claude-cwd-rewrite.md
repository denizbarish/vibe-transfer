# Spike: Claude jsonl `cwd` yeniden yazımı

Tarih: 2026-10-29
Durum: çalıştırılmadı

Spike gerçek bir Claude hesabıyla `claude -p --resume` çağrıları gerektiriyordu. Bu çağrılar için onay verilmedi, bu yüzden deney yapılmadı.

## Karar

`CLAUDE_REWRITE_JSONL_CWD = false`

Oturum dosyalarının (jsonl) içeriğine dokunulmaz; yalnız proje klasör adı (slug) hedef yola göre yeniden hesaplanır. Gerekçe: daha az dönüşüm, daha az risk. `hash` ve `requestDigest` gibi alanların içerik değişikliğinden etkilenip etkilenmediği bilinmiyor.

## Doğrulama

Faz 1 kabul testinde (`docs/acceptance.md`) iki gerçek makinede kontrol edilir:

- `claude --resume` listesi, farklı kullanıcı adı ve proje yolu olan hedefte kaynakla aynı sayıda oturum gösteriyor mu.
- Rastgele seçilen oturumlar devam ettirilebiliyor mu.
- `/rewind` en az bir oturumda çalışıyor mu.

Sonuç olumsuzsa karar `true` yapılır ve `rewrite_jsonl_cwd` etkinleştirilir.
