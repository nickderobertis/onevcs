//! The reader options every YAML document this repository reads is read under.
//!
//! Its own file so that the test binaries read YAML exactly as the crate does: they
//! include this file by path, since the module is private to the crate.

/// Plain `yes`, `no`, `on` and `off` stay strings and `<<` stays an ordinary key, as
/// they were before this crate read YAML through serde-saphyr; an error is one line
/// carrying its line and column rather than a rendered snippet; and a document may nest
/// 127 levels deep, the limit the earlier reader had, where serde-saphyr's own refuses
/// from 64.
pub(crate) fn options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        with_snippet: false,
        budget: serde_saphyr::budget! { max_depth: 128 },
    }
}
