//! The rules on imports (`mojo-run-per-test` is in [`crate::runs`]).
//!
//! 1. **No import cycles.** A module imports only modules that don't import
//!    it back, directly or through others. Every import counts, a nested
//!    one (inside a function) too, and so does an import of a package's
//!    `__init__.mojo`.
//! 2. **Import a name from the file that defines it.** `from m import x`
//!    and `m.x` (after `import m`) name `x`'s own module, not one that only
//!    imports it and passes it on; a package's `__init__.mojo` is the one
//!    place a re-export is imported from.
//! 3. **No import of a file that calls `__functions_in_module()`.** In Mojo
//!    1.2.0 that list is incomplete in a module that is imported rather
//!    than built as the main file, so a test file whose `main` discovers
//!    its tests with it runs only some of them, or none, when another file
//!    imports it and calls that `main`, and still passes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::lex::{Import, What};
use crate::repo::{File, Repo};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub message: String,
    /// Further lines, shown under the message.
    pub detail: Vec<String>,
}

fn import_text(i: &Import, name: Option<&str>) -> String {
    match (&i.what, name) {
        (What::Names(_), Some(n)) => format!("from {} import {n}", i.module),
        _ => format!("import {}", i.module),
    }
}

/// The local modules each local module imports, with the import that does.
fn graph(repo: &Repo) -> BTreeMap<String, BTreeMap<String, (usize, usize, String)>> {
    let mut g: BTreeMap<String, BTreeMap<String, (usize, usize, String)>> = BTreeMap::new();
    for (fi, f) in repo.files.iter().enumerate() {
        let (Some(me), true) = (&f.module, f.local) else {
            continue;
        };
        let edges = g.entry(me.clone()).or_default();
        for imp in &f.imports {
            let Some(target) = repo.resolve(f, &imp.module) else {
                continue;
            };
            let mut add = |to: String, name: Option<&str>| {
                if &to != me && repo.get(&to).is_some_and(|t| t.local) {
                    edges
                        .entry(to)
                        .or_insert((fi, imp.line, import_text(imp, name)));
                }
            };
            match &imp.what {
                What::Names(names) => {
                    for n in names {
                        match repo.submodule(&target, &n.name) {
                            Some(sub) => add(sub, Some(&n.name)),
                            None => add(target.clone(), Some(&n.name)),
                        }
                    }
                }
                What::Module(_) => add(target.clone(), None),
            }
        }
    }
    g
}

/// Tarjan's strongly connected components, of more than one module.
fn groups(g: &BTreeMap<String, BTreeMap<String, (usize, usize, String)>>) -> Vec<Vec<String>> {
    struct T<'a> {
        g: &'a BTreeMap<String, BTreeMap<String, (usize, usize, String)>>,
        index: HashMap<&'a str, usize>,
        low: HashMap<&'a str, usize>,
        stack: Vec<&'a str>,
        on: HashSet<&'a str>,
        next: usize,
        out: Vec<Vec<String>>,
    }
    fn visit<'a>(t: &mut T<'a>, v: &'a str) {
        t.index.insert(v, t.next);
        t.low.insert(v, t.next);
        t.next += 1;
        t.stack.push(v);
        t.on.insert(v);
        if let Some(es) = t.g.get(v) {
            for w in es.keys() {
                let w = w.as_str();
                if !t.index.contains_key(w) {
                    visit(t, w);
                    let lw = t.low[w];
                    let lv = t.low.get_mut(v).unwrap();
                    *lv = (*lv).min(lw);
                } else if t.on.contains(w) {
                    let iw = t.index[w];
                    let lv = t.low.get_mut(v).unwrap();
                    *lv = (*lv).min(iw);
                }
            }
        }
        if t.low[v] == t.index[v] {
            let mut comp = Vec::new();
            loop {
                let w = t.stack.pop().unwrap();
                t.on.remove(w);
                comp.push(w.to_string());
                if w == v {
                    break;
                }
            }
            if comp.len() > 1 {
                comp.sort();
                t.out.push(comp);
            }
        }
    }
    let mut t = T {
        g,
        index: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        on: HashSet::new(),
        next: 0,
        out: Vec::new(),
    };
    for v in g.keys() {
        if !t.index.contains_key(v.as_str()) {
            visit(&mut t, v);
        }
    }
    t.out.sort();
    t.out
}

