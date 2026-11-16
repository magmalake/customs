//! `mojo-run-per-test` and `mojo-build-per-test`: a command that runs
//! `mojo run`, or `mojo build`, on many test files compiles the code those
//! files share once per file.
//!
//! What it reads, at the project's root:
//!
//! - the tasks of `pixi.toml`, `[tasks]` and `[feature.*.tasks]` (and
//!   their `target.*` variants), a task's `cmd` with its `depends-on`;
//! - the `run:` steps of `.github/workflows/*.yml` and `*.yaml`;
//! - the shell scripts those run (`sh x.sh`, `bash -c '...'`, `./x.sh`,
//!   `pixi run TASK`), and the scripts those run in turn.
//!
//! In each it finds `mojo run X.mojo`, and `mojo X.mojo`, which is the
//! same, and `mojo build X.mojo`, as the words of simple commands
//! ([`crate::shell`]): `mojo run "$f"` inside `for f in tests/*.mojo`
//! counts each file the glob matches. A shell function or a script that
//! passes its arguments on to `mojo build` (`compile() { mojo build "$@"; }`,
//! `scripts/mojo-build.sh`) is a `mojo build` where it's called. A file run
//! or built that it can't tell (a variable it doesn't know, a matrix value,
//! `xargs`) is a note, not a count. Only test files count ([`Scan::is_test`]).
//! Each task, step or script is counted with everything it runs; it's
//! reported when, leaving out the files of what it runs that is reported
//! already, that's more than the threshold: the place to change.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::check::Finding;
use crate::shell::{self, Word};
use crate::toml::{self, Value};

struct Task {
    name: String,
    feature: Option<String>,
    line: usize,
    cmd: Option<String>,
    deps: Vec<String>,
}

/// `mojo run` or `mojo build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Run,
    Build,
}

impl Kind {
    fn verb(self) -> &'static str {
        match self {
            Kind::Run => "mojo run",
            Kind::Build => "mojo build",
        }
    }
}

struct Unit {
    /// `task \`test\``, `this step`, `this script`.
    label: String,
    file: String,
    line: usize,
    /// Each test file run or built, with where (`file:line`) and the line.
    runs: Vec<(Kind, String, String, usize)>,
    calls: Vec<usize>,
    /// A script that passes its arguments on to `mojo build`.
    forwards: bool,
}

/// Where a piece of shell text is.
#[derive(Clone)]
struct Ctx {
    /// The file shown, relative to the root.
    file: String,
    /// The file's line of the text's first line.
    line: usize,
    /// A script's own directory, relative to the root.
    dir: Option<String>,
    /// The pixi feature of the task the text is from.
    feature: Option<String>,
}

struct Scan<'a> {
    root: &'a Path,
    shown: &'a dyn Fn(&Path) -> String,
    tasks: Vec<Task>,
    /// Task `i` is unit `task_units[i]`.
    task_units: Vec<usize>,
    units: Vec<Unit>,
    scripts: HashMap<String, usize>,
    notes: BTreeSet<(Kind, Finding)>,
    /// Set by a `mojo build "$@"` in the text being read.
    forwarded: bool,
    /// How deep in shell functions.
    depth: usize,
    /// Whether a file has a `def test_` at its top level, by path.
    has_tests: HashMap<String, bool>,
}

const SUBCOMMANDS: [&str; 12] = [
    "build",
    "format",
    "test",
    "package",
    "doc",
    "debug",
    "repl",
    "demangle",
    "lsp",
    "help",
    "--version",
    "-v",
];

/// `mojo` options that take a value.
const VALUE_OPTS: [&str; 10] = [
    "-I",
    "-D",
    "-o",
    "--split-cache-dir",
    "--sanitize",
    "--target-triple",
    "--target-cpu",
    "--target-features",
    "--debug-level",
    "--num-threads",
];

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && cs.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// `NAME=VALUE`, split.
fn assignment(w: &str) -> Option<(&str, &str)> {
    let (n, v) = w.split_once('=')?;
    is_ident(n).then_some((n, v))
}

/// `a/./b/../c` as `a/c`; `None` above the root or absolute.
fn normal(p: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for c in Path::new(p).components() {
        match c {
            Component::Normal(s) => out.push(s.to_str()?),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop()?;
            }
            _ => return None,
        }
    }
    Some(out.join("/"))
}

fn join(dir: &str, p: &str) -> String {
    if dir.is_empty() {
        p.to_string()
    } else {
        format!("{dir}/{p}")
    }
}

