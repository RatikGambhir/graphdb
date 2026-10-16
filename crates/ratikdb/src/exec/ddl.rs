//! Schema statements: CREATE / ALTER / DROP of types, indexes and stored queries.

use std::collections::BTreeSet;

use crate::error::{Error, Result};
use crate::graph::{Op, StoredQuery};
use crate::rql::ast::{IndexFlavor, Statement};
use crate::schema::{DefaultValue, Kind, PropDef, PropType, TypeDef};
use crate::value::Value;

use super::compile::Params;
use super::{QueryResult, Txn, run_write};

pub(crate) fn create_type(
    txn: &mut Txn<'_>,
    kind: Kind,
    name: &str,
    if_not_exists: bool,
    props: &[PropDef],
    from: &[String],
    to: &[String],
) -> Result<QueryResult> {
    if let Some(existing) = txn.g.schema.by_name(name) {
        if if_not_exists && existing.kind == kind {
            return Ok(QueryResult::status(format!(
                "{kind} type {name} already exists"
            )));
        }
        return Err(Error::schema(format!(
            "a type named '{name}' already exists"
        )));
    }
    if name.starts_with('_') || name.is_empty() {
        return Err(Error::schema(format!("invalid type name '{name}'")));
    }
    let mut seen = BTreeSet::new();
    for p in props {
        if !seen.insert(p.name.as_str()) {
            return Err(Error::schema(format!(
                "{name} declares property '{}' twice",
                p.name
            )));
        }
        p.validate(name)?;
    }
    for endpoint in from.iter().chain(to) {
        txn.g.schema.resolve(endpoint, Kind::Node)?;
    }
    let def = TypeDef {
        id: txn.g.schema.next_type_id(),
        kind,
        name: name.to_owned(),
        props: props.to_vec(),
        from: from.to_vec(),
        to: to.to_vec(),
    };
    txn.apply(Op::CreateType(def))?;
    Ok(QueryResult::status(format!(
        "created {} type {name}",
        kind.to_string().to_lowercase()
    )))
}

pub(crate) fn drop_type(
    txn: &mut Txn<'_>,
    kind: Kind,
    name: &str,
    if_exists: bool,
    cascade: bool,
) -> Result<QueryResult> {
    let def = match txn.g.schema.resolve(name, kind) {
        Ok(d) => d.clone(),
        Err(_) if if_exists && txn.g.schema.by_name(name).is_none() => {
            return Ok(QueryResult::status(format!("{name} does not exist")));
        }
        Err(e) => return Err(e),
    };
    let refs: Vec<String> = txn
        .g
        .schema
        .edges_referencing(name)
        .map(|e| e.name.clone())
        .collect();
    if kind == Kind::Node && !refs.is_empty() {
        return Err(Error::schema(format!(
            "cannot drop {name}: edge type(s) {} connect to it — drop those first",
            refs.join(", ")
        )));
    }
    let count = txn.g.member_count(def.id);
    if count > 0 {
        if !cascade {
            return Err(Error::schema(format!(
                "cannot drop {name}: it has {count} element(s). Use `DROP {kind} {name} CASCADE` to delete them too"
            )));
        }
        super::write::delete_all_of_type(txn, def.id)?;
    }
    txn.apply(Op::DropType(def.id))?;
    Ok(QueryResult::status(format!("dropped {name}")))
}

pub(crate) fn alter_add(
    txn: &mut Txn<'_>,
    kind: Kind,
    name: &str,
    prop: &PropDef,
    now: i64,
) -> Result<QueryResult> {
    let def = txn.g.schema.resolve(name, kind)?;
    prop.validate(name)?;
    let fill = match &prop.default {
        Some(DefaultValue::Value(v)) => prop.ty.coerce(v.clone(), &prop.name)?,
        Some(DefaultValue::Now) => Value::Int(now),
        None => Value::Null,
    };
    if prop.not_null && fill.is_null() && txn.g.member_count(def.id) > 0 {
        return Err(Error::schema(format!(
            "cannot add NOT NULL property {name}.{} without a DEFAULT: {name} already has data",
            prop.name
        )));
    }
    let ty = def.id;
    txn.apply(Op::AddProp {
        ty,
        def: prop.clone(),
        fill,
    })?;
    Ok(QueryResult::status(format!("added {name}.{}", prop.name)))
}

