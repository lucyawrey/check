use crate::diag::{Code, Diagnostic, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    Str(Vec<StrPart>),
    Ident(String),
    Kw(&'static str),
    Sym(&'static str),
    Newline,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Text(String),
    Expr(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub(crate) const RESERVED: &[&str] = &[
    "func",
    "let",
    "var",
    "const",
    "class",
    "enum",
    "interface",
    "implement",
    "namespace",
    "import",
    "if",
    "else",
    "match",
    "for",
    "in",
    "while",
    "loop",
    "break",
    "continue",
    "return",
    "try",
    "and",
    "or",
    "not",
    "div",
    "mod",
    "as",
    "is",
    "true",
    "false",
    "this",
    "yield",
];

const SYMBOLS: &[&str] = &[
    "...", "..=", "**", "->", "=>", "==", "!=", "<=", ">=", "<<", ">>", "??", "?.", "+=", "-=",
    "*=", "/=", "..", "&&", "||", "+", "-", "*", "/", "%", "<", ">", "=", "!", "&", "|", "^", "~",
    "(", ")", "[", "]", "{", "}", ",", ";", ":", ".", "?", "@",
];

const FOREIGN: &[&str] = &["&&", "||", "%", "!"];

const CONTINUES_AFTER: &[&str] = &[
    "+", "-", "*", "/", "**", "==", "!=", "<", ">", "<=", ">=", "<<", ">>", "&", "|", "^", "??",
    "..", "..=", "=", "+=", "-=", "*=", "/=", "->", "=>", "(", "[", "{", ".", "?.",
];

const CONTINUING_WORDS: &[&str] = &["and", "or", "not", "div", "mod", "is", "in", "as"];

pub fn lex(src: &str, base: usize, diags: &mut Vec<Diagnostic>) -> Vec<Token> {
    let mut lexer = Lexer {
        src: src.as_bytes(),
        text: src,
        pos: 0,
        base,
        diags,
    };
    let raw = lexer.run();
    join_lines(raw)
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    base: usize,
    diags: &'a mut Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn span(&self, start: usize) -> Span {
        Span::new(self.base + start, self.base + self.pos)
    }

    fn peek(&self, ahead: usize) -> u8 {
        *self.src.get(self.pos + ahead).unwrap_or(&0)
    }

    fn error(&mut self, code: Code, start: usize) {
        let span = self.span(start);
        self.diags.push(Diagnostic::error(code, span));
    }

    fn run(&mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while self.pos < self.src.len() {
            let start = self.pos;
            let c = self.peek(0);
            match c {
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'\n' => {
                    self.pos += 1;
                    out.push(Token {
                        tok: Tok::Newline,
                        span: self.span(start),
                    });
                }
                b'/' if self.peek(1) == b'/' => {
                    while self.pos < self.src.len() && self.peek(0) != b'\n' {
                        self.pos += 1;
                    }
                }
                b'0'..=b'9' if matches!(out.last(), Some(t) if t.tok == Tok::Sym(".")) => {
                    out.push(self.field_index())
                }
                b'0'..=b'9' => out.push(self.number()),
                b'"' => out.push(self.string(false)),
                b'r' if self.peek(1) == b'"' => {
                    self.pos += 1;
                    out.push(self.string(true));
                }
                c if c.is_ascii_alphabetic() || c == b'_' => out.push(self.word()),
                _ => match SYMBOLS
                    .iter()
                    .find(|s| self.src[self.pos..].starts_with(s.as_bytes()))
                {
                    Some(sym) => {
                        self.pos += sym.len();
                        if FOREIGN.contains(sym) && !(*sym == "!" && self.peek(0) == b'=') {
                            self.error(Code::foreign_operator, start);
                        }
                        out.push(Token {
                            tok: Tok::Sym(sym),
                            span: self.span(start),
                        });
                    }
                    None => {
                        let len = self.text[self.pos..]
                            .chars()
                            .next()
                            .map_or(1, char::len_utf8);
                        self.pos += len;
                        self.error(Code::unknown_character, start);
                    }
                },
            }
        }
        out.push(Token {
            tok: Tok::Eof,
            span: self.span(self.pos),
        });
        out
    }

    fn word(&mut self) -> Token {
        let start = self.pos;
        while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
            self.pos += 1;
        }
        let word = &self.text[start..self.pos];
        let tok = match RESERVED.iter().find(|k| **k == word) {
            Some(k) => Tok::Kw(k),
            None => Tok::Ident(word.to_string()),
        };
        Token {
            tok,
            span: self.span(start),
        }
    }

    fn number(&mut self) -> Token {
        let start = self.pos;
        let radix = match (self.peek(0), self.peek(1)) {
            (b'0', b'x') => 16,
            (b'0', b'b') => 2,
            (b'0', b'o') => 8,
            _ => 10,
        };
        if radix != 10 {
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                self.pos += 1;
            }
            let digits: String = self.text[digits_start..self.pos]
                .chars()
                .filter(|c| *c != '_')
                .collect();
            return match i64::from_str_radix(&digits, radix) {
                Ok(n) if !digits.is_empty() => Token {
                    tok: Tok::Int(n),
                    span: self.span(start),
                },
                Err(e) if matches!(e.kind(), std::num::IntErrorKind::PosOverflow) => {
                    self.error(Code::literal_too_big, start);
                    Token {
                        tok: Tok::Int(0),
                        span: self.span(start),
                    }
                }
                _ => {
                    self.error(Code::malformed_number, start);
                    Token {
                        tok: Tok::Int(0),
                        span: self.span(start),
                    }
                }
            };
        }
        let mut is_float = false;
        self.digits();
        if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
            is_float = true;
            self.pos += 1;
            self.digits();
        }
        if self.peek(0) == b'e'
            && (self.peek(1).is_ascii_digit()
                || (matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit()))
        {
            is_float = true;
            self.pos += 2;
            self.digits();
        }
        if self.peek(0).is_ascii_alphabetic() || self.peek(0) == b'_' {
            while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                self.pos += 1;
            }
            self.error(Code::malformed_number, start);
            return Token {
                tok: Tok::Int(0),
                span: self.span(start),
            };
        }
        let text: String = self.text[start..self.pos]
            .chars()
            .filter(|c| *c != '_')
            .collect();
        if text.len() > 1 && text.starts_with('0') && text.as_bytes()[1].is_ascii_digit() {
            self.error(Code::leading_zero, start);
        }
        let tok = if is_float {
            Tok::Float(text.parse().unwrap_or(0.0))
        } else {
            match text.parse() {
                Ok(n) => Tok::Int(n),
                Err(_) => {
                    self.error(Code::literal_too_big, start);
                    Tok::Int(0)
                }
            }
        };
        Token {
            tok,
            span: self.span(start),
        }
    }

    fn field_index(&mut self) -> Token {
        let start = self.pos;
        while self.peek(0).is_ascii_digit() {
            self.pos += 1;
        }
        let digits = &self.text[start..self.pos];
        let tok = match digits.parse() {
            Ok(_) if digits.len() > 1 && digits.starts_with('0') => {
                self.error(Code::leading_zero, start);
                Tok::Int(0)
            }
            Ok(n) => Tok::Int(n),
            Err(_) => {
                self.error(Code::malformed_number, start);
                Tok::Int(0)
            }
        };
        Token {
            tok,
            span: self.span(start),
        }
    }

    fn digits(&mut self) {
        while self.peek(0).is_ascii_digit()
            || (self.peek(0) == b'_' && self.peek(1).is_ascii_digit())
        {
            self.pos += 1;
        }
    }

    fn string(&mut self, raw: bool) -> Token {
        let start = if raw { self.pos - 1 } else { self.pos };
        let triple = self.src[self.pos..].starts_with(b"\"\"\"");
        self.pos += if triple { 3 } else { 1 };
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            if self.pos >= self.src.len() || (!triple && self.peek(0) == b'\n') {
                self.error(Code::unclosed_string, start);
                break;
            }
            if triple && self.src[self.pos..].starts_with(b"\"\"\"") {
                self.pos += 3;
                break;
            }
            if !triple && self.peek(0) == b'"' {
                self.pos += 1;
                break;
            }
            let c = self.peek(0);
            if c == b'\\' && !raw {
                self.escape(&mut text);
            } else if c == b'{' && !raw {
                if !text.is_empty() {
                    parts.push(StrPart::Text(std::mem::take(&mut text)));
                }
                parts.push(self.interpolation());
            } else {
                let ch = self.text[self.pos..].chars().next().unwrap_or('\0');
                text.push(ch);
                self.pos += ch.len_utf8();
            }
        }
        if !text.is_empty() || parts.is_empty() {
            parts.push(StrPart::Text(text));
        }
        if triple {
            parts = dedent(parts);
        }
        Token {
            tok: Tok::Str(parts),
            span: self.span(start),
        }
    }

    fn escape(&mut self, text: &mut String) {
        let start = self.pos;
        self.pos += 1;
        let c = self.peek(0);
        self.pos += 1;
        let ch = match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0' => '\0',
            b'\\' => '\\',
            b'"' => '"',
            b'{' => '{',
            b'u' if self.peek(0) == b'{' => {
                let open = self.pos + 1;
                let close = self.text[open..].find('}').map(|i| open + i);
                match close.and_then(|c| {
                    u32::from_str_radix(&self.text[open..c], 16)
                        .ok()
                        .map(|n| (c, n))
                }) {
                    Some((c, n)) => {
                        self.pos = c + 1;
                        match char::from_u32(n) {
                            Some(ch) => ch,
                            None => {
                                self.error(Code::unknown_escape, start);
                                return;
                            }
                        }
                    }
                    None => {
                        self.error(Code::unknown_escape, start);
                        return;
                    }
                }
            }
            _ => {
                self.error(Code::unknown_escape, start);
                return;
            }
        };
        text.push(ch);
    }

    fn interpolation(&mut self) -> StrPart {
        let start = self.pos;
        self.pos += 1;
        let inner_start = self.pos;
        let mut depth = 0;
        while self.pos < self.src.len() {
            match self.peek(0) {
                b'{' => depth += 1,
                b'}' if depth == 0 => break,
                b'}' => depth -= 1,
                b'"' => {
                    self.string(false);
                    continue;
                }
                b'\n' => break,
                _ => {}
            }
            self.pos += 1;
        }
        if self.peek(0) != b'}' || self.text[inner_start..self.pos].trim().is_empty() {
            self.error(Code::unclosed_interpolation, start);
            if self.peek(0) == b'}' {
                self.pos += 1;
            }
            return StrPart::Expr(vec![Token {
                tok: Tok::Eof,
                span: self.span(start),
            }]);
        }
        let inner = &self.text[inner_start..self.pos];
        self.pos += 1;
        let mut sub = Lexer {
            src: inner.as_bytes(),
            text: inner,
            pos: 0,
            base: self.base + inner_start,
            diags: self.diags,
        };
        let tokens = sub
            .run()
            .into_iter()
            .filter(|t| t.tok != Tok::Newline)
            .collect();
        StrPart::Expr(tokens)
    }
}

