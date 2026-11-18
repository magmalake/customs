//! Reading one Mojo file: what it imports, and the names it defines at its
//! top level. Mojo's import syntax is line-oriented, so this is a scanner,
//! not a parser: strings and comments are blanked first, so an import in a
//! docstring or after a `#` doesn't count.

use std::collections::HashSet;

/// `src` with every string literal and comment replaced by spaces. Newlines
/// are kept, so line numbers still match the file.
pub fn strip(src: &str) -> String {
    strip_keeping_comments(src).0
}

/// [`strip`], and each comment it blanked, with its 1-based line.
pub fn strip_keeping_comments(src: &str) -> (String, Vec<(usize, String)>) {
    let mut comments = Vec::new();
    let mut line = 1;
    let b: Vec<char> = src.chars().collect();
    let n = b.len();
    let mut out = String::with_capacity(src.len());
    let blank = |out: &mut String, s: &[char]| {
        for &c in s {
            out.push(if c == '\n' { '\n' } else { ' ' });
        }
    };
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == '#' {
            let mut j = i;
            while j < n && b[j] != '\n' {
                j += 1;
            }
            comments.push((line, b[i..j].iter().collect()));
            blank(&mut out, &b[i..j]);
            i = j;
        } else if c == '"' || c == '\'' {
            let triple = i + 2 < n && b[i + 1] == c && b[i + 2] == c;
            let end = if triple {
                let mut j = i + 3;
                loop {
                    if j >= n {
                        break n;
                    }
                    if b[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if j + 2 < n && b[j] == c && b[j + 1] == c && b[j + 2] == c {
                        break j + 3;
                    }
                    j += 1;
                }
            } else {
                let mut j = i + 1;
                while j < n && b[j] != c && b[j] != '\n' {
                    j += if b[j] == '\\' { 2 } else { 1 };
                }
                if j < n && b[j] == c {
                    j + 1
                } else {
                    j.min(n)
                }
            };
            line += b[i..end].iter().filter(|&&c| c == '\n').count();
            blank(&mut out, &b[i..end]);
            i = end;
        } else {
            if c == '\n' {
                line += 1;
            }
            out.push(c);
            i += 1;
        }
    }
    (out, comments)
}

/// What a `# noqa` comment allows on its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Noqa {
    /// `# noqa`: every rule.
    All,
    /// `# noqa: a, b`: these rules or groups, by name.
    Names(Vec<String>),
}

impl Noqa {
    /// Whether it allows a rule, by its name or its group's.
    pub fn allows(&self, rule: &str, group: &str) -> bool {
        match self {
            Noqa::All => true,
            Noqa::Names(ns) => ns.iter().any(|n| n == rule || n == group),
        }
    }
}

/// The `# noqa` in `comment` (a comment's text, from its `#`), as flake8
/// and ruff read one: `# noqa` alone, or `# noqa: NAME, NAME`, anywhere in
/// the comment and in any case. `None` without one.
pub fn noqa(comment: &str) -> Option<Noqa> {
    let low = comment.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = low[from..].find('#') {
        let at = from + at;
        from = at + 1;
        let rest = low[at + 1..].trim_start();
        let Some(rest) = rest.strip_prefix("noqa") else {
            continue;
        };
        if rest.starts_with(|c: char| c == '_' || c == '-' || c.is_alphanumeric()) {
            continue;
        }
        let Some(list) = rest.trim_start().strip_prefix(':') else {
            return Some(Noqa::All);
        };
        let names: Vec<String> = list
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|w| !w.is_empty())
            .take_while(|w| {
                w.chars()
                    .all(|c| c == '-' || c == '_' || c.is_alphanumeric())
            })
            .map(str::to_string)
            .collect();
        return Some(if names.is_empty() {
            Noqa::All
        } else {
            Noqa::Names(names)
        });
    }
    None
}

