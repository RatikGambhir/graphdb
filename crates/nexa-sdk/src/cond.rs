//! Conditions: `prop("age").gte(18) & prop("name").starts_with("A")`.

use std::ops::{BitAnd, BitOr, Not};

use serde_json::{Map, Value as Json};

/// Collects query parameters while a builder renders NQL, so values are
/// always sent as `$params` and never spliced into the query text.
#[derive(Debug, Default)]
pub(crate) struct Bag {
    pub params: Map<String, Json>,
}

impl Bag {
    pub(crate) fn push(&mut self, v: Json) -> String {
        let key = format!("p{}", self.params.len());
        self.params.insert(key.clone(), v);
        format!("${key}")
    }
}

/// Quotes an identifier so any type or property name is safe in NQL.
pub(crate) fn ident(name: &str) -> String {
    format!("`{}`", name.replace('`', ""))
}

/// Starts a condition on a property of the current node.
///
/// ```
/// use nexa::prelude::*;
/// let adults = prop("age").gte(18);
/// let a_names = prop("name").starts_with("A");
/// let both = adults & a_names;
/// ```
pub fn prop(name: impl Into<String>) -> Prop {
    Prop(name.into())
}

/// A property, waiting for a comparison. Created by [`prop`].
#[derive(Debug, Clone)]
#[must_use = "a property does nothing until compared, e.g. prop(\"age\").gt(18)"]
pub struct Prop(String);

