//! What a consumer does with these providers.
//!
//! Every journey here reaches them the way a consumer does: it builds a provider,
//! hands it to `onevcs` as a [`Providers`](onevcs::Providers), and asserts on what
//! came back and on the event stream that was written — or, for the calls the CLI
//! cannot reach without a real git repository, on the two traits themselves, which
//! are the public interface a consumer implements against rather than an internal.

mod failures;
mod host;
mod refs;
mod repository;
mod round_trip;
mod support;
