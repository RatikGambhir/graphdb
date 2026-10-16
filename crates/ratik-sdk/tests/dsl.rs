//! The DSL against an embedded database (`cargo test -p ratik --features embedded`).
#![cfg(feature = "embedded")]

use ratik::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct User {
    name: String,
    age: Option<i64>,
}

fn setup() -> Ratik {
    let db = Ratik::memory();
    db.define_all([
        node("User")
            .string("name").required().unique()
            .int("age").indexed()
            .text("bio")
            .vector("taste", 2)
            .timestamp("joined").default_now(),
        node("Post").string("title").required().int("likes").default(0),
        edge("FOLLOWS").from("User").to("User").int("since"),
        edge("WROTE").from("User").to("Post"),
    ])
    .unwrap();
    db
}

#[test]
fn reads_like_a_sentence() -> ratik::Result<()> {
    let db = setup();
    let ada = db.create("User").set("name", "Ada").set("age", 36).set("bio", "poetical science").set("taste", [1.0, 0.0]).run()?;
    let alan = db.create("User").set("name", "Alan").set("age", 41).set("bio", "computable numbers").set("taste", [0.0, 1.0]).run()?;
    let grace = db.create("User").from(User { name: "Grace".into(), age: Some(45) }).run()?;
    assert_eq!(ada.label, "User");
    assert!(ada.get::<i64>("joined")? > 0, "default now() filled in");

    db.link(&ada, "FOLLOWS", &alan).set("since", 2020).run()?;
    db.link(&alan, "FOLLOWS", &grace).set("since", 2023).run()?;

    // Find & traverse.
    let follows = db.find("User").with("name", "Ada").outgoing("FOLLOWS").all()?;
    assert_eq!(follows.len(), 1);
    assert_eq!(follows[0].get::<String>("name")?, "Alan");

    let reach: Vec<String> =
        db.find("User").with("name", "Ada").outgoing("FOLLOWS").hops(1..=3).order_by("name").pluck("name")?;
    assert_eq!(reach, ["Alan", "Grace"]);

    let recent = db.find("User").outgoing("FOLLOWS").where_edge(prop("since").gte(2021)).pluck::<String>("name")?;
    assert_eq!(recent, ["Grace"]);

    let followers_of_alan = db.find("User").with("name", "Alan").incoming("FOLLOWS").one()?;
    assert_eq!(followers_of_alan.id, ada.id);

    // Conditions compose with & | !
    let n = db.find("User").filter(prop("age").gt(40) & !prop("name").starts_with("G")).count()?;
    assert_eq!(n, 1);
    let typed: Vec<User> = db.find("User").filter(prop("age").between(30, 42)).order_by_desc("age").all_as()?;
    assert_eq!(typed, [User { name: "Alan".into(), age: Some(41) }, User { name: "Ada".into(), age: Some(36) }]);

    // Vectors and full-text.
    let closest = db.find("User").nearest("taste", [0.9, 0.1]).limit(1).one()?;
    assert_eq!(closest.id, ada.id);
    let hits: Vec<String> = db.find("User").search("bio", "numbers").pluck("name")?;
    assert_eq!(hits, ["Alan"]);

    // Edges.
    let edges = db.find("User").with("name", "Ada").outgoing("FOLLOWS").edges()?;
    assert_eq!((edges[0].from, edges[0].to, edges[0].get::<i64>("since")?), (ada.id, alan.id, 2020));
    Ok(())
}

#[test]
fn updates_deletes_and_batches() -> ratik::Result<()> {
    let db = setup();
    let users = db.create_many(
        "User",
        [User { name: "a".into(), age: Some(1) }, User { name: "b".into(), age: Some(2) }, User { name: "c".into(), age: None }],
    )?;
    assert_eq!(users.len(), 3);

    let updated = db.find("User").filter(prop("age").exists()).update().increment("age", 10).run()?;
    assert_eq!(updated.len(), 2);
    assert_eq!(db.find("User").with("name", "b").one()?.get::<i64>("age")?, 12);

    // The same node reached twice is updated once.
    db.link(&users[0], "FOLLOWS", &users[2]).run()?;
    db.link(&users[1], "FOLLOWS", &users[2]).run()?;
    db.find("User").outgoing("FOLLOWS").update().set("age", 100).run()?;
    db.find("User").outgoing("FOLLOWS").update().increment("age", 1).run()?;
    assert_eq!(db.find("User").with("name", "c").one()?.get::<i64>("age")?, 101);

    assert_eq!(db.unlink(&users[0], "FOLLOWS", &users[2])?, 1);
    assert_eq!(db.find("User").filter(prop("age").lt(50)).delete()?, 2);
    assert_eq!(db.find("User").count()?, 1);
    assert!(db.node(users[2].id)?.is_some());
    assert!(db.node(users[0].id)?.is_none());
    Ok(())
}

#[test]
fn rql_and_stored_queries() -> ratik::Result<()> {
    let db = setup();
    db.execute("INSERT (:User {name: 'x', age: 5})-[:WROTE]->(:Post {title: 'hello'})")?;
    let rows = db
        .query("SELECT u.name AS author, p.title, p.likes FROM (u:User)-[:WROTE]->(p) WHERE u.age < $max")
        .bind("max", 10)
        .fetch()?;
    let row = rows.first().expect("one row");
    assert_eq!(row.get::<String>("author")?, "x");
    assert_eq!(row.get::<i64>("p.likes")?, 0);

    db.execute("CREATE QUERY by_name($name STRING) AS SELECT u FROM User u WHERE u.name = $name")?;
    let found: Option<User> = db.call("by_name").bind("name", "x").first_as()?;
    assert_eq!(found, Some(User { name: "x".into(), age: Some(5) }));
    assert!(db.schema()?.contains("CREATE EDGE WROTE FROM User TO Post"));
    Ok(())
}

#[test]
fn schema_errors_are_helpful() {
    let db = setup();
    let err = db.create("Usr").set("name", "x").run().unwrap_err();
    assert_eq!(err.code(), Some("schema_error"));
    assert!(err.to_string().contains("did you mean 'User'"), "{err}");
    let err = db.find("User").with("nmae", "x").all().unwrap_err();
    assert!(err.to_string().contains("did you mean 'name'"), "{err}");
    let err = db.create("User").set("age", 3).run().unwrap_err();
    assert_eq!(err.code(), Some("constraint_violation"));
}

#[test]
fn shows_the_generated_rql() {
    let db = setup();
    let (rql, params) = db
        .find("User")
        .with("name", "Ada")
        .outgoing("FOLLOWS")
        .filter(prop("age").gte(18))
        .order_by("name")
        .limit(5)
        .to_rql()
        .unwrap();
    assert_eq!(
        rql,
        "SELECT n1 FROM (n0:`User`)-[e1:`FOLLOWS`]->(n1) WHERE n0.`name` = $p0 AND n1.`age` >= $p1 ORDER BY n1.`name` LIMIT 5"
    );
    assert_eq!(params.len(), 2);
}
