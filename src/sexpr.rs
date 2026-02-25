//! Zero-dependency recursive descent S-expression parser.
//!
//! Grammar: `sexpr = atom | '(' sexpr* ')'`
//! - Atoms: bare words (`foo`, `123`) or `"quoted strings"` with `\"` / `\\` escapes
//! - `;` starts a line comment (to end of line)
//! - Whitespace is insignificant outside quotes

use std::fmt;

/// A parsed S-expression: either an atom, a quoted string, or a list of sub-expressions.
///
/// The distinction between `Atom` and `Str` matters for the Lisp evaluator:
/// - `Atom` is a bare word; the evaluator treats it as a symbol to look up.
/// - `Str` was written with double quotes; the evaluator treats it as a string literal.
///
/// The compose DSL does not care about this distinction — both `as_atom()` and `as_list()`
/// behave identically to the old `Atom`-only world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SExpr {
    /// An unquoted bare word: a symbol in Lisp context.
    Atom(String),
    /// A double-quoted string literal.
    Str(String),
    List(Vec<SExpr>),
    /// An improper list: `(a b . c)` — head items plus a non-nil tail.
    DottedList(Vec<SExpr>, Box<SExpr>),
}

impl SExpr {
    /// Return the string value, or `None` if this is a list.
    ///
    /// Returns `Some` for both `Atom` and `Str` variants — callers that don't
    /// distinguish between symbols and strings (e.g. the compose DSL) can use
    /// this method unchanged.
    pub fn as_atom(&self) -> Option<&str> {
        match self {
            SExpr::Atom(s) | SExpr::Str(s) => Some(s.as_str()),
            SExpr::List(_) | SExpr::DottedList(_, _) => None,
        }
    }

    /// Return the list contents, or `None` if this is an atom, string, or dotted list.
    pub fn as_list(&self) -> Option<&[SExpr]> {
        match self {
            SExpr::Atom(_) | SExpr::Str(_) | SExpr::DottedList(_, _) => None,
            SExpr::List(v) => Some(v),
        }
    }

    /// Returns `true` if this is a bare-word atom (not a quoted string).
    pub fn is_symbol(&self) -> bool {
        matches!(self, SExpr::Atom(_))
    }
}

impl fmt::Display for SExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SExpr::Atom(s) => {
                if s.contains(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '"') {
                    write!(f, "\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    write!(f, "{}", s)
                }
            }
            SExpr::Str(s) => {
                write!(f, "\"")?;
                for c in s.chars() {
                    match c {
                        '"' => write!(f, "\\\"")?,
                        '\\' => write!(f, "\\\\")?,
                        '\n' => write!(f, "\\n")?,
                        '\t' => write!(f, "\\t")?,
                        c => write!(f, "{}", c)?,
                    }
                }
                write!(f, "\"")
            }
            SExpr::List(items) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, ")")
            }
            SExpr::DottedList(items, tail) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, " . {}", tail)?;
                write!(f, ")")
            }
        }
    }
}

/// Parse error with position information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Byte offset in the original input.
    pub position: usize,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number (character, not byte).
    pub col: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

/// Convert a byte offset to a (line, col) pair (both 1-based).
fn line_col(input: &str, pos: usize) -> (usize, usize) {
    let prefix = &input[..pos.min(input.len())];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = prefix
        .rfind('\n')
        .map_or(prefix.len(), |n| prefix.len() - n - 1)
        + 1;
    (line, col)
}

/// Build a `ParseError` at `pos` with the given message.
fn make_err(input: &str, pos: usize, message: impl Into<String>) -> ParseError {
    let (line, col) = line_col(input, pos);
    ParseError {
        message: message.into(),
        position: pos,
        line,
        col,
    }
}

impl std::error::Error for ParseError {}

