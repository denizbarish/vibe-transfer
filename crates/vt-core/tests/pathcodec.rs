use vt_core::pathcodec::{claude_slug, java_hash};

#[test]
fn slug_basic() {
    assert_eq!(
        claude_slug("/Users/alice/Projects/web/app.example"),
        "-Users-alice-Projects-web-app-example"
    );
    assert_eq!(
        claude_slug("/Users/bob/Code/my_app"),
        "-Users-bob-Code-my-app"
    );
}

#[test]
fn slug_utf16_units() {
    // NFC: one UTF-16 unit, one dash.
    assert_eq!(
        claude_slug("/Users/alice/Projects/Ki\u{015F}isel"),
        "-Users-alice-Projects-Ki-isel"
    );
    // NFD: base letter kept, combining mark becomes one dash.
    assert_eq!(
        claude_slug("/Users/alice/Projects/Kis\u{0327}isel"),
        "-Users-alice-Projects-Kis-isel"
    );
    // Surrogate pair counts as two units, so two dashes.
    assert_eq!(claude_slug("/Users/alice/a\u{1F600}b"), "-Users-alice-a--b");
}

#[test]
fn slug_long_path_hash_suffix() {
    let p = format!("/Users/alice/{}", "x".repeat(210));
    let s = claude_slug(&p);
    assert_eq!(s.len(), 207);
    assert!(s.starts_with("-Users-alice-xxx"));
    assert!(s.ends_with("-ilfi6z"));
}

#[test]
fn slug_exactly_200_units_has_no_suffix() {
    let p = format!("/{}", "x".repeat(199));
    assert_eq!(claude_slug(&p), format!("-{}", "x".repeat(199)));
}

#[test]
fn java_hash_matches_java_string_hashcode() {
    assert_eq!(java_hash(""), 0);
    assert_eq!(java_hash("a"), 97);
    assert_eq!(java_hash("hello"), 99_162_322);
    // Overflow wraps like Java int arithmetic.
    assert_eq!(java_hash("polygenelubricants"), i32::MIN);
}
