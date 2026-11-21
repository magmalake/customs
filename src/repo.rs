//! The files of a project and the modules they are.
//!
//! A project's modules are the `.mojo` files under its `src` (or under the
//! project itself when it has no `src`), named by their path as Mojo names
//! them with `-I src`: `src/a/b.mojo` is `a.b`, and `src/a/__init__.mojo`
//! is the package `a`. A file is a module only when each directory between
//! the root and it is a package (has an `__init__.mojo`), as Mojo imports
//! them: `tests/t.mojo` in a project without a `src` is no module. Its
//! other `.mojo` files (tests, examples, binaries) import modules but
//! aren't modules others import. Directories given with
//! `-I` add modules of other projects, to look names up in: they're read,
//! never reported on.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::lex::{self, Import, Noqa};

/// Directories never walked.
const SKIP: [&str; 4] = ["build", "target", "node_modules", "__pycache__"];

pub struct File {
    /// The path as read.
    pub path: PathBuf,
    /// The path as reported: relative to the current directory, when it
    /// is under it.
    pub display: String,
    /// The module's name; `None` for a file that only imports.
    pub module: Option<String>,
    /// An `__init__.mojo`: the module is a package.
    pub is_pkg: bool,
    /// In the project checked, not from `-I`.
    pub local: bool,
    pub imports: Vec<Import>,
    pub defs: HashSet<String>,
    /// The file through [`lex::strip`].
    pub text: String,
    /// The file as read, to quote its lines.
    pub source: String,
    /// The `# noqa` comments, by line.
    pub noqa: HashMap<usize, Noqa>,
}

impl File {
    /// Line `line` (1-based) as written, trimmed.
    pub fn quote(&self, line: usize) -> &str {
        self.source.lines().nth(line - 1).unwrap_or("").trim()
    }

    /// Whether a `# noqa` on any of `lines` allows `rule` (in `group`).
    pub fn noqa(&self, lines: &[usize], rule: &str, group: &str) -> bool {
        lines
            .iter()
            .any(|l| self.noqa.get(l).is_some_and(|n| n.allows(rule, group)))
    }
}

pub struct Repo {
    pub files: Vec<File>,
    /// Module name to its index in `files`.
    pub modules: HashMap<String, usize>,
}

fn walk(dir: &Path, skip: &[PathBuf], out: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        let ty = e.file_type()?;
        if ty.is_dir() {
            if name.starts_with('.')
                || SKIP.contains(&name.as_ref())
                || skip.iter().any(|s| same(s, &p))
            {
                continue;
            }
            walk(&p, skip, out)?;
        } else if ty.is_file() && name.ends_with(".mojo") {
            out.push(p);
        }
    }
    Ok(())
}

fn same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `src/a/b.mojo` under `src` is `a.b`; `src/a/__init__.mojo` is `a`.
fn module_name(root: &Path, path: &Path) -> Option<(String, bool)> {
    let rel = path.strip_prefix(root).ok()?.with_extension("");
    let mut parts: Vec<String> = rel
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let is_pkg = parts.last().is_some_and(|p| p == "__init__");
    if is_pkg {
        parts.pop();
    }
    if parts.is_empty() {
        return None;
    }
    // Each directory from the root down to the file's must be a package.
    let mut dir = root.to_path_buf();
    for d in &parts[..parts.len() - usize::from(!is_pkg)] {
        dir.push(d);
        if !dir.join("__init__.mojo").is_file() {
            return None;
        }
    }
    Some((parts.join("."), is_pkg))
}

pub fn display(path: &Path) -> String {
    let shown = std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            path.canonicalize()
                .ok()?
                .strip_prefix(cwd.canonicalize().ok()?)
                .ok()
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| path.to_path_buf());
    shown.to_string_lossy().into_owned()
}

fn read(path: &Path, module: Option<(String, bool)>, local: bool) -> io::Result<File> {
    let src = fs::read_to_string(path)?;
    let (text, comments) = lex::strip_keeping_comments(&src);
    let noqa = comments
        .iter()
        .filter_map(|(l, c)| lex::noqa(c).map(|n| (*l, n)))
        .collect();
    Ok(File {
        path: path.to_path_buf(),
        display: display(path),
        is_pkg: module.as_ref().is_some_and(|m| m.1),
        module: module.map(|m| m.0),
        local,
        imports: lex::imports(&text),
        defs: lex::defs(&text),
        text,
        source: src,
        noqa,
    })
}

impl Repo {
    /// The project at `root`, with the modules under each of `includes`.
    pub fn load(root: &Path, includes: &[PathBuf]) -> io::Result<Repo> {
        let src = root.join("src");
        let src = if src.is_dir() {
            src
        } else {
            root.to_path_buf()
        };
        let mut repo = Repo {
            files: Vec::new(),
            modules: HashMap::new(),
        };

        let mut paths = Vec::new();
        walk(&src, includes, &mut paths)?;
        for p in paths {
            repo.add(read(&p, module_name(&src, &p), true)?);
        }
        for inc in includes {
            let mut paths = Vec::new();
            walk(inc, &[], &mut paths)?;
            for p in paths {
                if let Some(m) = module_name(inc, &p) {
                    if !repo.modules.contains_key(&m.0) {
                        repo.add(read(&p, Some(m), false)?);
                    }
                }
            }
        }
        if src != root {
            let mut skip = includes.to_vec();
            skip.push(src.clone());
            let mut paths = Vec::new();
            walk(root, &skip, &mut paths)?;
            for p in paths {
                repo.add(read(&p, None, true)?);
            }
        }
        Ok(repo)
    }

    fn add(&mut self, f: File) {
        if let Some(m) = &f.module {
            self.modules.entry(m.clone()).or_insert(self.files.len());
        }
        self.files.push(f);
    }

    pub fn get(&self, module: &str) -> Option<&File> {
        self.modules.get(module).map(|&i| &self.files[i])
    }

    /// `module` as written in `file`, made absolute: `.x` in `a.b` (a
    /// module of package `a`) is `a.x`, `..x` one package up. `None` for a
    /// relative import in a file that isn't a module.
    pub fn resolve(&self, file: &File, module: &str) -> Option<String> {
        let dots = module.len() - module.trim_start_matches('.').len();
        if dots == 0 {
            return Some(module.to_string());
        }
        let own = file.module.as_ref()?;
        let mut base: Vec<&str> = own.split('.').collect();
        if !file.is_pkg {
            base.pop();
        }
        for _ in 1..dots {
            base.pop()?;
        }
        let rest = &module[dots..];
        if !rest.is_empty() {
            base.push(rest);
        }
        Some(base.join("."))
    }

    /// `parent.name`, if that's a module.
    pub fn submodule(&self, parent: &str, name: &str) -> Option<String> {
        let m = format!("{parent}.{name}");
        self.modules.contains_key(&m).then_some(m)
    }
}
