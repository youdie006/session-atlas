//! The GitHub release must say what changed.
//!
//! Releases were published with an EMPTY body while CHANGELOG.md carried the
//! notes, so a reader on GitHub saw a version number and nothing else - five of
//! the last twelve were blank. The workflow reads the version's CHANGELOG
//! section now, which makes a MISSING section the new way to ship a blank page.
//! These are the checks that catch that on the tag.

fn cargo_version() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml declares a version")
        .to_string()
}

fn notes(version: &str) -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("sh")
        .arg(root.join("scripts/release-notes.sh"))
        .arg(version)
        .arg(root.join("CHANGELOG.md"))
        .output()
        .expect("run scripts/release-notes.sh");
    assert!(
        out.status.success(),
        "release-notes.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_changelog_has_notes_for_the_version_cargo_declares() {
    let version = cargo_version();
    let body = notes(&version);
    assert!(
        !body.trim().is_empty(),
        "CHANGELOG.md has no section for {version}, so the release would be a blank page"
    );
    // A heading with nothing under it is the same blank page with extra steps.
    assert!(
        body.lines().filter(|l| !l.trim().is_empty()).count() >= 2,
        "the section for {version} has no content: {body:?}"
    );
}

#[test]
fn release_notes_match_one_version_exactly() {
    // A prefix match would hand one version's notes to another, which reads as
    // deliberate and is worse than a blank page.
    assert!(
        notes("0.2").trim().is_empty(),
        "a prefix of a real version must match nothing"
    );
    assert!(
        notes("9.999.0").trim().is_empty(),
        "a version with no section must match nothing"
    );
    // A `v` prefix is what the tag carries, and it names the same release.
    let version = cargo_version();
    assert_eq!(
        notes(&version),
        notes(&format!("v{version}")),
        "the tag name and the bare version name the same section"
    );
}
