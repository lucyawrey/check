mod data;
mod de;

use std::fmt;

use check_syntax::diag::{Code, Diagnostic, Severity, Sources};
use check_syntax::{lexer, parser};
use serde::de::DeserializeOwned;

use data::Data;
pub use de::Error as ValueError;

pub enum Error {
    /// The file didn't parse or pass the checker, or a constructor's `init` panicked.
    Check {
        diagnostics: Vec<Diagnostic>,
        sources: Sources,
        panic_text: Option<String>,
    },
    /// The value doesn't fit the Rust type, so the type doesn't match the file or Check class.
    Value(ValueError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Check {
                diagnostics,
                sources,
                panic_text,
            } => {
                for d in diagnostics.iter().filter(|d| d.severity == Severity::Error) {
                    write!(f, "{}", d.render(sources, false))?;
                }
                write!(f, "{}", panic_text.as_deref().unwrap_or_default())
            }
            Error::Value(e) => write!(f, "{e}"),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for Error {}

fn deserialize<T: DeserializeOwned>(data: Data) -> Result<T, Error> {
    T::deserialize(de::ValueDe(data)).map_err(Error::Value)
}

/// Reads a `.con` file into `T`, checking it as `check_core::check_data` does.
#[cfg(feature = "check")]
pub fn from_str<T: DeserializeOwned>(
    files: &dyn check_core::Files,
    schema: &str,
    name: &str,
    path: &str,
    text: &str,
    expected_type: &str,
) -> Result<T, Error> {
    from_str_with_class(files, schema, name, path, text, expected_type).map(|(value, _)| value)
}

/// Like `from_str`, and also returns the class the file's value is written as, as
/// `check_core::check_data` does.
#[cfg(feature = "check")]
pub fn from_str_with_class<T: DeserializeOwned>(
    files: &dyn check_core::Files,
    schema: &str,
    name: &str,
    path: &str,
    text: &str,
    expected_type: &str,
) -> Result<(T, Option<String>), Error> {
    let (value, class) = check_core::check_data(files, schema, name, path, text, expected_type)
        .map_err(|r| Error::Check {
            diagnostics: r.diagnostics,
            sources: r.sources,
            panic_text: r.panic_text,
        })?;
    Ok((deserialize(value.into())?, class))
}

/// Reads a `.con` file into `T` without a Check class: no defaults, `init` or type checks,
/// a constructor's class name is ignored, and variants match by name as written.
pub fn from_str_unchecked<T: DeserializeOwned>(path: &str, text: &str) -> Result<T, Error> {
    let mut sources = Sources::default();
    let mut diagnostics = Vec::new();
    let base = sources.add(path, text);
    let tokens = lexer::lex(text, base, true, &mut diagnostics);
    let file = parser::parse_data(tokens, &mut 0, &mut diagnostics);
    if !path.ends_with(".con") {
        diagnostics.push(Diagnostic::error(Code::not_pure_data, file.value.span));
    }
    let data = Data::from_expr(&file.value, &mut diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(Error::Check {
            diagnostics,
            sources,
            panic_text: None,
        });
    }
    deserialize(data)
}
