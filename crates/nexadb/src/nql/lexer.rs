//! Tokenizer for NQL.

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    /// Bare identifier or keyword (keywords are matched case-insensitively by
    /// the parser, so `select` and `SELECT` are the same).
    Ident(String),
    /// Back-quoted identifier: never a keyword.
    Quoted(String),
    Str(String),
    Int(i64),
    Float(f64),
    Param(String),
    Sym(&'static str),
    Eof,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
    /// Byte offset of the first character.
    pub pos: usize,
}

const SYMBOLS: &[&str] = &[
    "->", "<=", ">=", "<>", "!=", "==", "||", "..", "(", ")", "[", "]", "{", "}", ",", ".", ":",
    ";", "*", "+", "-", "/", "%", "=", "<", ">", "|",
];

pub(crate) fn tokenize(src: &str) -> Result<Vec<Token>> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut line, mut line_start) = (0usize, 1u32, 0usize);
    // (byte position, chars before it on the current line): keeps column
    // tracking linear even for a multi-megabyte single-line script.
    let mut cc = (0usize, 0usize);
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            line += 1;
            i += 1;
            line_start = i;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Comments: `-- ...` and `// ...` to end of line, `/* ... */` blocks.
        if src[i..].starts_with("--") || src[i..].starts_with("//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if src[i..].starts_with("/*") {
            let Some(end) = src[i + 2..].find("*/") else {
                return Err(Error::Syntax {
                    line,
                    col: column(src, i, line_start, &mut cc),
                    msg: "unterminated /* comment".into(),
                });
            };
            for b in &bytes[i..i + 2 + end + 2] {
                if *b == b'\n' {
                    line += 1;
                }
            }
            let comment_start = i;
            i += 2 + end + 2;
            if let Some(nl) = src[comment_start..i].rfind('\n') {
                line_start = comment_start + nl + 1;
            }
            continue;
        }
        let start = i;
        let (tline, tcol) = (line, column(src, i, line_start, &mut cc));
        let tok = if c == b'\'' || c == b'"' {
            let (s, next) = lex_string(src, i, c).map_err(|msg| Error::Syntax {
                line,
                col: tcol,
                msg,
            })?;
            for ch in src[i..next].chars() {
                if ch == '\n' {
                    line += 1;
                }
            }
            // Only scan the literal itself: searching the whole prefix made
            // lexing quadratic for long single-line scripts.
            if let Some(nl) = src[i..next].rfind('\n') {
                line_start = i + nl + 1;
            }
            i = next;
            Tok::Str(s)
        } else if c == b'`' {
            let Some(end) = src[i + 1..].find('`') else {
                return Err(Error::Syntax {
                    line,
                    col: tcol,
                    msg: "unterminated `identifier`".into(),
                });
            };
            let name = src[i + 1..i + 1 + end].to_owned();
            i += end + 2;
            Tok::Quoted(name)
        } else if c == b'$' {
            i += 1;
            let s = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if s == i {
                return Err(Error::Syntax {
                    line,
                    col: tcol,
                    msg: "expected a parameter name after '$'".into(),
                });
            }
            Tok::Param(src[s..i].to_owned())
        } else if c.is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let mut float = false;
            // `1.5` is a float but `1..3` is a range.
            if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                float = true;
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                let mut j = i + 1;
                if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    float = true;
                    i = j;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text = &src[start..i];
            let bad = || Error::Syntax {
                line,
                col: tcol,
                msg: format!("invalid number '{text}'"),
            };
            if float {
                Tok::Float(text.parse().map_err(|_| bad())?)
            } else {
                Tok::Int(text.parse().map_err(|_| bad())?)
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            Tok::Ident(src[start..i].to_owned())
        } else if let Some(sym) = SYMBOLS.iter().find(|s| src[i..].starts_with(**s)) {
            i += sym.len();
            Tok::Sym(sym)
        } else {
            let ch = src[i..].chars().next().unwrap_or('?');
            return Err(Error::Syntax {
                line,
                col: tcol,
                msg: format!("unexpected character '{ch}'"),
            });
        };
        out.push(Token {
            tok,
            line: tline,
            col: tcol,
            pos: start,
        });
    }
    out.push(Token {
        tok: Tok::Eof,
        line,
        col: column(src, src.len(), line_start.min(src.len()), &mut cc),
        pos: src.len(),
    });
    Ok(out)
}

/// 1-based column of byte offset `i`, counted in characters.
fn column(src: &str, i: usize, line_start: usize, cache: &mut (usize, usize)) -> u32 {
    if cache.0 < line_start || cache.0 > i {
        *cache = (line_start, 0);
    }
    cache.1 += src[cache.0..i].chars().count();
    cache.0 = i;
    u32::try_from(cache.1 + 1).unwrap_or(u32::MAX)
}

fn lex_string(src: &str, start: usize, quote: u8) -> std::result::Result<(String, usize), String> {
    let mut out = String::new();
    let mut chars = src[start + 1..].char_indices().peekable();
    while let Some((off, ch)) = chars.next() {
        let at = start + 1 + off;
        match ch {
            c if c == quote as char => {
                // SQL-style doubled quote: 'it''s'
                if chars.peek().map(|(_, c)| *c) == Some(quote as char) {
                    chars.next();
                    out.push(c);
                } else {
                    return Ok((out, at + 1));
                }
            }
            '\\' => match chars.next().map(|(_, c)| c) {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some(c) => out.push(c),
                None => break,
            },
            c => out.push(c),
        }
    }
    Err("unterminated string literal".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        tokenize(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn lexes_patterns_ranges_and_comments() {
        assert_eq!(
            toks("(a)-[:R*1..3]->(b) -- trailing"),
            vec![
                Tok::Sym("("),
                Tok::Ident("a".into()),
                Tok::Sym(")"),
                Tok::Sym("-"),
                Tok::Sym("["),
                Tok::Sym(":"),
                Tok::Ident("R".into()),
                Tok::Sym("*"),
                Tok::Int(1),
                Tok::Sym(".."),
                Tok::Int(3),
                Tok::Sym("]"),
                Tok::Sym("->"),
                Tok::Sym("("),
                Tok::Ident("b".into()),
                Tok::Sym(")"),
                Tok::Eof,
            ]
        );
        assert_eq!(
            toks("'it''s' 1.5e2 $p"),
            vec![
                Tok::Str("it's".into()),
                Tok::Float(150.0),
                Tok::Param("p".into()),
                Tok::Eof
            ]
        );
    }
}

#[cfg(test)]
mod scale {
    use super::*;

    #[test]
    fn long_single_line_scripts_lex_in_linear_time() {
        let row = "('name', 42, [0.1, 0.2, 0.3]),";
        let src = row.repeat(200_000);
        let started = std::time::Instant::now();
        let toks = tokenize(&src).unwrap();
        assert_eq!(toks.len(), 200_000 * 14 + 1);
        // Quadratic behaviour took minutes here; linear takes well under a second.
        assert!(
            started.elapsed().as_secs() < 5,
            "lexing took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn positions_stay_correct_across_multiline_tokens() {
        let toks = tokenize("'a\nb' /* x\ny */ foo").unwrap();
        let foo = toks
            .iter()
            .find(|t| t.tok == Tok::Ident("foo".into()))
            .unwrap();
        assert_eq!((foo.line, foo.col), (3, 6));
    }
}
