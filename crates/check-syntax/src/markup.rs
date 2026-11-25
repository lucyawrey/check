use crate::ast::{Arg, DataFile, Expr, ExprKind, NodeId, StrSeg};
use crate::diag::{Code, Diagnostic, Span};
use crate::{lexer, parser};

/// Reads a `.cml` file, or a `.cmlx` file when `pure` is false, into the value the same data
/// written as `.con` gives.
pub fn parse_markup(
    src: &str,
    base: usize,
    pure: bool,
    next_id: &mut NodeId,
    diags: &mut Vec<Diagnostic>,
) -> DataFile {
    let mut m = Markup {
        src,
        base,
        pure,
        pos: 0,
        next_id,
        diags,
    };
    let namespace = m.namespace();
    let start = m.pos;
    let content = m.content(None);
    let span = m.span(start, m.pos);
    if let Some(text) = &content.text {
        m.error(Code::unexpected_token, text.span);
    }
    if let Some(field) = content.fields.first() {
        m.error(Code::unexpected_token, field.value.span);
    }
    let mut elements = content.elements;
    let value = match elements.len() {
        0 => m.node(span, ExprKind::Record(Vec::new())),
        1 => elements.remove(0),
        _ => m.node(span, ExprKind::List(elements)),
    };
    DataFile { namespace, value }
}

struct Markup<'a> {
    src: &'a str,
    base: usize,
    pure: bool,
    pos: usize,
    next_id: &'a mut NodeId,
    diags: &'a mut Vec<Diagnostic>,
}

#[derive(Default)]
struct Content {
    elements: Vec<Expr>,
    fields: Vec<Arg>,
    text: Option<Expr>,
}

enum Item {
    Element(Expr),
    Field(Arg),
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

impl Markup<'_> {
    fn rest(&self) -> &str {
        &self.src[self.pos..]
    }