/// The shortest cycle through `group`, as the modules in order with the
/// first one again at the end; the first module in name order among ties.
fn shortest(
    g: &BTreeMap<String, BTreeMap<String, (usize, usize, String)>>,
    group: &[String],
) -> Vec<String> {
    let members: HashSet<&str> = group.iter().map(String::as_str).collect();
    let mut best: Option<Vec<String>> = None;
    for s in group {
        let mut prev: HashMap<&str, Option<&str>> = HashMap::from([(s.as_str(), None)]);
        let mut q = VecDeque::from([s.as_str()]);
        let mut last = None;
        'bfs: while let Some(v) = q.pop_front() {
            for w in g.get(v).into_iter().flat_map(|e| e.keys()) {
                let w = w.as_str();
                if !members.contains(w) {
                    continue;
                }
                if w == s {
                    last = Some(v);
                    break 'bfs;
                }
                if !prev.contains_key(w) {
                    prev.insert(w, Some(v));
                    q.push_back(w);
                }
            }
        }
        if let Some(mut v) = last {
            let mut path = vec![v.to_string()];
            while let Some(Some(p)) = prev.get(v) {
                path.push(p.to_string());
                v = p;
            }
            path.reverse();
            path.push(s.clone());
            if best.as_ref().is_none_or(|b| path.len() < b.len()) {
                best = Some(path);
            }
        }
    }
    best.unwrap_or_default()
}

/// Rule 1: one finding per group of modules that import each other.
pub fn cycles(repo: &Repo) -> Vec<Finding> {
    let g = graph(repo);
    let mut out = Vec::new();
    for group in groups(&g) {
        let cycle = shortest(&g, &group);
        let mut detail = Vec::new();
        let mut first = None;
        for pair in cycle.windows(2) {
            let (fi, line, text) = &g[&pair[0]][&pair[1]];
            let file = &repo.files[*fi].display;
            first.get_or_insert((file.clone(), *line));
            detail.push(format!("{file}:{line}: {text}"));
        }
        if group.len() > cycle.len() - 1 {
            detail.push(format!(
                "the {} modules that import each other: {}",
                group.len(),
                group.join(", ")
            ));
        }
        let (file, line) = first.unwrap_or_default();
        out.push(Finding {
            file,
            line,
            message: format!("import cycle: {}", cycle.join(" -> ")),
            detail,
        });
    }
    out
}

/// Where `name`, as `module` has it, is defined: `module` itself, a
/// submodule of it, or (following `module`'s top-level imports) another
/// module, by name. `None` if `module` neither defines nor imports it.
fn definer(
    repo: &Repo,
    module: &str,
    name: &str,
    seen: &mut HashSet<(String, String)>,
) -> Option<String> {
    if !seen.insert((module.to_string(), name.to_string())) {
        return None;
    }
    let f = repo.get(module)?;
    if f.defs.contains(name) {
        return Some(module.to_string());
    }
    if let Some(sub) = repo.submodule(module, name) {
        return Some(sub);
    }
    for imp in f.imports.iter().filter(|i| !i.nested) {
        let Some(target) = repo.resolve(f, &imp.module) else {
            continue;
        };
        match &imp.what {
            What::Names(names) => {
                if let Some(n) = names.iter().find(|n| n.alias == name) {
                    if let Some(sub) = repo.submodule(&target, &n.name) {
                        return Some(sub);
                    }
                    if repo.get(&target).is_none() {
                        return Some(target);
                    }
                    return definer(repo, &target, &n.name, seen).or(Some(target));
                }
            }
            What::Module(Some(alias)) if alias == name => return Some(target),
            What::Module(_) => {}
        }
    }
    None
}

/// What `name` from `module` is: fine, through a re-export (and from
/// where), or not found.
enum Lookup {
    Fine,
    Reexport(String),
    Unknown,
}

fn lookup(repo: &Repo, module: &File, module_name: &str, name: &str) -> Lookup {
    if name == "*"
        || module.is_pkg
        || module.defs.contains(name)
        || repo.submodule(module_name, name).is_some()
    {
        return Lookup::Fine;
    }
    match definer(repo, module_name, name, &mut HashSet::new()) {
        Some(d) => Lookup::Reexport(d),
        None => Lookup::Unknown,
    }
}

