//! The rules on compiler bugs: source shapes that a known Mojo compiler
//! bug turns into wrong code. Each is tied to an upstream issue, names the
//! versions it affects (in [`BUGS`], printed in its findings). A rule stays
//! once its bug is fixed, with the version that fixed it, for a project that
//! still pins an affected compiler, and goes when no one builds with one.
//!
//! 1. **`append-before-loop-exit`: no `while True:` loop that appends
//!    before its exit test.** Mojo 1.3.0.dev2026111905 to
//!    1.3.0.dev2026112105, at the default optimization level, copy the statements ahead of such a
//!    loop's exit test to the loop's end with the operands of the
//!    iteration before: the append stores through the List's buffer and
//!    length from before it grew, so elements are lost or shifted and
//!    freed memory is written
//!    ([modular/modular#7716](https://github.com/modular/modular/issues/7716)).
//!    Fixed in 1.3.0.dev2026112316, by modular/modular#7725.
//!
//! 2. **`work-before-counted-exit`: no `while True:` loop that counts to
//!    its exit test after other work.** Mojo 1.2.0 to 1.3.0.dev2026111805
//!    (`<` and `>`; `==` from 1.3.0.dev2026111105) turn a `while True:`
//!    loop that steps an `Int` by a constant and leaves on `if k == n:
//!    break` into a counted loop tested at the top, at every optimization
//!    level, without checking that the test came first: what comes before
//!    the test runs one time too few. Fixed in 1.3.0.dev2026111905, before
//!    an issue was filed.
//!
//! Each runs only when the project's compiler is in its bug's range
//! ([`crate::version`]).
//!
//! A `# noqa: RULE` on the `while True:` line allows a rule there.

use std::collections::{BTreeSet, HashSet};

use crate::check::Finding;
use crate::config::Rule;
use crate::lex::Stmt;
use crate::repo::{File, Repo};

/// A rule's compiler bug: the upstream issue, and the versions it's in.
pub struct Bug {
    pub rule: Rule,
    /// The upstream issue's URL, if there is one.
    pub issue: Option<&'static str>,
    /// The first version with the bug.
    pub from: &'static str,
    /// The first version without it again; `None` while it isn't fixed.
    pub fixed_in: Option<&'static str>,
    /// As a finding says it: "Mojo VERSIONS miscompile...".
    pub versions: &'static str,
}

pub const BUGS: [Bug; 2] = [
    Bug {
        rule: Rule::AppendBeforeLoopExit,
        issue: Some("https://github.com/modular/modular/issues/7716"),
        from: "1.3.0.dev2026111905",
        // Fixed by modular/modular#7725: wrong on 2026112105 (the last
        // nightly before it), right on 2026112316, 2026112405 and 2026112505.
        fixed_in: Some("1.3.0.dev2026112316"),
        versions: "1.3.0.dev2026111905 to 1.3.0.dev2026112105",
    },
    Bug {
        rule: Rule::WorkBeforeCountedExit,
        // No issue: it was fixed before one was filed.
        issue: None,
        from: "1.2.0",
        fixed_in: Some("1.3.0.dev2026111905"),
        versions: "1.2.0 to 1.3.0.dev2026111805 (with `==`, 1.3.0.dev2026111105 to 2026111805)",
    },
];

impl Bug {
    /// The issue's URL, or the version that fixed it.
    pub fn cite(&self) -> String {
        match (self.issue, self.fixed_in) {
            (Some(url), _) => url.to_string(),
            (None, Some(v)) => format!("fixed in {v}"),
            (None, None) => "not filed upstream".to_string(),
        }
    }
}

/// `rule`'s bug.
pub fn bug(rule: Rule) -> &'static Bug {
    BUGS.iter()
        .find(|b| b.rule == rule)
        .expect("a compiler-bugs rule has its bug")
}

/// `while True:`, `while 1:`, `while (True):`.
fn always(s: &Stmt) -> bool {
    if !s.compound || s.keyword() != "while" {
        return false;
    }
    let cond = s.text["while".len()..].trim();
    let cond = cond
        .strip_prefix('(')
        .and_then(|c| c.strip_suffix(')'))
        .unwrap_or(cond)
        .trim();
    matches!(cond, "True" | "1")
}

fn is_loop(s: &Stmt) -> bool {
    s.compound && matches!(s.keyword(), "for" | "while")
}

fn is_def(s: &Stmt) -> bool {
    s.compound && matches!(s.keyword(), "def" | "fn" | "struct" | "trait")
}

/// How a statement can leave the loop it's in, or its iteration: by a
/// `break` of that loop (not of a loop inside it), else by a `return`, a
/// `raise` or a `continue`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Exit {
    Other,
    Break,
}

fn exits(s: &Stmt, in_loop: bool) -> Option<Exit> {
    if is_def(s) {
        return None;
    }
    if !s.compound {
        return match s.keyword() {
            "break" if !in_loop => Some(Exit::Break),
            "continue" if !in_loop => Some(Exit::Other),
            "return" | "raise" => Some(Exit::Other),
            _ => None,
        };
    }
    let inner = in_loop || is_loop(s);
    s.body.iter().filter_map(|b| exits(b, inner)).max()
}