fn dedent(parts: Vec<StrPart>) -> Vec<StrPart> {
    let mut joined = String::new();
    for part in &parts {
        if let StrPart::Text(t) = part {
            joined.push_str(t);
        } else {
            joined.push('\u{1}');
        }
    }
    let Some(body) = joined.strip_prefix('\n') else {
        return parts;
    };
    let last_break = body.rfind('\n').unwrap_or(0);
    let indent = &body[last_break..].trim_start_matches('\n');
    if !indent.chars().all(|c| c == ' ' || c == '\t') {
        return parts;
    }
    let content = &body[..last_break];
    let mut lines = Vec::new();
    for line in content.split('\n') {
        lines.push(line.strip_prefix(indent).unwrap_or(line.trim_start()));
    }
    let result = lines.join("\n");
    let mut exprs = parts.into_iter().filter(|p| matches!(p, StrPart::Expr(_)));
    let mut out = Vec::new();
    for (i, piece) in result.split('\u{1}').enumerate() {
        if i > 0
            && let Some(e) = exprs.next()
        {
            out.push(e);
        }
        if !piece.is_empty() {
            out.push(StrPart::Text(piece.to_string()));
        }
    }
    if out.is_empty() {
        out.push(StrPart::Text(String::new()));
    }
    out
}

fn join_lines(raw: Vec<Token>) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let token = &raw[i];
        if token.tok == Tok::Newline {
            let prev_continues = match out.last().map(|t| &t.tok) {
                None | Some(Tok::Newline) => true,
                Some(Tok::Sym(s)) => CONTINUES_AFTER.contains(s),
                Some(Tok::Kw(k)) => CONTINUING_WORDS.contains(k),
                _ => false,
            };
            let mut j = i;
            while j < raw.len() && raw[j].tok == Tok::Newline {
                j += 1;
            }
            let next_continues = matches!(raw.get(j).map(|t| &t.tok), Some(Tok::Sym("." | "?.")));
            if prev_continues || next_continues {
                i += 1;
                continue;
            }
        }
        out.push(token.clone());
        i += 1;
    }
    out
}
