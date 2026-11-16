//! The rules, their groups and levels, and the `[tool.customs]` table of a
//! project's `pixi.toml`, after clippy: a level per rule or per group
//! (`allow`, `warn`, `deny`), a rule's own level over its group's, the
//! command line's over the file's; and each rule's numbers as flat
//! kebab-case keys.
//!
//! ```toml
//! [tool.customs]
//! mojo-run-per-test-threshold = 3
//!
//! [tool.customs.lints]
//! imports = "deny"
//! tests = "deny"
//! mojo-run-per-test = "warn"
//! ```

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use crate::toml::{self, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Allow,
    Warn,
    Deny,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "allow" => Some(Level::Allow),
            "warn" => Some(Level::Warn),
            "deny" => Some(Level::Deny),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    ImportCycle,
    ImportThroughReexport,
    FunctionsInImportedModule,
    MojoRunPerTest,
    MojoBuildPerTest,
}

pub const RULES: [Rule; 5] = [
    Rule::ImportCycle,
    Rule::ImportThroughReexport,
    Rule::FunctionsInImportedModule,
    Rule::MojoRunPerTest,
    Rule::MojoBuildPerTest,
];

/// The groups, each with its rules; both are `deny` unless set.
pub const GROUPS: [(&str, &[Rule]); 2] = [
    ("imports", &[Rule::ImportCycle, Rule::ImportThroughReexport]),
    (
        "tests",
        &[
            Rule::FunctionsInImportedModule,
            Rule::MojoRunPerTest,
            Rule::MojoBuildPerTest,
        ],
    ),
];

impl Rule {
    pub fn name(self) -> &'static str {
        match self {
            Rule::ImportCycle => "import-cycle",
            Rule::ImportThroughReexport => "import-through-reexport",
            Rule::FunctionsInImportedModule => "functions-in-imported-module",
            Rule::MojoRunPerTest => "mojo-run-per-test",
            Rule::MojoBuildPerTest => "mojo-build-per-test",
        }
    }

    pub fn group(self) -> &'static str {
        GROUPS
            .iter()
            .find(|(_, rs)| rs.contains(&self))
            .map(|(g, _)| *g)
            .expect("every rule is in a group")
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A rule or a group, by name.
fn known(name: &str) -> bool {
    RULES.iter().any(|r| r.name() == name) || GROUPS.iter().any(|(g, _)| *g == name)
}

pub const DEFAULT_THRESHOLD: usize = 3;

#[derive(Debug, Clone, Default)]
struct Levels(Vec<(String, Level)>);

impl Levels {
    /// The last level set for `name`.
    fn get(&self, name: &str) -> Option<Level> {
        self.0
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, l)| *l)
    }

    fn of(&self, rule: Rule) -> Option<Level> {
        self.get(rule.name()).or_else(|| self.get(rule.group()))
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    file: Levels,
    cli: Levels,
    /// `mojo-run-per-test` reports a command that runs more files than this.
    pub threshold: usize,
    /// `mojo-build-per-test` reports a command that builds more than this.
    pub build_threshold: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            file: Levels::default(),
            cli: Levels::default(),
            threshold: DEFAULT_THRESHOLD,
            build_threshold: DEFAULT_THRESHOLD,
        }
    }
}

