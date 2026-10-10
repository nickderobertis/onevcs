//! The guard itself, proved without touching anything.
//!
//! Every journey in this tier calls [`scratch_repo`] on its first line and pushes,
//! opens, and merges afterwards, so what that function refuses is the whole of what
//! stands between this tier and a repository somebody works in. These are the only
//! tests here that make no API call — and they are in this binary rather than the
//! offline one on purpose, because the rule belongs to the tier it protects.

use crate::scratch::{scratch_repo, SMOKE_REPO_ENV};

#[test]
#[should_panic(expected = "ONEVCS_SMOKE_REPO is not set")]
fn nothing_named_is_refused_naming_the_key_rather_than_defaulted() {
    std::env::remove_var(SMOKE_REPO_ENV);
    scratch_repo();
}

#[test]
#[should_panic(expected = "ONEVCS_SMOKE_REPO is not set")]
fn a_blank_value_is_not_a_repository_and_is_refused_as_unset() {
    std::env::set_var(SMOKE_REPO_ENV, "   ");
    scratch_repo();
}

#[test]
fn the_named_scratch_repository_is_the_one_every_journey_reaches() {
    std::env::set_var(SMOKE_REPO_ENV, "hiddenco/quietharbor-smoke");
    assert_eq!(scratch_repo(), "hiddenco/quietharbor-smoke");
}

#[test]
#[should_panic(expected = "is not a scratch repository")]
fn a_repository_somebody_works_in_is_refused() {
    // This repository, which is exactly the mistake the guard exists for: it is
    // reachable with the same credential the tier holds.
    std::env::set_var(SMOKE_REPO_ENV, "nickderobertis/onevcs");
    scratch_repo();
}

#[test]
#[should_panic(expected = "does not name one repository as owner/name")]
fn something_that_is_not_one_repository_is_refused_rather_than_guessed_at() {
    std::env::set_var(SMOKE_REPO_ENV, "github.com/hiddenco/quietharbor-smoke");
    scratch_repo();
}

#[test]
#[should_panic(expected = "names an empty owner or repository")]
fn a_half_written_slug_is_refused() {
    std::env::set_var(SMOKE_REPO_ENV, "/quietharbor-smoke");
    scratch_repo();
}