pub(crate) fn alter_drop(
    txn: &mut Txn<'_>,
    kind: Kind,
    name: &str,
    prop: &str,
) -> Result<QueryResult> {
    let def = txn.g.schema.resolve(name, kind)?;
    def.prop(prop)?;
    let ty = def.id;
    txn.apply(Op::DropProp {
        ty,
        name: prop.to_owned(),
    })?;
    Ok(QueryResult::status(format!("dropped {name}.{prop}")))
}

pub(crate) fn set_index(
    txn: &mut Txn<'_>,
    name: &str,
    prop: &str,
    flavor: Option<IndexFlavor>,
) -> Result<QueryResult> {
    let def = txn
        .g
        .schema
        .by_name(name)
        .ok_or_else(|| Error::schema(format!("unknown type '{name}'")))?;
    let (_, p) = def.prop(prop)?;
    if matches!(p.ty, PropType::Vector(_)) {
        return Err(Error::schema(format!(
            "{name}.{prop} is a VECTOR — vector properties are always indexed (HNSW)"
        )));
    }
    let (mut unique, mut indexed, mut searchable) = (p.unique, p.indexed, p.searchable);
    match flavor {
        Some(IndexFlavor::BTree) => indexed = true,
        Some(IndexFlavor::Unique) => unique = true,
        Some(IndexFlavor::Search) => searchable = true,
        None => (unique, indexed, searchable) = (false, false, false),
    }
    let ty = def.id;
    txn.apply(Op::SetFlags {
        ty,
        name: prop.to_owned(),
        unique,
        indexed,
        searchable,
    })?;
    Ok(QueryResult::status(match flavor {
        Some(_) => format!("indexed {name}.{prop}"),
        None => format!("dropped indexes on {name}.{prop}"),
    }))
}

fn dummy(ty: &PropType) -> Value {
    match ty {
        PropType::Bool => Value::Bool(false),
        PropType::Int | PropType::Timestamp => Value::Int(0),
        PropType::Float => Value::Float(0.0),
        PropType::String => Value::String(String::new()),
        PropType::Vector(n) => Value::Vector(vec![0.0; *n as usize]),
        PropType::Json => Value::Null,
        PropType::List(_) => Value::List(Vec::new()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_query(
    txn: &mut Txn<'_>,
    name: &str,
    or_replace: bool,
    params: &[(String, PropType)],
    body: &str,
    stmt: &Statement,
    now: i64,
    depth: u32,
) -> Result<QueryResult> {
    if txn.g.queries.contains_key(name) && !or_replace {
        return Err(Error::schema(format!(
            "query '{name}' already exists (use CREATE OR REPLACE QUERY)"
        )));
    }
    // Validate the body against the schema by running it with placeholder
    // arguments inside a savepoint that is always rolled back.
    let dummies: Params = params.iter().map(|(n, t)| (n.clone(), dummy(t))).collect();
    let sp = txn.savepoint();
    let check = run_write(txn, &dummies, stmt, now, depth + 1);
    txn.rollback_to(sp);
    check.map_err(|e| match e {
        Error::Query(m) if m.starts_with("missing value for parameter") => {
            Error::query(format!("{m} — declare it in the query signature"))
        }
        e => e,
    })?;
    txn.apply(Op::PutQuery(StoredQuery {
        name: name.to_owned(),
        params: params.to_vec(),
        body: body.to_owned(),
    }))?;
    Ok(QueryResult::status(format!("stored query {name}")))
}

pub(crate) fn drop_query(txn: &mut Txn<'_>, name: &str, if_exists: bool) -> Result<QueryResult> {
    if !txn.g.queries.contains_key(name) {
        if if_exists {
            return Ok(QueryResult::status(format!("query {name} does not exist")));
        }
        return Err(Error::NotFound(format!("query '{name}'")));
    }
    txn.apply(Op::DropQuery(name.to_owned()))?;
    Ok(QueryResult::status(format!("dropped query {name}")))
}