/// The receiver of each `.append(` in `text`, and its root name:
/// `self.items.append(x)` gives `self.items` and `self`.
fn appends(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(".append(") {
        let at = from + at;
        from = at + 1;
        // Back over `a.b[i].c` to its first name.
        let b: Vec<char> = text[..at].chars().collect();
        let mut j = b.len();
        let mut depth = 0;
        while j > 0 {
            let c = b[j - 1];
            match c {
                ']' | ')' => depth += 1,
                '[' | '(' if depth > 0 => depth -= 1,
                _ if depth > 0 => {}
                c if c == '_' || c == '.' || c.is_alphanumeric() => {}
                _ => break,
            }
            j -= 1;
        }
        let root: String = b[j..]
            .iter()
            .take_while(|c| **c == '_' || c.is_alphanumeric())
            .collect();
        if !root.is_empty() && !root.starts_with(|c: char| c.is_ascii_digit()) {
            out.push((b[j..].iter().collect(), root));
        }
    }
    out
}

/// The function or method a statement calls, if it may grow a container
/// it's given: a call whose result is dropped (`push(out, k)`,
/// `acc.add(k)`), or one assigned (`var n = r.read(buf)`) when it's a
/// method or is given a name. Not `print`, nor a type's constructor.
fn call_of(s: &Stmt) -> Option<&str> {
    if s.compound {
        return None;
    }
    let t = s.text.as_str();
    let (value, assigned) = match split_top_assign(t) {
        Some(v) => (v, true),
        None => (t, false),
    };
    let value = value.trim();
    if !value.ends_with(')') {
        return None;
    }
    // The `(` the last `)` closes: `a.b().c(x)` calls `a.b().c`.
    let mut depth = 0;
    let mut open = None;
    for (i, c) in value.char_indices().rev() {
        match c {
            ')' | ']' => depth += 1,
            '(' | '[' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let open = open?;
    let callee = value[..open].trim_end();
    let name_like = callee.starts_with(|c: char| c == '_' || c.is_alphabetic())
        && callee
            .chars()
            .all(|c| matches!(c, '_' | '.' | '(' | ')' | '[' | ']') || c.is_alphanumeric());
    let last = callee.rsplit('.').next().unwrap_or(callee);
    if !name_like
        || last.starts_with(|c: char| c.is_ascii_uppercase())
        || matches!(
            callee,
            "print" | "debug_assert" | "assert" | "len" | "min" | "max" | "abs"
        )
    {
        return None;
    }
    let args = &value[open + 1..value.len() - 1];
    let gives_name = args.split(',').any(|a| {
        let a = a.trim();
        a.starts_with(|c: char| c == '_' || c.is_alphabetic())
            && a.chars()
                .all(|c| c == '_' || c == '.' || c.is_alphanumeric())
    });
    (!assigned || callee.contains('.') || gives_name).then_some(callee)
}

/// What `a = v`, `var a = v` or `var a: T = v` assigns: `v`.
fn split_top_assign(t: &str) -> Option<&str> {
    let mut depth = 0i32;
    let b = t.as_bytes();
    for (i, c) in t.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0
                && b.get(i + 1) != Some(&b'=')
                && i > 0
                && !matches!(
                    b[i - 1],
                    b'=' | b'!'
                        | b'<'
                        | b'>'
                        | b'+'
                        | b'-'
                        | b'*'
                        | b'/'
                        | b'%'
                        | b'&'
                        | b'|'
                        | b'^'
                ) =>
            {
                return Some(&t[i + 1..]);
            }
            _ => {}
        }
    }
    None
}

/// The `var` names a statement declares: `var a = ...`, `var a: T`,
/// `var a, b = ...`.
fn declares(s: &Stmt) -> Vec<String> {
    let Some(rest) = s.text.strip_prefix("var ") else {
        return Vec::new();
    };
    let names = rest.split(['=', ':']).next().unwrap_or("");
    names
        .split(',')
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Each simple statement in `s` and its blocks, nested defs left out.
fn simple<'a>(s: &'a Stmt, out: &mut Vec<&'a Stmt>) {
    if is_def(s) {
        return;
    }
    if s.compound {
        for b in &s.body {
            simple(b, out);
        }
    } else {
        out.push(s);
    }
}

struct Ctx<'a> {
    f: &'a File,
    found: BTreeSet<Finding>,
    notes: BTreeSet<Finding>,
}

fn show(f: &File, s: &Stmt) -> String {
    format!("{}:{}: {}", f.display, s.line, f.quote(s.line))
}

