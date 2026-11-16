use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use customs::check::{self, Finding};
use customs::config::{Config, Level, Rule, RULES};
use customs::repo::{self, Repo};
use customs::runs;

const USAGE: &str = "\
usage: customs [-I DIR]... [-A|-W|-D NAME]... [-v] [PROJECT]

Checks the Mojo project at PROJECT (default: .). Its rules, by group:

  imports
    import-cycle                  no import cycles between its modules
    import-through-reexport       each name imported from the module that
                                  defines it, not one that re-exports it (a
                                  package's __init__.mojo may re-export)
  tests
    functions-in-imported-module  no import of a file that calls
                                  __functions_in_module()
    mojo-run-per-test             no task, CI step or script that runs
                                  `mojo run` on more than 3 test files
    mojo-build-per-test           no task, CI step or script that runs
                                  `mojo build` on more than 3 test files

Its modules are the .mojo files under PROJECT/src (or PROJECT, without a
src); its other .mojo files (tests, examples, ...) are checked as importers.
Levels and options are read from the [tool.customs] table of
PROJECT/pixi.toml; every rule is `deny` unless set there or here.

  -I DIR         modules of another project, as `mojo build -I DIR`: names
                 imported from them are checked, they aren't
  -A NAME        allow (don't check) a rule or a group
  -W NAME        warn: report it, but don't fail
  -D NAME        deny: report it and fail
  -v, --verbose  also note what it can't place: names not found in the module
                 they're imported from, `mojo run`s whose files it can't tell
  -h, --help     this
  -V, --version  the version

Exits 1 when a denied rule finds anything, 2 on a usage, configuration or
read error. Under GitHub Actions (GITHUB_ACTIONS=true) it also prints each
finding as an annotation.
";

struct Args {
    project: PathBuf,
    includes: Vec<PathBuf>,
    levels: Vec<(String, Level)>,
    verbose: bool,
}

fn parse() -> Result<Option<Args>, String> {
    let mut args = Args {
        project: PathBuf::from("."),
        includes: Vec::new(),
        levels: Vec::new(),
        verbose: false,
    };
    let mut project = None;
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        let level = match a.get(..2) {
            Some("-A") => Some(Level::Allow),
            Some("-W") => Some(Level::Warn),
            Some("-D") => Some(Level::Deny),
            _ => None,
        };
        if let Some(l) = level {
            let name = if a.len() > 2 {
                a[2..].to_string()
            } else {
                it.next().ok_or(format!("{a} needs a rule or group"))?
            };
            args.levels.push((name, l));
            continue;
        }
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("customs {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-v" | "--verbose" => args.verbose = true,
            "-I" => args
                .includes
                .push(PathBuf::from(it.next().ok_or("-I needs a directory")?)),
            s if s.starts_with("-I") => args.includes.push(PathBuf::from(&s[2..])),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => {
                if project.replace(PathBuf::from(s)).is_some() {
                    return Err("one PROJECT only".into());
                }
            }
        }
    }
    if let Some(p) = project {
        args.project = p;
    }
    for d in std::iter::once(&args.project).chain(&args.includes) {
        if !d.is_dir() {
            return Err(format!("{}: not a directory", d.display()));
        }
    }
    Ok(Some(args))
}

/// GitHub's workflow-command escaping.
fn escape(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn show(f: &Finding, kind: Option<Level>, rule: Rule, github: bool) {
    let (prefix, command) = match kind {
        Some(Level::Deny) => ("", Some("error")),
        Some(Level::Warn) => ("warning: ", Some("warning")),
        _ => ("note: ", None),
    };
    println!("{}:{}: {prefix}{} [{rule}]", f.file, f.line, f.message);
    for d in &f.detail {
        println!("    {d}");
    }
    if let (true, Some(command)) = (github, command) {
        let mut msg = format!("{} [{rule}]", f.message);
        for d in &f.detail {
            msg.push('\n');
            msg.push_str(d);
        }
        println!(
            "::{command} file={},line={},title=customs::{}",
            f.file,
            f.line,
            escape(&msg)
        );
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn main() -> ExitCode {
    let args = match parse() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("customs: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut config = match Config::load(&args.project) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("customs: {e}");
            return ExitCode::from(2);
        }
    };
    for (name, l) in &args.levels {
        if let Err(e) = config.set(name, *l) {
            eprintln!("customs: {e}");
            return ExitCode::from(2);
        }
    }
    let repo = match Repo::load(&args.project, &args.includes) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("customs: {e}");
            return ExitCode::from(2);
        }
    };
    let github = env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true");
    let on = |r: Rule| config.level(r) != Level::Allow;
    let mut results: Vec<(Rule, Vec<Finding>, Vec<Finding>)> = Vec::new();
    let scan = (on(Rule::MojoRunPerTest) || on(Rule::MojoBuildPerTest)).then(|| {
        runs::check(
            &args.project,
            &|p| repo::display(p),
            config.threshold,
            config.build_threshold,
        )
    });
    let (mut run_found, mut build_found) = match scan {
        Some((r, b)) => (Some(r), Some(b)),
        None => (None, None),
    };
    for rule in RULES {
        if !on(rule) {
            continue;
        }
        let (found, notes) = match rule {
            Rule::ImportCycle => (check::cycles(&repo), Vec::new()),
            Rule::ImportThroughReexport => check::reexports(&repo),
            Rule::FunctionsInImportedModule => (check::functions_in_imported(&repo), Vec::new()),
            Rule::MojoRunPerTest => {
                let f = run_found.take().expect("scanned");
                (f.findings, f.notes)
            }
            Rule::MojoBuildPerTest => {
                let f = build_found.take().expect("scanned");
                (f.findings, f.notes)
            }
        };
        results.push((rule, found, notes));
    }
    let mut failed = false;
    for (rule, found, _) in &results {
        let level = config.level(*rule);
        failed |= level == Level::Deny && !found.is_empty();
        for f in found {
            show(f, Some(level), *rule, github);
        }
    }
    if args.verbose {
        for (rule, _, notes) in &results {
            for f in notes {
                show(f, None, *rule, github);
            }
        }
    }
    let modules = repo
        .files
        .iter()
        .filter(|f| f.local && f.module.is_some())
        .count();
    let files = repo.files.iter().filter(|f| f.local).count();
    let counts: Vec<String> = results
        .iter()
        .map(|(rule, found, _)| {
            let n = found.len();
            let what = match rule {
                Rule::ImportCycle => plural(n, "import cycle", "import cycles"),
                Rule::ImportThroughReexport => plural(
                    n,
                    "import through a re-export",
                    "imports through a re-export",
                ),
                Rule::FunctionsInImportedModule => plural(
                    n,
                    "import of a file that calls __functions_in_module()",
                    "imports of a file that calls __functions_in_module()",
                ),
                Rule::MojoRunPerTest => format!(
                    "{} more than {} files",
                    plural(
                        n,
                        "command that runs `mojo run` on",
                        "commands that run `mojo run` on"
                    ),
                    config.threshold
                ),
                Rule::MojoBuildPerTest => format!(
                    "{} more than {} files",
                    plural(
                        n,
                        "command that runs `mojo build` on",
                        "commands that run `mojo build` on"
                    ),
                    config.build_threshold
                ),
            };
            if config.level(*rule) == Level::Warn && n > 0 {
                format!("{what} (warn)")
            } else {
                what
            }
        })
        .collect();
    eprintln!(
        "customs: {modules} modules, {files} files: {}",
        if counts.is_empty() {
            "every rule allowed".to_string()
        } else {
            counts.join(", ")
        }
    );
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
