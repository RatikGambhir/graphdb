//! NQL support that lives in the app: statement splitting and classification,
//! a line tokenizer for the editor and the splash preview, the example
//! queries, and the handful of queries the workspace generates itself.
//!
//! The database owns parsing and execution. Nothing here needs to be a full
//! parser; it only has to decide what *kind* of request a script is and paint
//! it legibly.

use std::ops::Range;

/// What a statement does, judged from its leading keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementKind {
    /// `SELECT`, `SHOW`, `DESCRIBE`, `EXPLAIN`, `CALL`.
    Read,
    /// `INSERT`, `UPDATE`, `DELETE`.
    Write,
    /// `CREATE`, `ALTER`, `DROP`.
    Schema,
}

impl StatementKind {
    /// `true` for anything that can change the database.
    #[must_use]
    pub fn is_write(self) -> bool {
        !matches!(self, Self::Read)
    }
}

/// One statement of a script, with the leading keyword resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub source: String,
    pub keyword: String,
    pub kind: StatementKind,
}

/// The analysed script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    pub statements: Vec<Statement>,
}

impl Analysis {
    /// Splits and classifies a script.
    #[must_use]
    pub fn of(source: &str) -> Self {
        let statements = split_statements(source)
            .into_iter()
            .map(|source| {
                let keyword = first_keyword(&source).unwrap_or_default();
                let kind = classify_keyword(&keyword);
                Statement {
                    source,
                    keyword,
                    kind,
                }
            })
            .collect();
        Self { statements }
    }

    /// `true` when the script has no statements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// `true` when any statement writes.
    #[must_use]
    pub fn has_write(&self) -> bool {
        self.statements.iter().any(|s| s.kind.is_write())
    }

    /// A short description such as `SELECT` or `INSERT, INSERT, SELECT`.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.statements.len() == 1 {
            return self.statements[0].keyword.clone();
        }
        let mut words: Vec<&str> = self.statements.iter().map(|s| s.keyword.as_str()).collect();
        if words.len() > 4 {
            words.truncate(3);
            return format!(
                "{} … ({} statements)",
                words.join(", "),
                self.statements.len()
            );
        }
        words.join(", ")
    }
}

fn classify_keyword(keyword: &str) -> StatementKind {
    match keyword {
        "INSERT" | "UPDATE" | "DELETE" => StatementKind::Write,
        "CREATE" | "ALTER" | "DROP" => StatementKind::Schema,
        _ => StatementKind::Read,
    }
}

/// The first word of a statement in upper case, skipping comments.
#[must_use]
pub fn first_keyword(statement: &str) -> Option<String> {
    let stripped = strip_comments(statement);
    let word: String = stripped
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
        .collect();
    (!word.is_empty()).then(|| word.to_ascii_uppercase())
}

/// Removes `--`, `//` and `/* */` comments.
#[must_use]
pub fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            out.push(b as char);
            if b == q {
                // '' escapes a quote inside a string.
                if i + 1 < bytes.len() && bytes[i + 1] == q {
                    out.push(q as char);
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' | b'`' => {
                quote = Some(b);
                out.push(b as char);
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push(' ');
            }
            _ => {
                // Copy whole UTF-8 sequences untouched.
                let len = utf8_len(b);
                out.push_str(&source[i..(i + len).min(bytes.len())]);
                i += len;
            }
        }
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Splits on `;` outside strings and comments, dropping empty statements.
#[must_use]
pub fn split_statements(source: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            current.push(b as char);
            if b == q {
                if bytes.get(i + 1) == Some(&q) {
                    current.push(q as char);
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' | b'`' => {
                quote = Some(b);
                current.push(b as char);
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    current.push(bytes[i] as char);
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    current.push(bytes[i] as char);
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                current.push_str(&source[start..i]);
            }
            b';' => {
                push_statement(&mut statements, &mut current);
                i += 1;
            }
            _ => {
                let len = utf8_len(b);
                current.push_str(&source[i..(i + len).min(bytes.len())]);
                i += len;
            }
        }
    }
    push_statement(&mut statements, &mut current);
    statements
}

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let text = std::mem::take(current);
    let trimmed = text.trim();
    if !trimmed.is_empty() && !strip_comments(trimmed).trim().is_empty() {
        statements.push(trimmed.to_owned());
    }
}