/// One `while True:` loop.
fn check_loop(c: &mut Ctx, lp: &Stmt) {
    let r = Rule::AppendBeforeLoopExit;
    if c.f.noqa(&[lp.line], r.name(), r.group()) {
        return;
    }
    let body = &lp.body;
    // The first statement that can leave the loop, and the `if` of its
    // `elif`/`else` chain (or the `try` of its `except`).
    let Some(at) = body.iter().position(|s| exits(s, false).is_some()) else {
        return;
    };
    let first = &body[at];
    if !first.compound {
        // An unconditional `break`, `return` or `raise`: no test to move.
        return;
    }
    let kind = exits(first, false).expect("found");
    let mut head = at;
    while head > 0 && matches!(body[head].keyword(), "elif" | "else" | "except" | "finally") {
        head -= 1;
    }
    let test = &body[head];
    let bug = bug(r);
    let cite = bug.cite();
    let mut local: HashSet<String> = HashSet::new();
    let mut reported = false;
    for s in &body[..head] {
        if !s.compound {
            local.extend(declares(s));
        }
        let mut stmts = Vec::new();
        simple(s, &mut stmts);
        for st in stmts {
            let all = appends(&st.text);
            let grows: Vec<&String> = all
                .iter()
                .filter(|(_, root)| !local.contains(root))
                .map(|(r, _)| r)
                .collect();
            let nested = s.compound;
            let file = c.f.display.clone();
            match grows.first() {
                Some(name) if !nested && kind == Exit::Break => {
                    if reported {
                        continue;
                    }
                    reported = true;
                    c.found.insert(Finding {
                        file,
                        line: lp.line,
                        message: format!(
                            "this `{}:` loop appends to `{name}` before its exit test: Mojo {} miscompile it at the default optimization level, losing or shifting elements and writing freed memory ({}); append the first one before the loop and test at the top (`while k != last:`), or loop with `for`",
                            lp.text, bug.versions, cite
                        ),
                        detail: vec![show(c.f, st), show(c.f, test)],
                    });
                }
                Some(name) => {
                    let why = if nested {
                        "inside a block"
                    } else {
                        "its first exit a `return`, `raise` or `continue`"
                    };
                    c.notes.insert(Finding {
                        file,
                        line: st.line,
                        message: format!(
                            "append-before-loop-exit: `{name}.append` before the exit test of the `{}:` loop on line {}, {why}, which hasn't been seen to miscompile ({})",
                            lp.text, lp.line, cite
                        ),
                        detail: vec![],
                    });
                }
                None if all.is_empty() && !nested && call_of(st).is_some() => {
                    let call = call_of(st).expect("a call");
                    c.notes.insert(Finding {
                        file,
                        line: st.line,
                        message: format!(
                            "append-before-loop-exit: can't tell whether `{call}(...)` grows a container before the exit test of the `{}:` loop on line {} ({})",
                            lp.text, lp.line, cite
                        ),
                        detail: vec![],
                    });
                }
                None => {}
            }
        }
    }
}

fn walk(c: &mut Ctx, stmts: &[Stmt]) {
    for s in stmts {
        if always(s) {
            check_loop(c, s);
        }
        walk(c, &s.body);
    }
}

/// Rule 1: each `while True:` loop that appends before its exit test, and
/// (as notes) what may grow a container there.
pub fn append_before_loop_exit(repo: &Repo) -> (Vec<Finding>, Vec<Finding>) {
    let mut found = BTreeSet::new();
    let mut notes = BTreeSet::new();
    for f in repo.files.iter().filter(|f| f.local) {
        let mut c = Ctx {
            f,
            found: BTreeSet::new(),
            notes: BTreeSet::new(),
        };
        walk(&mut c, &crate::lex::statements(&f.text));
        found.extend(c.found);
        notes.extend(c.notes);
    }
    (found.into_iter().collect(), notes.into_iter().collect())
}

// Rule 2: work-before-counted-exit.

fn is_ident(s: &str) -> bool {
    s.starts_with(|c: char| c == '_' || c.is_alphabetic())
        && s.chars().all(|c| c == '_' || c.is_alphanumeric())
}

fn is_int(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '_')
}

/// What a simple statement assigns: its target, the operator (`=`,
/// `+=`...; `var` for a declaration) and the value.
fn assignment(t: &str) -> Option<(String, String, String)> {
    let (decl, rest) = match t.strip_prefix("var ") {
        Some(r) => (true, r),
        None => (false, t),
    };
    // The `=` of `=`, `+=`, `<<=`..., outside brackets, not of `==`, `!=`,
    // `<=`, `>=`.
    let b = rest.as_bytes();
    let mut depth = 0i32;
    let mut at = None;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                if b.get(i + 1) == Some(&b'=') {
                    return None;
                }
                let prev = if i > 0 { b[i - 1] } else { b' ' };
                let shift = i > 1 && (b[i - 2] == prev) && matches!(prev, b'<' | b'>');
                if matches!(prev, b'!' | b'=') || matches!(prev, b'<' | b'>') && !shift {
                    return None;
                }
                at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let at = at?;
    let mut lhs = rest[..at].trim_end();
    let value = rest[at + 1..].trim();
    let mut op = String::from("=");
    let ops = [
        "<<", ">>", "//", "**", "+", "-", "*", "/", "%", "&", "|", "^", "@",
    ];
    if let Some(o) = ops.iter().find(|o| lhs.ends_with(*o)) {
        lhs = lhs[..lhs.len() - o.len()].trim_end();
        op = format!("{o}=");
    }
    if decl {
        op = "var".into();
        lhs = lhs.split(':').next().unwrap_or("").trim();
    }
    Some((lhs.to_string(), op, value.to_string()))
}

/// The names `stmts` (and their blocks) assign or declare, and each
/// simple statement by line, nested defs left out.
fn assigned_in(stmts: &[Stmt]) -> Vec<(String, &Stmt)> {
    let mut all = Vec::new();
    for s in stmts {
        simple(s, &mut all);
    }
    all.into_iter()
        .filter_map(|st| assignment(&st.text).map(|(n, _, _)| (n, st)))
        .collect()
}

