//! Just enough TOML to read `pixi.toml`: its tables, keys, strings, arrays
//! and inline tables, each key with its line. Anything else (numbers,
//! booleans, dates) is kept as its text. It doesn't validate: a line it
//! can't read is skipped, as the rest of customs skips what it can't place.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Arr(Vec<Value>),
    Table(Vec<(String, Value)>),
    /// A number, boolean or date, as written.
    Other(String),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The value of `key`, in an inline table.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Table(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// `key = value` under the table header `table`, with the 1-based line of
/// the key. A dotted key is split: `a.b = 1` under `[t]` has key `[a, b]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub table: Vec<String>,
    pub key: Vec<String>,
    pub value: Value,
    pub line: usize,
}

struct P<'a> {
    s: &'a [char],
    i: usize,
    line: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, lit: &str) -> bool {
        lit.chars()
            .enumerate()
            .all(|(k, c)| self.s.get(self.i + k) == Some(&c))
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    /// Spaces and tabs.
    fn blank(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
    }

    /// Spaces, newlines and comments, as inside an array.
    fn space(&mut self) {
        loop {
            self.blank();
            match self.peek() {
                Some('\n') => {
                    self.bump();
                }
                Some('#') => self.rest_of_line(),
                _ => break,
            }
        }
    }

    fn rest_of_line(&mut self) {
        while !matches!(self.peek(), None | Some('\n')) {
            self.bump();
        }
    }

    fn key_part(&mut self) -> Option<String> {
        match self.peek()? {
            '"' => self.basic(),
            '\'' => self.literal(),
            _ => {
                let mut k = String::new();
                while let Some(c) = self.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '-' {
                        k.push(c);
                        self.bump();
                    } else {
                        break;
                    }
                }
                (!k.is_empty()).then_some(k)
            }
        }
    }

    /// `a.b."c"`, up to what follows it.
    fn key(&mut self) -> Option<Vec<String>> {
        let mut parts = Vec::new();
        loop {
            self.blank();
            parts.push(self.key_part()?);
            self.blank();
            if self.peek() == Some('.') {
                self.bump();
            } else {
                return Some(parts);
            }
        }
    }

    fn escape(&mut self, out: &mut String) {
        match self.bump() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('e') => out.push('\u{1b}'),
            Some(c @ ('u' | 'U')) => {
                let n = if c == 'u' { 4 } else { 8 };
                let hex: String = (0..n).filter_map(|_| self.bump()).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(c) => out.push(c),
            None => {}
        }
    }

    fn basic(&mut self) -> Option<String> {
        if self.at("\"\"\"") {
            self.i += 3;
            if self.peek() == Some('\n') {
                self.bump();
            }
            let mut out = String::new();
            loop {
                if self.at("\"\"\"") && !self.at("\"\"\"\"") {
                    self.i += 3;
                    return Some(out);
                }
                match self.bump()? {
                    '\\' => {
                        // A backslash at the end of a line joins the next.
                        let save = (self.i, self.line);
                        self.blank();
                        if self.peek() == Some('\n') {
                            while matches!(self.peek(), Some(' ' | '\t' | '\r' | '\n')) {
                                self.bump();
                            }
                        } else {
                            (self.i, self.line) = save;
                            self.escape(&mut out);
                        }
                    }
                    c => out.push(c),
                }
            }
        }
        self.bump();
        let mut out = String::new();
        loop {
            match self.bump()? {
                '"' => return Some(out),
                '\\' => self.escape(&mut out),
                '\n' => return None,
                c => out.push(c),
            }
        }
    }

    fn literal(&mut self) -> Option<String> {
        if self.at("'''") {
            self.i += 3;
            if self.peek() == Some('\n') {
                self.bump();
            }
            let mut out = String::new();
            loop {
                if self.at("'''") && !self.at("''''") {
                    self.i += 3;
                    return Some(out);
                }
                out.push(self.bump()?);
            }
        }
        self.bump();
        let mut out = String::new();
        loop {
            match self.bump()? {
                '\'' => return Some(out),
                '\n' => return None,
                c => out.push(c),
            }
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.blank();
        match self.peek()? {
            '"' => self.basic().map(Value::Str),
            '\'' => self.literal().map(Value::Str),
            '[' => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.space();
                    match self.peek()? {
                        ']' => {
                            self.bump();
                            return Some(Value::Arr(items));
                        }
                        ',' => {
                            self.bump();
                        }
                        _ => items.push(self.value()?),
                    }
                }
            }
            '{' => {
                self.bump();
                let mut kv = Vec::new();
                loop {
                    self.space();
                    match self.peek()? {
                        '}' => {
                            self.bump();
                            return Some(Value::Table(kv));
                        }
                        ',' => {
                            self.bump();
                        }
                        _ => {
                            let k = self.key()?;
                            self.blank();
                            if self.bump()? != '=' {
                                return None;
                            }
                            let v = self.value()?;
                            // `a.b = 1` inside braces: nest it.
                            let mut v = v;
                            for part in k[1..].iter().rev() {
                                v = Value::Table(vec![(part.clone(), v)]);
                            }
                            kv.push((k[0].clone(), v));
                        }
                    }
                }
            }
            _ => {
                let mut t = String::new();
                while let Some(c) = self.peek() {
                    if matches!(c, ',' | ']' | '}' | '\n' | '#') {
                        break;
                    }
                    t.push(c);
                    self.bump();
                }
                let t = t.trim().to_string();
                (!t.is_empty()).then_some(Value::Other(t))
            }
        }
    }
}

