//! Mojo's versions, and the one a project builds with.
//!
//! A release is `A.B.C`; a nightly `A.B.C.devYYYYMMDDHH`, which comes
//! before the `A.B.C` release and after every earlier one. The project's
//! compiler is each exact pin of `mojo-compiler` (else of `mojo`) in the
//! dependency tables of its `pixi.toml`: `[dependencies]`,
//! `[package.host-dependencies]` and the other `package` ones,
//! `[feature.*.dependencies]` and their `target.*` forms. A range, `*`, or
//! no pin at all is unknown: the compiler-bugs rules then all apply.

use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::path::Path;

use crate::toml::{self, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// `A.B.C`, missing parts 0.
    release: [u64; 3],
    /// The stamp of a nightly, `YYYYMMDDHH`.
    dev: Option<u64>,
    text: String,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let (rel, dev) = match s.split_once(".dev") {
            Some((r, d)) => (r, Some(d.parse::<u64>().ok()?)),
            None => (s, None),
        };
        let parts: Vec<&str> = rel.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let mut release = [0; 3];
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            release[i] = p.parse().ok()?;
        }
        Some(Version {
            release,
            dev,
            text: s.to_string(),
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        self.release
            .cmp(&other.release)
            .then(match (self.dev, other.dev) {
                (None, None) => Ordering::Equal,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => a.cmp(&b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// `from <= v`, and `v < fixed` when it's fixed.
pub fn within(v: &Version, from: &str, fixed: Option<&str>) -> bool {
    let from = Version::parse(from).expect("a bound is a version");
    let below = fixed.is_none_or(|f| *v < Version::parse(f).expect("a bound is a version"));
    from <= *v && below
}

/// The compiler a project builds with, as far as customs can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compiler {
    /// Each exact pin, with where it is (`pixi.toml:19`, `--mojo-version`).
    Pinned(Vec<(Version, String)>),
    /// Why it can't tell.
    Unknown(String),
}

/// `==X`, `=X` or a bare `X`: `X`.
fn exact(spec: &str) -> Option<Version> {
    let s = spec.trim();
    let s = s
        .strip_prefix("==")
        .or_else(|| s.strip_prefix('='))
        .unwrap_or(s);
    Version::parse(s)
}

/// A table of dependencies: `dependencies`, `package.host-dependencies`,
/// `feature.x.dependencies`, `target.linux-64.dependencies`...
fn is_deps(path: &[String]) -> bool {
    let Some((last, head)) = path.split_last() else {
        return false;
    };
    let deps = matches!(
        last.as_str(),
        "dependencies" | "host-dependencies" | "build-dependencies" | "run-dependencies"
    );
    let head_ok = match head {
        [] => last == "dependencies",
        [p] => p == "package",
        [f, _] => f == "feature" || f == "target",
        [f, _, t, _] => f == "feature" && t == "target",
        _ => false,
    };
    deps && head_ok
}

/// The project's compiler from `PROJECT/pixi.toml`.
pub fn project(dir: &Path) -> Compiler {
    let Ok(src) = fs::read_to_string(dir.join("pixi.toml")) else {
        return Compiler::Unknown("no pixi.toml".into());
    };
    // Each table's `mojo-compiler` and `mojo`: (spec, line).
    type Spec = Option<(Value, usize)>;
    let mut tables: Vec<(Vec<String>, Spec, Spec)> = Vec::new();
    let mut put = |table: Vec<String>, name: &str, v: Value, line: usize| {
        let i = match tables.iter().position(|(t, _, _)| *t == table) {
            Some(i) => i,
            None => {
                tables.push((table, None, None));
                tables.len() - 1
            }
        };
        if name == "mojo-compiler" {
            tables[i].1 = Some((v, line));
        } else {
            tables[i].2 = Some((v, line));
        }
    };
    for e in toml::entries(&src) {
        let mut full = e.table.clone();
        full.extend(e.key.iter().cloned());
        let (last, head) = full.split_last().expect("a key");
        if matches!(last.as_str(), "mojo" | "mojo-compiler") && is_deps(head) {
            put(head.to_vec(), last, e.value.clone(), e.line);
        } else if is_deps(&full) {
            if let Value::Table(kv) = &e.value {
                for (k, v) in kv {
                    if k == "mojo" || k == "mojo-compiler" {
                        put(full.clone(), k, v.clone(), e.line);
                    }
                }
            }
        }
    }
    let mut pins = Vec::new();
    for (table, compiler, mojo) in tables {
        let Some((v, line)) = compiler.or(mojo) else {
            continue;
        };
        let spec = match &v {
            Value::Str(s) => Some(s.clone()),
            Value::Table(_) => v.get("version").and_then(Value::as_str).map(str::to_string),
            _ => None,
        };
        match spec.as_deref().and_then(exact) {
            Some(ver) => pins.push((ver, format!("pixi.toml:{line} [{}]", table.join(".")))),
            None => {
                return Compiler::Unknown(format!(
                    "pixi.toml:{line} [{}] pins no one version ({})",
                    table.join("."),
                    spec.unwrap_or_else(|| "?".into())
                ))
            }
        }
    }
    if pins.is_empty() {
        return Compiler::Unknown("no pin of mojo-compiler or mojo in pixi.toml".into());
    }
    Compiler::Pinned(pins)
}

impl Compiler {
    /// `version` from the command line or `[tool.customs]`, over the pins.
    pub fn given(version: &str, from: &str) -> Option<Compiler> {
        Some(Compiler::Pinned(vec![(
            Version::parse(version)?,
            from.to_string(),
        )]))
    }

    /// Whether a bug from `from` (fixed in `fixed`) can be in it: when any
    /// pin is in the range, or it's unknown.
    pub fn exposed(&self, from: &str, fixed: Option<&str>) -> bool {
        match self {
            Compiler::Pinned(pins) => pins.iter().any(|(v, _)| within(v, from, fixed)),
            Compiler::Unknown(_) => true,
        }
    }

    /// The versions, for a message: `1.3.0.dev2026111805`.
    pub fn versions(&self) -> String {
        match self {
            Compiler::Pinned(pins) => {
                let mut vs: Vec<String> = pins.iter().map(|(v, _)| v.to_string()).collect();
                vs.dedup();
                vs.sort();
                vs.dedup();
                vs.join(", ")
            }
            Compiler::Unknown(_) => "unknown".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn ordering() {
        let order = [
            "1.2.0",
            "1.3.0.dev2026102302",
            "1.3.0.dev2026111005",
            "1.3.0.dev2026111105",
            "1.3.0.dev2026111805",
            "1.3.0.dev2026111905",
            "1.3.0.dev2026112105",
            "1.3.0",
            "1.3.1.dev2026120105",
            "1.10",
        ];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("1.3").cmp(&v("1.3.0")), Ordering::Equal);
        assert!(Version::parse("1.3.0.devx").is_none());
        assert!(Version::parse(">=1.2.0").is_none());
        assert!(Version::parse("1.*").is_none());
    }

    #[test]
    fn ranges() {
        // work-before-counted-exit: 1.2.0 to the last before 2026111905.
        let old = |s: &str| within(&v(s), "1.2.0", Some("1.3.0.dev2026111905"));
        // append-before-loop-exit: from 2026111905, not fixed yet.
        let new = |s: &str| within(&v(s), "1.3.0.dev2026111905", None);
        for (s, o, n) in [
            ("1.2.0", true, false),
            ("1.3.0.dev2026111005", true, false),
            ("1.3.0.dev2026111105", true, false),
            ("1.3.0.dev2026111805", true, false),
            ("1.3.0.dev2026111905", false, true),
            ("1.3.0.dev2026112105", false, true),
            ("1.3.0", false, true),
            ("1.1.0", false, false),
        ] {
            assert_eq!((old(s), new(s)), (o, n), "{s}");
        }
    }

    #[test]
    fn pins_in_pixi_toml() {
        let dir = std::env::temp_dir().join(format!("customs-version-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let at = |pixi: &str| {
            fs::write(dir.join("pixi.toml"), pixi).unwrap();
            project(&dir)
        };
        let c = at("[dependencies]\nmojo-compiler = \"==1.3.0.dev2026111805\"\nmojo = \"==1.3.0.dev2026111705\"\n[feature.stable.dependencies]\nmojo = { version = \"=1.2.0\", channel = \"x\" }\n[package.host-dependencies]\nmojo-compiler = \"1.3.0.dev2026111805\"\n");
        assert_eq!(c.versions(), "1.2.0, 1.3.0.dev2026111805");
        assert!(!c.exposed("1.3.0.dev2026111905", None));
        assert!(c.exposed("1.2.0", Some("1.3.0.dev2026111905")));
        assert!(matches!(
            at("[dependencies]\nmojo = \">=1.2.0\"\n"),
            Compiler::Unknown(_)
        ));
        assert!(matches!(
            at("[dependencies]\nmojo = \"*\"\n"),
            Compiler::Unknown(_)
        ));
        assert!(matches!(
            at("[dependencies]\npython = \"3.12\"\n"),
            Compiler::Unknown(_)
        ));
        assert!(matches!(
            at("[tool.x.dependencies]\nmojo = \"==1.2.0\"\n"),
            Compiler::Unknown(_)
        ));
        let c = at("dependencies = { mojo = \"==1.3.0.dev2026111905\" }\n");
        assert_eq!(c.versions(), "1.3.0.dev2026111905");
        fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(project(&dir), Compiler::Unknown(_)));
    }
}