// ---- highlighting ----------------------------------------------------------

/// Token classes painted by the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    /// A node or edge type, such as `User` in `(u:User)`.
    TypeName,
    String,
    Number,
    /// `$name` parameters.
    Param,
    Comment,
    Operator,
    Punctuation,
    Whitespace,
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "DISTINCT",
    "FROM",
    "WHERE",
    "GROUP",
    "BY",
    "HAVING",
    "ORDER",
    "ASC",
    "DESC",
    "LIMIT",
    "OFFSET",
    "INSERT",
    "INTO",
    "VALUES",
    "RETURNING",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "ALTER",
    "DROP",
    "NODE",
    "EDGE",
    "INDEX",
    "UNIQUE",
    "SEARCH",
    "QUERY",
    "CALL",
    "SHOW",
    "DESCRIBE",
    "EXPLAIN",
    "IF",
    "NOT",
    "EXISTS",
    "NULL",
    "TRUE",
    "FALSE",
    "AND",
    "OR",
    "IN",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "IS",
    "AS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "ADD",
    "CASCADE",
    "TO",
    "PRIMARY",
    "KEY",
    "DEFAULT",
    "SEARCHABLE",
    "METRIC",
    "COSINE",
    "EUCLIDEAN",
    "DOT",
    "REPLACE",
    "SCHEMA",
    "NODES",
    "EDGES",
    "INDEXES",
    "QUERIES",
    "STATS",
    "ON",
    "STRING",
    "INT",
    "FLOAT",
    "BOOL",
    "TEXT",
    "VARCHAR",
    "TIMESTAMP",
    "VECTOR",
    "JSON",
    "BEGIN",
    "COMMIT",
];

/// Keywords after which a bare identifier names a type.
const TYPE_CONTEXT: &[&str] = &["INTO", "NODE", "EDGE", "UPDATE", "DESCRIBE", "FROM", "TO"];

/// Tokenises one line (the editor paints line by line). Ranges are byte
/// offsets into `line`, contiguous and covering the whole line.
#[must_use]
pub fn highlight_line(line: &str) -> Vec<(Range<usize>, TokenKind)> {
    let bytes = line.as_bytes();
    let mut tokens: Vec<(Range<usize>, TokenKind)> = Vec::new();
    let mut i = 0;
    let mut previous_word: Option<String> = None;
    let mut previous_char: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        let start = i;
        let kind = if b.is_ascii_whitespace() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            TokenKind::Whitespace
        } else if (b == b'-' && bytes.get(i + 1) == Some(&b'-'))
            || (b == b'/' && bytes.get(i + 1) == Some(&b'/'))
        {
            i = bytes.len();
            TokenKind::Comment
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            TokenKind::Comment
        } else if b == b'\'' || b == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b {
                    if bytes.get(i + 1) == Some(&b) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            TokenKind::String
        } else if b == b'`' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'`' {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            TokenKind::Identifier
        } else if b == b'$' {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            TokenKind::Param
        } else if b.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                i += 1;
            }
            TokenKind::Number
        } else if b.is_ascii_alphabetic() || b == b'_' || b >= 0x80 {
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
            {
                i += 1;
            }
            let word = &line[start..i];
            let upper = word.to_ascii_uppercase();
            if KEYWORDS.contains(&upper.as_str()) {
                TokenKind::Keyword
            } else if previous_char == Some(b':')
                || previous_word
                    .as_deref()
                    .is_some_and(|w| TYPE_CONTEXT.contains(&w))
            {
                TokenKind::TypeName
            } else {
                TokenKind::Identifier
            }
        } else if matches!(
            b,
            b'=' | b'<' | b'>' | b'!' | b'+' | b'-' | b'*' | b'/' | b'%' | b'|'
        ) {
            while i < bytes.len()
                && matches!(
                    bytes[i],
                    b'=' | b'<' | b'>' | b'!' | b'+' | b'-' | b'*' | b'/' | b'%' | b'|'
                )
            {
                i += 1;
            }
            TokenKind::Operator
        } else {
            i += utf8_len(b);
            TokenKind::Punctuation
        };
        let end = i.min(bytes.len());
        match kind {
            TokenKind::Whitespace | TokenKind::Comment => {}
            TokenKind::Keyword => {
                previous_word = Some(line[start..end].to_ascii_uppercase());
                previous_char = None;
            }
            TokenKind::Punctuation | TokenKind::Operator => {
                previous_char = Some(bytes[start]);
                previous_word = None;
            }
            _ => {
                previous_word = None;
                previous_char = None;
            }
        }
        tokens.push((start..end, kind));
    }
    tokens
}