macro_rules! cmp {
    ($($(#[$doc:meta])* $name:ident => $op:literal),* $(,)?) => {
        $($(#[$doc])* pub fn $name(self, value: impl Into<Json>) -> Cond {
            Cond(Kind::Cmp(self.0, $op, value.into()))
        })*
    };
}

macro_rules! call {
    ($($(#[$doc:meta])* $name:ident => $func:literal),* $(,)?) => {
        $($(#[$doc])* pub fn $name(self, value: impl Into<String>) -> Cond {
            Cond(Kind::Call($func, self.0, Json::String(value.into())))
        })*
    };
}

impl Prop {
    cmp! {
        /// `prop = value` (`IS NULL` when `value` is null).
        eq => "=",
        /// `prop != value`
        ne => "!=",
        /// `prop > value`
        gt => ">",
        /// `prop >= value`
        gte => ">=",
        /// `prop < value`
        lt => "<",
        /// `prop <= value`
        lte => "<=",
        /// `prop LIKE pattern` (`%` = anything, `_` = one character).
        like => "LIKE",
    }

    call! {
        /// The string contains `text`.
        contains => "contains",
        /// The string starts with `prefix`.
        starts_with => "starts_with",
        /// The string ends with `suffix`.
        ends_with => "ends_with",
        /// Full-text match on a `SEARCHABLE` property (any of the words).
        matches => "matches",
    }

    /// `lo <= prop <= hi`
    pub fn between(self, lo: impl Into<Json>, hi: impl Into<Json>) -> Cond {
        Cond(Kind::Between(self.0, lo.into(), hi.into()))
    }

    /// `prop IN [...]`
    pub fn one_of<V: Into<Json>>(self, values: impl IntoIterator<Item = V>) -> Cond {
        Cond(Kind::In(
            self.0,
            values.into_iter().map(Into::into).collect(),
        ))
    }

    /// `prop IS NULL`
    pub fn is_null(self) -> Cond {
        Cond(Kind::Null(self.0, false))
    }

    /// `prop IS NOT NULL`
    pub fn exists(self) -> Cond {
        Cond(Kind::Null(self.0, true))
    }
}

/// A boolean condition. Combine with `&`, `|` and `!`, or [`Cond::and`],
/// [`Cond::or`] and [`Cond::not`].
#[derive(Debug, Clone)]
#[must_use = "conditions do nothing until passed to .filter(...)"]
pub struct Cond(Kind);

#[derive(Debug, Clone)]
enum Kind {
    Cmp(String, &'static str, Json),
    Between(String, Json, Json),
    In(String, Vec<Json>),
    Null(String, bool),
    Call(&'static str, String, Json),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Not(Box<Cond>),
    Raw(String),
}

impl Cond {
    /// Both conditions.
    pub fn and(self, other: Self) -> Self {
        Self(Kind::And(Box::new(self), Box::new(other)))
    }

    /// Either condition.
    pub fn or(self, other: Self) -> Self {
        Self(Kind::Or(Box::new(self), Box::new(other)))
    }

    /// The opposite condition.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        Self(Kind::Not(Box::new(self)))
    }

    /// An NQL expression used verbatim. `it` refers to the current node,
    /// e.g. `Cond::raw("it.age % 2 = 0")`. Values should go through
    /// parameters, not string formatting.
    pub fn raw(nql: impl Into<String>) -> Self {
        Self(Kind::Raw(nql.into()))
    }

    pub(crate) fn render(&self, var: &str, bag: &mut Bag) -> String {
        let p = |name: &str| format!("{var}.{}", ident(name));
        match &self.0 {
            Kind::Cmp(name, "=", Json::Null) => format!("{} IS NULL", p(name)),
            Kind::Cmp(name, "!=", Json::Null) => format!("{} IS NOT NULL", p(name)),
            Kind::Cmp(name, op, v) => format!("{} {op} {}", p(name), bag.push(v.clone())),
            Kind::Between(name, lo, hi) => {
                format!(
                    "{} BETWEEN {} AND {}",
                    p(name),
                    bag.push(lo.clone()),
                    bag.push(hi.clone())
                )
            }
            Kind::In(name, values) => {
                format!("{} IN {}", p(name), bag.push(Json::Array(values.clone())))
            }
            Kind::Null(name, negated) => {
                format!("{} IS {}NULL", p(name), if *negated { "NOT " } else { "" })
            }
            Kind::Call(func, name, v) => format!("{func}({}, {})", p(name), bag.push(v.clone())),
            Kind::And(a, b) => format!("({} AND {})", a.render(var, bag), b.render(var, bag)),
            Kind::Or(a, b) => format!("({} OR {})", a.render(var, bag), b.render(var, bag)),
            Kind::Not(a) => format!("NOT ({})", a.render(var, bag)),
            Kind::Raw(nql) => format!("({})", replace_it(nql, var)),
        }
    }
}

/// Replaces the word `it` (outside of string literals) with `var`.
fn replace_it(nql: &str, var: &str) -> String {
    let mut out = String::with_capacity(nql.len());
    let mut quote: Option<char> = None;
    let chars: Vec<char> = nql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
        }
        let boundary_before = i == 0
            || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '.');
        let boundary_after = chars
            .get(i + 2)
            .is_none_or(|n| !(n.is_alphanumeric() || *n == '_'));
        if c == 'i' && chars.get(i + 1) == Some(&'t') && boundary_before && boundary_after {
            out.push_str(var);
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

impl BitAnd for Cond {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        self.and(rhs)
    }
}

impl BitOr for Cond {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.or(rhs)
    }
}

impl Not for Cond {
    type Output = Self;
    fn not(self) -> Self {
        Self(Kind::Not(Box::new(self)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_parameters() {
        let mut bag = Bag::default();
        let c =
            (prop("age").gte(18) & !prop("name").starts_with("A")) | prop("nick").eq(Json::Null);
        assert_eq!(
            c.render("n0", &mut bag),
            "((n0.`age` >= $p0 AND NOT (starts_with(n0.`name`, $p1))) OR n0.`nick` IS NULL)"
        );
        assert_eq!(bag.params.len(), 2);
    }

    #[test]
    fn raw_conditions_bind_it() {
        assert_eq!(
            replace_it("it.age % 2 = 0 AND 'it' = x.it", "n3"),
            "n3.age % 2 = 0 AND 'it' = x.it"
        );
    }
}