/// Rule 2: imports through a re-export, and (as notes) names not found in
/// the module they're imported from.
pub fn reexports(repo: &Repo) -> (Vec<Finding>, Vec<Finding>) {
    let mut found = BTreeSet::new();
    let mut notes = BTreeSet::new();
    for f in repo.files.iter().filter(|f| f.local) {
        let mut report = |line: usize, shown: String, module: &str, name: &str, l: Lookup| match l {
            Lookup::Fine => {}
            Lookup::Reexport(d) => {
                found.insert(Finding {
                    file: f.display.clone(),
                    line,
                    message: format!("{shown}: `{name}` is defined in {d}, and {module} only re-exports it; import it from {d}"),
                    detail: vec![],
                });
            }
            Lookup::Unknown => {
                notes.insert(Finding {
                    file: f.display.clone(),
                    line,
                    message: format!(
                        "{shown}: `{name}` isn't defined or imported at the top level of {module}"
                    ),
                    detail: vec![],
                });
            }
        };
        // `prefix.x` for each module the file names by `prefix`.
        let mut attrs: Vec<(String, String)> = Vec::new();
        for imp in &f.imports {
            let Some(target) = repo.resolve(f, &imp.module) else {
                continue;
            };
            match &imp.what {
                What::Names(names) => {
                    for n in names {
                        if let Some(sub) = repo.submodule(&target, &n.name) {
                            attrs.push((n.alias.clone(), sub));
                            continue;
                        }
                        let Some(m) = repo.get(&target) else { continue };
                        let shown = import_text(imp, Some(&n.name));
                        report(
                            imp.line,
                            shown,
                            &target,
                            &n.name,
                            lookup(repo, m, &target, &n.name),
                        );
                    }
                }
                What::Module(alias) => {
                    if repo.get(&target).is_some() {
                        attrs.push((alias.clone().unwrap_or_else(|| target.clone()), target));
                    }
                }
            }
        }
        for (prefix, target) in attrs {
            let m = repo.get(&target).expect("a module named");
            if m.is_pkg {
                continue;
            }
            for (name, line) in crate::lex::uses(&f.text, &prefix) {
                report(
                    line,
                    format!("{prefix}.{name}"),
                    &target,
                    &name,
                    lookup(repo, m, &target, &name),
                );
            }
        }
    }
    (found.into_iter().collect(), notes.into_iter().collect())
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// The files `module` (absolute, as written) can be from `f`: the module
/// of that name, and a file beside `f`, as Mojo finds one next to the file
/// it builds.
fn files_of(repo: &Repo, f: &File, module: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(m) = repo.get(module) {
        out.push(m.path.clone());
    }
    if !module.starts_with('.') {
        if let Some(dir) = f.path.parent() {
            let rel: PathBuf = module.split('.').collect();
            for p in [
                dir.join(&rel).with_extension("mojo"),
                dir.join(&rel).join("__init__.mojo"),
            ] {
                if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Rule 3: imports of a file that calls `__functions_in_module()`.
pub fn functions_in_imported(repo: &Repo) -> Vec<Finding> {
    let mut lists: HashMap<PathBuf, (usize, usize)> = HashMap::new();
    for (i, f) in repo.files.iter().enumerate().filter(|(_, f)| f.local) {
        if let Some(ln) = f
            .text
            .lines()
            .position(|l| l.contains("__functions_in_module("))
        {
            lists.insert(canon(&f.path), (i, ln + 1));
        }
    }
    let mut out = BTreeSet::new();
    if lists.is_empty() {
        return Vec::new();
    }
    for f in repo.files.iter().filter(|f| f.local) {
        let me = canon(&f.path);
        for imp in &f.imports {
            let Some(target) = repo.resolve(f, &imp.module) else {
                continue;
            };
            let mut cands: Vec<(PathBuf, Option<&str>)> = Vec::new();
            match &imp.what {
                What::Names(names) => {
                    for n in names {
                        let sub = format!("{target}.{}", n.name);
                        let subs = files_of(repo, f, &sub);
                        if subs.is_empty() {
                            cands.extend(
                                files_of(repo, f, &target)
                                    .into_iter()
                                    .map(|p| (p, Some(n.name.as_str()))),
                            );
                        } else {
                            cands.extend(subs.into_iter().map(|p| (p, Some(n.name.as_str()))));
                        }
                    }
                }
                What::Module(_) => {
                    cands.extend(files_of(repo, f, &target).into_iter().map(|p| (p, None)))
                }
            }
            let mut done = HashSet::new();
            for (p, name) in cands {
                let c = canon(&p);
                if c == me || !done.insert(c.clone()) {
                    continue;
                }
                let Some(&(fi, line)) = lists.get(&c) else {
                    continue;
                };
                let lister = &repo.files[fi].display;
                out.insert(Finding {
                    file: f.display.clone(),
                    line: imp.line,
                    message: format!(
                        "{}: {lister} calls __functions_in_module(), which lists only some of a module's functions when it's imported rather than built as the main file (Mojo 1.2.0), so tests found with it can be skipped and still pass; import its tests by name and don't run its main",
                        import_text(imp, name)
                    ),
                    detail: vec![format!("{lister}:{line}: __functions_in_module()")],
                });
            }
        }
    }
    out.into_iter().collect()
}
