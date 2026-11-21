//! The rules on compiler bugs: source shapes that a known Mojo compiler
//! bug turns into wrong code. Each is tied to an upstream issue, names the
//! versions it affects (in [`BUGS`], printed in its findings), and is to be
//! deleted when the issue is fixed.
//!
//! 1. **`append-before-loop-exit`: no `while True:` loop that appends
//!    before its exit test.** Mojo 1.3.0.dev2026111905 and later, at the
//!    default optimization level, copy the statements ahead of such a
//!    loop's exit test to the loop's end with the operands of the
//!    iteration before: the append stores through the List's buffer and
//!    length from before it grew, so elements are lost or shifted and
//!    freed memory is written
//!    ([modular/modular#7716](https://github.com/modular/modular/issues/7716)).
//!
//! A `# noqa: append-before-loop-exit` on the `while True:` line allows it.

use std::collections::{BTreeSet, HashSet};

use crate::check::Finding;
use crate::config::Rule;
use crate::lex::Stmt;
use crate::repo::{File, Repo};

/// A rule's compiler bug: the upstream issue, and the versions it's in.
pub struct Bug {
    pub rule: Rule,
    pub issue: &'static str,
    /// As a finding says it: "Mojo VERSIONS miscompile...".
    pub versions: &'static str,
}

pub const BUGS: [Bug; 1] = [Bug {
    rule: Rule::AppendBeforeLoopExit,
    issue: "https://github.com/modular/modular/issues/7716",
    versions: "1.3.0.dev2026111905 and later",
}];

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
                            lp.text, bug.versions, bug.issue
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
                            lp.text, lp.line, bug.issue
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
                            lp.text, lp.line, bug.issue
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{statements, strip};

    /// The loops of `src` it reports, by line, and the lines of its notes.
    fn check(src: &str) -> (Vec<usize>, Vec<usize>) {
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
        walk(&mut c, &statements(&f.text));
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
}