/// `cond` as `(a, op, b)`, `op` one of `==`, `<`, `>`, `!=`, `<=`, `>=`,
/// with `not (a != b)` read as `a == b` (and `>=`, `<=` likewise); `None`
/// for anything else (`and`, `or`, a chain, a call).
fn comparison(cond: &str) -> Option<(String, &'static str, String)> {
    let mut c = cond.trim();
    let mut negate = false;
    if let Some(r) = c.strip_prefix("not ").or_else(|| c.strip_prefix("not(")) {
        negate = true;
        c = if c.starts_with("not(") {
            &c[3..]
        } else {
            r.trim()
        };
    }
    while c.starts_with('(') && c.ends_with(')') && split_top(c).len() == 1 {
        c = c[1..c.len() - 1].trim();
    }
    let words: Vec<&str> = c.split_whitespace().collect();
    if words
        .iter()
        .any(|w| matches!(*w, "and" | "or" | "not" | "in" | "is"))
    {
        return None;
    }
    let mut found = None;
    let mut depth = 0i32;
    let b = c.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' | b'!' | b'<' | b'>' if depth == 0 => {
                let two = b.get(i + 1) == Some(&b'=');
                let op = match (b[i], two) {
                    (b'=', true) => "==",
                    (b'!', true) => "!=",
                    (b'<', true) => "<=",
                    (b'>', true) => ">=",
                    (b'<', false) if b.get(i + 1) != Some(&b'<') => "<",
                    (b'>', false) if b.get(i + 1) != Some(&b'>') => ">",
                    _ => {
                        i += 2;
                        continue;
                    }
                };
                if found.is_some() {
                    return None;
                }
                found = Some((i, op));
                i += op.len();
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let (at, raw) = found?;
    let op = if negate {
        match raw {
            "!=" => "==",
            ">=" => "<",
            "<=" => ">",
            _ => return None,
        }
    } else {
        raw
    };
    let (a, b) = (c[..at].trim(), c[at + raw.len()..].trim());
    Some((a.to_string(), op, b.to_string()))
}

/// `s` cut into its bracketed groups and what's between them, to tell
/// `(a == b)` (one part) from `(a) == (b)`.
fn split_top(s: &str) -> Vec<&str> {
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut from = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    parts.push(&s[from..=i]);
                    from = i + 1;
                }
            }
            _ => {}
        }
    }
    if from < s.len() {
        parts.push(&s[from..]);
    }
    parts
}

/// A parameter's or a `var`'s type as declared, if it says: `UInt8` for
/// `var k: UInt8 = 1` and `var k = UInt8(1)`; `Int` for `var k = 1`.
fn declared_type(func: Option<&Stmt>, before: usize, name: &str) -> Option<Option<String>> {
    let func = func?;
    let mut all = Vec::new();
    for b in &func.body {
        simple(b, &mut all);
    }
    let decl = all
        .iter()
        .rev()
        .filter(|st| st.line < before)
        .find(|st| declares(st).iter().any(|n| n == name));
    if let Some(st) = decl {
        let rest = st.text.strip_prefix("var ").unwrap_or("");
        let (lhs, value) = match split_top_assign(rest) {
            Some(v) => (&rest[..rest.len() - v.len() - 1], v.trim()),
            None => (rest, ""),
        };
        if let Some((_, ty)) = lhs.split_once(':') {
            return Some(Some(ty.trim().to_string()));
        }
        if is_int(value) || value.starts_with('-') && is_int(value[1..].trim()) {
            return Some(Some("Int".into()));
        }
        let callee: String = value
            .chars()
            .take_while(|c| *c == '_' || *c == '.' || c.is_alphanumeric())
            .collect();
        if value[callee.len()..].starts_with(['(', '['])
            && !callee.contains('.')
            && callee.starts_with(|c: char| c.is_ascii_uppercase())
        {
            return Some(Some(callee));
        }
        return Some(None);
    }
    // A parameter: `def f(mut k: Int, n: Int = 3)`.
    let open = func.text.find('(')?;
    let params = &func.text[open + 1..];
    for p in crate::lex::split_top_commas(params) {
        let p = p.trim().trim_end_matches(')').trim();
        let p = ["mut ", "var ", "owned ", "read ", "ref ", "out "]
            .iter()
            .find_map(|k| p.strip_prefix(k))
            .unwrap_or(p);
        let (n, ty) = p.split_once(':').unwrap_or((p, ""));
        if n.trim() == name {
            let ty = ty.split('=').next().unwrap_or("").trim();
            return Some((!ty.is_empty()).then(|| ty.to_string()));
        }
    }
    None
}

/// Whether a type is a signed integer the bug applies to; `None`: unknown.
fn signed_int(ty: &Option<String>) -> Option<bool> {
    let t = ty.as_deref()?;
    Some(matches!(
        t,
        "Int" | "Int8" | "Int16" | "Int32" | "Int64" | "Int128" | "Int256" | "Scalar[DType.index]"
    ))
}

fn scalar_type(ty: &Option<String>) -> bool {
    ty.as_deref().is_some_and(|t| {
        signed_int(&Some(t.to_string())) == Some(true)
            || t.starts_with("UInt")
            || t.starts_with("Float")
            || t == "Bool"
    })
}

/// `v += c`, `v -= c`, `v = v + c`, `v = v - c` with `c` an integer: `c`.
fn step(target: &str, op: &str, value: &str, v: &str) -> Option<String> {
    if target != v {
        return None;
    }
    match op {
        "+=" | "-=" if is_int(value) => Some(value.to_string()),
        "=" => {
            let rest = value.strip_prefix(v)?.trim_start();
            let c = rest.strip_prefix(['+', '-'])?.trim();
            is_int(c).then(|| c.to_string())
        }
        _ => None,
    }
}

/// How many `break`s and `continue`s of the loop `stmts` hold.
fn jumps(stmts: &[Stmt]) -> (usize, usize) {
    let mut n = (0, 0);
    for s in stmts {
        if is_def(s) || is_loop(s) {
            continue;
        }
        match (s.compound, s.keyword()) {
            (false, "break") => n.0 += 1,
            (false, "continue") => n.1 += 1,
            (true, _) => {
                let (b, c) = jumps(&s.body);
                n = (n.0 + b, n.1 + c);
            }
            _ => {}
        }
    }
    n
}

