//! Concurrent direct publications share the lender's real git configuration.
//! The CLI and git both run as subprocesses against one registered checkout.

use crate::registry::configure_rules;
use crate::world::World;

#[test]
fn concurrent_publications_configure_the_shared_borrower_lender_without_refusals() {
    use std::io::Write;
    let world = World::new();
    let origin = world.bare_origin("shared");
    let checkout = world.clone_of(&origin, "shared");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 3\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
    );
    world.install_pre_push(&checkout, "exit 0");
    // A realistic large checkout config widens git's rewrite window so an
    // unguarded concurrent write is observable rather than a scheduling lottery.
    let mut configuration = std::fs::OpenOptions::new()
        .append(true)
        .open(checkout.join(".git/config"))
        .expect("real git config");
    writeln!(configuration, "[journey]").expect("fixture configuration");
    for index in 0..4000 {
        writeln!(configuration, "entry{index} = value{index}").expect("fixture configuration");
    }
    let mut branches = Vec::new();
    for index in 0..8 {
        let branch = format!("feature/concurrent-{index}");
        world.git(&checkout, &["switch", "-c", &branch, "main"]);
        world.commit_file(
            &checkout,
            &format!("work-{index}.txt"),
            "work\n",
            &format!("feat: concurrent work {index}"),
        );
        world.git(&checkout, &["switch", "main"]);
        branches.push(branch);
    }
    let mut children = Vec::new();
    for branch in &branches {
        children.push(
            world
                .onevcs_std()
                .args([
                    "publish-branch",
                    branch,
                    "--repo",
                    &checkout.to_string_lossy(),
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("real CLI publication"),
        );
    }
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().expect("CLI result"))
        .collect();
    for output in outputs {
        assert!(
            output.status.success(),
            "concurrent publication refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(world.git(&checkout, &["config", "--get", "gc.auto"]), "0");
    assert_eq!(
        world.git(&checkout, &["config", "--get", "gc.pruneExpire"]),
        "never"
    );
    for index in 0..8 {
        assert!(checkout.join(format!("work-{index}.txt")).exists());
    }
}

/// A lender whose own configuration carries `shape`, registered and then lent to one
/// session that opens and closes, and the configuration file it is left with.
///
/// The retention keys are asked of that file at every step — in process wherever the
/// file can only answer as `git config --local --get` does — so what the file ends up
/// holding is what git's own reading of it decided at each.
fn lent_with(
    shape: impl Fn(&std::path::Path) -> String,
    beside: &[(&str, &str)],
) -> (World, std::path::PathBuf, String) {
    let world = World::new();
    let origin = world.bare_origin("lender");
    let checkout = world.clone_of(&origin, "lender");
    let config = checkout.join(".git/config");
    let mut held = std::fs::read_to_string(&config).expect("git's own configuration");
    held.push_str(&shape(&checkout.join(".git")));
    std::fs::write(&config, &held).expect("the shape under test");
    for (name, body) in beside {
        std::fs::write(checkout.join(".git").join(name), body).expect("a file beside it");
    }
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 3\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
    );
    let opened = world
        .onevcs()
        .args(["session", "open", "lender"])
        .assert()
        .success();
    let token = crate::world::token_of(&opened.get_output().stdout);
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let left = std::fs::read_to_string(&config).expect("the configuration after the session");
    (world, checkout, left)
}

#[test]
fn retention_keys_only_an_included_file_sets_are_added_to_the_lenders_own_file() {
    let (world, checkout, _) = lent_with(
        |git_dir| {
            format!(
                "[include]\n\tpath = {}\n",
                git_dir.join("retention.inc").display()
            )
        },
        &[("retention.inc", "[gc]\n\tauto = 0\n\tpruneExpire = never\n")],
    );
    // `git config --local` does not follow the include, so the lender's own file is
    // what has to carry them.
    for (key, value) in [("gc.auto", "0"), ("gc.pruneExpire", "never")] {
        assert_eq!(
            world.git(
                &checkout,
                &["config", "--local", "--no-includes", "--get", key]
            ),
            value,
            "{key} is in the lender's own file"
        );
    }
}

#[test]
fn retention_keys_a_lender_already_sets_add_nothing_to_its_file_whatever_their_spelling() {
    for shape in [
        // Set twice: git reads the last.
        "[gc]\n\tauto = 5\n[gc]\n\tauto = 0\n\tpruneExpire = never\n",
        // Section and variable in another case, which git reads as the same keys.
        "[GC]\n\tAUTO = 0\n\tPRUNEEXPIRE = never\n",
    ] {
        let (_, _, left) = lent_with(|_| shape.to_owned(), &[]);
        assert!(
            left.ends_with(shape),
            "{shape:?}: nothing is added to a file that already retains"
        );
    }
}

#[test]
fn a_retention_key_set_with_no_value_is_completed_in_the_lenders_own_file() {
    let (world, checkout, left) =
        lent_with(|_| "[gc]\n\tauto\n\tpruneExpire = never\n".to_owned(), &[]);
    assert!(
        left.contains("[gc]\n\tauto\n"),
        "the operator's line is kept"
    );
    assert_eq!(
        world.git(&checkout, &["config", "--local", "--get", "gc.auto"]),
        "0",
        "a key with no value is read as empty, which is not the retention value"
    );
}
