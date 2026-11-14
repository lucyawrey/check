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

pub fn parse_program(
    tokens: Vec<Token>,
    next_id: &mut NodeId,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Stmt> {
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

    fn program(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if *self.peek() == Tok::Eof {
                break;
            }
            match self.statement() {
                Ok(stmt) => stmts.push(stmt),
                Err(()) => self.recover(),
            }
            self.end_statement(false);
        }
        stmts
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
                self.recover();
            }
        }
    }

    fn statement(&mut self) -> PResult<Stmt> {
        if self.is_kw("let") || self.is_kw("var") {
            return self.let_stmt();
        }
        if self.is_kw("func") && matches!(self.peek_at(1), Tok::Ident(_)) {
            return Ok(Stmt::Func(self.func_decl()?));
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

    fn func_decl(&mut self) -> PResult<FuncDecl> {
        let start = self.advance().span;
        let (name, _) = self.ident()?;
        let params = self.params()?;
        let ret = if self.eat_sym("->") {
            Some(self.type_expr()?)
        } else {
            None
        };
        let body = self.func_body(ret.is_some())?;
        Ok(FuncDecl {
            id: self.id(),
            name,
            span: start.to(self.prev_span()),
            params,
            ret,
            body,
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

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect_sym("(")?;
        let mut params = Vec::new();
        self.list_items(")", |p| {
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
            });
            Ok(())
        })?;
        Ok(params)
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
        } else {
            let (name, span) = self.ident()?;
            if self.eat_sym("<") {
                let mut args = vec![self.type_expr()?];
                while self.eat_sym(",") {
                    args.push(self.type_expr()?);
                }
                self.expect_sym(">")?;
                TypeExpr::Generic(name, args, span)
            } else {
                TypeExpr::Name(name, span)
            }
        };
        while self.eat_sym("?") {
            ty = TypeExpr::Maybe(Box::new(ty));
        }
        Ok(ty)
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
            args.push(Arg {
                name,
                value: p.expr()?,
            });
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
                let (name, name_span) = self.ident()?;
                if !self.eat_sym("(") {
                    return self.error(Code::unexpected_token, name_span);
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
            Tok::Ident(name) => ExprKind::Name(name),
            Tok::Str(parts) => ExprKind::Str(self.string_parts(parts)?),
            Tok::Sym("(") => {
                self.skip_newlines();
                let inner = self.expr()?;
                self.skip_newlines();
                self.expect_sym(")")?;
                return Ok(inner);
            }
            Tok::Sym("[") => {
                let mut items = Vec::new();
                let end = self.list_items("]", |p| {
                    items.push(p.expr()?);
                    Ok(())
                })?;
                return Ok(self.expr_node(span.to(end), ExprKind::List(items)));
            }
            Tok::Sym("{") => {
                self.pos -= 1;
                return self.block();
            }
            Tok::Kw("func") => {
                self.pos -= 1;
                self.advance();
                let params = self.params()?;
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
                let (name, name_span) = self.ident()?;
                if !self.eat_kw("in") {
                    return self.error(Code::unexpected_token, self.span());
                }
                let iter = self.expr()?;
                let body = self.block()?;
                return Ok(self.expr_node(
                    span.to(body.span),
                    ExprKind::For(name, name_span, Box::new(iter), Box::new(body)),
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
