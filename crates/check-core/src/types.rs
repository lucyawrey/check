use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Void,
    Never,
    Range,
    List(Box<Ty>),
    Maybe(Box<Ty>),
    Func(Vec<Ty>, Box<Ty>),
    Named(String, Vec<Ty>),
    /// A type parameter inside its own declaration, equal only to itself.
    Param(String),
    Record(Vec<String>, Vec<Ty>),
    Var(usize),
    Error,
}

impl Ty {
    pub fn list(elem: Ty) -> Ty {
        Ty::List(Box::new(elem))
    }

    pub fn maybe(inner: Ty) -> Ty {
        Ty::Maybe(Box::new(inner))
    }

    pub fn named(name: &str) -> Ty {
        Ty::Named(name.to_string(), Vec::new())
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float)
    }

    /// Replaces the type parameters `map` names.
    pub fn substitute(&self, map: &HashMap<String, Ty>) -> Ty {
        if map.is_empty() {
            return self.clone();
        }
        let each = |tys: &[Ty]| tys.iter().map(|t| t.substitute(map)).collect();
        match self {
            Ty::Param(p) => map.get(p).cloned().unwrap_or_else(|| self.clone()),
            Ty::List(e) => Ty::list(e.substitute(map)),
            Ty::Maybe(e) => Ty::maybe(e.substitute(map)),
            Ty::Func(params, ret) => Ty::Func(each(params), Box::new(ret.substitute(map))),
            Ty::Named(n, args) => Ty::Named(n.clone(), each(args)),
            Ty::Record(names, fields) => Ty::Record(names.clone(), each(fields)),
            other => other.clone(),
        }
    }
}

pub struct Shape {
    pub params: Vec<String>,
    pub is_enum: bool,
    pub variants: Vec<(String, Vec<(String, Ty, bool)>)>,
}

#[derive(Default)]
pub struct Subst {
    bindings: Vec<Option<Ty>>,
}

impl Subst {
    pub fn fresh(&mut self) -> Ty {
        self.bindings.push(None);
        Ty::Var(self.bindings.len() - 1)
    }

    pub fn shallow(&self, ty: &Ty) -> Ty {
        let mut ty = ty.clone();
        while let Ty::Var(v) = ty {
            match &self.bindings[v] {
                Some(bound) => ty = bound.clone(),
                None => return ty,
            }
        }
        ty
    }

    pub fn resolve(&self, ty: &Ty) -> Ty {
        match self.shallow(ty) {
            Ty::List(e) => Ty::list(self.resolve(&e)),
            Ty::Maybe(e) => Ty::maybe(self.resolve(&e)),
            Ty::Func(params, ret) => Ty::Func(
                params.iter().map(|p| self.resolve(p)).collect(),
                Box::new(self.resolve(&ret)),
            ),
            Ty::Record(names, fields) => {
                Ty::Record(names, fields.iter().map(|f| self.resolve(f)).collect())
            }
            Ty::Named(n, args) => Ty::Named(n, args.iter().map(|a| self.resolve(a)).collect()),
            other => other,
        }
    }

    fn occurs(&self, v: usize, ty: &Ty) -> bool {
        match self.shallow(ty) {
            Ty::Var(w) => v == w,
            Ty::List(e) | Ty::Maybe(e) => self.occurs(v, &e),
            Ty::Func(params, ret) => {
                params.iter().any(|p| self.occurs(v, p)) || self.occurs(v, &ret)
            }
            Ty::Record(_, fields) | Ty::Named(_, fields) => {
                fields.iter().any(|f| self.occurs(v, f))
            }
            _ => false,
        }
    }

    pub fn unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let (a, b) = (self.shallow(a), self.shallow(b));
        match (&a, &b) {
            (Ty::Var(x), Ty::Var(y)) if x == y => true,
            (Ty::Var(v), other) | (other, Ty::Var(v)) => {
                if self.occurs(*v, other) {
                    return false;
                }
                self.bindings[*v] = Some(other.clone());
                true
            }
            (Ty::Error, _) | (_, Ty::Error) => true,
            (Ty::List(x), Ty::List(y)) | (Ty::Maybe(x), Ty::Maybe(y)) => self.unify(x, y),
            (Ty::Func(p1, r1), Ty::Func(p2, r2)) => {
                p1.len() == p2.len()
                    && p1.iter().zip(p2).all(|(x, y)| self.unify(x, y))
                    && self.unify(r1, r2)
            }
            (Ty::Named(n1, a1), Ty::Named(n2, a2)) => {
                n1 == n2 && a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| self.unify(x, y))
            }
            (Ty::Record(n1, f1), Ty::Record(n2, f2)) => {
                n1 == n2 && f1.len() == f2.len() && f1.iter().zip(f2).all(|(x, y)| self.unify(x, y))
            }
            _ => a == b,
        }
    }
}

pub struct Show<'a>(pub &'a Ty);

impl fmt::Display for Show<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0 {
            Ty::Int => f.write_str("Int"),
            Ty::Float => f.write_str("Float"),
            Ty::Bool => f.write_str("Bool"),
            Ty::Str => f.write_str("String"),
            Ty::Void => f.write_str("Void"),
            Ty::Never => f.write_str("Never"),
            Ty::Range => f.write_str("Range<Int>"),
            Ty::Named(name, args) => {
                f.write_str(name)?;
                if !args.is_empty() {
                    let args: Vec<String> = args.iter().map(|a| Show(a).to_string()).collect();
                    write!(f, "<{}>", args.join(", "))?;
                }
                Ok(())
            }
            Ty::Param(name) => f.write_str(name),
            Ty::List(e) => write!(f, "List<{}>", Show(e)),
            Ty::Maybe(e) => write!(f, "{}?", Show(e)),
            Ty::Func(params, ret) => {
                f.write_str("func(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", Show(p))?;
                }
                f.write_str(")")?;
                if **ret != Ty::Void {
                    write!(f, " -> {}", Show(ret))?;
                }
                Ok(())
            }
            Ty::Record(names, fields) => {
                f.write_str("(")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    match names.get(i) {
                        Some(name) => write!(f, "{name}: {}", Show(field))?,
                        None => write!(f, "{}", Show(field))?,
                    }
                }
                f.write_str(")")
            }
            Ty::Var(_) | Ty::Error => f.write_str("_"),
        }
    }
}