/// One name of a `from M import a as b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub name: String,
    /// What the importer calls it: `b`, or `a` without `as`.
    pub alias: String,
    /// 1-based line the name is on (in a parenthesized list, its own).
    pub line: usize,
    /// `a as a`: the name kept on purpose, as Python's typing convention
    /// marks an explicit re-export.
    pub redundant: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// `from M import a, b as c`
    Names(Vec<Name>),
    /// `import M` or `import M as c`
    Module(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// 1-based line of the `from` or `import`.
    pub line: usize,
    /// 1-based line the statement ends on (the same, on one line).
    pub end: usize,
    /// Inside a function or other block, not at the file's top level.
    pub nested: bool,
    /// The module as written, relative (`.x`) or not.
    pub module: String,
    pub what: What,
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c == '_' || c.is_alphabetic())
        && cs.all(|c| c == '_' || c.is_alphanumeric())
}

fn is_module_path(s: &str) -> bool {
    let rest = s.trim_start_matches('.');
    rest.is_empty() && !s.is_empty() || rest.split('.').all(is_ident)
}

/// `keyword rest`, if `line` starts with `keyword` and then whitespace.
fn after_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    if rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

/// `a` or `a as b`.
fn name_and_alias(part: &str) -> Option<(String, Option<String>)> {
    let words: Vec<&str> = part.split_whitespace().collect();
    match words.as_slice() {
        [n] => Some((n.to_string(), None)),
        [n, "as", a] if is_ident(a) => Some((n.to_string(), Some(a.to_string()))),
        _ => None,
    }
}

/// The imports in `stripped` (a file through [`strip`]), in order.
pub fn imports(stripped: &str) -> Vec<Import> {
    let lines: Vec<&str> = stripped.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_start();
        let nested = line.len() != raw.len();
        let start = i;
        if let Some(rest) = after_keyword(line, "from") {
            let (module, rest) = match rest.split_once(char::is_whitespace) {
                Some((m, r)) => (m, r.trim_start()),
                None => (rest, ""),
            };
            if let Some(names) = after_keyword(rest, "import").filter(|_| is_module_path(module)) {
                // The list, a piece per line, each with its line.
                let first = names.trim_end();
                let mut segs: Vec<(String, usize)> = Vec::new();
                if let Some(open) = first.strip_prefix('(') {
                    segs.push((open.to_string(), start + 1));
                    while !segs.last().is_some_and(|(s, _)| s.contains(')')) && i + 1 < lines.len()
                    {
                        i += 1;
                        segs.push((lines[i].trim().to_string(), i + 1));
                    }
                    if let Some((s, _)) = segs.last_mut() {
                        if let Some(close) = s.find(')') {
                            s.truncate(close);
                        }
                    }
                } else {
                    segs.push((first.to_string(), start + 1));
                    while segs.last().is_some_and(|(s, _)| s.ends_with('\\')) && i + 1 < lines.len()
                    {
                        segs.last_mut().unwrap().0.pop();
                        i += 1;
                        segs.push((lines[i].trim().to_string(), i + 1));
                    }
                }
                let names: Vec<Name> = segs
                    .iter()
                    .flat_map(|(s, ln)| s.split(',').map(move |p| (p, *ln)))
                    .filter_map(|(p, ln)| name_and_alias(p.trim()).map(|na| (na, ln)))
                    .filter(|((n, _), _)| n == "*" || is_ident(n))
                    .map(|((n, a), ln)| Name {
                        redundant: a.as_ref() == Some(&n),
                        alias: a.unwrap_or_else(|| n.clone()),
                        name: n,
                        line: ln,
                    })
                    .collect();
                out.push(Import {
                    line: start + 1,
                    end: i + 1,
                    nested,
                    module: module.to_string(),
                    what: What::Names(names),
                });
            }
        } else if let Some(rest) = after_keyword(line, "import") {
            for part in rest.trim_end().split(',') {
                if let Some((m, alias)) = name_and_alias(part.trim()) {
                    if is_module_path(&m) && !m.starts_with('.') {
                        out.push(Import {
                            line: start + 1,
                            end: start + 1,
                            nested,
                            module: m,
                            what: What::Module(alias),
                        });
                    }
                }
            }
        }
        i += 1;
    }
    out
}

const DEFINERS: [&str; 7] = ["def", "fn", "struct", "trait", "comptime", "alias", "var"];

