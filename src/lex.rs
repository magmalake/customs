//! Reading one Mojo file: what it imports, and the names it defines at its
//! top level. Mojo's import syntax is line-oriented, so this is a scanner,
//! not a parser: strings and comments are blanked first, so an import in a
//! docstring or after a `#` doesn't count.

use std::collections::HashSet;

/// `src` with every string literal and comment replaced by spaces. Newlines
/// are kept, so line numbers still match the file.
pub fn strip(src: &str) -> String {
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
            blank(&mut out, &b[i..end]);
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// One name of a `from M import a as b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub name: String,
    /// What the importer calls it: `b`, or `a` without `as`.
    pub alias: String,
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
                let mut names = names.trim_end().to_string();
                if let Some(open) = names.strip_prefix('(') {
                    let mut buf = open.to_string();
                    while !buf.contains(')') && i + 1 < lines.len() {
                        i += 1;
                        buf.push(' ');
                        buf.push_str(lines[i].trim());
                    }
                    names = buf.split(')').next().unwrap_or("").to_string();
                } else {
                    while names.ends_with('\\') && i + 1 < lines.len() {
                        names.pop();
                        i += 1;
                        names.push(' ');
                        names.push_str(lines[i].trim());
                    }
                }
                let names: Vec<Name> = names
                    .split(',')
                    .filter_map(|p| name_and_alias(p.trim()))
                    .filter(|(n, _)| n == "*" || is_ident(n))
                    .map(|(n, a)| Name {
                        alias: a.unwrap_or_else(|| n.clone()),
                        name: n,
                    })
                    .collect();
                out.push(Import {
                    line: start + 1,
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