/// Parse an S-expression from input text.
///
/// The input should contain exactly one top-level expression (typically a list).
pub fn parse(input: &str) -> Result<SExpr, ParseError> {
    let mut pos = 0;
    skip_ws_and_comments(input, &mut pos);
    if pos >= input.len() {
        return Err(make_err(input, 0, "empty input"));
    }
    let expr = parse_sexpr(input, &mut pos)?;
    skip_ws_and_comments(input, &mut pos);
    if pos < input.len() {
        return Err(make_err(input, pos, "unexpected trailing input"));
    }
    Ok(expr)
}

/// Parse all top-level S-expressions from `input`.
///
/// Unlike [`parse`], this accepts zero or more expressions and returns them all.
/// Whitespace and comments between expressions are ignored.
/// Used by the Lisp interpreter to read a `.reml` program.
pub fn parse_all(input: &str) -> Result<Vec<SExpr>, ParseError> {
    let mut pos = 0;
    let mut exprs = Vec::new();
    loop {
        skip_ws_and_comments(input, &mut pos);
        if pos >= input.len() {
            break;
        }
        exprs.push(parse_sexpr(input, &mut pos)?);
    }
    Ok(exprs)
}

fn parse_sexpr(input: &str, pos: &mut usize) -> Result<SExpr, ParseError> {
    skip_ws_and_comments(input, pos);
    if *pos >= input.len() {
        return Err(make_err(input, *pos, "unexpected end of input"));
    }

    let ch = input.as_bytes()[*pos];
    match ch {
        b'(' => parse_list(input, pos),
        b')' => Err(make_err(input, *pos, "unexpected ')'")),
        // Reader macros: 'x → (quote x), `x → (quasiquote x),
        //                ,@x → (unquote-splicing x), ,x → (unquote x)
        b'\'' => {
            *pos += 1;
            let inner = parse_sexpr(input, pos)?;
            Ok(SExpr::List(vec![SExpr::Atom("quote".into()), inner]))
        }
        b'`' => {
            *pos += 1;
            let inner = parse_sexpr(input, pos)?;
            Ok(SExpr::List(vec![SExpr::Atom("quasiquote".into()), inner]))
        }
        b',' => {
            *pos += 1;
            if *pos < input.len() && input.as_bytes()[*pos] == b'@' {
                *pos += 1;
                let inner = parse_sexpr(input, pos)?;
                Ok(SExpr::List(vec![
                    SExpr::Atom("unquote-splicing".into()),
                    inner,
                ]))
            } else {
                let inner = parse_sexpr(input, pos)?;
                Ok(SExpr::List(vec![SExpr::Atom("unquote".into()), inner]))
            }
        }
        _ => parse_atom(input, pos),
    }
}

fn parse_list(input: &str, pos: &mut usize) -> Result<SExpr, ParseError> {
    let open_pos = *pos;
    *pos += 1; // skip '('
    let mut items = Vec::new();
    loop {
        skip_ws_and_comments(input, pos);
        if *pos >= input.len() {
            return Err(make_err(input, open_pos, "unmatched '('"));
        }
        if input.as_bytes()[*pos] == b')' {
            *pos += 1; // skip ')'
            return Ok(SExpr::List(items));
        }
        // Dotted pair separator: '.' followed immediately by a delimiter.
        // This distinguishes `(a . b)` from atoms like `.5` or `...`.
        if is_dot_separator(input, *pos) {
            *pos += 1; // skip '.'
            skip_ws_and_comments(input, pos);
            if *pos >= input.len() {
                return Err(make_err(
                    input,
                    open_pos,
                    "dotted pair: expected tail after '.'",
                ));
            }
            let tail = parse_sexpr(input, pos)?;
            skip_ws_and_comments(input, pos);
            if *pos >= input.len() || input.as_bytes()[*pos] != b')' {
                return Err(make_err(
                    input,
                    *pos,
                    "dotted pair: expected ')' after tail expression",
                ));
            }
            *pos += 1; // skip ')'
            return Ok(SExpr::DottedList(items, Box::new(tail)));
        }
        items.push(parse_sexpr(input, pos)?);
    }
}