fn glob_match(pat: &[char], s: &[char]) -> bool {
    match pat.split_first() {
        None => s.is_empty(),
        Some(('*', rest)) => (0..=s.len()).any(|k| glob_match(rest, &s[k..])),
        Some(('?', rest)) => !s.is_empty() && glob_match(rest, &s[1..]),
        Some(('[', rest)) => {
            let Some(end) = rest.iter().position(|&c| c == ']') else {
                return s.first() == Some(&'[') && glob_match(rest, &s[1..]);
            };
            let Some(&c) = s.first() else { return false };
            let (neg, set) = match rest[..end].split_first() {
                Some(('!' | '^', set)) => (true, set),
                _ => (false, &rest[..end]),
            };
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == '-' {
                    hit |= set[i] <= c && c <= set[i + 2];
                    i += 3;
                } else {
                    hit |= set[i] == c;
                    i += 1;
                }
            }
            hit != neg && glob_match(&rest[end + 1..], &s[1..])
        }
        Some((&p, rest)) => s.first() == Some(&p) && glob_match(rest, &s[1..]),
    }
}

fn looks(s: &str) -> bool {
    s.ends_with(".mojo") || s.ends_with(".🔥")
}

/// A word among `args`, not an option or an option's value, that could be
/// the source file: a `.mojo` path or a variable.
fn later_source(args: &[Word]) -> bool {
    let mut k = 0;
    while k < args.len() {
        let a = args[k].text.as_str();
        if VALUE_OPTS.contains(&a) {
            k += 2;
            continue;
        }
        if !a.starts_with('-') && (looks(a) || a.contains('$')) {
            return true;
        }
        k += 1;
    }
    false
}

/// `$X`, `${X}` or `${X:-default}`, and nothing else.
fn lone_var(s: &str) -> bool {
    if let Some(inner) = s.strip_prefix("${").and_then(|r| r.strip_suffix('}')) {
        let name = inner
            .split([':', '-', '=', '+', '?'])
            .next()
            .unwrap_or_default();
        return is_ident(name) && !inner[name.len()..].contains(['{', '}']);
    }
    s.strip_prefix('$').is_some_and(is_ident)
}

/// Under a directory `test` or `tests`, or named `test_*` or `test-*`.
fn test_path(path: &str) -> bool {
    let p = Path::new(path);
    let in_tests = p
        .parent()
        .is_some_and(|d| d.iter().any(|c| c == "test" || c == "tests"));
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    in_tests || stem.starts_with("test_") || stem.starts_with("test-")
}

fn has_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// `$@`, `$*` and their braced forms: a script's or function's arguments.
fn all_args(s: &str) -> bool {
    matches!(s, "$@" | "$*" | "${@}" | "${*}")
}

/// The shell functions `src` defines, and `src` with their lines blanked
/// (line numbers kept): `name() { ...; }` on one line, or `name() {` (or
/// `function name {`) down to its closing brace.
fn functions(src: &str) -> (String, Vec<(String, String, usize)>) {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut funcs = Vec::new();
    let depth = |t: &str| -> i64 {
        t.chars()
            .map(|c| match c {
                '{' => 1,
                '}' => -1,
                _ => 0,
            })
            .sum()
    };
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        let (decl, rest) = match t.strip_prefix("function ") {
            Some(r) => (true, r.trim_start()),
            None => (false, t),
        };
        let name: String = rest
            .chars()
            .take_while(|&c| c == '_' || c == '-' || c.is_ascii_alphanumeric())
            .collect();
        let after = rest[name.len()..].trim_start();
        let after =
            after
                .strip_prefix("()")
                .map(str::trim_start)
                .or(if decl { Some(after) } else { None });
        let Some(after) = after.filter(|a| !name.is_empty() && a.starts_with('{')) else {
            out.push(lines[i].to_string());
            i += 1;
            continue;
        };
        let mut d = depth(after);
        let mut body = after[1..].to_string();
        let start = i;
        out.push(String::new());
        while d > 0 && i + 1 < lines.len() {
            i += 1;
            d += depth(lines[i]);
            body.push('\n');
            body.push_str(lines[i]);
            out.push(String::new());
        }
        if let Some(end) = body.rfind('}') {
            body.truncate(end);
        }
        funcs.push((name, body, start + 1));
        i += 1;
    }
    (out.join("\n"), funcs)
}

