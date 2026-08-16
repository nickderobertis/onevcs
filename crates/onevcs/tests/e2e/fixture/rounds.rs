//! The suite of the scratch repository `tests/e2e/scripts.rs` builds, run there by
//! `cargo nextest` through that repository's own `just test-one`. Nothing in *this*
//! workspace compiles or runs it — it is carried as text and written out — which is
//! why it lives under `fixture/`.

/// Reads the file a mutation changes, so a round observes the mutation itself.
#[test]
fn the_test_the_mutation_breaks() {
    let subject = std::fs::read_to_string("subject.txt").expect("the subject is there");
    // The fixture's one seam for breaking the repository *between* a round's apply
    // and its restore: nothing else runs in between.
    if std::path::Path::new("scripts/hook.sh").is_file() {
        let hook = std::fs::read_to_string("scripts/hook.sh").expect("the hook");
        std::fs::remove_file("scripts/hook.sh").expect("a hook runs once");
        std::process::Command::new("bash")
            .arg("-c")
            .arg(hook)
            .status()
            .expect("bash runs the hook");
    }
    assert!(
        subject.contains("intact"),
        "Unexpected failure: subject.txt says {}",
        subject.trim()
    );
}

/// Green whatever the tree says: what it asserts is real, and is something no
/// mutation of this repository touches — which is what a round has to be able to
/// tell from a test that observed one.
#[test]
fn a_test_that_never_fails() {
    assert!(
        std::path::Path::new("Cargo.toml").is_file(),
        "the crate this suite belongs to is there"
    );
}

/// Red whatever the tree says: it asserts something real that is never true here,
/// which is what a round has to be able to tell from a test that observed a
/// mutation — a test red on both sides proves nothing about the behaviour.
#[test]
fn a_test_that_always_fails() {
    assert!(
        std::path::Path::new("a-file-this-repository-does-not-have").exists(),
        "Unexpected failure: this one is red either way"
    );
}

/// Fails naming the directory it ran in, which is a scratch path spelled differently
/// on every run — what a transcript may not carry if it is to be re-made byte for
/// byte.
#[test]
fn the_test_whose_failure_names_where_it_ran() {
    let subject = std::fs::read_to_string("subject.txt").expect("the subject is there");
    let here = std::env::current_dir().expect("a working directory");
    assert!(
        subject.contains("intact"),
        "Unexpected failure: the repository at {} says {}",
        here.display(),
        subject.trim()
    );
}
