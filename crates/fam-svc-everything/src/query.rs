//! Compiling a [`SearchRequest`] into the string `Everything_SetSearchW` takes.
//!
//! Everything's search box is a small language: `ext:`, `path:`, `!`, `|`, `<…>` and bare words
//! separated by whitespace each mean something specific. A file name a user typed is not that
//! language, so every piece of the request goes out **quoted**, which is the one form meaning "this
//! text, in a file name" — typing `exe:setup` must search for a file called that, not filter by
//! extension.
//!
//! Where quoting cannot express the request — a term containing a quote character, for which
//! Everything has no escape — the term is split and its fragments AND'ed. That can only *widen* the
//! answer, and nothing downstream depends on the query being exact: a provider re-checks every hit
//! with [`SearchRequest::matches_name`], so a wide query costs rows examined, never a wrong row
//! shown.
//!
//! Two parts of the request deliberately do **not** become query text. `limit` is a paging parameter
//! (`Everything_SetMax`), not a filter. And `kind` is not pushed down as an `ext:` list: that would
//! mean keeping a second copy of the extension-to-bucket table that [`fam_core::Kind`] is derived
//! from, with the two drifting — so the provider applies the same bucket decision the walker applies,
//! after Everything has narrowed the field.

use fam_core::{raw_path_units, SearchRequest};

const QUOTE: u16 = b'"' as u16;
const SPACE: u16 = b' ' as u16;

/// The compiled query, or `None` when there is nothing worth asking.
///
/// Two ways to get `None`: the terms compile to no tokens at all (a query of nothing but a quote
/// character), and there are no terms in the first place. The second is not academic caution - a bare
/// `path:"D:/Docs"` is a perfectly good Everything query meaning "everything under here", which is a
/// *listing*, and listings never go to Everything. A caller that asks for a search with no search
/// words gets `None` rather than a whole-subtree dump.
pub fn compile(request: &SearchRequest) -> Option<Vec<u16>> {
    let mut tokens: Vec<Vec<u16>> = Vec::new();
    for term in &request.terms {
        tokens.extend(quoted(&term.encode_utf16().collect::<Vec<u16>>()));
    }
    if tokens.is_empty() {
        return None;
    }
    if let Some(scope) = request
        .scope
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
    {
        tokens.extend(
            quoted(&raw_path_units(scope))
                .into_iter()
                .map(|token| prefix("path:", token)),
        );
    }
    Some(tokens.join(&SPACE))
}

/// `text` as one quoted token per piece, splitting on the quote character Everything cannot escape.
fn quoted(text: &[u16]) -> Vec<Vec<u16>> {
    text.split(|unit| *unit == QUOTE)
        .filter(|piece| !piece.is_empty())
        .map(|piece| {
            let mut token = Vec::with_capacity(piece.len() + 2);
            token.push(QUOTE);
            token.extend_from_slice(piece);
            token.push(QUOTE);
            token
        })
        .collect()
}

fn prefix(head: &str, mut token: Vec<u16>) -> Vec<u16> {
    let mut prefixed = head.encode_utf16().collect::<Vec<u16>>();
    prefixed.append(&mut token);
    prefixed
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn text(units: &[u16]) -> String {
        String::from_utf16(units).expect("compiled queries are valid UTF-16 by construction")
    }

    fn compiled(query: &str) -> String {
        text(&compile(&SearchRequest::new(query)).expect("a query with terms compiles"))
    }

    #[test]
    fn every_word_goes_out_quoted_and_anded() {
        assert_eq!(compiled("q1  Report "), r#""q1" "report""#);
    }

    /// The whole point of quoting: Everything's own syntax characters reach it as text.
    #[test]
    fn a_word_is_never_read_as_an_everything_function() {
        for typed in [
            "exe:setup",
            "!hide",
            "a|b",
            "<pdf>",
            "dm:2026",
            "size:>1mb",
            "\\escape",
        ] {
            let out = compiled(typed);
            assert_eq!(
                out,
                format!(r#""{typed}""#),
                "{typed} leaked into the language"
            );
        }
    }

    #[test]
    fn the_scope_becomes_a_path_qualifier() {
        let mut request = SearchRequest::new("report").scope(PathBuf::from("D:/Docs"));
        request.terms.push("2026".into());
        let out = text(&compile(&request).unwrap());
        assert_eq!(out, r#""report" "2026" path:"D:/Docs""#);
    }

    #[test]
    fn a_scope_is_kept_verbatim_except_for_the_quotes_around_it() {
        // `raw_path_units` is what keeps `C:\Users\Kirig` from arriving folded; the compiler adds
        // quotes and nothing else.
        let request = SearchRequest::new("x").scope(Path::new(r"C:\Users\Kirig\Docs"));
        assert_eq!(
            text(&compile(&request).unwrap()),
            r#""x" path:"C:\Users\Kirig\Docs""#
        );
    }

    #[test]
    fn a_quote_in_a_word_splits_it_rather_than_breaking_the_query() {
        assert_eq!(compiled(r#"a"b"#), r#""a" "b""#);
        assert_eq!(compiled(r#""quoted""#), r#""quoted""#);
    }

    #[test]
    fn nothing_to_ask_is_not_an_empty_search() {
        assert_eq!(compile(&SearchRequest::new("   ")), None);
        assert_eq!(compile(&SearchRequest::new("\"")), None);
        // A scope alone would be "everything under here", which is a listing, not a search.
        let scope_only = SearchRequest {
            tag_groups: Vec::new(),
            tags_not: Vec::new(),
            terms: Vec::new(),
            tags: Vec::new(),
            scope: Some(PathBuf::from("D:/Docs")),
            kind: None,
            limit: 0,
        };
        assert_eq!(compile(&scope_only), None);
    }

    #[test]
    fn quoting_loses_no_character_the_user_typed() {
        // `SearchRequest::new` lower-cases terms on the way in, which is the form Everything matches
        // against anyway; the point here is that quoting neither drops nor rewrites a character.
        let out = compiled("报告 2026-Q1.final");
        assert_eq!(out, r#""报告" "2026-q1.final""#);
    }
}
