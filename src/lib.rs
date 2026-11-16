//! customs: checks a Mojo project's imports, and how it runs its tests.
//!
//! See [`check`] for the rules on imports, [`runs`] for `mojo-run-per-test`,
//! [`config`] for the levels and options, and [`repo`] for what a project's
//! modules are.

pub mod check;
pub mod config;
pub mod lex;
pub mod repo;
pub mod runs;
pub mod shell;
pub mod toml;