/// The names `stripped` defines at its top level: `def`, `fn`, `struct`,
/// `trait`, `comptime`, `alias` and `var` at the start of a line.
pub fn defs(stripped: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in stripped.lines() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        for kw in DEFINERS {
            if let Some(rest) = after_keyword(line, kw) {
                let name: String = rest
                    .chars()
                    .take_while(|&c| c == '_' || c.is_alphanumeric())
                    .collect();
                if is_ident(&name) && !matches!(name.as_str(), "if" | "for" | "assert" | "while") {
                    out.insert(name);
                }
                break;
            }
        }
    }
    out
}

/// The names `stripped` uses outside its import statements: each
/// identifier that isn't an attribute (after a `.`), on a line that isn't
/// part of one of `imports`.
pub fn used(stripped: &str, imports: &[Import]) -> HashSet<String> {
    let skip: HashSet<usize> = imports.iter().flat_map(|i| i.line..=i.end).collect();
    let mut out = HashSet::new();
    for (ln, line) in stripped.lines().enumerate() {
        if skip.contains(&(ln + 1)) {
            continue;
        }
        let cs: Vec<char> = line.chars().collect();
        let mut j = 0;
        while j < cs.len() {
            let c = cs[j];
            if c == '_' || c.is_alphabetic() {
                let s = j;
                while j < cs.len() && (cs[j] == '_' || cs[j].is_alphanumeric()) {
                    j += 1;
                }
                let before = cs[..s].iter().rev().find(|c| !c.is_whitespace());
                if before != Some(&'.') {
                    out.insert(cs[s..j].iter().collect());
                }
            } else if c.is_ascii_digit() {
                while j < cs.len() && (cs[j] == '_' || cs[j].is_alphanumeric()) {
                    j += 1;
                }
            } else {
                j += 1;
            }
        }
    }
    out
}