fn only(body: &[Stmt], word: &str) -> bool {
    body.len() == 1 && !body[0].compound && body[0].text == word
}

/// What makes a loop of the shape uncertain, for a note.
enum Counted {
    Report {
        test: usize,
        step: usize,
        work: usize,
        v: String,
        op: &'static str,
        other: String,
        /// Stepped by making a new value (`k = k.next()`), not by a number.
        remade: bool,
    },
    Unsure(String),
}

fn counted(lp: &Stmt, func: Option<&Stmt>) -> Option<Counted> {
    let body = &lp.body;
    if jumps(body) != (1, 0) {
        return None;
    }
    // The test: `if C:` whose block ends with the `break`, no `elif`, and
    // an `else` only of `pass`; or `if C: pass` / `else: break`.
    let (test, cond) = body.iter().enumerate().find_map(|(i, s)| {
        if !s.compound || s.keyword() != "if" {
            return None;
        }
        let next = body.get(i + 1);
        let next_kw = next.filter(|n| n.compound).map(|n| n.keyword());
        let cond = s.text["if".len()..].trim().to_string();
        let ends = s
            .body
            .last()
            .is_some_and(|b| !b.compound && b.text == "break");
        if ends
            && (next_kw.is_none_or(|k| k != "elif" && k != "else")
                || next_kw == Some("else") && only(&next.unwrap().body, "pass"))
        {
            return Some((i, cond));
        }
        if only(&s.body, "pass") && next_kw == Some("else") && only(&next.unwrap().body, "break") {
            return Some((i, cond));
        }
        None
    })?;
    // `var done = k == n` before `if done:`.
    let mut cond = cond;
    let mut flag = None;
    if is_ident(&cond) {
        let (j, value) = body[..test].iter().enumerate().find_map(|(j, s)| {
            let (n, op, v) = assignment(&s.text).filter(|_| !s.compound)?;
            (n == cond && op == "var").then_some((j, v))
        })?;
        flag = Some(j);
        cond = value;
    }
    let (a, op, b) = comparison(&cond)?;
    if !matches!(op, "==" | "<" | ">") {
        return None;
    }
    let assigns = assigned_in(body);
    let top_assign = |j: usize| assignment(&body[j].text).filter(|_| !body[j].compound);
    for (side, other) in [(&a, &b), (&b, &a)] {
        if !is_ident(side) {
            continue;
        }
        // `var cur = k` before the test: `cur` is `k` as the iteration began.
        let alias = body[..test].iter().enumerate().find_map(|(j, s)| {
            let (n, o, v) = assignment(&s.text).filter(|_| !s.compound)?;
            (n == *side && o == "var" && is_ident(&v)).then_some((j, v))
        });
        let (v, from) = match &alias {
            Some((j, src)) => (src.clone(), *j + 1),
            None => (side.clone(), test + 1),
        };
        if alias.is_some() && assigns.iter().filter(|(n, _)| n == side).count() != 1 {
            continue;
        }
        let to_v: Vec<&(String, &Stmt)> = assigns.iter().filter(|(n, _)| *n == v).collect();
        if to_v.len() != 1 {
            continue;
        }
        let Some(at) = (from..body.len()).find(|&j| std::ptr::eq(&body[j], to_v[0].1)) else {
            continue;
        };
        if at == test {
            continue;
        }
        let Some((t, o, val)) = top_assign(at) else {
            continue;
        };
        // `k += 1`; or, for a key that isn't a number, `k = k.next()` or
        // `k = Day(k.n + 1)`: a new value made from the old one.
        let by = step(&t, &o, &val, &v);
        let remade = by.is_none()
            && t == v
            && o == "="
            && val
                .split(|c: char| !(c == '_' || c.is_alphanumeric()))
                .any(|w| w == v);
        if by.is_none() && !remade {
            continue;
        }
        if op == "=="
            && by
                .as_ref()
                .is_some_and(|c| c.trim_start_matches('0') != "1")
        {
            continue;
        }
        let Some(ty) = declared_type(func, lp.line, &v) else {
            continue;
        };
        if by.is_some() && signed_int(&ty) == Some(false) {
            continue;
        }
        // The other side: unchanged by the loop.
        let names: Vec<String> = other
            .split(|c: char| !(c == '_' || c.is_alphanumeric()))
            .filter(|w| is_ident(w))
            .map(str::to_string)
            .collect();
        if names
            .iter()
            .any(|n| *n == v || n == side || assigns.iter().any(|(a, _)| a == n))
        {
            continue;
        }
        let work: Vec<usize> = (0..test)
            .filter(|&j| Some(j) != flag && alias.as_ref().is_none_or(|(a, _)| *a != j))
            .collect();
        let Some(&first) = work.first() else {
            continue;
        };
        if other.contains('(') {
            return Some(Counted::Unsure(format!(
                "the bound `{other}` is a call, which may change"
            )));
        }
        // A value the loop makes before the test that has a destructor (a
        // `List`, a `String`, a struct) kept such a loop right.
        for s in &body[..test] {
            let Some((n, o, val)) = assignment(&s.text).filter(|_| !s.compound) else {
                continue;
            };
            let callee: String = val
                .chars()
                .take_while(|c| *c == '_' || *c == '.' || c.is_alphanumeric())
                .collect();
            let owned = o == "var"
                && callee.starts_with(|c: char| c.is_ascii_uppercase())
                && val[callee.len()..].starts_with(['(', '['])
                && !scalar_type(&Some(callee.clone()));
            if owned {
                return Some(Counted::Unsure(format!(
                    "`{n}` is made in the loop with a destructor (`{callee}`), which kept such a loop right"
                )));
            }
        }
        // An outer scalar updated before the test keeps the loop as it is.
        for (n, st) in assigns.iter().filter(|(_, st)| st.line < body[test].line) {
            if *n == v || n == side || !is_ident(n) {
                continue;
            }
            let local = body[..test]
                .iter()
                .any(|s| !s.compound && declares(s).contains(n));
            if local || assignment(&st.text).is_some_and(|(_, o, _)| o == "var") {
                continue;
            }
            match declared_type(func, lp.line, n) {
                Some(t) if scalar_type(&t) => return None,
                Some(Some(t)) if !t.is_empty() => {}
                _ => {
                    return Some(Counted::Unsure(format!(
                        "`{n}` changes before the test, and if it's a number the compiler keeps the loop as it is"
                    )))
                }
            }
        }
        // An `if` on the counter before the test.
        for j in &work {
            let s = &body[*j];
            if s.compound
                && s.keyword() == "if"
                && s.text
                    .split(|c: char| !(c == '_' || c.is_alphanumeric()))
                    .any(|w| w == v || w == side)
            {
                let mut inner = Vec::new();
                simple(s, &mut inner);
                let exits_only = inner
                    .iter()
                    .all(|b| matches!(b.keyword(), "raise" | "return"));
                if !exits_only {
                    return Some(Counted::Unsure(format!(
                        "`{}` tests the counter before the exit test, which kept some such loops right",
                        s.text
                    )));
                }
            }
        }
        return Some(Counted::Report {
            test,
            step: at,
            work: first,
            v,
            op,
            other: other.clone(),
            remade,
        });
    }
    None
}