    fn peek(&self, ahead: usize) -> u8 {
        self.src
            .as_bytes()
            .get(self.pos + ahead)
            .copied()
            .unwrap_or(0)
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.base + start, self.base + end)
    }

    fn error(&mut self, code: Code, span: Span) {
        self.diags.push(Diagnostic::error(code, span));
    }

    fn node(&mut self, span: Span, kind: ExprKind) -> Expr {
        *self.next_id += 1;
        Expr {
            id: *self.next_id,
            span,
            kind,
        }
    }

    fn tokens(&mut self, start: usize, end: usize) -> Vec<lexer::Token> {
        lexer::lex(
            &self.src[start..end],
            self.base + start,
            self.pure,
            self.diags,
        )
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self.peek(0).is_ascii_whitespace() {
                self.pos += 1;
            }
            if !self.rest().starts_with("//") {
                return;
            }
            self.skip_line();
        }
    }

    fn skip_line(&mut self) {
        self.pos = self
            .rest()
            .find('\n')
            .map_or(self.src.len(), |n| self.pos + n);
    }

    fn namespace(&mut self) -> Option<(String, Span)> {
        self.skip_space_and_comments();
        let word = self.rest().strip_prefix("namespace")?;
        if !word.starts_with([' ', '\t']) {
            return None;
        }
        let start = self.pos;
        self.skip_line();
        let tokens = self.tokens(start, self.pos);
        let src = &self.src[start..self.pos];
        parser::parse_namespace(tokens, src, self.base + start, self.next_id, self.diags)
    }

    /// Where the code starting at `self.pos` ends: at the first `stop` outside brackets,
    /// strings and comments, or `None` at the end of the file.
    fn code_end(&self, stop: &[&str]) -> Option<usize> {
        let bytes = self.src.as_bytes();
        let mut depth = 0usize;
        let mut i = self.pos;
        while i < bytes.len() {
            let rest = &self.src[i..];
            if depth == 0 && stop.iter().any(|s| rest.starts_with(s)) {
                return Some(i);
            }
            match bytes[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'/' if rest.starts_with("//") => {
                    i += rest.find('\n')?;
                    continue;
                }
                b'"' if rest.starts_with("\"\"\"") => {
                    i += 3 + rest[3..].find("\"\"\"")? + 3;
                    continue;
                }
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                        i += if bytes[i] == b'\\' { 2 } else { 1 };
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// An element's content, up to its closing tag (`closing` is its name, empty for `<>`),
    /// or the whole file when `closing` is `None`.
    fn content(&mut self, closing: Option<(&str, Span)>) -> Content {
        let mut content = Content::default();
        let mut segs: Vec<StrSeg> = Vec::new();
        let mut text_start = None;
        let mut text_end = self.pos;
        let top_level = closing.is_none();
        let mut line_blank = top_level;
        loop {
            let at = self.pos;
            if at >= self.src.len() {
                if let Some((_, open)) = closing {
                    self.error(Code::unclosed_tag, open);
                }
                break;
            }
            let c = self.peek(0);
            if self.rest().starts_with("</")
                && let Some((name, _)) = closing
            {
                self.closing_tag(name);
                break;
            }
            if c == b'<' {
                match self.element() {
                    Some(Item::Element(e)) => content.elements.push(e),
                    Some(Item::Field(f)) => content.fields.push(f),
                    None => {}
                }
                line_blank = top_level;
                continue;
            }
            if line_blank && self.rest().starts_with("//") {
                self.skip_line();
                continue;
            }
            if c == b'{' && !self.pure {
                if let Some(expr) = self.interpolation() {
                    text_start.get_or_insert(at);
                    text_end = self.pos;
                    segs.push(StrSeg::Expr(expr));
                    line_blank = false;
                }
                continue;
            }
            let ch = if c == b'\\' {
                self.escape()
            } else {
                let ch = self.rest().chars().next().unwrap_or_default();
                self.pos += ch.len_utf8();
                ch
            };
            if ch == '\n' {
                line_blank = true;
            } else if !ch.is_ascii_whitespace() {
                line_blank = false;
                text_start.get_or_insert(at);
                text_end = self.pos;
            }
            match segs.last_mut() {
                Some(StrSeg::Text(t)) => t.push(ch),
                _ => segs.push(StrSeg::Text(ch.to_string())),
            }
        }
        if let Some(start) = text_start {
            let span = self.span(start, text_end);
            content.text = Some(self.node(span, ExprKind::Str(collapse(segs))));
        }
        content
    }

    fn escape(&mut self) -> char {
        let start = self.pos;
        self.pos += 1;
        let ch = self.rest().chars().next().unwrap_or_default();
        self.pos += ch.len_utf8();
        if !matches!(ch, '<' | '{' | '\\') {
            self.error(Code::unknown_escape, self.span(start, self.pos));
        }
        ch
    }

    fn interpolation(&mut self) -> Option<Expr> {
        let open = self.pos;
        self.pos += 1;
        let Some(end) = self.code_end(&["}"]) else {
            self.error(Code::unclosed_interpolation, self.span(open, open + 1));
            self.pos = self.src.len();
            return None;
        };
        let start = self.pos;
        self.pos = end + 1;
        let tokens = self.tokens(start, end);
        let src = &self.src[start..end];
        let expr = parser::parse_expr(tokens, src, self.base + start, self.next_id, self.diags);
        if expr.is_none() {
            self.error(Code::unclosed_interpolation, self.span(open, end + 1));
        }
        expr
    }

    fn name(&mut self) -> &str {
        let start = self.pos;
        if !is_name_start(self.peek(0)) {
            return "";
        }
        while is_name_char(self.peek(0)) || (self.peek(0) == b'.' && is_name_start(self.peek(1))) {
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }

    fn closing_tag(&mut self, expected: &str) {
        let start = self.pos;
        self.pos += 2;
        let name = self.name().to_string();
        while self.peek(0).is_ascii_whitespace() {
            self.pos += 1;
        }
        if self.peek(0) == b'>' {
            self.pos += 1;
        } else {
            self.error(Code::unexpected_token, self.span(self.pos, self.pos + 1));
        }
        if name != expected {
            self.error(Code::mismatched_closing_tag, self.span(start, self.pos));
        }
    }

    fn element(&mut self) -> Option<Item> {
        let start = self.pos;
        self.pos += 1;
        let name_start = self.pos;
        let mut name = self.name().to_string();
        if self.peek(0) == b'=' {
            self.pos = name_start;
            name.clear();
        } else if name.is_empty() && self.peek(0) != b'>' {
            self.error(Code::unexpected_token, self.span(start, start + 1));
            return None;
        }
        let name_span = self.span(start, self.pos);
        let attrs_start = self.pos;
        let Some(end) = self.code_end(&["/>", ">"]) else {
            self.error(Code::unclosed_tag, name_span);
            self.pos = self.src.len();
            return None;
        };
        let tokens = self.tokens(attrs_start, end);
        let src = &self.src[attrs_start..end];
        let attrs = parser::parse_attributes(
            tokens,
            src,
            self.base + attrs_start,
            self.next_id,
            self.diags,
        );
        let self_closing = self.src[end..].starts_with("/>");
        self.pos = end + if self_closing { 2 } else { 1 };
        let content = if self_closing {
            Content::default()
        } else {
            self.content(Some((&name, name_span)))
        };
        let span = self.span(start, self.pos);
        let is_field =
            name.bytes().next().is_some_and(|c| c.is_ascii_lowercase()) && !name.contains('.');
        Some(if is_field {
            Item::Field(self.field_tag(name, attrs, content, span))
        } else {
            Item::Element(self.type_element(&name, name_span, attrs, content, span))
        })
    }

    fn field_tag(&mut self, name: String, attrs: Vec<Arg>, content: Content, span: Span) -> Arg {
        for attr in attrs.iter().chain(&content.fields) {
            self.error(Code::unexpected_token, attr.value.span);
        }
        let value = match content.text {
            Some(text) if content.elements.is_empty() => text,
            text => {
                if text.is_some() {
                    self.error(Code::mixed_text_and_children, span);
                }
                self.node(span, ExprKind::FieldTag(content.elements))
            }
        };
        Arg {
            name: Some(name),
            value,
        }
    }

    fn type_element(
        &mut self,
        name: &str,
        name_span: Span,
        mut args: Vec<Arg>,
        content: Content,
        span: Span,
    ) -> Expr {
        args.extend(content.fields);
        if content.text.is_some() && !content.elements.is_empty() {
            self.error(Code::mixed_text_and_children, span);
        }
        if let Some(first) = content.elements.first() {
            let children_span = first
                .span
                .to(content.elements[content.elements.len() - 1].span);
            args.push(Arg {
                name: Some("children".to_string()),
                value: self.node(children_span, ExprKind::List(content.elements)),
            });
        }
        if let Some(text) = content.text {
            args.push(Arg {
                name: Some("text".to_string()),
                value: text,
            });
        }
        if name.is_empty() {
            return self.node(span, ExprKind::Record(args));
        }
        let name_span = Span::new(name_span.start + 1, name_span.end);
        let kind = match name.rsplit_once('.') {
            None => {
                let callee = self.node(name_span, ExprKind::Name(name.to_string()));
                ExprKind::Call(Box::new(callee), args)
            }
            Some((path, last)) => {
                let base = self.path(path, name_span);
                ExprKind::Method(Box::new(base), last.to_string(), args)
            }
        };
        self.node(span, kind)
    }

    fn path(&mut self, path: &str, span: Span) -> Expr {
        let span = Span::new(span.start, span.start + path.len());
        match path.rsplit_once('.') {
            None => self.node(span, ExprKind::Name(path.to_string())),
            Some((base, last)) => {
                let base = self.path(base, span);
                self.node(span, ExprKind::Field(Box::new(base), last.to_string()))
            }
        }
    }
}

/// Collapses runs of whitespace to one space and trims the text's two ends, as HTML does.
fn collapse(segs: Vec<StrSeg>) -> Vec<StrSeg> {
    let mut out: Vec<StrSeg> = segs
        .into_iter()
        .map(|seg| match seg {
            StrSeg::Text(t) => {
                let mut text = String::new();
                for (i, word) in t.split_ascii_whitespace().enumerate() {
                    if i > 0 {
                        text.push(' ');
                    }
                    text.push_str(word);
                }
                if t.starts_with(|c: char| c.is_ascii_whitespace()) {
                    text.insert(0, ' ');
                }
                if t.ends_with(|c: char| c.is_ascii_whitespace()) && !text.ends_with(' ') {
                    text.push(' ');
                }
                StrSeg::Text(text)
            }
            e => e,
        })
        .collect();
    if let Some(StrSeg::Text(t)) = out.first_mut() {
        *t = t.trim_start().to_string();
    }
    if let Some(StrSeg::Text(t)) = out.last_mut() {
        *t = t.trim_end().to_string();
    }
    out.retain(|s| !matches!(s, StrSeg::Text(t) if t.is_empty()));
    out
}