// ---- generated queries ------------------------------------------------------

/// The default Graph workspace snapshot: every node and every relationship,
/// bounded so the canvas stays readable.
pub const GRAPH_SNAPSHOT: &str = "-- Graph snapshot: nodes first, then the relationships among them\nSELECT n FROM (n) LIMIT 400;\nSELECT a, e, b FROM (a)-[e]->(b) LIMIT 2000;";

/// The script loaded by double-clicking a node: the node and everything
/// attached to it.
#[must_use]
pub fn neighbourhood_query(id: u64) -> String {
    format!(
        "-- Neighbourhood of node {id}\nSELECT n FROM (n) WHERE id(n) = {id};\nSELECT a, e, b FROM (a)-[e]->(b)\nWHERE id(a) = {id} OR id(b) = {id}\nLIMIT 200;"
    )
}

/// Reads one node with its degree.
#[must_use]
pub fn node_query(id: u64) -> String {
    format!("SELECT n, degree(n) AS degree FROM (n) WHERE id(n) = {id}")
}

/// Reads the edges incident to a node, with both endpoints.
#[must_use]
pub fn incident_edges_query(id: u64, limit: usize) -> String {
    format!("SELECT a, e, b FROM (a)-[e]->(b) WHERE id(a) = {id} OR id(b) = {id} LIMIT {limit}")
}

/// Reads one edge with its endpoints.
#[must_use]
pub fn edge_query(id: u64) -> String {
    format!("SELECT a, e, b FROM (a)-[e]->(b) WHERE id(e) = {id}")
}

/// Table browse of a node type.
#[must_use]
pub fn browse_node_type(name: &str) -> String {
    format!("SELECT * FROM {} LIMIT 50", ident(name))
}

/// Graph of a node type and its relationships.
#[must_use]
pub fn graph_node_type(name: &str) -> String {
    let name = ident(name);
    format!(
        "SELECT n FROM (n:{name}) LIMIT 150;\nSELECT a, e, b FROM (a:{name})-[e]->(b) LIMIT 600;"
    )
}

/// Table browse of an edge type (also drawn, since the rows carry elements).
#[must_use]
pub fn browse_edge_type(name: &str) -> String {
    format!("SELECT a, e, b FROM (a)-[e:{}]->(b) LIMIT 50", ident(name))
}

/// Graph of an edge type.
#[must_use]
pub fn graph_edge_type(name: &str) -> String {
    format!("SELECT a, e, b FROM (a)-[e:{}]->(b) LIMIT 500", ident(name))
}

/// A `CALL` for a stored query with its parameters as `$name` placeholders.
#[must_use]
pub fn call_query(name: &str, params: &[String]) -> String {
    let args: Vec<String> = params.iter().map(|p| format!("${p}")).collect();
    format!("CALL {}({})", ident(name), args.join(", "))
}

