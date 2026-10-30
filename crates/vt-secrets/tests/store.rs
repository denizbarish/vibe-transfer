use std::path::Path;

use sha2::{Digest, Sha256};
use vt_core::secret_types::SecretBytes;
use vt_secrets::{
    CLAUDE_KEYCHAIN_SERVICE, KeyringStore, MemStore, SecretStore, claude_service_name,
};

fn hex8(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn mem_store_set_get_delete() {
    let store = MemStore::new();
    let value = SecretBytes::new(br#"{"claudeAiOauth":{"accessToken":"synthetic"}}"#.to_vec());

    assert!(store.get("svc", "alice").unwrap().is_none());

    store.set("svc", "alice", &value).unwrap();
    let got = store.get("svc", "alice").unwrap().unwrap();
    assert_eq!(got.expose(), value.expose());

    // Different account or service is a different item.
    assert!(store.get("svc", "bob").unwrap().is_none());
    assert!(store.get("other", "alice").unwrap().is_none());

    // Overwrite replaces the value.
    let newer = SecretBytes::new(vec![0, 159, 146, 150, 255]);
    store.set("svc", "alice", &newer).unwrap();
    assert_eq!(
        store.get("svc", "alice").unwrap().unwrap().expose(),
        newer.expose()
    );

    store.delete("svc", "alice").unwrap();
    assert!(store.get("svc", "alice").unwrap().is_none());

    // Deleting a missing item is not an error.
    store.delete("svc", "alice").unwrap();
}

#[test]
fn mem_store_debug_hides_values() {
    let store = MemStore::new();
    store
        .set(
            "svc",
            "alice",
            &SecretBytes::new(b"hunter2-synthetic".to_vec()),
        )
        .unwrap();
    let dbg = format!("{store:?}");
    assert!(!dbg.contains("hunter2"), "{dbg}");
}

#[test]
fn claude_service_name_default_and_override() {
    assert_eq!(CLAUDE_KEYCHAIN_SERVICE, "Claude Code-credentials");
    assert_eq!(claude_service_name(None), "Claude Code-credentials");

    let expected = format!("Claude Code-credentials-{}", hex8("/tmp/x"));
    assert_eq!(claude_service_name(Some(Path::new("/tmp/x"))), expected);

    let suffix = expected.rsplit('-').next().unwrap();
    assert_eq!(suffix.len(), 8);
    assert!(
        suffix
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    );

    // The path string is hashed as given, without normalization.
    assert_ne!(
        claude_service_name(Some(Path::new("/tmp/x/"))),
        claude_service_name(Some(Path::new("/tmp/x")))
    );
}

/// Deletes the throwaway Keychain item even when an assertion panics.
struct CleanupGuard<'a> {
    store: &'a KeyringStore,
    service: &'a str,
    account: &'a str,
}

impl Drop for CleanupGuard<'_> {
    fn drop(&mut self) {
        let _ = self.store.delete(self.service, self.account);
    }
}

/// Round-trips a throwaway generic password through the real login Keychain.
/// Never touches the Claude item: the service is `vibe-transfer-test-<uuid>`.
/// Run with `VT_KEYCHAIN_TEST=1 cargo test -p vt-secrets -- --ignored`.
#[test]
#[ignore = "touches the real Keychain; set VT_KEYCHAIN_TEST=1 and pass --ignored"]
fn keyring_store_round_trip() {
    if std::env::var("VT_KEYCHAIN_TEST").as_deref() != Ok("1") {
        eprintln!("SKIPPED: VT_KEYCHAIN_TEST=1 değil");
        return;
    }
    let service = format!("vibe-transfer-test-{}", uuid::Uuid::new_v4());
    assert!(service.starts_with("vibe-transfer-test-"));
    assert_ne!(service, CLAUDE_KEYCHAIN_SERVICE);
    let account = "vt-test-account";
    let store = KeyringStore::new();
    let _guard = CleanupGuard {
        store: &store,
        service: &service,
        account,
    };

    assert!(store.get(&service, account).unwrap().is_none());

    let value = SecretBytes::new(br#"{"claudeAiOauth":{"accessToken":"synthetic"}}"#.to_vec());
    store.set(&service, account, &value).unwrap();
    assert_eq!(
        store.get(&service, account).unwrap().unwrap().expose(),
        value.expose()
    );

    let newer = SecretBytes::new(br#"{"v":2}"#.to_vec());
    store.set(&service, account, &newer).unwrap();
    assert_eq!(
        store.get(&service, account).unwrap().unwrap().expose(),
        newer.expose()
    );

    store.delete(&service, account).unwrap();
    assert!(store.get(&service, account).unwrap().is_none());
    store.delete(&service, account).unwrap();
}