impl Config {
    /// The `[tool.customs]` table of `PROJECT/pixi.toml`; the defaults
    /// without one. An unknown key, rule or level is an error, so that a
    /// misspelt one doesn't go unnoticed.
    pub fn load(project: &Path) -> io::Result<Config> {
        let path = project.join("pixi.toml");
        let mut c = Config::default();
        let src = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(c),
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        };
        let bad = |line: usize, msg: String| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}:{line}: [tool.customs]: {msg}", path.display()),
            )
        };
        let level = |name: &str, v: &Value, line: usize, c: &mut Config| {
            if !known(name) {
                return Err(bad(line, format!("no rule or group `{name}`")));
            }
            let l = v.as_str().and_then(Level::parse).ok_or_else(|| {
                bad(
                    line,
                    format!("`{name}`: the level is \"allow\", \"warn\" or \"deny\""),
                )
            })?;
            c.file.0.push((name.to_string(), l));
            Ok(())
        };
        for e in toml::entries(&src) {
            // The key's path from `tool.customs` down.
            let mut full = e.table.clone();
            full.extend(e.key.iter().cloned());
            let Some(rest) = full.strip_prefix(&["tool".to_string(), "customs".to_string()]) else {
                continue;
            };
            match rest {
                [lints, name] if lints == "lints" => level(name, &e.value, e.line, &mut c)?,
                [lints] if lints == "lints" => {
                    let Value::Table(kv) = &e.value else {
                        return Err(bad(e.line, "`lints` is a table".into()));
                    };
                    for (k, v) in kv {
                        level(k, v, e.line, &mut c)?;
                    }
                }
                [opt]
                    if opt == "mojo-run-per-test-threshold"
                        || opt == "mojo-build-per-test-threshold" =>
                {
                    let n = match &e.value {
                        Value::Other(n) => n.parse().ok(),
                        _ => None,
                    }
                    .ok_or_else(|| bad(e.line, format!("`{opt}` is a whole number")))?;
                    if opt == "mojo-run-per-test-threshold" {
                        c.threshold = n;
                    } else {
                        c.build_threshold = n;
                    }
                }
                _ => return Err(bad(e.line, format!("no option `{}`", rest.join(".")))),
            }
        }
        Ok(c)
    }

    /// `-A`, `-W` or `-D` NAME: over the file's levels.
    pub fn set(&mut self, name: &str, level: Level) -> Result<(), String> {
        if !known(name) {
            return Err(format!("no rule or group `{name}`"));
        }
        self.cli.0.push((name.to_string(), level));
        Ok(())
    }

    /// A rule's level: the command line's (the rule's, else its group's),
    /// then the file's (likewise), then `deny`.
    pub fn level(&self, rule: Rule) -> Level {
        self.cli
            .of(rule)
            .or_else(|| self.file.of(rule))
            .unwrap_or(Level::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pixi: &str) -> io::Result<Config> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "customs-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pixi.toml"), pixi).unwrap();
        let c = Config::load(&dir);
        fs::remove_dir_all(&dir).unwrap();
        c
    }

    #[test]
    fn defaults_deny_everything() {
        let c = Config::default();
        for r in RULES {
            assert_eq!(c.level(r), Level::Deny);
        }
        assert_eq!(c.threshold, 3);
    }

    #[test]
    fn a_rule_wins_over_its_group_and_the_command_line_over_the_file() {
        let mut c = with(
            "[tasks]\nx = \"y\"\n[tool.customs]\nmojo-run-per-test-threshold = 7\n[tool.customs.lints]\nmojo-run-per-test = \"warn\"\ntests = \"allow\"\n",
        )
        .unwrap();
        assert_eq!(c.threshold, 7);
        assert_eq!(c.level(Rule::MojoRunPerTest), Level::Warn);
        assert_eq!(c.level(Rule::FunctionsInImportedModule), Level::Allow);
        assert_eq!(c.level(Rule::ImportCycle), Level::Deny);
        c.set("tests", Level::Deny).unwrap();
        assert_eq!(c.level(Rule::MojoRunPerTest), Level::Deny);
        c.set("mojo-run-per-test", Level::Allow).unwrap();
        c.set("tests", Level::Warn).unwrap();
        assert_eq!(c.level(Rule::MojoRunPerTest), Level::Allow);
        assert_eq!(c.level(Rule::FunctionsInImportedModule), Level::Warn);
        assert!(c.set("nope", Level::Allow).is_err());
    }

    #[test]
    fn inline_and_dotted_lints() {
        let c = with("[tool.customs]\nlints = { imports = \"warn\" }\nlints.tests = \"allow\"\n")
            .unwrap();
        assert_eq!(c.level(Rule::ImportThroughReexport), Level::Warn);
        assert_eq!(c.level(Rule::MojoRunPerTest), Level::Allow);
    }

    #[test]
    fn unknown_names_and_levels_are_errors() {
        let e = with("[tool.customs.lints]\nmojo-run = \"warn\"\n").unwrap_err();
        assert!(
            e.to_string()
                .contains(":2: [tool.customs]: no rule or group `mojo-run`"),
            "{e}"
        );
        let e = with("[tool.customs.lints]\ntests = \"off\"\n").unwrap_err();
        assert!(e.to_string().contains("allow"), "{e}");
        let e = with("[tool.customs]\nthreshold = 3\n").unwrap_err();
        assert!(e.to_string().contains("no option `threshold`"), "{e}");
        let e = with("[tool.customs]\nmojo-run-per-test-threshold = \"3\"\n").unwrap_err();
        assert!(e.to_string().contains("whole number"), "{e}");
    }
}