impl Scan<'_> {
    /// A test file, by its path ([`test_path`]) and, when it exists, by
    /// having a `def test_` at its top level: a program under `tests/` (a
    /// driver a shell test starts, a program that must not compile) isn't
    /// one. Examples, benchmarks and tools aren't either.
    pub fn is_test(&mut self, path: &str) -> bool {
        if !test_path(path) {
            return false;
        }
        if let Some(&t) = self.has_tests.get(path) {
            return t;
        }
        let t = match fs::read_to_string(self.root.join(path)) {
            Ok(src) => src
                .lines()
                .any(|l| l.starts_with("def test_") || l.starts_with("fn test_")),
            Err(_) => true,
        };
        self.has_tests.insert(path.to_string(), t);
        t
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).is_file()
    }

    /// The files `pattern` (relative to the root) matches, sorted.
    fn glob(&self, pattern: &str) -> Vec<String> {
        let mut found = vec![String::new()];
        for part in pattern.split('/').filter(|p| !p.is_empty() && *p != ".") {
            let mut next = Vec::new();
            for base in &found {
                if !has_glob(part) {
                    let p = join(base, part);
                    if self.root.join(&p).exists() {
                        next.push(p);
                    }
                    continue;
                }
                let pat: Vec<char> = part.chars().collect();
                let Ok(rd) = fs::read_dir(self.root.join(base)) else {
                    continue;
                };
                let mut names: Vec<String> = rd
                    .filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .filter(|n| !n.starts_with('.') || part.starts_with('.'))
                    .filter(|n| glob_match(&pat, &n.chars().collect::<Vec<_>>()))
                    .collect();
                names.sort();
                next.extend(names.into_iter().map(|n| join(base, &n)));
            }
            found = next;
        }
        found.retain(|p| !p.is_empty());
        found
    }

    /// A path as written, relative to the root: as it is from the root, or
    /// from the script's directory when only that one exists.
    fn place(&self, p: &str, ctx: &Ctx) -> Option<String> {
        let from_root = normal(p)?;
        if let Some(d) = &ctx.dir {
            if !self.root.join(&from_root).exists() {
                if let Some(q) = normal(&join(d, p)) {
                    if self.root.join(&q).exists() {
                        return Some(q);
                    }
                }
            }
        }
        Some(from_root)
    }

    /// The files a glob names, from the root or else the script's directory.
    fn place_glob(&self, p: &str, ctx: &Ctx) -> Vec<String> {
        let mut m = normal(p).map(|q| self.glob(&q)).unwrap_or_default();
        if m.is_empty() {
            if let Some(q) = ctx.dir.as_ref().and_then(|d| normal(&join(d, p))) {
                m = self.glob(&q);
            }
        }
        m
    }

    fn note(&mut self, kind: Kind, ctx: &Ctx, line: usize, msg: String) {
        self.notes.insert((
            kind,
            Finding {
                file: ctx.file.clone(),
                line: ctx.line + line - 1,
                message: msg,
                detail: vec![],
            },
        ));
    }

    /// The task `name` as seen from `feature`: its own feature's, else the
    /// default one, else the one feature that has it.
    fn task(&self, name: &str, feature: Option<&str>) -> Option<usize> {
        let named: Vec<usize> = (0..self.tasks.len())
            .filter(|&i| self.tasks[i].name == name)
            .collect();
        named
            .iter()
            .find(|&&i| self.tasks[i].feature.as_deref() == feature)
            .or_else(|| named.iter().find(|&&i| self.tasks[i].feature.is_none()))
            .copied()
            .or_else(|| (named.len() == 1).then(|| named[0]))
    }

    fn script(&mut self, rel: String) -> usize {
        if let Some(&u) = self.scripts.get(&rel) {
            return u;
        }
        let u = self.units.len();
        self.units.push(Unit {
            label: "this script".into(),
            file: rel.clone(),
            line: 1,
            runs: Vec::new(),
            calls: Vec::new(),
            forwards: false,
        });
        self.scripts.insert(rel.clone(), u);
        if let Ok(src) = fs::read_to_string(self.root.join(&rel)) {
            let dir = Path::new(&rel)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ctx = Ctx {
                file: (self.shown)(&self.root.join(&rel)),
                line: 1,
                dir: Some(dir),
                feature: None,
            };
            self.units[u].file = ctx.file.clone();
            let outer = std::mem::replace(&mut self.forwarded, false);
            self.shell(u, &src, 1, &ctx, &mut Vars::default());
            self.units[u].forwards = std::mem::replace(&mut self.forwarded, outer);
            if let Some(first) = self.units[u].runs.iter().map(|r| r.3).min() {
                self.units[u].line = first;
            }
        }
        u
    }

    fn shell(&mut self, unit: usize, src: &str, line: usize, ctx: &Ctx, vars: &mut Vars) {
        let (src, funcs) = functions(src);
        for (name, body, at) in funcs {
            vars.funcs
                .entry(name)
                .or_default()
                .push((body, line + at - 1));
        }
        for cmd in shell::commands(&src, line) {
            self.command(unit, &cmd, ctx, vars);
        }
    }

    /// `w` as the paths it can stand for, or `None` when it can't tell.
    fn expand(&self, w: &str, ctx: &Ctx, vars: &Vars) -> Option<Vec<String>> {
        let dir = ctx.dir.clone().unwrap_or_default();
        let dir = if dir.is_empty() { ".".to_string() } else { dir };
        let mut s = w.to_string();
        for root in ["$PIXI_PROJECT_ROOT", "${PIXI_PROJECT_ROOT}"] {
            if let Some(rest) = s.strip_prefix(root) {
                s = format!(".{rest}");
            }
        }
        for me in [
            "$(dirname \"$0\")",
            "$(dirname $0)",
            "$(dirname -- \"$0\")",
            "$(dirname \"${BASH_SOURCE[0]}\")",
            "${0%/*}",
        ] {
            s = s.replace(me, &dir);
        }
        if let Some(inner) = s.strip_prefix("$(ls ").and_then(|r| r.strip_suffix(')')) {
            let mut out = Vec::new();
            for word in inner.split_whitespace().filter(|w| !w.starts_with('-')) {
                out.extend(self.expand(word.trim_matches(['"', '\'']), ctx, vars)?);
            }
            return Some(out);
        }
        if s.contains("$(") || s.contains('`') || s.contains("{{") {
            return None;
        }
        let mut results = vec![String::new()];
        let mut rest = s.as_str();
        while let Some(at) = rest.find('$') {
            let lit = &rest[..at];
            for r in &mut results {
                r.push_str(lit);
            }
            let after = &rest[at + 1..];
            let (name, len) = if let Some(b) = after.strip_prefix('{') {
                let end = b.find('}')?;
                (&b[..end], end + 2)
            } else {
                let n = after
                    .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
                    .unwrap_or(after.len());
                (&after[..n], n)
            };
            let values = vars.get(name)?;
            results = results
                .iter()
                .flat_map(|r| values.iter().map(move |v| format!("{r}{v}")))
                .collect();
            rest = &after[len..];
        }
        for r in &mut results {
            r.push_str(rest);
        }
        let mut out = Vec::new();
        for r in results {
            if has_glob(&r) {
                out.extend(self.place_glob(&r, ctx));
            } else {
                out.push(r);
            }
        }
        Some(out)
    }

    fn command(&mut self, unit: usize, cmd: &[Word], ctx: &Ctx, vars: &mut Vars) {
        let head = cmd[0].text.as_str();
        if !matches!(head, "echo" | "printf" | "[" | "test") {
            for w in cmd {
                for (sub, line) in &w.subs {
                    self.shell(unit, sub, *line, ctx, &mut vars.clone());
                }
            }
        }
        // Assignments alone set variables.
        let words: Vec<&str> = cmd.iter().map(|w| w.text.as_str()).collect();
        let decl = matches!(head, "export" | "local" | "readonly" | "declare");
        let assigns = &words[usize::from(decl)..];
        if !assigns.is_empty() && assigns.iter().all(|w| assignment(w).is_some()) {
            for w in assigns {
                let (n, v) = assignment(w).unwrap();
                let val =
                    if v.contains("dirname") && (v.contains("$0") || v.contains("BASH_SOURCE")) {
                        let d = ctx.dir.clone().unwrap_or_default();
                        Some(vec![if d.is_empty() { ".".into() } else { d }])
                    } else if v.starts_with("$(") && !v.starts_with("$(ls ") {
                        None
                    } else {
                        let mut all = Some(Vec::new());
                        for part in v.split_whitespace() {
                            match (self.expand(part, ctx, vars), all.as_mut()) {
                                (Some(x), Some(a)) => a.extend(x),
                                _ => all = None,
                            }
                        }
                        if v.is_empty() {
                            Some(vec![String::new()])
                        } else {
                            all
                        }
                    };
                vars.set(n, val);
            }
            return;
        }
        let mut i = 0;
        let mut xargs = false;
        // Prefixes: assignments, `env`, `time`, `pixi run`, ...
        while i < cmd.len() {
            let w = cmd[i].text.as_str();
            match w {
                _ if assignment(w).is_some() => i += 1,
                "exec" | "time" | "command" | "nice" | "nohup" | "then" | "do" | "else"
                | "elif" | "if" | "!" | "{" | "}" | "sudo" | "stdbuf" => i += 1,
                "env" => {
                    i += 1;
                    while i < cmd.len() && cmd[i].text.starts_with('-') {
                        i += if cmd[i].text == "-u" { 2 } else { 1 };
                    }
                }
                "timeout" => {
                    i += 1;
                    while i < cmd.len() && cmd[i].text.starts_with('-') {
                        i += 1;
                    }
                    i += 1;
                }
                "xargs" => {
                    xargs = true;
                    i += 1;
                    while i < cmd.len() && cmd[i].text.starts_with('-') {
                        let o = cmd[i].text.as_str();
                        i += if matches!(o, "-n" | "-I" | "-P" | "-L" | "-d" | "-a") {
                            2
                        } else {
                            1
                        };
                    }
                }
                "pixi"
                    if matches!(
                        cmd.get(i + 1).map(|w| w.text.as_str()),
                        Some("run" | "r" | "exec")
                    ) =>
                {
                    i += 2;
                    while i < cmd.len() && cmd[i].text.starts_with('-') {
                        let o = cmd[i].text.as_str();
                        i += if !o.contains('=')
                            && matches!(
                                o,
                                "-e" | "--environment" | "--manifest-path" | "-m" | "--executable"
                            ) {
                            2
                        } else {
                            1
                        };
                    }
                    if let Some(name) = cmd.get(i) {
                        if let Some(t) = self.task(&name.text, ctx.feature.as_deref()) {
                            let tu = self.task_units[t];
                            self.units[unit].calls.push(tu);
                            return;
                        }
                    }
                }
                _ => break,
            }
        }
        let Some(head) = cmd.get(i) else { return };
        let args = &cmd[i + 1..];
        let h = head.text.as_str();
        match h {
            "for" | "select" => {
                let name = args.first().map(|w| w.text.clone()).unwrap_or_default();
                let values = if args.get(1).map(|w| w.text.as_str()) == Some("in") {
                    let mut all = Some(Vec::new());
                    for w in &args[2..] {
                        match (self.expand(&w.text, ctx, vars), all.as_mut()) {
                            (Some(x), Some(a)) => a.extend(x),
                            _ => all = None,
                        }
                    }
                    all
                } else {
                    None
                };
                vars.push_loop(&name, values);
            }
            _ if vars.funcs.contains_key(h) => self.call_function(unit, h, args, ctx, vars),
            "while" | "until" => vars.push_loop("", None),
            "done" => vars.pop_loop(),
            "sh" | "bash" | "zsh" | "dash" | "source" | "." => {
                let mut j = 0;
                while j < args.len() && args[j].text.starts_with('-') {
                    if args[j].text == "-c" {
                        if let Some(code) = args.get(j + 1) {
                            self.shell(unit, &code.text, code.line, ctx, &mut vars.clone());
                        }
                        return;
                    }
                    j += if args[j].text == "-o" { 2 } else { 1 };
                }
                if let Some(s) = args.get(j) {
                    self.call_script(unit, &s.text, &args[j + 1..], ctx, vars);
                }
            }
            _ if h == "mojo" || h.ends_with("/mojo") => {
                self.mojo(unit, head, args, ctx, vars, xargs)
            }
            // The experimental compiler of the same name, as `mojo build`.
            "ouromo" if args.first().is_some_and(|a| a.text == "build") => {
                self.mojo(unit, head, args, ctx, vars, xargs)
            }
            _ if h.ends_with(".sh") || h.contains('/') => {
                self.call_script(unit, h, args, ctx, vars)
            }
            _ => {}
        }
    }

    /// A shell function, called: its body read where it's called, with
    /// `$1`... as the arguments; one that passes them on to `mojo build` is
    /// a `mojo build` of them.
    fn call_function(&mut self, unit: usize, name: &str, args: &[Word], ctx: &Ctx, vars: &Vars) {
        if self.depth > 8 {
            return;
        }
        let bodies = vars.funcs.get(name).cloned().unwrap_or_default();
        let mut inner = vars.clone();
        for (k, a) in args.iter().enumerate() {
            let v = self.expand(&a.text, ctx, vars);
            inner.set(&(k + 1).to_string(), v);
        }
        let outer = std::mem::replace(&mut self.forwarded, false);
        self.depth += 1;
        for (body, line) in bodies {
            // `ctx.line` is where the text starts; `line` is in its numbering.
            self.shell(unit, &body, line, ctx, &mut inner.clone());
        }
        self.depth -= 1;
        let forwards = std::mem::replace(&mut self.forwarded, outer);
        if forwards {
            self.forward(unit, args, ctx, vars);
        }
    }

    /// `args`, passed on to `mojo build`.
    fn forward(&mut self, unit: usize, args: &[Word], ctx: &Ctx, vars: &Vars) {
        let Some(first) = args.first() else { return };
        let mut words = vec![Word {
            text: "build".into(),
            line: first.line,
            subs: vec![],
        }];
        words.extend(args.iter().cloned());
        let head = Word {
            text: "mojo".into(),
            line: first.line,
            subs: vec![],
        };
        self.mojo(unit, &head, &words, ctx, vars, false);
    }

    fn call_script(&mut self, unit: usize, path: &str, args: &[Word], ctx: &Ctx, vars: &Vars) {
        let Some(paths) = self.expand(path, ctx, vars) else {
            return;
        };
        for p in paths {
            let Some(rel) = self.place(&p, ctx) else {
                continue;
            };
            if !self.exists(&rel) || rel.ends_with(".mojo") || rel.ends_with(".py") {
                continue;
            }
            let shellish = rel.ends_with(".sh")
                || fs::read_to_string(self.root.join(&rel)).is_ok_and(|s| {
                    s.starts_with("#!") && s.lines().next().is_some_and(|l| l.contains("sh"))
                });
            if shellish {
                let s = self.script(rel);
                if s != unit {
                    self.units[unit].calls.push(s);
                }
                if self.units[s].forwards {
                    self.forward(unit, args, ctx, vars);
                }
            }
        }
    }

    /// `mojo run f` or `mojo build f`, at `line`; counted when `f` is a
    /// test.
    fn run(&mut self, kind: Kind, unit: usize, f: String, ctx: &Ctx, line: usize) {
        if self.is_test(&f) {
            let at = format!("{}:{line}", ctx.file);
            self.units[unit].runs.push((kind, f, at, line));
        }
    }

    fn mojo(
        &mut self,
        unit: usize,
        head: &Word,
        args: &[Word],
        ctx: &Ctx,
        vars: &Vars,
        xargs: bool,
    ) {
        let mut j = 0;
        let mut explicit = false;
        let mut kind = Kind::Run;
        while j < args.len() {
            let a = args[j].text.as_str();
            if (a == "run" || a == "build") && !explicit {
                explicit = true;
                if a == "build" {
                    kind = Kind::Build;
                }
                j += 1;
            } else if VALUE_OPTS.contains(&a) {
                j += 2;
            } else if a.starts_with('-') && !SUBCOMMANDS.contains(&a) {
                j += 1;
            } else if lone_var(a) && later_source(&args[j + 1..]) {
                // `mojo $FLAGS x.mojo`: options in a variable.
                j += 1;
            } else {
                break;
            }
        }
        let verb = kind.verb();
        let does = match kind {
            Kind::Run => "runs",
            Kind::Build => "builds",
        };
        let line = ctx.line + head.line - 1;
        let Some(target) = args.get(j) else {
            if explicit || xargs {
                self.note(
                    kind,
                    ctx,
                    head.line,
                    format!("can't tell which file this `{verb}` {does}"),
                );
            }
            return;
        };
        let t = target.text.as_str();
        if !explicit && SUBCOMMANDS.contains(&t) {
            return;
        }
        if kind == Kind::Build && all_args(t) {
            // `mojo build "$@"`: what the caller passes is built.
            self.forwarded = true;
            return;
        }
        if !t.contains('$') && !t.contains("{{") {
            if !looks(t) {
                return;
            }
            // A glob is expanded by the shell: `mojo run` gets the first.
            let file = if has_glob(t) {
                self.place_glob(t, ctx).into_iter().next()
            } else {
                self.place(t, ctx)
            };
            if let Some(f) = file {
                self.run(kind, unit, f, ctx, line);
            }
            return;
        }
        match self.expand(t, ctx, vars) {
            Some(files) if files.iter().all(|f| looks(f)) => {
                if files.is_empty() {
                    self.note(
                        kind,
                        ctx,
                        head.line,
                        format!("`{verb} {t}`: it matches no file"),
                    );
                }
                for f in files {
                    if let Some(f) = self.place(&f, ctx) {
                        self.run(kind, unit, f, ctx, line);
                    }
                }
            }
            _ => self.note(
                kind,
                ctx,
                head.line,
                format!("can't tell which files `{verb} {t}` {does}"),
            ),
        }
    }
}