/// Quotes an identifier with backticks when it is not a plain word.
#[must_use]
pub fn ident(name: &str) -> String {
    let plain = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !KEYWORDS.contains(&name.to_ascii_uppercase().as_str());
    if plain {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// A starting point offered in the sidebar.
#[derive(Clone, Copy, Debug)]
pub struct Example {
    pub title: &'static str,
    pub query: &'static str,
}

/// The sidebar examples, in display order.
pub const EXAMPLES: &[Example] = &[
    Example {
        title: "Insert nodes",
        query: "INSERT INTO User (name, age)\nVALUES ('Alice', 30), ('Bob', 28)\nRETURNING _id, name;",
    },
    Example {
        title: "Insert edges",
        query: "INSERT (a)-[:FOLLOWS {since: 2026}]->(b)\nFROM User a, User b\nWHERE a.name = 'Alice' AND b.name = 'Bob'\nRETURNING a.name, b.name;",
    },
    Example {
        title: "Create connected nodes",
        query: "INSERT (:User {name: 'Alice'})-[:FOLLOWS]->(:User {name: 'Bob'});",
    },
    Example {
        title: "Update a property",
        query: "UPDATE User SET age = age + 1\nWHERE name = 'Alice'\nRETURNING name, age;",
    },
    Example {
        title: "Delete nodes",
        query: "DELETE FROM User WHERE name = 'Bob';",
    },
    Example {
        title: "Declare a schema",
        query: "CREATE NODE IF NOT EXISTS User (\n  name STRING NOT NULL UNIQUE,\n  age INT INDEX,\n  bio STRING SEARCHABLE\n);\nCREATE EDGE IF NOT EXISTS FOLLOWS FROM User TO User (since INT);",
    },
    Example {
        title: "Whole graph",
        query: GRAPH_SNAPSHOT,
    },
    Example {
        title: "Browse nodes",
        query: "SELECT n FROM (n) LIMIT 50",
    },
    Example {
        title: "Filter by property",
        query: "SELECT _id, name\nFROM User\nWHERE name LIKE 'A%'\nORDER BY name\nLIMIT 25",
    },
    Example {
        title: "Follow a relationship",
        query: "SELECT f.name\nFROM (u:User {name: 'Alice'})-[:FOLLOWS]->(f)",
    },
    Example {
        title: "Read edge properties",
        query: "SELECT a.name, e.since, b.name\nFROM (a:User)-[e:FOLLOWS]->(b)\nLIMIT 50",
    },
    Example {
        title: "Two hops out",
        query: "SELECT DISTINCT f.name\nFROM (u:User {name: 'Alice'})-[:FOLLOWS*1..2]->(f)",
    },
    Example {
        title: "Count by label",
        query: "SELECT label(n) AS label, count(*) AS total\nFROM (n)\nGROUP BY label(n)\nORDER BY total DESC",
    },
    Example {
        title: "Instance stats",
        query: "SHOW STATS",
    },
    Example {
        title: "Explain a plan",
        query: "EXPLAIN SELECT * FROM User WHERE age > 30",
    },
];

/// A token of the splash-screen preview.
#[derive(Clone, Copy, Debug)]
pub struct PreviewToken {
    pub text: &'static str,
    pub kind: PreviewKind,
}

/// Token classes of the splash preview, which has a richer palette than the
/// editor so the animation reads as a poster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Keyword,
    Entity,
    Label,
    Property,
    Direction,
    Operator,
    Literal,
    Number,
    Punctuation,
}

use PreviewKind as P;

/// A valid NQL graph query, tokenised for the animated editor preview.
pub const SPLASH_PREVIEW: &[&[PreviewToken]] = &[
    &[
        PreviewToken {
            text: "SELECT",
            kind: P::Keyword,
        },
        PreviewToken {
            text: " u",
            kind: P::Entity,
        },
        PreviewToken {
            text: ", ",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "e",
            kind: P::Entity,
        },
        PreviewToken {
            text: ", ",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "f",
            kind: P::Entity,
        },
    ],
    &[
        PreviewToken {
            text: "FROM",
            kind: P::Keyword,
        },
        PreviewToken {
            text: " (",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "u",
            kind: P::Entity,
        },
        PreviewToken {
            text: ":",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "User",
            kind: P::Label,
        },
        PreviewToken {
            text: ")",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "-[",
            kind: P::Direction,
        },
        PreviewToken {
            text: "e",
            kind: P::Entity,
        },
        PreviewToken {
            text: ":",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "FOLLOWS",
            kind: P::Label,
        },
        PreviewToken {
            text: "]->",
            kind: P::Direction,
        },
        PreviewToken {
            text: "(",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "f",
            kind: P::Entity,
        },
        PreviewToken {
            text: ")",
            kind: P::Punctuation,
        },
    ],
    &[
        PreviewToken {
            text: "WHERE",
            kind: P::Keyword,
        },
        PreviewToken {
            text: " u.active ",
            kind: P::Property,
        },
        PreviewToken {
            text: "=",
            kind: P::Operator,
        },
        PreviewToken {
            text: " ",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "true",
            kind: P::Literal,
        },
    ],
    &[
        PreviewToken {
            text: "  AND",
            kind: P::Keyword,
        },
        PreviewToken {
            text: " e.since ",
            kind: P::Property,
        },
        PreviewToken {
            text: ">=",
            kind: P::Operator,
        },
        PreviewToken {
            text: " ",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "2024",
            kind: P::Number,
        },
    ],
    &[
        PreviewToken {
            text: "ORDER BY",
            kind: P::Keyword,
        },
        PreviewToken {
            text: " f.name ",
            kind: P::Property,
        },
        PreviewToken {
            text: "ASC",
            kind: P::Keyword,
        },
    ],
    &[
        PreviewToken {
            text: "LIMIT",
            kind: P::Keyword,
        },
        PreviewToken {
            text: " ",
            kind: P::Punctuation,
        },
        PreviewToken {
            text: "150",
            kind: P::Number,
        },
        PreviewToken {
            text: ";",
            kind: P::Punctuation,
        },
    ],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_semicolons_outside_strings_and_comments() {
        let script = "SELECT 'a;b' AS x; -- trailing; comment\nINSERT INTO T {k: 1};\n\n";
        let statements = split_statements(script);
        assert_eq!(statements.len(), 2);
        assert!(statements[0].starts_with("SELECT 'a;b'"));
        assert_eq!(first_keyword(&statements[1]).as_deref(), Some("INSERT"));
    }

    #[test]
    fn classifies_leading_keywords() {
        let analysis =
            Analysis::of("-- note\nselect 1;\nINSERT INTO T {a: 1}; CREATE NODE X (a INT)");
        let kinds: Vec<_> = analysis.statements.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                StatementKind::Read,
                StatementKind::Write,
                StatementKind::Schema
            ]
        );
        assert!(analysis.has_write());
        assert_eq!(analysis.summary(), "SELECT, INSERT, CREATE");
    }

    #[test]
    fn highlights_types_after_colons_and_keywords() {
        let tokens = highlight_line("SELECT n FROM (n:User) WHERE n.age > $min -- hi");
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|(_, k)| *k != TokenKind::Whitespace)
            .map(|(_, k)| *k)
            .collect();
        assert_eq!(kinds[0], TokenKind::Keyword);
        assert!(kinds.contains(&TokenKind::TypeName));
        assert!(kinds.contains(&TokenKind::Param));
        assert_eq!(kinds.last(), Some(&TokenKind::Comment));
        let covered: usize = tokens.iter().map(|(r, _)| r.len()).sum();
        assert_eq!(
            covered,
            "SELECT n FROM (n:User) WHERE n.age > $min -- hi".len()
        );
    }

    #[test]
    fn quotes_identifiers_that_need_it() {
        assert_eq!(ident("User"), "User");
        assert_eq!(ident("order"), "`order`");
        assert_eq!(ident("my type"), "`my type`");
    }
}
