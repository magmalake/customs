use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use customs::check::{self, Finding};
use customs::repo::Repo;

const USAGE: &str = "\
usage: customs [-I DIR]... [-v] [PROJECT]

Checks the imports of the Mojo project at PROJECT (default: .):

  1. no import cycles between its modules;
  2. each name imported from the module that defines it, not one that
     re-exports it (a package's __init__.mojo may re-export).

Its modules are the .mojo files under PROJECT/src (or PROJECT, without a
src); its other .mojo files (tests, examples, ...) are checked as importers.

  -I DIR         modules of another project, as `mojo build -I DIR`: names
                 imported from them are checked, they aren't
  -v, --verbose  also note names not found in the module they're imported from
  -h, --help     this
  -V, --version  the version

Exits 1 when it finds anything, 2 on a usage or read error. Under GitHub
Actions (GITHUB_ACTIONS=true) it also prints each finding as an annotation.
";

struct Args {
    project: PathBuf,
    includes: Vec<PathBuf>,
    verbose: bool,
}

fn parse() -> Result<Option<Args>, String> {
    let mut args = Args {
        project: PathBuf::from("."),
        includes: Vec::new(),
        verbose: false,
    };
    let mut project = None;
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
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

fn show(f: &Finding, kind: &str, github: bool) {
    println!("{}:{}: {kind}{}", f.file, f.line, f.message);
    for d in &f.detail {
        println!("    {d}");
    }
    if github && kind.is_empty() {
        let mut msg = f.message.clone();
        for d in &f.detail {
            msg.push('\n');
            msg.push_str(d);
        }
        println!(
            "::error file={},line={},title=customs::{}",
            f.file,
            f.line,
            escape(&msg)
        );
    }
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
    let repo = match Repo::load(&args.project, &args.includes) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("customs: {e}");
            return ExitCode::from(2);
        }
    };
    let github = env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true");
    let cycles = check::cycles(&repo);
    let (reexports, notes) = check::reexports(&repo);
    for f in cycles.iter().chain(&reexports) {
        show(f, "", github);
    }
    if args.verbose {
        for f in &notes {
            show(f, "note: ", github);
        }
    }
    let modules = repo
        .files
        .iter()
        .filter(|f| f.local && f.module.is_some())
        .count();
    let files = repo.files.iter().filter(|f| f.local).count();
    eprintln!(
        "customs: {modules} modules, {files} files: {} import cycle{}, {} import{} through a re-export",
        cycles.len(),
        if cycles.len() == 1 { "" } else { "s" },
        reexports.len(),
        if reexports.len() == 1 { "" } else { "s" },
    );
    if cycles.is_empty() && reexports.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
