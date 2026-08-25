//! crap4rust — CRAP metric for Rust cargo workspaces.
//!
//! The pure core (CC engine, CRAP domain, coverage join) and the adapters
//! (LCOV reader, table reporter) live in private modules; [`cli`] is the only
//! surface the binary needs (see `docs/features/001-crap4rust.md`).

pub mod cli;

mod complexity;
mod coverage;
mod crap;
mod diagnostic;
mod join;
mod report;
mod runner;
mod workspace;
