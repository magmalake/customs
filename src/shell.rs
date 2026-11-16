//! Shell text cut into simple commands of words, enough to find what a
//! script or a task runs. Quotes are removed from words, comments and
//! here-documents dropped, and a `$(...)` or backquoted command is kept in
//! its word as written and also given separately, to be read in turn. It
//! is not a shell: there's no expansion here, and `;`, `&`, `|`, `(` and
//! `)` all just end a command.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// 1-based, of the word's first character.
    pub line: usize,
    /// Command substitutions in the word: their text and first line.
    pub subs: Vec<(String, usize)>,
}

pub type Command = Vec<Word>;

struct L<'a> {
    s: &'a [char],
    i: usize,
    line: usize,
}

impl L<'_> {
    fn peek(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek(0)?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    /// After `$(`: up to the matching `)`, which is consumed.
    fn paren(&mut self) -> String {
        let mut depth = 1;
        let mut out = String::new();
        let mut quote = None;
        while let Some(c) = self.bump() {
            match (quote, c) {
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), _) if c == q => quote = None,
                (q, '\\') if q != Some('\'') => {
                    out.push(c);
                    if let Some(n) = self.bump() {
                        out.push(n);
                    }
                    continue;
                }
                (None, '(') => depth += 1,
                (None, ')') => {
                    depth -= 1;
                    if depth == 0 {
                        return out;
                    }
                }
                _ => {}
            }
            out.push(c);
        }
        out
    }

    /// After `` ` ``: up to the next one, which is consumed.
    fn backquote(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '`' => break,
                '\\' => {
                    if let Some(n) = self.bump() {
                        out.push(n);
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// `$...` at the cursor (on the `$`), into `w`.
    fn dollar(&mut self, w: &mut Word) {
        self.bump();
        match self.peek(0) {
            Some('(') => {
                self.bump();
                let line = self.line;
                let inner = self.paren();
                w.text.push_str("$(");
                w.text.push_str(&inner);
                w.text.push(')');
                w.subs.push((inner, line));
            }
            Some('{') => {
                w.text.push('$');
                while let Some(c) = self.bump() {
                    w.text.push(c);
                    if c == '}' {
                        break;
                    }
                }
            }
            _ => w.text.push('$'),
        }
    }
}

/// The simple commands of `src`, the first line being `line`.
pub fn commands(src: &str, line: usize) -> Vec<Command> {
    let chars: Vec<char> = src.chars().collect();
    let mut l = L {
        s: &chars,
        i: 0,
        line,
    };
    let mut out: Vec<Command> = Vec::new();
    let mut cmd: Command = Vec::new();
    let mut w: Option<Word> = None;
    let mut heredocs: Vec<String> = Vec::new();
    fn end_word(w: &mut Option<Word>, cmd: &mut Command) {
        if let Some(w) = w.take() {
            cmd.push(w);
        }
    }
    fn end_cmd(w: &mut Option<Word>, cmd: &mut Command, out: &mut Vec<Command>) {
        end_word(w, cmd);
        if !cmd.is_empty() {
            out.push(std::mem::take(cmd));
        }
    }
    while let Some(c) = l.peek(0) {
        let line = l.line;
        let word = |w: &mut Option<Word>| -> () {
            if w.is_none() {
                *w = Some(Word {
                    text: String::new(),
                    line,
                    subs: Vec::new(),
                });
            }
        };
        match c {
            ' ' | '\t' | '\r' => {
                l.bump();
                end_word(&mut w, &mut cmd);
            }
            '\n' => {
                l.bump();
                end_cmd(&mut w, &mut cmd, &mut out);
                // The bodies of here-documents begun on that line.
                for delim in heredocs.drain(..) {
                    loop {
                        let start = l.i;
                        while !matches!(l.peek(0), None | Some('\n')) {
                            l.bump();
                        }
                        let text: String = chars[start..l.i].iter().collect();
                        l.bump();
                        if text.trim() == delim || l.peek(0).is_none() {
                            break;
                        }
                    }
                }
            }
            ';' | '&' | '|' | '(' | ')' => {
                l.bump();
                end_cmd(&mut w, &mut cmd, &mut out);
            }
            '#' if w.is_none() => {
                while !matches!(l.peek(0), None | Some('\n')) {
                    l.bump();
                }
            }
            '<' if l.peek(1) == Some('<') && l.peek(2) != Some('<') => {
                l.bump();
                l.bump();
                end_word(&mut w, &mut cmd);
                if l.peek(0) == Some('-') {
                    l.bump();
                }
                while matches!(l.peek(0), Some(' ' | '\t')) {
                    l.bump();
                }
                let mut delim = String::new();
                while let Some(c) = l.peek(0) {
                    if c.is_whitespace() || matches!(c, ';' | '&' | '|' | ')' | '(') {
                        break;
                    }
                    l.bump();
                    if c != '\'' && c != '"' && c != '\\' {
                        delim.push(c);
                    }
                }
                if !delim.is_empty() {
                    heredocs.push(delim);
                }
            }
            '\\' => {
                l.bump();
                match l.bump() {
                    Some('\n') | None => {}
                    Some(n) => {
                        word(&mut w);
                        w.as_mut().unwrap().text.push(n);
                    }
                }
            }
            '\'' => {
                l.bump();
                word(&mut w);
                let wd = w.as_mut().unwrap();
                while let Some(c) = l.bump() {
                    if c == '\'' {
                        break;
                    }
                    wd.text.push(c);
                }
            }
            '"' => {
                l.bump();
                word(&mut w);
                let wd = w.as_mut().unwrap();
                while let Some(c) = l.peek(0) {
                    match c {
                        '"' => {
                            l.bump();
                            break;
                        }
                        '\\' => {
                            l.bump();
                            match l.bump() {
                                Some(n @ ('"' | '\\' | '$' | '`')) => wd.text.push(n),
                                Some('\n') => {}
                                Some(n) => {
                                    wd.text.push('\\');
                                    wd.text.push(n);
                                }
                                None => {}
                            }
                        }
                        '$' => l.dollar(wd),
                        '`' => {
                            l.bump();
                            let line = l.line;
                            let inner = l.backquote();
                            wd.text.push_str(&inner);
                            wd.subs.push((inner, line));
                        }
                        _ => {
                            l.bump();
                            wd.text.push(c);
                        }
                    }
                }
            }
            '$' => {
                word(&mut w);
                l.dollar(w.as_mut().unwrap());
            }
            '`' => {
                l.bump();
                word(&mut w);
                let line = l.line;
                let inner = l.backquote();
                let wd = w.as_mut().unwrap();
                wd.text.push_str(&inner);
                wd.subs.push((inner, line));
            }
            _ => {
                l.bump();
                word(&mut w);
                w.as_mut().unwrap().text.push(c);
            }
        }
    }
    end_cmd(&mut w, &mut cmd, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<Vec<String>> {
        commands(src, 1)
            .into_iter()
            .map(|c| c.into_iter().map(|w| w.text).collect())
            .collect()
    }

    #[test]
    fn commands_words_and_quotes() {
        assert_eq!(
            texts("a \"b c\" 'd e'f && g; h | i\n# not\nj # k\n"),
            vec![
                vec!["a", "b c", "d ef"],
                vec!["g"],
                vec!["h"],
                vec!["i"],
                vec!["j"]
            ]
        );
    }

    #[test]
    fn substitutions_and_lines() {
        let c = commands("x=1\\\n  y \"$(ls a/*.mojo)\" ${z}\n`date`", 10);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0][0].text, "x=1");
        assert_eq!(c[0][1].text, "y");
        assert_eq!(c[0][1].line, 11);
        assert_eq!(c[0][2].text, "$(ls a/*.mojo)");
        assert_eq!(c[0][2].subs, vec![("ls a/*.mojo".to_string(), 11)]);
        assert_eq!(c[0][3].text, "${z}");
        assert_eq!(c[1][0].line, 12);
    }

    #[test]
    fn heredocs_are_skipped() {
        assert_eq!(
            texts("cat > f <<'EOF'\nmojo run x.mojo\nEOF\necho $#\n"),
            vec![vec!["cat", ">", "f"], vec!["echo", "$#"]]
        );
    }
}
