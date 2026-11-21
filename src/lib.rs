//! customs: checks a Mojo project's imports, how it runs its tests, and
//! its code for shapes a known compiler bug miscompiles.
//!
//! See [`check`] for the rules on imports, [`runs`] for `mojo-run-per-test`,
//! [`bugs`] for the rules on compiler bugs, [`config`] for the levels and
//! options, and [`repo`] for what a project's modules are.

pub mod bugs;
pub mod check;
pub mod config;
pub mod lex;
pub mod repo;
pub mod runs;
pub mod shell;
pub mod toml;
pub mod version;
