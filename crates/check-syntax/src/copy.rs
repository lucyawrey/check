use std::collections::HashMap;
use std::sync::OnceLock;

use crate::ast::{ExprKind, StrSeg};
use crate::diag::Severity;
use crate::{lexer, parser};

macro_rules! texts {
    ($($key:ident,)*) => {
        /// Text other than diagnostic messages, kept in `copy.con`.
        #[derive(Debug, Clone, Copy)]
        #[allow(non_camel_case_types)]
        pub enum Text { $($key,)* }

        impl Text {
            pub const ALL: &[Text] = &[$(Text::$key,)*];

            pub fn key(self) -> &'static str {
                match self { $(Text::$key => stringify!($key),)* }
            }

            pub fn get(self) -> &'static str {
                text(self.key())
            }
        }
    };
}

texts! {
    hint_ref_argument,
    hint_shared_argument,
    hint_try_returns,
    detail_or_shared,
    detail_already_shared,
    fix_title,
    fix_all_title,
    tidy_summary,
    tidy_errors_remain,
    tidy_too_many_passes,
    tidy_dry_run,
    calls_left_out,
    con_value_unsupported,
}

pub(crate) fn text(key: &str) -> &'static str {
    static COPY: OnceLock<HashMap<String, String>> = OnceLock::new();
    match COPY.get_or_init(load).get(key) {
        Some(text) => text,
        None => panic!("copy.con has no `{key}`"),
    }
}

fn load() -> HashMap<String, String> {
    let mut diags = Vec::new();
    let tokens = lexer::lex(include_str!("../copy.con"), 0, true, &mut diags);
    let file = parser::parse_data(tokens, &mut 0, &mut diags);
    // Rendering a diagnostic needs its message, which is in the copy being loaded.
    if let Some(d) = diags.iter().find(|d| d.severity == Severity::Error) {
        panic!(
            "copy.con doesn't parse: {} at byte {}",
            d.code, d.span.start
        );
    }
    let ExprKind::Record(lines) = file.value.kind else {
        panic!("copy.con isn't a list of `key = \"text\"` lines");
    };
    let mut copy = HashMap::new();
    for line in lines {
        let key = line.name.expect("each line in copy.con has a key");
        let ExprKind::Str(segments) = line.value.kind else {
            panic!("`{key}` in copy.con isn't a string");
        };
        let text = segments
            .into_iter()
            .map(|s| match s {
                StrSeg::Text(t) => t,
                StrSeg::Expr(_) => unreachable!("pure data has no interpolation"),
            })
            .collect();
        if copy.insert(key.clone(), text).is_some() {
            panic!("copy.con has `{key}` twice");
        }
    }
    copy
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::diag::Code;

    #[test]
    fn every_message_has_copy_and_no_copy_is_unused() {
        let mut wanted: HashSet<&str> = (Code::ALL.iter().map(|c| c.name()))
            .chain(Text::ALL.iter().map(|t| t.key()))
            .collect();
        for (key, text) in load() {
            assert!(
                wanted.remove(key.as_str()),
                "copy.con has `{key}`, which nothing uses"
            );
            assert!(!text.trim().is_empty(), "`{key}` in copy.con is empty");
        }
        assert!(wanted.is_empty(), "copy.con is missing {wanted:?}");
    }
}