/// Return `true` if position `pos` is a dotted-pair `.` separator, i.e. a lone `.`
/// followed by whitespace, `(`, `)`, `;`, or end-of-input.
fn is_dot_separator(input: &str, pos: usize) -> bool {
    let bytes = input.as_bytes();
    if bytes[pos] != b'.' {
        return false;
    }
    let next = pos + 1;
    if next >= bytes.len() {
        return true;
    }
    let ch = bytes[next];
    ch.is_ascii_whitespace() || ch == b'(' || ch == b')' || ch == b';' || ch == b'"'
}

fn parse_atom(input: &str, pos: &mut usize) -> Result<SExpr, ParseError> {
    if input.as_bytes()[*pos] == b'"' {
        parse_quoted_string(input, pos)
    } else {
        parse_bare_word(input, pos)
    }
}

fn parse_quoted_string(input: &str, pos: &mut usize) -> Result<SExpr, ParseError> {
    let start = *pos;
    *pos += 1; // skip opening '"'
    let mut s = String::new();
    let bytes = input.as_bytes();
    while *pos < bytes.len() {
        if bytes[*pos] == b'\\' {
            *pos += 1;
            if *pos >= bytes.len() {
                return Err(make_err(input, start, "unterminated escape in string"));
            }
            match bytes[*pos] {
                b'"' => s.push('"'),
                b'\\' => s.push('\\'),
                b'n' => s.push('\n'),
                b't' => s.push('\t'),
                other => {
                    s.push('\\');
                    s.push(other as char); // escape sequences are ASCII
                }
            }
            *pos += 1;
        } else if bytes[*pos] == b'"' {
            *pos += 1; // skip closing '"'
            return Ok(SExpr::Str(s));
        } else {
            // Decode one full Unicode scalar value, advancing by its byte length.
            let ch = input[*pos..].chars().next().unwrap();
            s.push(ch);
            *pos += ch.len_utf8();
        }
    }
    Err(make_err(input, start, "unterminated string"))
}

fn parse_bare_word(input: &str, pos: &mut usize) -> Result<SExpr, ParseError> {
    let start = *pos;
    let bytes = input.as_bytes();
    while *pos < bytes.len() {
        let ch = bytes[*pos];
        if ch.is_ascii_whitespace()
            || ch == b'('
            || ch == b')'
            || ch == b';'
            || ch == b'"'
            || ch == b'\''
            || ch == b'`'
            || ch == b','
        {
            break;
        }
        *pos += 1;
    }
    if *pos == start {
        return Err(make_err(input, start, "expected atom"));
    }
    Ok(SExpr::Atom(input[start..*pos].to_string()))
}