fn walk_counted(c: &mut Ctx, stmts: &[Stmt], func: Option<&Stmt>) {
    let r = Rule::WorkBeforeCountedExit;
    let bug = bug(r);
    for s in stmts {
        if always(s) && !c.f.noqa(&[s.line], r.name(), r.group()) {
            match counted(s, func) {
                Some(Counted::Report {
                    test,
                    step,
                    work,
                    v,
                    op,
                    other,
                    remade,
                }) => {
                    let st = &s.body[step].text;
                    let down = st.contains("-=") || st.contains(&format!("= {v} -"));
                    let top = match (op, down) {
                        ("==", false) => format!("`while {v} <= {other}:`"),
                        ("==", true) => format!("`while {v} >= {other}:`"),
                        _ => "a `while` on the bound".to_string(),
                    };
                    let fix = if remade {
                        format!("do the first item before the loop and `while {v} != {other}:`")
                    } else {
                        format!("test at the top ({top}, or a `for` over a `range`), or do the first item before the loop and `while {v} != last:`")
                    };
                    c.found.insert(Finding {
                        file: c.f.display.clone(),
                        line: s.line,
                        message: format!(
                            "this `{}:` loop steps `{v}` and leaves on `{v} {op} {other}` after other work: Mojo {} compile it as a loop tested at the top, at every optimization level, so the work before the test runs one time too few ({}); {fix}",
                            s.text, bug.versions, bug.cite()
                        ),
                        detail: vec![show(c.f, &s.body[work]), show(c.f, &s.body[test]), show(c.f, &s.body[step])],
                    });
                }
                Some(Counted::Unsure(why)) => {
                    c.notes.insert(Finding {
                        file: c.f.display.clone(),
                        line: s.line,
                        message: format!(
                            "work-before-counted-exit: this `{}:` loop counts to its exit test after other work, but {why}",
                            s.text
                        ),
                        detail: vec![],
                    });
                }
                None => {}
            }
        }
        let inner = if is_def(s) && matches!(s.keyword(), "def" | "fn") {
            Some(s)
        } else {
            func
        };
        walk_counted(c, &s.body, inner);
    }
}

