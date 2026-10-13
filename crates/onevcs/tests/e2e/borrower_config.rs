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