/// Each `prefix.name` in `stripped` whose `prefix` isn't the tail of a
/// longer name or path, with the line of its first use, in order.
pub fn uses(stripped: &str, prefix: &str) -> Vec<(String, usize)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let pat = format!("{prefix}.");
    for (ln, line) in stripped.lines().enumerate() {
        let mut from = 0;
        while let Some(at) = line[from..].find(&pat) {
            let at = from + at;
            from = at + pat.len();
            let before = line[..at].chars().next_back();
            if matches!(before, Some(c) if c == '_' || c == '.' || c.is_alphanumeric()) {
                continue;
            }
            let name: String = line[from..]
                .chars()
                .take_while(|&c| c == '_' || c.is_alphanumeric())
                .collect();
            if is_ident(&name) && seen.insert(name.clone()) {
                out.push((name, ln + 1));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(i: &Import) -> Vec<(&str, &str)> {
        match &i.what {
            What::Names(ns) => ns
                .iter()
                .map(|n| (n.name.as_str(), n.alias.as_str()))
                .collect(),
            What::Module(_) => vec![],
        }
    }

    #[test]
    fn strip_blanks_strings_and_comments_keeping_lines() {
        let src = "a = \"from x import y\"  # import z\n\"\"\"doc\nfrom q import r\n\"\"\"\nb = 'it''s'\n";
        let s = strip(src);
        assert_eq!(s.lines().count(), src.lines().count());
        assert!(!s.contains("import"));
        assert!(s.starts_with("a = "));
        assert!(s.contains("b = "));
    }

    #[test]
    fn strip_handles_escapes() {
        let s = strip("x = \"a\\\"b\"\nfrom m import n\n");
        assert_eq!(imports(&s).len(), 1);
    }

    #[test]
    fn from_imports_single_multi_line_and_aliases() {
        let s = strip(
            "from a.b import c, d as e\nfrom .x import (\n    y,\n    z as w,\n)\ndef f():\n    from q import r\n",
        );
        let is = imports(&s);
        assert_eq!(is.len(), 3);
        assert_eq!(is[0].module, "a.b");
        assert_eq!(names(&is[0]), vec![("c", "c"), ("d", "e")]);
        assert!(!is[0].nested);
        assert_eq!(is[1].module, ".x");
        assert_eq!(is[1].line, 2);
        assert_eq!(names(&is[1]), vec![("y", "y"), ("z", "w")]);
        assert!(is[2].nested);
        assert_eq!(is[2].line, 7);
    }

    #[test]
    fn plain_imports() {
        let is = imports(&strip("import a.b as c, d\nimport std\n"));
        assert_eq!(is.len(), 3);
        assert_eq!(is[0].what, What::Module(Some("c".into())));
        assert_eq!(is[1].module, "d");
        assert_eq!(is[2].module, "std");
    }

    #[test]
    fn not_imports() {
        let is = imports(&strip("fromage = 1\nimported = 2\nraise e from f\n"));
        assert!(is.is_empty());
    }

    #[test]
    fn top_level_defs_only() {
        let d = defs(&strip(
            "def f(x: Int):\n    def g(): pass\nstruct S[T: AnyType](Movable):\ncomptime K = 1\ncomptime if True:\n    comptime L = 2\ntrait T:\nalias A = Int\nvar v = 0\n@export(\"x\")\ndef h():\n",
        ));
        let mut v: Vec<_> = d.into_iter().collect();
        v.sort();
        assert_eq!(v, vec!["A", "K", "S", "T", "f", "h", "v"]);
    }

    #[test]
    fn names_have_their_own_lines_and_imports_their_end() {
        let is = imports(&strip(
            "from m import (\n    a,\n    b as b,  c as d,\n)\nfrom n import x, \\\n    y\nimport q\n",
        ));
        let lines = |i: &Import| match &i.what {
            What::Names(ns) => ns
                .iter()
                .map(|n| (n.alias.clone(), n.line, n.redundant))
                .collect(),
            What::Module(_) => vec![],
        };
        assert_eq!(
            lines(&is[0]),
            vec![
                ("a".into(), 2, false),
                ("b".into(), 3, true),
                ("d".into(), 3, false)
            ]
        );
        assert_eq!((is[0].line, is[0].end), (1, 4));
        assert_eq!(
            lines(&is[1]),
            vec![("x".into(), 5, false), ("y".into(), 6, false)]
        );
        assert_eq!((is[1].line, is[1].end), (5, 6));
        assert_eq!((is[2].line, is[2].end), (7, 7));
    }

    #[test]
    fn noqa_as_flake8_reads_it() {
        assert_eq!(noqa("# noqa"), Some(Noqa::All));
        assert_eq!(noqa("#NOQA"), Some(Noqa::All));
        assert_eq!(
            noqa("# kept # noqa: unused-import, imports"),
            Some(Noqa::Names(vec!["unused-import".into(), "imports".into()]))
        );
        assert_eq!(
            noqa("# noqa:unused-import why"),
            Some(Noqa::Names(vec!["unused-import".into(), "why".into()]))
        );
        assert_eq!(noqa("# noqa: F401"), Some(Noqa::Names(vec!["f401".into()])));
        assert_eq!(noqa("# noqaxyz"), None);
        assert_eq!(noqa("# see noqa"), None);
        let (_, cs) = strip_keeping_comments("a = 1  # noqa\n\"\"\"x\ny\"\"\"\nb = '#' # two\n");
        assert_eq!(
            cs,
            vec![(1, "# noqa".to_string()), (4, "# two".to_string())]
        );
    }

    #[test]
    fn used_names_leave_out_imports_and_attributes() {
        let s = strip("from m import a, b\nimport q.r\nx = a + c.b\nq.r.f()\n# b\n\"b\"\n");
        let u = used(&s, &imports(&s));
        assert!(u.contains("a") && u.contains("c") && u.contains("q") && u.contains("x"));
        assert!(!u.contains("b") && !u.contains("r") && !u.contains("f"));
    }

    #[test]
    fn uses_respects_boundaries() {
        let s = strip(
            "import a.calls as calls\ncalls.x()\nfoo.calls.y()\nmycalls.z()\ncalls.x\n  calls.w\n",
        );
        assert_eq!(
            uses(&s, "calls"),
            vec![("x".to_string(), 2), ("w".to_string(), 6)]
        );
    }
}