/// Rule 2: each `while True:` loop that counts to its exit test after other
/// work, and (as notes) those of the shape it can't be sure of.
pub fn work_before_counted_exit(repo: &Repo) -> (Vec<Finding>, Vec<Finding>) {
    let mut found = BTreeSet::new();
    let mut notes = BTreeSet::new();
    for f in repo.files.iter().filter(|f| f.local) {
        let mut c = Ctx {
            f,
            found: BTreeSet::new(),
            notes: BTreeSet::new(),
        };
        walk_counted(&mut c, &crate::lex::statements(&f.text), None);
        found.extend(c.found);
        notes.extend(c.notes);
    }
    (found.into_iter().collect(), notes.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{statements, strip};

    /// The loops of `src` it reports, by line, and the lines of its notes.
    fn check(src: &str) -> (Vec<usize>, Vec<usize>) {
        run(src, false)
    }

    /// The same, for work-before-counted-exit.
    fn counted_lines(src: &str) -> (Vec<usize>, Vec<usize>) {
        run(src, true)
    }

    fn run(src: &str, counted: bool) -> (Vec<usize>, Vec<usize>) {
        let text = strip(src);
        let (_, comments) = crate::lex::strip_keeping_comments(src);
        let f = File {
            path: "t.mojo".into(),
            display: "t.mojo".into(),
            module: None,
            is_pkg: false,
            local: true,
            imports: Vec::new(),
            defs: Default::default(),
            noqa: comments
                .iter()
                .filter_map(|(l, c)| crate::lex::noqa(c).map(|n| (*l, n)))
                .collect(),
            source: src.to_string(),
            text,
        };
        let mut c = Ctx {
            f: &f,
            found: BTreeSet::new(),
            notes: BTreeSet::new(),
        };
        if counted {
            walk_counted(&mut c, &statements(&f.text), None);
        } else {
            walk(&mut c, &statements(&f.text));
        }
        (
            c.found.iter().map(|x| x.line).collect(),
            c.notes.iter().map(|x| x.line).collect(),
        )
    }

    fn wrap(body: &str) -> String {
        format!("def f(last: Int) -> List[Int]:\n    var out = List[Int]()\n    var k = 1\n{body}    return out^\n")
    }

    #[test]
    fn the_shape_of_7716() {
        // The loop of the issue, and the same with the test at its end, the
        // break in an `else`, `while 1:`, and an append through `self`.
        for body in [
            "    while True:\n        out.append(k)\n        if k == last:\n            break\n        k += 1\n",
            "    while True:\n        out.append(k)\n        k += 1\n        if k > last: break\n",
            "    while True:\n        out.append(k)\n        if k != last:\n            k += 1\n        else:\n            break\n",
            "    while 1:\n        out.append(k)\n        if k == last:\n            break\n        k += 1\n",
            "    while (True):\n        self.items[0].append(k); k += 1\n        if k > last:\n            break\n",
        ] {
            assert_eq!(check(&wrap(body)), (vec![4], vec![]), "{body}");
        }
    }

    #[test]
    fn safe_shapes_are_not_reported() {
        for body in [
            // Tested at the top; a `for`; the test before the append.
            "    out.append(k)\n    while k != last:\n        k += 1\n        out.append(k)\n",
            "    for k in range(last):\n        out.append(k)\n        if k == last:\n            break\n",
            "    while True:\n        if k > last:\n            break\n        out.append(k)\n        k += 1\n",
            // Not always true.
            "    while not done:\n        out.append(k)\n        if k == last:\n            break\n",
            // The append on the exit's own path; a list of the loop's own.
            "    while True:\n        if k == last:\n            out.append(k)\n            break\n        out.append(k)\n        k += 1\n",
            "    while True:\n        var tmp = List[Int]()\n        tmp.append(k)\n        if k == last:\n            break\n",
            // Only an inner loop's `break`; an unconditional one.
            "    while True:\n        out.append(k)\n        for j in range(3):\n            if j == k:\n                break\n        break\n",
            // In a string or a comment.
            "    while True:\n        # out.append(k)\n        print(\"out.append(k)\")\n        if k == last:\n            break\n",
            // Allowed on the loop's line.
            "    while True:  # noqa: append-before-loop-exit\n        out.append(k)\n        if k == last:\n            break\n",
        ] {
            assert_eq!(check(&wrap(body)), (vec![], vec![]), "{body}");
        }
    }

    #[test]
    fn what_it_cant_tell_is_a_note() {
        // A call that may append; an append in a block, or before a
        // `continue`, `return` or `raise` that comes first.
        let (found, notes) = check(&wrap(
            "    while True:\n        push(out, k)\n        acc.add(k)\n        var n = r.read(buf)\n        if k == last:\n            break\n",
        ));
        assert_eq!((found, notes), (vec![], vec![5, 6, 7]));
        let (found, notes) = check(&wrap(
            "    while True:\n        for j in range(2):\n            out.append(j)\n        if k == last:\n            break\n",
        ));
        assert_eq!((found, notes), (vec![], vec![6]));
        for exit in ["continue", "return out^", "raise Error()"] {
            let (found, notes) = check(&wrap(&format!(
                "    while True:\n        out.append(k)\n        if k < last:\n            {exit}\n        break\n",
            )));
            assert_eq!((found, notes), (vec![], vec![5]), "{exit}");
        }
    }

    #[test]
    fn nested_loops_each_on_their_own() {
        let (found, _) = check(
            "def f():\n    while True:\n        while True:\n            out.append(1)\n            if a:\n                break\n        if b:\n            break\n",
        );
        assert_eq!(found, vec![3]);
    }

    #[test]
    fn receivers() {
        assert_eq!(
            appends("self.doc.nodes[i].keys.append(x); out.append(f(y.append(z)))"),
            vec![
                ("self.doc.nodes[i].keys".to_string(), "self".to_string()),
                ("out".to_string(), "out".to_string()),
                ("y".to_string(), "y".to_string()),
            ]
        );
    }

    /// `def f(n: Int)` around `body`, with `out` and `k` declared first.
    fn counting(body: &str) -> String {
        format!(
            "def f(n: Int, mut q: List[Int]):\n    var out = List[Int]()\n    var k = 1\n{body}"
        )
    }

    #[test]
    fn counted_loops_with_work_before_the_test() {
        for body in [
            "    while True:\n        out.append(k)\n        if k == n:\n            break\n        k += 1\n",
            "    while True:\n        out.append(k)\n        if k > n: break\n        k = k + 3\n",
            "    while True:\n        q.append(k)\n        if n < k:\n            q.append(0)\n            break\n        k -= 1\n",
            "    while 1:\n        out[k] = k\n        if not (k != n):\n            break\n        k += 1\n",
            "    while True:\n        out.append(k)\n        if k < n:\n            pass\n        else:\n            break\n        k += 1\n",
            "    while True:\n        out.append(k)\n        var done = k == n\n        if done:\n            break\n        k += 1\n",
            "    while True:\n        var cur = k\n        k += 1\n        out.append(cur)\n        if cur == n:\n            break\n",
            "    while True:\n        if k > 1000:\n            raise Error()\n        out.append(k)\n        if k == n:\n            break\n        k += 1\n",
        ] {
            assert_eq!(counted_lines(&counting(body)), (vec![4], vec![]), "{body}");
        }
        // A typed signed counter, a parameter's.
        let src = "def f(n: Int64, var k: Int64):\n    var s = String()\n    while True:\n        s += String(k)\n        if k == n:\n            break\n        k += 1\n";
        assert_eq!(counted_lines(src), (vec![3], vec![]));
    }

    #[test]
    fn counted_loops_the_compiler_gets_right() {
        for body in [
            // `>=`, `<=`, `!=`; the test first; the test after the step.
            "    while True:\n        out.append(k)\n        if k >= n:\n            break\n        k += 1\n",
            "    while True:\n        out.append(k)\n        if k != n:\n            pass\n        else:\n            break\n        k += 1\n",
            "    while True:\n        if k == n:\n            break\n        out.append(k)\n        k += 1\n",
            "    while True:\n        out.append(k)\n        k += 1\n        if k == n:\n            break\n",
            // `==` stepping by 2; a bound the loop changes; a second exit.
            "    while True:\n        out.append(k)\n        if k == n:\n            break\n        k += 2\n",
            "    while True:\n        out.append(k)\n        if k == n:\n            break\n        k += 1\n        n -= 1\n",
            "    while True:\n        out.append(k)\n        if k == n:\n            break\n        if k == 9:\n            continue\n        k += 1\n",
            "    while True:\n        out.append(k)\n        if k == n:\n            break\n        elif k == 3:\n            out.append(0)\n        k += 1\n",
            // A number the loop carries out, updated before the test.
            "    var total = 0\n    while True:\n        out.append(k)\n        total += k\n        if k == n:\n            break\n        k += 1\n",
            // Nothing before the test but the copy.
            "    while True:\n        var cur = k\n        if cur == n:\n            break\n        k += 1\n",
        ] {
            assert_eq!(counted_lines(&counting(body)), (vec![], vec![]), "{body}");
        }
        // Unsigned, or a struct.
        for decl in ["var k: UInt8 = 1", "var k = UInt(1)", "var k = Day(1)"] {
            let src = format!("def f(n: Int):\n    {decl}\n    while True:\n        out.append(k)\n        if k == n:\n            break\n        k += 1\n");
            assert_eq!(counted_lines(&src), (vec![], vec![]), "{decl}");
        }
    }

    #[test]
    fn a_key_made_anew_from_the_last_one() {
        // morca's `parse_keys`, and a struct around an `Int`.
        let src = "def f(first: Day, last: Day) -> List[Day]:\n    var out = List[Day]()\n    var k = first.copy()\n    while True:\n        out.append(k.copy())\n        if k == last:\n            break\n        k = k.next()\n    return out^\n";
        assert_eq!(counted_lines(src), (vec![4], vec![]));
        let src = "def f(n: Int):\n    var k = Day(1)\n    var end = Day(n)\n    while True:\n        out.append(k.copy())\n        if k == end:\n            break\n        k = Day(k.n + 1)\n";
        assert_eq!(counted_lines(src), (vec![4], vec![]));
        // Not made from the last one; reassigned twice.
        let src = "def f(n: Int):\n    var k = Day(1)\n    while True:\n        out.append(1)\n        if k == n:\n            break\n        k = Day(2)\n";
        assert_eq!(counted_lines(src), (vec![], vec![]));
        let src = "def f(n: Int):\n    var k = Day(1)\n    while True:\n        out.append(1)\n        if k == n:\n            break\n        k = k.next()\n        k = k.next()\n";
        assert_eq!(counted_lines(src), (vec![], vec![]));
    }

    #[test]
    fn counted_loops_it_cant_be_sure_of_are_notes() {
        for body in [
            "    while True:\n        if k % 2 == 1:\n            out.append(k)\n        if k == n:\n            break\n        k += 1\n",
            "    while True:\n        out.append(k)\n        if k == len(q):\n            break\n        k += 1\n",
            "    while True:\n        var tmp = List[Int]()\n        out.append(k)\n        if k == n:\n            break\n        k += 1\n",
        ] {
            assert_eq!(counted_lines(&counting(body)), (vec![], vec![4]), "{body}");
        }
    }

    #[test]
    fn comparisons() {
        assert_eq!(
            comparison("not (k != n)"),
            Some(("k".into(), "==", "n".into()))
        );
        assert_eq!(
            comparison("not (a >= b + 1)"),
            Some(("a".into(), "<", "b + 1".into()))
        );
        assert_eq!(
            comparison("(k > m - 1)"),
            Some(("k".into(), ">", "m - 1".into()))
        );
        assert_eq!(comparison("a == b and c"), None);
        assert_eq!(comparison("a < b < c"), None);
        assert_eq!(
            comparison("a << 2 == b"),
            Some(("a << 2".into(), "==", "b".into()))
        );
        assert_eq!(
            assignment("k += 1"),
            Some(("k".into(), "+=".into(), "1".into()))
        );
        assert_eq!(
            assignment("var x: Int = y"),
            Some(("x".into(), "var".into(), "y".into()))
        );
        assert_eq!(
            assignment("a[i] <<= 2"),
            Some(("a[i]".into(), "<<=".into(), "2".into()))
        );
        assert_eq!(assignment("f(a == b)"), None);
    }
}
