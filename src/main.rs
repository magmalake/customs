use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use customs::bugs;
use customs::check::{self, Finding};
use customs::config::{Config, Level, Rule, RULES};
use customs::repo::{self, Repo};
use customs::runs;
use customs::version::{self, Compiler, Version};

const USAGE: &str = "\
usage: customs [-I DIR]... [-A|-W|-D NAME]... [--mojo-version X] [-v] [PROJECT]

Checks the Mojo project at PROJECT (default: .). Its rules, by group:

  imports
    import-cycle                  no import cycles between its modules
    import-through-reexport       each name imported from the module that
                                  defines it, not one that re-exports it (a
                                  package's __init__.mojo may re-export)
    unused-import                 each imported name used in the file, outside
                                  a package's __init__.mojo; `x as x` or a
                                  `# noqa: unused-import` keeps one
  tests
    functions-in-imported-module  no import of a file that calls
                                  __functions_in_module()
    mojo-run-per-test             no task, CI step or script that runs
                                  `mojo run` on more than 3 test files
    mojo-build-per-test           no task, CI step or script that runs
                                  `mojo build` on more than 3 test files
  compiler-bugs
    append-before-loop-exit       no `while True:` loop that appends before
                                  its exit test (modular/modular#7716)
    work-before-counted-exit      no `while True:` loop that steps an Int and
                                  tests it after other work (Mojo 1.2.0 to
                                  1.3.0.dev2026111805)

Its modules are the .mojo files under PROJECT/src (or PROJECT, without a
src); its other .mojo files (tests, examples, ...) are checked as importers.
Levels and options are read from the [tool.customs] table of
PROJECT/pixi.toml; every rule is `deny` unless set there or here. A
`# noqa` comment on an import's line, or a `while True:` line, allows every
rule on it but import-cycle; `# noqa: NAME, ...` only the rules or groups
named.

  -I DIR         modules of another project, as `mojo build -I DIR`: names
                 imported from them are checked, they aren't
  -A NAME        allow (don't check) a rule or a group
  -W NAME        warn: report it, but don't fail
  -D NAME        deny: report it and fail
  --mojo-version X
                 the Mojo the compiler-bugs rules check for, over the exact
                 pins of mojo-compiler (or mojo) in pixi.toml; a rule runs
                 when one of them has its bug, or when it's unknown
  -v, --verbose  also note what it can't place: names not found in the module
                 they're imported from, `mojo run`s whose files it can't tell,
                 calls that may grow a container before a loop's exit test
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
    mojo_version: Option<String>,
    verbose: bool,
}

fn parse() -> Result<Option<Args>, String> {
    let mut args = Args {
        project: PathBuf::from("."),
        includes: Vec::new(),
        levels: Vec::new(),
        mojo_version: None,
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
            "--mojo-version" => {
                args.mojo_version = Some(it.next().ok_or("--mojo-version needs a version")?)
            }
            s if s.starts_with("--mojo-version=") => {
                args.mojo_version = Some(s["--mojo-version=".len()..].to_string())
            }
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
    if let Some(v) = &args.mojo_version {
        if Version::parse(v).is_none() {
            return Err(format!(
                "--mojo-version {v}: not a version, as 1.3.0.dev2026111805"
            ));
        }
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
    // The compiler the compiler-bugs rules check for.
    let compiler = match (&args.mojo_version, &config.mojo_version) {
        (Some(v), _) => Compiler::given(v, "--mojo-version"),
        (None, Some((v, line))) => {
            Compiler::given(v, &format!("pixi.toml:{line} [tool.customs] mojo-version"))
        }
        (None, None) => Some(version::project(&args.project)),
    }
    .expect("a version, checked");
    let mut skipped: Vec<Rule> = Vec::new();
    for rule in RULES {
        if !on(rule) {
            continue;
        }
        if rule.group() == "compiler-bugs" {
            let b = bugs::bug(rule);
            if !compiler.exposed(b.from, b.fixed_in) {
                skipped.push(rule);
                continue;
            }
        }
        let (found, notes) = match rule {
            Rule::ImportCycle => (check::cycles(&repo), Vec::new()),
            Rule::ImportThroughReexport => check::reexports(&repo),
            Rule::UnusedImport => (check::unused(&repo), Vec::new()),
            Rule::FunctionsInImportedModule => (check::functions_in_imported(&repo), Vec::new()),
            Rule::MojoRunPerTest => {
                let f = run_found.take().expect("scanned");
                (f.findings, f.notes)
            }
            Rule::MojoBuildPerTest => {
                let f = build_found.take().expect("scanned");
                (f.findings, f.notes)
            }
            Rule::AppendBeforeLoopExit => bugs::append_before_loop_exit(&repo),
            Rule::WorkBeforeCountedExit => bugs::work_before_counted_exit(&repo),
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
    if args.verbose {
        match &compiler {
            Compiler::Pinned(pins) => {
                // Each version once, with its first pin and how many more.
                let mut by: Vec<(String, &str, usize)> = Vec::new();
                for (v, at) in pins {
                    match by.iter_mut().find(|(w, _, _)| *w == v.to_string()) {
                        Some(e) => e.2 += 1,
                        None => by.push((v.to_string(), at, 0)),
                    }
                }
                let from: Vec<String> = by
                    .iter()
                    .map(|(v, at, more)| match more {
                        0 => format!("{v} ({at})"),
                        n => format!("{v} ({at}, and {n} more)"),
                    })
                    .collect();
                eprintln!(
                    "customs: Mojo {}, for the compiler-bugs rules",
                    from.join(", ")
                );
            }
            Compiler::Unknown(why) => eprintln!(
                "customs: Mojo version unknown ({why}): every compiler-bugs rule is checked"
            ),
        }
    }
    let mut counts: Vec<String> = results
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
                Rule::UnusedImport => plural(n, "unused import", "unused imports"),
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
                Rule::AppendBeforeLoopExit => plural(
                    n,
                    "`while True:` loop that appends before its exit test",
                    "`while True:` loops that append before their exit test",
                ),
                Rule::WorkBeforeCountedExit => plural(
                    n,
                    "`while True:` loop that counts to its exit test after other work",
                    "`while True:` loops that count to their exit test after other work",
                ),
            };
            if config.level(*rule) == Level::Warn && n > 0 {
                format!("{what} (warn)")
            } else {
                what
            }
        })
        .collect();
    if !skipped.is_empty() {
        let names: Vec<&str> = skipped.iter().map(|r| r.name()).collect();
        counts.push(format!(
            "{} not checked: Mojo {} doesn't have {}",
            names.join(" and "),
            compiler.versions(),
            if skipped.len() == 1 {
                "its bug"
            } else {
                "their bugs"
            }
        ));
    }
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
