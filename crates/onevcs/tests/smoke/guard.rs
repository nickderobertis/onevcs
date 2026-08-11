//! The guard itself, proved without touching anything.
//!
//! Every journey in this tier calls [`scratch_repo`] on its first line and pushes,
//! opens, and merges afterwards, so what that function refuses is the whole of what
//! stands between this tier and a repository somebody works in. These are the only
//! tests here that make no API call — and they are in this binary rather than the
//! offline one on purpose, because the rule belongs to the tier it protects.

use crate::scratch::{scratch_repo, DEFAULT_SMOKE_REPO, SMOKE_REPO_ENV};

#[test]
fn nothing_named_names_the_scratch_repository() {
    std::env::remove_var(SMOKE_REPO_ENV);
    assert_eq!(scratch_repo(), DEFAULT_SMOKE_REPO);
}

#[test]
fn a_blank_override_is_not_a_repository_and_falls_back() {
    std::env::set_var(SMOKE_REPO_ENV, "   ");
    assert_eq!(scratch_repo(), DEFAULT_SMOKE_REPO);
}

#[test]
#[should_panic(expected = "is not a scratch repository")]
fn a_repository_somebody_works_in_is_refused() {
    // This repository, which is exactly the mistake the guard exists for: it is
    // reachable with the same credential and one character from the default.
    std::env::set_var(SMOKE_REPO_ENV, "nickderobertis/onevcs");
    scratch_repo();
}

#[test]
#[should_panic(expected = "does not name one repository as owner/name")]
fn something_that_is_not_one_repository_is_refused_rather_than_guessed_at() {
    std::env::set_var(SMOKE_REPO_ENV, "github.com/nickderobertis/onevcs-smoke");
    scratch_repo();
}

#[test]
#[should_panic(expected = "names an empty owner or repository")]
fn a_half_written_slug_is_refused() {
    std::env::set_var(SMOKE_REPO_ENV, "/onevcs-smoke");
    scratch_repo();
}