/// Every `key = value` of `src`, in order.
pub fn entries(src: &str) -> Vec<Entry> {
    let chars: Vec<char> = src.chars().collect();
    let mut p = P {
        s: &chars,
        i: 0,
        line: 1,
    };
    let mut table: Vec<String> = Vec::new();
    let mut out = Vec::new();
    loop {
        p.space();
        let Some(c) = p.peek() else { break };
        let line = p.line;
        if c == '[' {
            let double = p.at("[[");
            p.i += if double { 2 } else { 1 };
            match p.key() {
                Some(k) if p.at(if double { "]]" } else { "]" }) => table = k,
                _ => table = vec!["?".into()],
            }
            p.rest_of_line();
            continue;
        }
        let start = p.i;
        let parsed = p.key().and_then(|k| {
            p.blank();
            (p.bump()? == '=').then_some(())?;
            Some((k, p.value()?))
        });
        match parsed {
            Some((key, value)) => out.push(Entry {
                table: table.clone(),
                key,
                value,
                line,
            }),
            None => {
                // Not something it reads: on to the next line.
                if p.i == start {
                    p.bump();
                }
                p.rest_of_line();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    #[test]
    fn tables_keys_and_values() {
        let e = entries(
            "# top\nname = \"x\"  # c\n[tasks]\na = \"mojo run t.mojo\"\nb = { cmd = 'sh x.sh', depends-on = [\"a\",\n  \"c\", # why\n] }\n\n[feature.dev.tasks.c]\ncmd = [\"mojo\", \"run\"]\n[tool.customs]\nmojo-run-per-test-threshold = 5\nlints.tests = \"warn\"\n",
        );
        assert_eq!(e.len(), 6, "{e:#?}");
        assert_eq!(e[0].key, vec!["name"]);
        assert_eq!(e[1].table, vec!["tasks"]);
        assert_eq!(e[1].line, 4);
        assert_eq!(e[1].value, s("mojo run t.mojo"));
        assert_eq!(e[2].line, 5);
        assert_eq!(e[2].value.get("cmd"), Some(&s("sh x.sh")));
        assert_eq!(
            e[2].value.get("depends-on"),
            Some(&Value::Arr(vec![s("a"), s("c")]))
        );
        assert_eq!(e[3].table, vec!["feature", "dev", "tasks", "c"]);
        assert_eq!(e[3].line, 10);
        assert_eq!(e[4].value, Value::Other("5".into()));
        assert_eq!(e[5].key, vec!["lints", "tests"]);
    }

    #[test]
    fn strings_with_escapes_and_multiline() {
        let e = entries(
            "a = \"say \\\"hi\\\"\\n\"\nb = \"\"\"\nline1\nline2\\\n   joined\"\"\"\nc = '''raw \\n'''\nd = 1\n",
        );
        assert_eq!(e[0].value, s("say \"hi\"\n"));
        assert_eq!(e[1].value, s("line1\nline2joined"));
        assert_eq!(e[2].value, s("raw \\n"));
        assert_eq!(e[3].line, 7);
    }

    #[test]
    fn unreadable_lines_are_skipped() {
        let e = entries("what is this\n= 3\nok = \"y\"\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, vec!["ok"]);
    }
}