/// Shell variables it knows the values of, and the loops it's in.
#[derive(Clone, Default)]
struct Vars {
    vars: HashMap<String, Option<Vec<String>>>,
    loops: Vec<(String, Option<Vec<String>>)>,
    /// Shell functions: each definition's body and first line.
    funcs: HashMap<String, Vec<(String, usize)>>,
}

impl Vars {
    fn get(&self, name: &str) -> Option<&Vec<String>> {
        if let Some((_, v)) = self.loops.iter().rev().find(|(n, _)| n == name) {
            return v.as_ref();
        }
        self.vars.get(name)?.as_ref()
    }

    fn set(&mut self, name: &str, v: Option<Vec<String>>) {
        self.vars.insert(name.to_string(), v);
    }

    fn push_loop(&mut self, name: &str, v: Option<Vec<String>>) {
        self.loops.push((name.to_string(), v));
    }

    fn pop_loop(&mut self) {
        self.loops.pop();
    }
}

/// The pixi tasks of `pixi.toml`'s text.
fn tasks(src: &str) -> Vec<Task> {
    let mut out: Vec<Task> = Vec::new();
    let mut at: BTreeMap<(Option<String>, String), usize> = BTreeMap::new();
    let strs = |v: &Value| -> Vec<String> {
        match v {
            Value::Str(s) => vec![s.clone()],
            Value::Arr(items) => items
                .iter()
                .filter_map(|i| match i {
                    Value::Str(s) => Some(s.clone()),
                    t @ Value::Table(_) => t.get("task").and_then(Value::as_str).map(String::from),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    };
    let cmd = |v: &Value| -> Option<String> {
        match v {
            Value::Str(s) => Some(s.clone()),
            Value::Arr(items) => Some(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        }
    };
    for e in toml::entries(src) {
        let mut full = e.table.clone();
        full.extend(e.key.iter().cloned());
        // [tasks], [feature.F.tasks], [target.T.tasks], [feature.F.target.T.tasks]
        let (feature, rest): (Option<String>, &[String]) = match full.as_slice() {
            [t, rest @ ..] if t == "tasks" => (None, rest),
            [f, name, t, rest @ ..] if f == "feature" && t == "tasks" => (Some(name.clone()), rest),
            [g, _, t, rest @ ..] if g == "target" && t == "tasks" => (None, rest),
            [f, name, g, _, t, rest @ ..] if f == "feature" && g == "target" && t == "tasks" => {
                (Some(name.clone()), rest)
            }
            _ => continue,
        };
        let (name, field) = match rest {
            [n] => (n.clone(), None),
            [n, f] => (n.clone(), Some(f.as_str())),
            _ => continue,
        };
        let key = (feature.clone(), name.clone());
        let i = *at.entry(key).or_insert_with(|| {
            out.push(Task {
                name,
                feature,
                line: e.line,
                cmd: None,
                deps: Vec::new(),
            });
            out.len() - 1
        });
        let t = &mut out[i];
        match field {
            None => match &e.value {
                Value::Table(_) => {
                    if let Some(c) = e.value.get("cmd").and_then(cmd) {
                        t.cmd = Some(c);
                    }
                    for k in ["depends-on", "depends_on"] {
                        if let Some(d) = e.value.get(k) {
                            t.deps.extend(strs(d));
                        }
                    }
                }
                v => t.cmd = cmd(v),
            },
            Some("cmd") => t.cmd = cmd(&e.value),
            Some("depends-on" | "depends_on") => t.deps.extend(strs(&e.value)),
            Some(_) => {}
        }
    }
    out
}

/// The `run:` scripts of a workflow file, each with the line its text
/// starts on.
fn run_steps(src: &str) -> Vec<(usize, usize, String)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim_start();
        let t2 = t.strip_prefix("- ").map(str::trim_start).unwrap_or(t);
        let col = l.len() - t2.len();
        let Some(rest) = t2.strip_prefix("run:") else {
            i += 1;
            continue;
        };
        let rest = rest.trim();
        let key_line = i + 1;
        if rest.starts_with('|') || rest.starts_with('>') {
            let folded = rest.starts_with('>');
            let mut body = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let b = lines[j];
                let ind = b.len() - b.trim_start().len();
                if !b.trim().is_empty() && ind <= col {
                    break;
                }
                body.push(b);
                j += 1;
            }
            let strip = body
                .iter()
                .filter(|b| !b.trim().is_empty())
                .map(|b| b.len() - b.trim_start().len())
                .min()
                .unwrap_or(0);
            let text: Vec<&str> = body.iter().map(|b| b.get(strip..).unwrap_or("")).collect();
            let text = if folded {
                text.join(" ")
            } else {
                text.join("\n")
            };
            out.push((key_line, key_line + 1, text));
            i = j;
        } else {
            let text = if let Some(q) = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
                q.replace("\\\"", "\"").replace("\\\\", "\\")
            } else if let Some(q) = rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
                q.replace("''", "'")
            } else {
                rest.to_string()
            };
            out.push((key_line, key_line, text));
            i += 1;
        }
    }
    out
}