fn skip_ws_and_comments(input: &str, pos: &mut usize) {
    let bytes = input.as_bytes();
    while *pos < bytes.len() {
        if bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        } else if bytes[*pos] == b';' {
            // Skip to end of line.
            while *pos < bytes.len() && bytes[*pos] != b'\n' {
                *pos += 1;
            }
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bare_atom() {
        assert_eq!(parse("hello").unwrap(), SExpr::Atom("hello".into()));
    }

    #[test]
    fn test_quoted_string() {
        assert_eq!(
            parse(r#""hello world""#).unwrap(),
            SExpr::Str("hello world".into())
        );
    }

    #[test]
    fn test_quoted_string_escapes() {
        assert_eq!(
            parse(r#""say \"hi\" \\""#).unwrap(),
            SExpr::Str(r#"say "hi" \"#.into())
        );
    }

    #[test]
    fn test_simple_list() {
        let expr = parse("(a b c)").unwrap();
        let items = expr.as_list().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].as_atom().unwrap(), "a");
        assert_eq!(items[1].as_atom().unwrap(), "b");
        assert_eq!(items[2].as_atom().unwrap(), "c");
    }

    #[test]
    fn test_nested_lists() {
        let expr = parse("(a (b c) (d (e)))").unwrap();
        let items = expr.as_list().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].as_atom().unwrap(), "a");
        let inner1 = items[1].as_list().unwrap();
        assert_eq!(inner1.len(), 2);
        let inner2 = items[2].as_list().unwrap();
        assert_eq!(inner2.len(), 2);
    }

    #[test]
    fn test_comments() {
        let input = "; this is a comment\n(a ; inline comment\n b)";
        let expr = parse(input).unwrap();
        let items = expr.as_list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_atom().unwrap(), "a");
        assert_eq!(items[1].as_atom().unwrap(), "b");
    }

    #[test]
    fn test_empty_list() {
        let expr = parse("()").unwrap();
        assert_eq!(expr.as_list().unwrap().len(), 0);
    }

    #[test]
    fn test_unmatched_open_paren() {
        let err = parse("(a b").unwrap_err();
        assert!(err.message.contains("unmatched '('"), "{}", err);
    }

    #[test]
    fn test_unmatched_close_paren() {
        let err = parse("a)").unwrap_err();
        assert!(
            err.message.contains("trailing input") || err.message.contains("unexpected"),
            "{}",
            err
        );
    }

    #[test]
    fn test_unterminated_string() {
        let err = parse(r#""hello"#).unwrap_err();
        assert!(err.message.contains("unterminated"), "{}", err);
    }

    #[test]
    fn test_empty_input() {
        let err = parse("").unwrap_err();
        assert!(err.message.contains("empty"), "{}", err);
    }

    #[test]
    fn test_comment_only_input() {
        let err = parse("; just a comment\n").unwrap_err();
        assert!(err.message.contains("empty"), "{}", err);
    }

    #[test]
    fn test_keyword_atoms() {
        let expr = parse("(:ready-port 5432)").unwrap();
        let items = expr.as_list().unwrap();
        assert_eq!(items[0].as_atom().unwrap(), ":ready-port");
        assert_eq!(items[1].as_atom().unwrap(), "5432");
    }

    #[test]
    fn test_display_round_trip() {
        let input = "(compose (service db (image \"postgres:16\")))";
        let expr = parse(input).unwrap();
        let printed = expr.to_string();
        let reparsed = parse(&printed).unwrap();
        assert_eq!(expr, reparsed);
    }

    #[test]
    fn test_full_compose_example() {
        let input = r#"
; A typical web application stack
(compose
  (network backend (subnet "10.88.1.0/24"))
  (volume pgdata)

  (service db
    (image "postgres:16")
    (network backend)
    (volume pgdata "/var/lib/postgresql/data")
    (env POSTGRES_PASSWORD "secret")
    (port 5432 5432)
    (memory "512m"))

  (service api
    (image "my-api:latest")
    (network backend)
    (depends-on (db :ready-port 5432))
    (port 8080 8080))

  (service web
    (image "my-web:latest")
    (depends-on (api :ready-port 8080))
    (port 80 3000)
    (command "/bin/sh" "-c" "nginx -g 'daemon off;'")))
"#;
        let expr = parse(input).unwrap();
        let items = expr.as_list().unwrap();
        assert_eq!(items[0].as_atom().unwrap(), "compose");
        // network, volume, 3 services = 5 items + "compose" = 6
        assert_eq!(items.len(), 6);
    }

    #[test]
    fn test_as_atom_on_list() {
        let expr = parse("(a)").unwrap();
        assert!(expr.as_atom().is_none());
    }

    #[test]
    fn test_as_list_on_atom() {
        let expr = parse("hello").unwrap();
        assert!(expr.as_list().is_none());
    }

    #[test]
    fn test_quoted_string_utf8() {
        // Multibyte UTF-8 characters must survive a round-trip through quoted strings.
        let input = r#""héllo wörld 🎉""#;
        let atom = parse(input).unwrap();
        assert_eq!(atom, SExpr::Str("héllo wörld 🎉".into()));
        assert_eq!(atom.as_atom().unwrap(), "héllo wörld 🎉");
    }

    #[test]
    fn test_reader_macro_quote() {
        let expr = parse("'foo").unwrap();
        assert_eq!(
            expr,
            SExpr::List(vec![SExpr::Atom("quote".into()), SExpr::Atom("foo".into())])
        );
    }

    #[test]
    fn test_reader_macro_quasiquote() {
        let expr = parse("`(a ,b ,@c)").unwrap();
        let items = expr.as_list().unwrap();
        assert_eq!(items[0].as_atom().unwrap(), "quasiquote");
        let inner = items[1].as_list().unwrap();
        assert_eq!(inner[0].as_atom().unwrap(), "a");
        // ,b → (unquote b)
        let unquote = inner[1].as_list().unwrap();
        assert_eq!(unquote[0].as_atom().unwrap(), "unquote");
        // ,@c → (unquote-splicing c)
        let splice = inner[2].as_list().unwrap();
        assert_eq!(splice[0].as_atom().unwrap(), "unquote-splicing");
    }

    #[test]
    fn test_parse_all_empty() {
        let exprs = parse_all("").unwrap();
        assert!(exprs.is_empty());
    }

    #[test]
    fn test_parse_all_multiple() {
        let exprs = parse_all("(define x 1) (define y 2) x").unwrap();
        assert_eq!(exprs.len(), 3);
        assert_eq!(exprs[0].as_list().unwrap()[0].as_atom().unwrap(), "define");
        assert_eq!(exprs[2].as_atom().unwrap(), "x");
    }

    #[test]
    fn test_dotted_pair() {
        let expr = parse(r#"("REDIS_HOST" . "redis")"#).unwrap();
        match expr {
            SExpr::DottedList(items, tail) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].as_atom().unwrap(), "REDIS_HOST");
                assert_eq!(tail.as_atom().unwrap(), "redis");
            }
            _ => panic!("expected DottedList"),
        }
    }

    #[test]
    fn test_dotted_pair_multi_head() {
        let expr = parse("(a b . c)").unwrap();
        match expr {
            SExpr::DottedList(items, tail) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].as_atom().unwrap(), "a");
                assert_eq!(items[1].as_atom().unwrap(), "b");
                assert_eq!(tail.as_atom().unwrap(), "c");
            }
            _ => panic!("expected DottedList"),
        }
    }

    #[test]
    fn test_dotted_pair_display_round_trip() {
        let input = r#"("KEY" . "val")"#;
        let expr = parse(input).unwrap();
        let printed = expr.to_string();
        let reparsed = parse(&printed).unwrap();
        assert_eq!(expr, reparsed);
    }

    #[test]
    fn test_dotted_number_is_not_separator() {
        // ".5" should parse as the atom ".5", not trigger dotted pair
        let expr = parse("(.5)").unwrap();
        assert!(matches!(expr, SExpr::List(_)));
        let items = expr.as_list().unwrap();
        assert_eq!(items[0].as_atom().unwrap(), ".5");
    }

    #[test]
    fn test_bare_word_stops_at_reader_macros() {
        // bare words should not swallow ', `, , characters
        let expr = parse("(a 'b)").unwrap();
        let items = expr.as_list().unwrap();
        assert_eq!(items[0].as_atom().unwrap(), "a");
        let quoted = items[1].as_list().unwrap();
        assert_eq!(quoted[0].as_atom().unwrap(), "quote");
        assert_eq!(quoted[1].as_atom().unwrap(), "b");
    }

    #[test]
    fn test_error_line_col() {
        // Error on line 2, column 3 (1-based).
        let input = "(a\n  )x";
        let err = parse(input).unwrap_err();
        // The trailing 'x' is unexpected; confirm the Display uses line:col format.
        let msg = err.to_string();
        assert!(msg.contains(':'), "expected line:col in '{}'", msg);
        assert_eq!(err.line, 2);
        assert_eq!(err.col, 4); // '  )' is 3 chars; 'x' is col 4
    }

    #[test]
    fn test_line_col_helper() {
        let input = "line1\nline2\nline3";
        assert_eq!(line_col(input, 0), (1, 1));
        assert_eq!(line_col(input, 5), (1, 6)); // '\n' itself
        assert_eq!(line_col(input, 6), (2, 1)); // first char of line2
        assert_eq!(line_col(input, 11), (2, 6));
        assert_eq!(line_col(input, 12), (3, 1));
    }
}
