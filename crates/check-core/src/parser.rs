use crate::ast::*;
use crate::diag::{Code, Diagnostic, Span};
use crate::lexer::{StrPart, Tok, Token};

pub struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    next_id: &'a mut NodeId,
    diags: &'a mut Vec<Diagnostic>,
}

type PResult<T> = Result<T, ()>;

pub fn parse_data(
    tokens: Vec<Token>,
    next_id: &mut NodeId,
    diags: &mut Vec<Diagnostic>,
) -> DataFile {
    let mut parser = Parser {
        tokens,
        pos: 0,
        next_id,
        diags,
    };
    parser.data_file()
}

pub fn parse_program(
    tokens: Vec<Token>,
    next_id: &mut NodeId,
    diags: &mut Vec<Diagnostic>,
) -> File {
    let mut parser = Parser {
        tokens,
        pos: 0,
        next_id,
        diags,
    };
    parser.program()
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn peek_at(&self, ahead: usize) -> &Tok {
        &self.tokens[(self.pos + ahead).min(self.tokens.len() - 1)].tok
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        token
    }

    fn is_sym(&self, sym: &str) -> bool {
        matches!(self.peek(), Tok::Sym(s) if *s == sym)
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Kw(k) if *k == kw)
    }

    fn eat_sym(&mut self, sym: &str) -> bool {
        if self.is_sym(sym) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error<T>(&mut self, code: Code, span: Span) -> PResult<T> {
        self.diags.push(Diagnostic::error(code, span));
        Err(())
    }

    fn expect_sym(&mut self, sym: &str) -> PResult<Span> {
        if self.is_sym(sym) {
            Ok(self.advance().span)
        } else if *self.peek() == Tok::Eof && matches!(sym, ")" | "]" | "}") {
            self.error(Code::unclosed_bracket, self.span())
        } else {
            self.error(Code::unexpected_token, self.span())
        }
    }

    fn is_soft(&self, word: &str) -> bool {
        matches!(self.peek(), Tok::Ident(w) if w == word)
    }

    fn is_type_decl(&self) -> bool {
        self.is_kw("class")
            || self.is_kw("enum")
            || self.is_kw("implement")
            || (self.is_soft("ref") && matches!(self.peek_at(1), Tok::Kw("class")))
    }

    fn field_name(&mut self) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(name) => Ok((name, self.advance().span)),
            Tok::Kw(kw) => Ok((kw.to_string(), self.advance().span)),
            _ => self.error(Code::unexpected_token, self.span()),
        }
    }

    fn ident(&mut self) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(name) => Ok((name, self.advance().span)),
            _ => self.error(Code::unexpected_token, self.span()),
        }
    }

    fn id(&mut self) -> NodeId {
        *self.next_id += 1;
        *self.next_id
    }

    fn expr_node(&mut self, span: Span, kind: ExprKind) -> Expr {
        Expr {
            id: self.id(),
            span,
            kind,
        }
    }

    fn skip_newlines(&mut self) {
        while *self.peek() == Tok::Newline {
            self.advance();
        }
    }

    fn program(&mut self) -> File {
        let mut file = File {
            namespace: None,
            imports: Vec::new(),
            stmts: Vec::new(),
        };
        let mut first = true;
        loop {
            self.skip_newlines();
            if *self.peek() == Tok::Eof {
                break;
            }
            let result = if self.is_kw("namespace") {
                let span = self.span();
                match self.namespace() {
                    Ok(ns) if first => {
                        file.namespace = Some((ns, span.to(self.prev_span())));
                        Ok(())
                    }
                    Ok(_) => self.error(Code::misplaced_namespace, span),
                    Err(()) => Err(()),
                }
            } else if self.is_kw("import") {
                self.import().map(|i| file.imports.push(i))
            } else {
                self.statement().map(|s| file.stmts.push(s))
            };
            if result.is_err() {
                self.recover();
            }
            first = false;
            self.end_statement(false);
        }
        file
    }

    fn data_file(&mut self) -> DataFile {
        self.skip_newlines();
        let mut namespace = None;
        if self.is_kw("namespace") {
            let span = self.span();
            match self.namespace() {
                Ok(ns) => namespace = Some((ns, span.to(self.prev_span()))),
                Err(()) => self.recover(),
            }
            self.end_statement(false);
            self.skip_newlines();
        }
        let start = self.span();
        if *self.peek() == Tok::Eof {
            let value = self.expr_node(start, ExprKind::Record(Vec::new()));
            return DataFile { namespace, value };
        }
        let value = match self.data_value() {
            Ok(value) => value,
            Err(()) => self.expr_node(start, ExprKind::Record(Vec::new())),
        };
        self.skip_newlines();
        if *self.peek() != Tok::Eof {
            self.diags
                .push(Diagnostic::error(Code::unexpected_token, self.span()));
        }
        DataFile { namespace, value }
    }

    /// A field list without its outer `( )` is a record literal; anything else is one expression.
    fn data_value(&mut self) -> PResult<Expr> {
        let is_field_list = matches!(self.peek(), Tok::Ident(_) | Tok::Kw(_))
            && matches!(self.peek_at(1), Tok::Sym("="));
        if !is_field_list {
            return self.expr();
        }
        let start = self.span();
        let mut args = Vec::new();
        while matches!(self.peek(), Tok::Ident(_) | Tok::Kw(_)) {
            let (name, _) = self.field_name()?;
            self.expect_sym("=")?;
            args.push(Arg {
                name: Some(name),
                value: self.expr()?,
            });
            self.end_statement(false);
            self.skip_newlines();
        }
        Ok(self.expr_node(start.to(self.prev_span()), ExprKind::Record(args)))
    }

    fn namespace(&mut self) -> PResult<String> {
        self.advance();
        let (mut name, _) = self.ident()?;
        while self.eat_sym(".") {
            name = format!("{name}.{}", self.ident()?.0);
        }
        Ok(name)
    }

    fn import(&mut self) -> PResult<Import> {
        let start = self.advance().span;
        let modifier = |p: &Self, word| p.is_soft(word) && matches!(p.peek_at(1), Tok::Str(_));
        let is_private = modifier(self, "private");
        let is_host = modifier(self, "host");
        if is_private || is_host {
            self.advance();
        }
        match self.peek().clone() {
            Tok::Str(parts) => match parts.as_slice() {
                [StrPart::Text(path)] => {
                    let span = start.to(self.advance().span);
                    Ok(Import {
                        path: path.clone(),
                        is_private,
                        is_host,
                        span,
                    })
                }
                _ => self.error(Code::unexpected_token, self.span()),
            },
            _ => self.error(Code::unexpected_token, self.span()),
        }
    }

    /// Eats a leading `private`, a soft keyword only in front of a declaration.
    fn private(&mut self) -> bool {
        let before_decl = match self.peek_at(1) {
            Tok::Kw("func" | "class" | "enum" | "interface" | "let" | "var" | "const") => true,
            Tok::Ident(w) => matches!(w.as_str(), "ref" | "static" | "external"),
            _ => false,
        };
        let is_private = self.is_soft("private") && before_decl;
        if is_private {
            self.advance();
        }
        is_private
    }

    fn recover(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Tok::Eof => break,
                Tok::Newline if depth <= 0 => break,
                Tok::Sym("(" | "[" | "{") => depth += 1,
                Tok::Sym(")" | "]" | "}") => {
                    if depth <= 0 {
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            self.advance();
        }
    }

    fn end_statement(&mut self, in_block: bool) {
        if self.is_sym(";") {
            let span = self.advance().span;
            if matches!(self.peek(), Tok::Newline | Tok::Eof) || self.is_sym("}") {
                self.diags
                    .push(Diagnostic::error(Code::trailing_separator, span));
            }
            return;
        }
        match self.peek() {
            Tok::Newline | Tok::Eof => {}
            Tok::Sym("}") if in_block => {}
            _ => {
                let span = self.span();
                self.diags
                    .push(Diagnostic::error(Code::unexpected_token, span));
                // recover() stops at a closing bracket, so a stray one has to be skipped
                // here or the caller's loop never moves past it.
                if matches!(self.peek(), Tok::Sym(")" | "]" | "}")) {
                    self.advance();
                }
                self.recover();
            }
        }
    }

    fn statement(&mut self) -> PResult<Stmt> {
        let is_private = self.private();
        if is_private
            && !(self.is_kw("enum")
                || self.is_kw("func")
                || self.is_kw("interface")
                || self.is_type_decl())
        {
            return self.error(Code::unexpected_token, self.span());
        }
        let mut stmt = self.declaration()?;
        match &mut stmt {
            Stmt::Func(d) => d.is_private = is_private,
            Stmt::Class(d) => d.is_private = is_private,
            Stmt::Enum(d) => d.is_private = is_private,
            Stmt::Interface(d) => d.is_private = is_private,
            _ => {}
        }
        Ok(stmt)
    }

    fn declaration(&mut self) -> PResult<Stmt> {
        if self.is_kw("enum") {
            return Ok(Stmt::Enum(self.enum_decl()?));
        }
        if self.is_kw("implement") {
            return Ok(Stmt::Implement(self.implement_decl()?));
        }
        if self.is_kw("interface") {
            return Ok(Stmt::Interface(self.interface_decl()?));
        }
        if self.is_external() {
            self.advance();
            if !self.is_kw("func") {
                return self.error(Code::unexpected_token, self.span());
            }
            return Ok(Stmt::Func(self.func_decl(true, false)?));
        }
        if self.is_type_decl() {
            return Ok(Stmt::Class(self.class_decl()?));
        }
        if (self.is_kw("let") || self.is_kw("var"))
            && !(matches!(self.peek_at(1), Tok::Ident(_))
                && matches!(self.peek_at(2), Tok::Sym("=" | ":")))
        {
            return self.let_pattern();
        }
        if self.is_kw("let") || self.is_kw("var") {
            return self.let_stmt();
        }
        if self.is_kw("func") && matches!(self.peek_at(1), Tok::Ident(_)) {
            return Ok(Stmt::Func(self.func_decl(false, false)?));
        }
        let target = self.expr()?;
        let op = match self.peek() {
            Tok::Sym("=") => None,
            Tok::Sym("+=") => Some(BinOp::Add),
            Tok::Sym("-=") => Some(BinOp::Sub),
            Tok::Sym("*=") => Some(BinOp::Mul),
            Tok::Sym("/=") => Some(BinOp::Div),
            _ => return Ok(Stmt::Expr(target)),
        };
        self.advance();
        let value = self.expr()?;
        let span = target.span.to(value.span);
        Ok(Stmt::Assign {
            target,
            op,
            value,
            span,
        })
    }

    fn let_stmt(&mut self) -> PResult<Stmt> {
        let is_var = self.is_kw("var");
        self.advance();
        let (name, span) = self.ident()?;
        let ty = if self.eat_sym(":") {
            Some(self.type_expr()?)
        } else {
            None
        };
        self.expect_sym("=")?;
        let value = self.expr()?;
        Ok(Stmt::Let {
            id: self.id(),
            name,
            span,
            is_var,
            ty,
            value,
        })
    }

    fn let_pattern(&mut self) -> PResult<Stmt> {
        let is_var = self.is_kw("var");
        let start = self.advance().span;
        let pattern = self.pattern()?;
        self.expect_sym("=")?;
        let value = self.expr()?;
        let otherwise = if self.eat_kw("else") {
            Some(self.block()?)
        } else {
            None
        };
        Ok(Stmt::LetPattern {
            span: start.to(self.prev_span()),
            pattern,
            is_var,
            value,
            otherwise,
        })
    }

    fn body(&mut self, mut item: impl FnMut(&mut Self) -> PResult<()>) -> PResult<()> {
        let start = self.expect_sym("{")?;
        loop {
            self.skip_newlines();
            if self.eat_sym("}") {
                return Ok(());
            }
            if *self.peek() == Tok::Eof {
                return self.error(Code::unclosed_bracket, start);
            }
            if item(self).is_err() {
                self.recover();
            }
            self.end_statement(true);
        }
    }

    fn is_external(&self) -> bool {
        self.is_soft("external")
            && match self.peek_at(1) {
                Tok::Kw("func") => true,
                Tok::Ident(w) => w == "static" && matches!(self.peek_at(2), Tok::Kw("func")),
                _ => false,
            }
    }

    fn implement_decl(&mut self) -> PResult<ImplementDecl> {
        let start = self.advance().span;
        let mut target = self.type_expr()?;
        let mut interface = None;
        if self.eat_kw("for") {
            interface = Some(target);
            target = self.type_expr()?;
        }
        let (target, target_args) = match target {
            TypeExpr::Name(name, _) => (name, Vec::new()),
            TypeExpr::Generic(name, args, _) if name != "List" => (name, args),
            _ => return self.error(Code::unexpected_token, self.prev_span()),
        };
        let mut members = Members::default();
        let owner = target.clone();
        self.body(|p| {
            if p.member(&mut members, &owner, false)? {
                return Ok(());
            }
            p.error(Code::unexpected_token, p.span())
        })?;
        Ok(ImplementDecl {
            target,
            target_args,
            interface,
            span: start.to(self.prev_span()),
            members,
        })
    }

    fn interface_decl(&mut self) -> PResult<InterfaceDecl> {
        let start = self.advance().span;
        let (name, _) = self.ident()?;
        let type_params = self.type_params()?;
        let parents = self.interface_list()?;
        let mut fields = Vec::new();
        let mut members = Members::default();
        let owner = name.clone();
        self.body(|p| {
            if p.member(&mut members, &owner, true)? {
                return Ok(());
            }
            fields.push(p.body_field()?);
            Ok(())
        })?;
        Ok(InterfaceDecl {
            name,
            span: start.to(self.prev_span()),
            type_params,
            parents,
            fields,
            members,
            is_private: false,
        })
    }

    /// `<T, U: Shape + Eq>` after a declaration's name, or nothing.
    fn type_params(&mut self) -> PResult<Vec<TypeParam>> {
        let mut params = Vec::new();
        if !self.eat_sym("<") {
            return Ok(params);
        }
        loop {
            let (name, span) = self.ident()?;
            let bounds = if self.eat_sym(":") {
                self.bounds()?
            } else {
                Vec::new()
            };
            params.push(TypeParam { name, span, bounds });
            if !self.eat_sym(",") {
                break;
            }
        }
        self.close_angle()?;
        Ok(params)
    }

    fn bounds(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut bounds = vec![self.type_expr()?];
        while self.eat_sym("+") {
            bounds.push(self.type_expr()?);
        }
        Ok(bounds)
    }

    /// `: Shape + Printable` after a type's header, or nothing.
    fn interface_list(&mut self) -> PResult<Vec<TypeExpr>> {
        if self.eat_sym(":") {
            self.bounds()
        } else {
            Ok(Vec::new())
        }
    }

    /// Eats a `>` closing type arguments, splitting a `>>` in two.
    fn close_angle(&mut self) -> PResult<()> {
        if self.is_sym(">>") {
            let token = &mut self.tokens[self.pos];
            token.tok = Tok::Sym(">");
            token.span.start += 1;
            return Ok(());
        }
        self.expect_sym(">").map(|_| ())
    }

    /// `optional_body` lets a method end after its signature, as in an interface.
    fn member(&mut self, members: &mut Members, owner: &str, optional_body: bool) -> PResult<bool> {
        if self.is_soft("private") && matches!(self.peek_at(1), Tok::Kw("let" | "var")) {
            return Ok(false);
        }
        let start = self.pos;
        let is_private = self.private();
        let found = self.member_decl(members, owner, is_private, optional_body)?;
        if !found {
            self.pos = start;
        }
        Ok(found)
    }

    fn member_decl(
        &mut self,
        members: &mut Members,
        owner: &str,
        is_private: bool,
        optional_body: bool,
    ) -> PResult<bool> {
        let is_external = self.is_external();
        if is_external {
            self.advance();
        }
        if self.is_soft("static") && matches!(self.peek_at(1), Tok::Kw("func")) {
            self.advance();
            let mut decl = self.func_decl(is_external, false)?;
            decl.name = format!("{owner}.{}", decl.name);
            decl.is_private = is_private;
            members.statics.push(decl);
        } else if self.is_kw("func") {
            let mut decl = self.func_decl(is_external, optional_body)?;
            decl.name = format!("{owner}.{}", decl.name);
            decl.is_private = is_private;
            members.methods.push(decl);
        } else if self.eat_kw("const") {
            let (name, span) = self.ident()?;
            let ty = if self.eat_sym(":") {
                Some(self.type_expr()?)
            } else {
                None
            };
            self.expect_sym("=")?;
            let value = self.expr()?;
            members.consts.push(ConstDecl {
                name,
                span,
                ty,
                value,
                is_private,
            });
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn class_decl(&mut self) -> PResult<ClassDecl> {
        let start = self.span();
        let is_ref = self.is_soft("ref");
        if is_ref {
            self.advance();
        }
        self.advance();
        let (name, _) = self.ident()?;
        let type_params = self.type_params()?;
        let mut fields = Vec::new();
        let mut init = None;
        let mut members = Members::default();
        let interfaces;
        if self.is_sym("(") {
            fields = self.params(true)?;
            interfaces = self.interface_list()?;
        } else {
            interfaces = self.interface_list()?;
            let owner = name.clone();
            self.body(|p| {
                if p.member(&mut members, &owner, false)? {
                    return Ok(());
                }
                if p.is_soft("init") && matches!(p.peek_at(1), Tok::Sym("{")) {
                    let span = p.advance().span;
                    let body = p.block()?;
                    init = Some(FuncDecl {
                        id: p.id(),
                        name: format!("{owner}.init"),
                        span: span.to(body.span),
                        type_params: Vec::new(),
                        params: Vec::new(),
                        ret: None,
                        body: Some(body),
                        is_private: false,
                    });
                    return Ok(());
                }
                fields.push(p.body_field()?);
                Ok(())
            })?;
        }
        Ok(ClassDecl {
            is_private: false,
            name,
            span: start.to(self.prev_span()),
            is_ref,
            type_params,
            interfaces,
            fields,
            init,
            members,
        })
    }

    /// A `let` or `var` field in a class or interface body.
    fn body_field(&mut self) -> PResult<Param> {
        let (is_private, is_readonly) = self.field_modifiers()?;
        let is_var = self.is_kw("var");
        if !is_var && !self.is_kw("let") {
            return self.error(Code::unexpected_token, self.span());
        }
        self.advance();
        let (name, span) = self.field_name()?;
        let ty = if self.eat_sym(":") {
            Some(self.type_expr()?)
        } else {
            None
        };
        let default = if self.eat_sym("=") {
            Some(self.expr()?)
        } else {
            None
        };
        Ok(Param {
            name,
            span,
            ty,
            default,
            is_var,
            is_rest: false,
            is_private,
            is_readonly,
        })
    }

    fn enum_decl(&mut self) -> PResult<EnumDecl> {
        let start = self.advance().span;
        let (name, _) = self.ident()?;
        let type_params = self.type_params()?;
        let interfaces = self.interface_list()?;
        let mut variants = Vec::new();
        let mut members = Members::default();
        let owner = name.clone();
        self.body(|p| {
            if p.member(&mut members, &owner, false)? {
                return Ok(());
            }
            loop {
                variants.push(p.variant()?);
                if !p.is_sym(",") {
                    return Ok(());
                }
                let comma = p.advance().span;
                if *p.peek() == Tok::Newline || p.is_sym("}") {
                    p.diags
                        .push(Diagnostic::error(Code::trailing_separator, comma));
                    return Ok(());
                }
            }
        })?;
        Ok(EnumDecl {
            name,
            span: start.to(self.prev_span()),
            type_params,
            interfaces,
            variants,
            members,
            is_private: false,
        })
    }

    fn variant(&mut self) -> PResult<VariantDecl> {
        let (name, span) = self.ident()?;
        let mut names = Vec::new();
        let mut fields = Vec::new();
        if self.eat_sym("(") {
            self.list_items(")", |p| {
                if matches!(p.peek_at(1), Tok::Sym(":")) {
                    names.push(p.field_name()?.0);
                    p.advance();
                }
                fields.push(p.type_expr()?);
                Ok(())
            })?;
        }
        if !names.is_empty() && names.len() != fields.len() {
            self.diags.push(Diagnostic::error(Code::mixed_fields, span));
        }
        Ok(VariantDecl {
            name,
            span,
            field_names: (!names.is_empty()).then_some(names),
            fields,
        })
    }

    fn pattern(&mut self) -> PResult<Pattern> {
        let first = self.single_pattern()?;
        if !self.is_sym("|") {
            return Ok(first);
        }
        let start = first.span;
        let mut alternatives = vec![first];
        while self.eat_sym("|") {
            alternatives.push(self.single_pattern()?);
        }
        Ok(Pattern {
            id: self.id(),
            span: start.to(self.prev_span()),
            kind: PatternKind::Or(alternatives),
        })
    }

    fn single_pattern(&mut self) -> PResult<Pattern> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Ident(name) if name == "_" => {
                self.advance();
                PatternKind::Wildcard
            }
            Tok::Ident(name) => {
                self.advance();
                let (qualifier, name) = if self.eat_sym(".") {
                    (Some(name), self.ident()?.0)
                } else {
                    (None, name)
                };
                if self.eat_sym("(") {
                    PatternKind::Variant(qualifier, name, self.pattern_items(")", true)?)
                } else if qualifier.is_some() {
                    PatternKind::Variant(qualifier, name, Vec::new())
                } else {
                    PatternKind::Name(name)
                }
            }
            Tok::Sym("(") => {
                self.advance();
                let mut items = self.pattern_items(")", false)?;
                if items.len() == 1 {
                    return Ok(items.remove(0));
                }
                PatternKind::Record(items)
            }
            Tok::Sym("[") => {
                self.advance();
                PatternKind::List(self.pattern_items("]", true)?)
            }
            _ => {
                let low = Box::new(self.literal_pattern()?);
                if self.eat_sym("..") {
                    PatternKind::Range(low, Box::new(self.literal_pattern()?), false)
                } else if self.eat_sym("..=") {
                    PatternKind::Range(low, Box::new(self.literal_pattern()?), true)
                } else {
                    PatternKind::Literal(low)
                }
            }
        };
        Ok(Pattern {
            id: self.id(),
            span: start.to(self.prev_span()),
            kind,
        })
    }

    fn pattern_items(&mut self, close: &str, allow_rest: bool) -> PResult<Vec<Pattern>> {
        let mut items = Vec::new();
        let mut has_rest = false;
        self.list_items(close, |p| {
            let start = p.span();
            if allow_rest && !has_rest && p.eat_sym("...") {
                has_rest = true;
                let (name, _) = p.ident()?;
                items.push(Pattern {
                    id: p.id(),
                    span: start.to(p.prev_span()),
                    kind: PatternKind::Rest(name),
                });
            } else {
                items.push(p.pattern()?);
            }
            Ok(())
        })?;
        Ok(items)
    }

    fn literal_pattern(&mut self) -> PResult<Expr> {
        let expr = self.unary()?;
        let is_literal = match &expr.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) => true,
            ExprKind::Str(segs) => segs.iter().all(|s| matches!(s, StrSeg::Text(_))),
            ExprKind::Unary(UnOp::Neg, inner) => {
                matches!(inner.kind, ExprKind::Int(_) | ExprKind::Float(_))
            }
            _ => false,
        };
        if !is_literal {
            return self.error(Code::unexpected_token, expr.span);
        }
        Ok(expr)
    }

    fn match_expr(&mut self, start: Span) -> PResult<Expr> {
        let scrutinee = self.condition()?;
        let open = self.expect_sym("{")?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.is_sym("}") {
                break;
            }
            if *self.peek() == Tok::Eof {
                return self.error(Code::unclosed_bracket, open);
            }
            match self.arm() {
                Ok(arm) => arms.push(arm),
                Err(()) => self.recover(),
            }
            if self.is_sym(",") {
                let comma = self.advance().span;
                if *self.peek() == Tok::Newline || self.is_sym("}") {
                    self.diags
                        .push(Diagnostic::error(Code::trailing_separator, comma));
                }
            } else {
                self.end_statement(true);
            }
        }
        let end = self.advance().span;
        Ok(self.expr_node(start.to(end), ExprKind::Match(Box::new(scrutinee), arms)))
    }

    fn arm(&mut self) -> PResult<Arm> {
        let pattern = self.pattern()?;
        let guard = if self.eat_kw("if") {
            Some(self.expr()?)
        } else {
            None
        };
        self.expect_sym("=>")?;
        let body = self.expr()?;
        Ok(Arm {
            pattern,
            guard,
            body,
        })
    }

    fn func_decl(&mut self, is_external: bool, optional_body: bool) -> PResult<FuncDecl> {
        let start = self.advance().span;
        let (name, _) = self.ident()?;
        let type_params = self.type_params()?;
        let params = self.params(false)?;
        let ret = if self.eat_sym("->") {
            Some(self.type_expr()?)
        } else {
            None
        };
        let ends =
            matches!(self.peek(), Tok::Newline | Tok::Eof) || self.is_sym("}") || self.is_sym(";");
        let body = if is_external || (optional_body && ends) {
            None
        } else {
            Some(self.func_body(ret.is_some())?)
        };
        Ok(FuncDecl {
            id: self.id(),
            name,
            span: start.to(self.prev_span()),
            type_params,
            params,
            ret,
            body,
            is_private: false,
        })
    }

    fn func_body(&mut self, has_arrow: bool) -> PResult<Expr> {
        if self.is_sym("{") {
            return self.block();
        }
        if has_arrow {
            return self.error(Code::arrow_without_braces, self.span());
        }
        if self.is_kw("let") || self.is_kw("var") {
            return self.error(Code::statement_body_without_braces, self.span());
        }
        self.expr()
    }

    /// `fields` allows `private` and `readonly`, as in a class header.
    fn params(&mut self, fields: bool) -> PResult<Vec<Param>> {
        self.expect_sym("(")?;
        let mut params = Vec::new();
        self.list_items(")", |p| {
            let (is_private, is_readonly) = if fields {
                p.field_modifiers()?
            } else {
                (false, false)
            };
            let is_var = p.eat_kw("var");
            let is_rest = p.eat_sym("...");
            let (name, span) = p.ident()?;
            let ty = if p.eat_sym(":") {
                Some(p.type_expr()?)
            } else {
                None
            };
            let default = if p.eat_sym("=") {
                Some(p.expr()?)
            } else {
                None
            };
            params.push(Param {
                name,
                span,
                ty,
                default,
                is_var,
                is_rest,
                is_private,
                is_readonly,
            });
            Ok(())
        })?;
        Ok(params)
    }

    /// Eats `private` or `readonly` in front of `let` or `var`.
    fn field_modifiers(&mut self) -> PResult<(bool, bool)> {
        let modifies = matches!(self.peek_at(1), Tok::Kw("let" | "var"));
        let is_private = modifies && self.is_soft("private");
        let is_readonly = modifies && self.is_soft("readonly");
        if is_private || is_readonly {
            self.advance();
        }
        if is_readonly && self.is_kw("let") {
            return self.error(Code::unexpected_token, self.span());
        }
        Ok((is_private, is_readonly))
    }

    fn list_items(
        &mut self,
        close: &str,
        mut item: impl FnMut(&mut Self) -> PResult<()>,
    ) -> PResult<Span> {
        loop {
            self.skip_newlines();
            if self.is_sym(close) {
                return Ok(self.advance().span);
            }
            item(self)?;
            if self.is_sym(",") {
                let comma = self.advance().span;
                if *self.peek() == Tok::Newline || self.is_sym(close) {
                    self.diags
                        .push(Diagnostic::error(Code::trailing_separator, comma));
                }
            } else if *self.peek() != Tok::Newline && !self.is_sym(close) {
                return self.expect_sym(close);
            }
        }
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        let mut ty = if self.eat_kw("func") {
            self.expect_sym("(")?;
            let mut params = Vec::new();
            self.list_items(")", |p| {
                params.push(p.type_expr()?);
                Ok(())
            })?;
            let ret = if self.eat_sym("->") {
                Some(Box::new(self.type_expr()?))
            } else {
                None
            };
            TypeExpr::Func(params, ret)
        } else if self.is_sym("(") {
            let start = self.advance().span;
            let mut names = Vec::new();
            let mut types = Vec::new();
            self.list_items(")", |p| {
                if matches!(p.peek_at(1), Tok::Sym(":")) {
                    names.push(p.field_name()?.0);
                    p.advance();
                }
                types.push(p.type_expr()?);
                Ok(())
            })?;
            let span = start.to(self.prev_span());
            if !names.is_empty() && names.len() != types.len() {
                self.diags.push(Diagnostic::error(Code::mixed_fields, span));
            }
            if names.is_empty() && types.len() == 1 {
                types.remove(0)
            } else {
                TypeExpr::Record(names, types, span)
            }
        } else {
            let (mut name, span) = self.ident()?;
            while self.is_sym(".") && matches!(self.peek_at(1), Tok::Ident(_)) {
                self.advance();
                name = format!("{name}.{}", self.ident()?.0);
            }
            if self.eat_sym("<") {
                let mut args = vec![self.type_expr()?];
                while self.eat_sym(",") {
                    args.push(self.type_expr()?);
                }
                self.close_angle()?;
                TypeExpr::Generic(name, args, span)
            } else {
                TypeExpr::Name(name, span)
            }
        };
        loop {
            if self.eat_sym("?") {
                ty = TypeExpr::Maybe(Box::new(ty));
            } else if self.is_sym("[") && matches!(self.peek_at(1), Tok::Sym("]")) {
                self.advance();
                self.advance();
                ty = TypeExpr::Generic("List".to_string(), vec![ty], start.to(self.prev_span()));
            } else {
                return Ok(ty);
            }
        }
    }

    fn block(&mut self) -> PResult<Expr> {
        let start = self.expect_sym("{")?;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.is_sym("}") {
                break;
            }
            if *self.peek() == Tok::Eof {
                return self.error(Code::unclosed_bracket, start);
            }
            if self.is_kw("func") && matches!(self.peek_at(1), Tok::Ident(_)) {
                self.diags
                    .push(Diagnostic::error(Code::nested_named_function, self.span()));
            }
            if self.is_type_decl() {
                self.diags
                    .push(Diagnostic::error(Code::nested_type, self.span()));
            }
            match self.statement() {
                Ok(stmt) => stmts.push(stmt),
                Err(()) => self.recover(),
            }
            self.end_statement(true);
        }
        let end = self.advance().span;
        Ok(self.expr_node(start.to(end), ExprKind::Block(stmts)))
    }

    fn expr(&mut self) -> PResult<Expr> {
        match self.peek() {
            Tok::Kw("break") => {
                let start = self.advance().span;
                let value = if self.at_expr_end() {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                Ok(self.expr_node(start.to(self.prev_span()), ExprKind::Break(value)))
            }
            Tok::Kw("continue") => {
                let span = self.advance().span;
                Ok(self.expr_node(span, ExprKind::Continue))
            }
            Tok::Kw("return") => {
                let start = self.advance().span;
                let value = if self.at_expr_end() {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                Ok(self.expr_node(start.to(self.prev_span()), ExprKind::Return(value)))
            }
            _ => self.or_expr(),
        }
    }

    fn at_expr_end(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Newline | Tok::Eof | Tok::Sym("}" | ")" | "]" | ";" | ",")
        )
    }

    fn binary(&mut self, op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
        let span = lhs.span.to(rhs.span);
        self.expr_node(span, ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)))
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        while self.eat_kw("or") {
            let rhs = self.and_expr()?;
            lhs = self.binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.not_expr()?;
        while self.eat_kw("and") {
            let rhs = self.not_expr()?;
            lhs = self.binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if self.is_kw("not") {
            let start = self.advance().span;
            let operand = self.not_expr()?;
            return Ok(self.expr_node(
                start.to(operand.span),
                ExprKind::Unary(UnOp::Not, Box::new(operand)),
            ));
        }
        self.comparison()
    }

    fn comparison_op(&self) -> Option<BinOp> {
        match self.peek() {
            Tok::Sym("==") => Some(BinOp::Eq),
            Tok::Sym("!=") => Some(BinOp::Ne),
            Tok::Sym("<") => Some(BinOp::Lt),
            Tok::Sym(">") => Some(BinOp::Gt),
            Tok::Sym("<=") => Some(BinOp::Le),
            Tok::Sym(">=") => Some(BinOp::Ge),
            _ => None,
        }
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let lhs = self.coalesce()?;
        if self.eat_kw("is") {
            let pattern = self.pattern()?;
            let span = lhs.span.to(pattern.span);
            return Ok(self.expr_node(span, ExprKind::Is(Box::new(lhs), pattern)));
        }
        let Some(op) = self.comparison_op() else {
            return Ok(lhs);
        };
        self.advance();
        let rhs = self.coalesce()?;
        let expr = self.binary(op, lhs, rhs);
        if self.comparison_op().is_some() {
            self.diags
                .push(Diagnostic::error(Code::chained_comparison, self.span()));
            while self.comparison_op().is_some() {
                self.advance();
                self.coalesce()?;
            }
        }
        Ok(expr)
    }

    fn coalesce(&mut self) -> PResult<Expr> {
        let lhs = self.range()?;
        if self.eat_sym("??") {
            let rhs = if self.is_kw("break") || self.is_kw("continue") || self.is_kw("return") {
                self.expr()?
            } else {
                self.coalesce()?
            };
            return Ok(self.binary(BinOp::Coalesce, lhs, rhs));
        }
        Ok(lhs)
    }

    fn range(&mut self) -> PResult<Expr> {
        let lhs = self.bitwise()?;
        let op = if self.eat_sym("..") {
            BinOp::Range
        } else if self.eat_sym("..=") {
            BinOp::RangeIncl
        } else {
            return Ok(lhs);
        };
        let rhs = self.bitwise()?;
        Ok(self.binary(op, lhs, rhs))
    }

    fn bitwise(&mut self) -> PResult<Expr> {
        let mut lhs = self.additive()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("&") => BinOp::BitAnd,
                Tok::Sym("|") => BinOp::BitOr,
                Tok::Sym("^") => BinOp::BitXor,
                Tok::Sym("<<") => BinOp::Shl,
                Tok::Sym(">>") => BinOp::Shr,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.additive()?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("+") => BinOp::Add,
                Tok::Sym("-") => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.multiplicative()?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("*") => BinOp::Mul,
                Tok::Sym("/") => BinOp::Div,
                Tok::Sym("%") => BinOp::Mod,
                Tok::Kw("div") => BinOp::IntDiv,
                Tok::Kw("mod") => BinOp::Mod,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.unary()?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Sym("-") => UnOp::Neg,
            Tok::Sym("~") => UnOp::BitNot,
            Tok::Sym("!") => UnOp::Not,
            Tok::Kw("try") => UnOp::Try,
            _ => return self.power(),
        };
        let start = self.advance().span;
        let operand = self.unary()?;
        Ok(self.expr_node(
            start.to(operand.span),
            ExprKind::Unary(op, Box::new(operand)),
        ))
    }

    fn power(&mut self) -> PResult<Expr> {
        let base = self.postfix()?;
        if self.eat_sym("**") {
            let exponent = self.unary()?;
            return Ok(self.binary(BinOp::Pow, base, exponent));
        }
        Ok(base)
    }

    fn args(&mut self) -> PResult<Vec<Arg>> {
        let mut args = Vec::new();
        self.list_items(")", |p| {
            let name = match (p.peek().clone(), p.peek_at(1)) {
                (Tok::Ident(name), Tok::Sym("=")) => {
                    p.advance();
                    p.advance();
                    Some(name)
                }
                _ => None,
            };
            let start = p.span();
            let value = if name.is_none() && p.eat_sym("...") {
                let inner = p.expr()?;
                p.expr_node(start.to(inner.span), ExprKind::Spread(Box::new(inner)))
            } else {
                p.expr()?
            };
            args.push(Arg { name, value });
            Ok(())
        })?;
        Ok(args)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.primary()?;
        loop {
            if self.eat_sym("(") {
                let args = self.args()?;
                let span = expr.span.to(self.prev_span());
                expr = self.expr_node(span, ExprKind::Call(Box::new(expr), args));
            } else if self.eat_sym(".") {
                let name = match self.peek().clone() {
                    Tok::Int(n) => {
                        self.advance();
                        n.to_string()
                    }
                    _ => self.field_name()?.0,
                };
                if !self.eat_sym("(") {
                    let span = expr.span.to(self.prev_span());
                    expr = self.expr_node(span, ExprKind::Field(Box::new(expr), name));
                    continue;
                }
                let args = self.args()?;
                let span = expr.span.to(self.prev_span());
                expr = self.expr_node(span, ExprKind::Method(Box::new(expr), name, args));
            } else if self.eat_sym("[") {
                self.skip_newlines();
                let index = self.expr()?;
                self.skip_newlines();
                self.expect_sym("]")?;
                let span = expr.span.to(self.prev_span());
                expr = self.expr_node(span, ExprKind::Index(Box::new(expr), Box::new(index)));
            } else {
                return Ok(expr);
            }
        }
    }

    fn condition(&mut self) -> PResult<Expr> {
        let cond = self.expr()?;
        if self.is_sym("=") {
            return self.error(Code::assignment_in_condition, self.span());
        }
        Ok(cond)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let token = self.advance();
        let span = token.span;
        let kind = match token.tok {
            Tok::Int(n) => ExprKind::Int(n),
            Tok::Float(f) => ExprKind::Float(f),
            Tok::Kw("true") => ExprKind::Bool(true),
            Tok::Kw("false") => ExprKind::Bool(false),
            Tok::Kw("this") => ExprKind::This,
            Tok::Ident(name) => ExprKind::Name(name),
            Tok::Str(parts) => ExprKind::Str(self.string_parts(parts)?),
            Tok::Sym("(") => {
                let mut args = self.args()?;
                let span = span.to(self.prev_span());
                if let [Arg { name: None, value }] = args.as_slice()
                    && !matches!(value.kind, ExprKind::Spread(_))
                {
                    return Ok(args.remove(0).value);
                }
                return Ok(self.expr_node(span, ExprKind::Record(args)));
            }
            Tok::Sym("[") => {
                if self.eat_sym("=") {
                    let end = self.expect_sym("]")?;
                    return Ok(self.expr_node(span.to(end), ExprKind::Map(Vec::new())));
                }
                let mut items = Vec::new();
                let mut entries = Vec::new();
                let end = self.list_items("]", |p| {
                    let item = p.expr()?;
                    if p.is_sym("=") && items.is_empty() {
                        p.advance();
                        entries.push((item, p.expr()?));
                    } else if entries.is_empty() {
                        items.push(item);
                    } else {
                        return p.error(Code::unexpected_token, item.span);
                    }
                    Ok(())
                })?;
                let kind = if entries.is_empty() {
                    ExprKind::List(items)
                } else {
                    ExprKind::Map(entries)
                };
                return Ok(self.expr_node(span.to(end), kind));
            }
            Tok::Sym("{") => {
                self.pos -= 1;
                return self.block();
            }
            Tok::Kw("func") => {
                self.pos -= 1;
                self.advance();
                let params = self.params(false)?;
                let ret = if self.eat_sym("->") {
                    Some(self.type_expr()?)
                } else {
                    None
                };
                let body = self.func_body(ret.is_some())?;
                return Ok(self.expr_node(
                    span.to(body.span),
                    ExprKind::Lambda(params, ret, Box::new(body)),
                ));
            }
            Tok::Kw("if") => return self.if_expr(span),
            Tok::Kw("match") => return self.match_expr(span),
            Tok::Kw("while") => {
                let cond = self.condition()?;
                let body = self.block()?;
                return Ok(self.expr_node(
                    span.to(body.span),
                    ExprKind::While(Box::new(cond), Box::new(body)),
                ));
            }
            Tok::Kw("loop") => {
                let body = self.block()?;
                return Ok(self.expr_node(span.to(body.span), ExprKind::Loop(Box::new(body))));
            }
            Tok::Kw("for") => {
                let pattern = self.pattern()?;
                if !self.eat_kw("in") {
                    return self.error(Code::unexpected_token, self.span());
                }
                let iter = self.expr()?;
                let body = self.block()?;
                return Ok(self.expr_node(
                    span.to(body.span),
                    ExprKind::For(pattern, Box::new(iter), Box::new(body)),
                ));
            }
            Tok::Kw("break" | "continue" | "return") => {
                self.pos -= 1;
                return self.expr();
            }
            Tok::Eof => return self.error(Code::unclosed_bracket, span),
            _ => return self.error(Code::unexpected_token, span),
        };
        Ok(self.expr_node(span, kind))
    }

    fn if_expr(&mut self, start: Span) -> PResult<Expr> {
        let cond = self.condition()?;
        let then = self.block()?;
        if *self.peek() == Tok::Newline {
            let mut ahead = 0;
            while *self.peek_at(ahead) == Tok::Newline {
                ahead += 1;
            }
            if matches!(self.peek_at(ahead), Tok::Kw("else")) {
                let span = self.tokens[self.pos + ahead].span;
                self.diags
                    .push(Diagnostic::error(Code::else_on_new_line, span));
                self.pos += ahead;
            }
        }
        let otherwise = if self.eat_kw("else") {
            if self.is_kw("if") {
                let span = self.advance().span;
                Some(Box::new(self.if_expr(span)?))
            } else {
                Some(Box::new(self.block()?))
            }
        } else {
            None
        };
        let end = otherwise.as_ref().map_or(then.span, |e| e.span);
        Ok(self.expr_node(
            start.to(end),
            ExprKind::If(Box::new(cond), Box::new(then), otherwise),
        ))
    }

    fn string_parts(&mut self, parts: Vec<StrPart>) -> PResult<Vec<StrSeg>> {
        let mut segs = Vec::new();
        for part in parts {
            match part {
                StrPart::Text(t) => segs.push(StrSeg::Text(t)),
                StrPart::Expr(tokens) => {
                    if tokens.len() == 1 {
                        return Err(());
                    }
                    let mut sub = Parser {
                        tokens,
                        pos: 0,
                        next_id: self.next_id,
                        diags: self.diags,
                    };
                    let expr = sub.expr()?;
                    if *sub.peek() != Tok::Eof {
                        let span = sub.span();
                        return sub.error(Code::unexpected_token, span);
                    }
                    segs.push(StrSeg::Expr(expr));
                }
            }
        }
        Ok(segs)
    }
}