/// What one of the two rules found: findings, and notes where it can't tell.
pub struct Found {
    pub findings: Vec<Finding>,
    pub notes: Vec<Finding>,
}

/// `mojo-run-per-test` and `mojo-build-per-test` over the project at
/// `root`, each with its threshold.
pub fn check(
    root: &Path,
    shown: &dyn Fn(&Path) -> String,
    run_threshold: usize,
    build_threshold: usize,
) -> (Found, Found) {
    let mut s = Scan {
        root,
        shown,
        tasks: Vec::new(),
        task_units: Vec::new(),
        units: Vec::new(),
        scripts: HashMap::new(),
        notes: BTreeSet::new(),
        forwarded: false,
        depth: 0,
        has_tests: HashMap::new(),
    };
    let pixi = root.join("pixi.toml");
    let pixi_shown = shown(&pixi);
    if let Ok(src) = fs::read_to_string(&pixi) {
        s.tasks = tasks(&src);
        for t in &s.tasks {
            s.task_units.push(s.units.len());
            s.units.push(Unit {
                label: format!("task `{}`", t.name),
                file: pixi_shown.clone(),
                line: t.line,
                runs: Vec::new(),
                calls: Vec::new(),
                forwards: false,
            });
        }
        for i in 0..s.tasks.len() {
            let u = s.task_units[i];
            let deps: Vec<usize> = s.tasks[i]
                .deps
                .iter()
                .filter_map(|d| s.task(d, s.tasks[i].feature.as_deref()))
                .map(|t| s.task_units[t])
                .collect();
            s.units[u].calls.extend(deps);
            if let Some(cmd) = s.tasks[i].cmd.clone() {
                let ctx = Ctx {
                    file: pixi_shown.clone(),
                    line: s.tasks[i].line,
                    dir: None,
                    feature: s.tasks[i].feature.clone(),
                };
                s.shell(u, &cmd, 1, &ctx, &mut Vars::default());
            }
        }
    }
    let wf = root.join(".github").join("workflows");
    let mut files: Vec<PathBuf> = fs::read_dir(&wf)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    for f in files {
        let Ok(src) = fs::read_to_string(&f) else {
            continue;
        };
        let file = shown(&f);
        for (key_line, text_line, text) in run_steps(&src) {
            let u = s.units.len();
            s.units.push(Unit {
                label: "this step".into(),
                file: file.clone(),
                line: key_line,
                runs: Vec::new(),
                calls: Vec::new(),
                forwards: false,
            });
            let ctx = Ctx {
                file: file.clone(),
                line: text_line,
                dir: None,
                feature: None,
            };
            s.shell(u, &text, 1, &ctx, &mut Vars::default());
        }
    }
    let n = s.units.len();
    let closure = |u: usize| -> Vec<usize> {
        let mut seen = vec![false; n];
        let mut stack = vec![u];
        let mut out = Vec::new();
        while let Some(v) = stack.pop() {
            if std::mem::replace(&mut seen[v], true) {
                continue;
            }
            out.push(v);
            stack.extend(s.units[v].calls.iter().copied());
        }
        out
    };
    // Units whose closures are smaller first: what a unit runs is decided
    // before the unit.
    let closures: Vec<Vec<usize>> = (0..n).map(closure).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&u| (closures[u].len(), u));
    let found = |kind: Kind, threshold: usize| -> Found {
        let files_of = |u: usize| -> BTreeMap<String, String> {
            let mut m = BTreeMap::new();
            for &v in &closures[u] {
                for (k, f, w, _) in &s.units[v].runs {
                    if *k == kind {
                        m.entry(f.clone()).or_insert_with(|| w.clone());
                    }
                }
            }
            m
        };
        let mut reported = vec![false; n];
        let mut out = BTreeSet::new();
        for &u in &order {
            let mut files = files_of(u);
            for &v in &closures[u] {
                if v != u && reported[v] {
                    for f in files_of(v).keys() {
                        files.remove(f);
                    }
                }
            }
            if files.len() <= threshold {
                continue;
            }
            reported[u] = true;
            let unit = &s.units[u];
            let verb = kind.verb();
            let mut sites: Vec<(&String, &String)> = files.iter().map(|(f, w)| (w, f)).collect();
            sites.sort();
            let mut detail: Vec<String> = sites
                .iter()
                .take(3)
                .map(|(w, f)| format!("{w}: {verb} {f}"))
                .collect();
            if files.len() > 3 {
                detail.push(format!("... and {} more", files.len() - 3));
            }
            let what = match kind {
                Kind::Run => "each compiles the code they share again",
                Kind::Build => "each build compiles the code they share again",
            };
            out.insert(Finding {
                file: unit.file.clone(),
                line: unit.line,
                message: format!(
                    "{} runs `{verb}` on {} test files, more than {threshold}: {what}; build them once, into one binary",
                    unit.label,
                    files.len()
                ),
                detail,
            });
        }
        let rule = match kind {
            Kind::Run => "mojo-run-per-test",
            Kind::Build => "mojo-build-per-test",
        };
        let notes = s
            .notes
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, f)| {
                let mut f = f.clone();
                f.message = format!("{rule}: {}", f.message);
                f
            })
            .collect();
        Found {
            findings: out.into_iter().collect(),
            notes,
        }
    };
    let runs = found(Kind::Run, run_threshold);
    let builds = found(Kind::Build, build_threshold);
    (runs, builds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let m = |p: &str, s: &str| {
            glob_match(
                &p.chars().collect::<Vec<_>>(),
                &s.chars().collect::<Vec<_>>(),
            )
        };
        assert!(m("test_*.mojo", "test_a.mojo"));
        assert!(!m("test_*.mojo", "helper.mojo"));
        assert!(m("t?[a-c].mojo", "t1b.mojo"));
        assert!(!m("t[!a].mojo", "ta.mojo"));
    }

    #[test]
    fn task_forms() {
        let t = tasks(
            "[tasks]\na = \"mojo run x.mojo\"\nb = { depends-on = [\"a\", { task = \"c\" }] }\n[tasks.c]\ncmd = [\"mojo\", \"run\", \"y.mojo\"]\n[feature.dev.tasks]\na = \"z\"\n",
        );
        assert_eq!(t.len(), 4);
        assert_eq!(t[1].deps, vec!["a", "c"]);
        assert_eq!(t[2].cmd.as_deref(), Some("mojo run y.mojo"));
        assert_eq!(t[2].line, 5);
        assert_eq!(t[3].feature.as_deref(), Some("dev"));
    }

    #[test]
    fn workflow_steps() {
        let st = run_steps(
            "jobs:\n  t:\n    steps:\n      - run: pixi run test\n      - name: x\n        run: |\n          a\n            b\n\n          c\n        shell: bash\n      - run: \"echo \\\"hi\\\"\"\n",
        );
        assert_eq!(st.len(), 3);
        assert_eq!(st[0], (4, 4, "pixi run test".into()));
        assert_eq!(st[1].0, 6);
        assert_eq!(st[1].2, "a\n  b\n\nc");
        assert_eq!(st[2].2, "echo \"hi\"");
    }
}
