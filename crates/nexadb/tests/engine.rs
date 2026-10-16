//! End-to-end tests of the engine through its public API.

use nexadb::{Database, Error, Params, Value};

fn db() -> Database {
    let db = Database::in_memory();
    db.execute(
        "CREATE NODE User (
            name   STRING NOT NULL UNIQUE,
            age    INT INDEX,
            bio    STRING SEARCHABLE,
            emb    VECTOR(3),
            joined TIMESTAMP DEFAULT now()
         );
         CREATE NODE Post (title STRING NOT NULL, likes INT DEFAULT 0);
         CREATE EDGE FOLLOWS FROM User TO User (since INT);
         CREATE EDGE WROTE FROM User TO Post;",
    )
    .unwrap();
    db.execute(
        "INSERT INTO User (name, age, bio, emb) VALUES
            ('ada',    36, 'mathematician and first programmer', [1, 0, 0]),
            ('linus',  28, 'kernel hacker who loves graph theory', [0, 1, 0]),
            ('grace',  45, 'compiler pioneer', [0.9, 0.1, 0]),
            ('alan',   41, 'graph and computation theory', [0, 0, 1]);
         INSERT (a)-[:FOLLOWS {since: 2020}]->(b) FROM User a, User b WHERE a.name = 'ada' AND b.name = 'linus';
         INSERT (a)-[:FOLLOWS {since: 2021}]->(b) FROM User a, User b WHERE a.name = 'linus' AND b.name = 'grace';
         INSERT (a)-[:FOLLOWS {since: 2022}]->(b) FROM User a, User b WHERE a.name = 'grace' AND b.name = 'alan';
         INSERT (a)-[:WROTE]->(:Post {title: 'Notes on the Analytical Engine', likes: 99}) FROM User a WHERE a.name = 'ada';",
    )
    .unwrap();
    db
}

fn strings(db: &Database, q: &str) -> Vec<String> {
    db.execute(q)
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .rows
        .into_iter()
        .map(|r| {
            r.into_iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

#[test]
fn sql_style_selects() {
    let db = db();
    assert_eq!(
        strings(
            &db,
            "SELECT name FROM User WHERE age > 40 ORDER BY age DESC"
        ),
        ["grace", "alan"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT name, age FROM User u ORDER BY u.age LIMIT 2 OFFSET 1"
        ),
        ["ada|36", "alan|41"]
    );
    assert_eq!(
        strings(&db, "SELECT count(*) AS n, max(age), avg(age) FROM User"),
        ["4|45|37.5"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT name FROM User WHERE name LIKE '%a%' AND name NOT IN ['ada'] ORDER BY name"
        ),
        ["alan", "grace"]
    );
    assert_eq!(
        strings(&db, "SELECT name FROM User WHERE age BETWEEN 30 AND 40"),
        ["ada"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT CASE WHEN age >= 40 THEN 'senior' ELSE 'junior' END AS band, count(*) FROM User GROUP BY band ORDER BY band"
        ),
        ["junior|2", "senior|2"]
    );
    let star = db.execute("SELECT * FROM Post").unwrap();
    assert_eq!(star.columns, ["_id", "title", "likes"]);
}

#[test]
fn graph_patterns_and_traversals() {
    let db = db();
    assert_eq!(
        strings(
            &db,
            "SELECT b.name FROM (a:User {name: 'ada'})-[:FOLLOWS]->(b)"
        ),
        ["linus"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT a.name FROM (a)-[:FOLLOWS]->(b:User {name: 'grace'})"
        ),
        ["linus"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT b.name FROM (a:User {name: 'ada'})-[:FOLLOWS*1..3]->(b) ORDER BY b.name"
        ),
        ["alan", "grace", "linus"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT b.name FROM (a:User {name: 'ada'})-[:FOLLOWS*2]->(b)"
        ),
        ["grace"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT a.name, f.since, b.name FROM (a)-[f:FOLLOWS]->(b) WHERE f.since >= 2021 ORDER BY f.since"
        ),
        ["linus|2021|grace", "grace|2022|alan"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT p.title FROM (u:User)-[:WROTE]->(p) WHERE u.name = 'ada'"
        ),
        ["Notes on the Analytical Engine"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT x.name FROM (x)<-[:FOLLOWS]-(:User {name: 'linus'})"
        ),
        ["grace"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT n.name FROM (n:User {name: 'linus'})-[:FOLLOWS]-(m), (m)-[]-(n) WHERE m.name = 'ada'"
        ),
        ["linus"]
    );
    // Friends-of-friends with an aggregate.
    assert_eq!(
        strings(
            &db,
            "SELECT a.name, count(c) AS fof FROM (a:User)-[:FOLLOWS]->(b)-[:FOLLOWS]->(c) GROUP BY a.name ORDER BY a.name"
        ),
        ["ada|1", "linus|1"]
    );
}

#[test]
fn vectors_and_full_text() {
    let db = db();
    assert_eq!(
        strings(
            &db,
            "SELECT name FROM User ORDER BY similarity(emb, [1, 0, 0]) DESC LIMIT 2"
        ),
        ["ada", "grace"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT name FROM User WHERE age > 40 ORDER BY similarity(emb, [1, 0, 0]) DESC LIMIT 1"
        ),
        ["grace"]
    );
    let plan = strings(
        &db,
        "EXPLAIN SELECT name FROM User ORDER BY similarity(emb, [1, 0, 0]) DESC LIMIT 2",
    );
    assert!(plan[0].contains("HNSW"), "{plan:?}");
    assert_eq!(
        strings(
            &db,
            "SELECT name FROM User WHERE matches(bio, 'graph theory') ORDER BY bm25(bio, 'graph theory') DESC"
        ),
        ["alan", "linus"]
    );
    let plan = strings(
        &db,
        "EXPLAIN SELECT name FROM User WHERE matches(bio, 'graph')",
    );
    assert!(plan[0].contains("BM25"), "{plan:?}");
}

#[test]
fn index_access_paths() {
    let db = db();
    let plan = strings(&db, "EXPLAIN SELECT * FROM User WHERE name = 'ada'");
    assert!(plan[0].contains("index name"), "{plan:?}");
    let plan = strings(&db, "EXPLAIN SELECT * FROM User WHERE age > 40");
    assert!(plan[0].contains("range"), "{plan:?}");
    let plan = strings(
        &db,
        "EXPLAIN SELECT b FROM (a:User {name: 'ada'})-[:FOLLOWS]->(b)",
    );
    assert!(plan[0].contains("(a:User) via index"), "{plan:?}");
}

#[test]
fn strict_schema_rejects_undeclared_things() {
    let db = db();
    let err = |q: &str| db.execute(q).unwrap_err().to_string();
    assert!(err("SELECT * FROM Usr").contains("did you mean 'User'"));
    assert!(err("SELECT agee FROM User").contains("did you mean 'age'"));
    assert!(err("SELECT u.agee FROM User u").contains("did you mean 'age'"));
    assert!(
        err("INSERT INTO User (name, nickname) VALUES ('x', 'y')")
            .contains("no property 'nickname'")
    );
    assert!(err("INSERT INTO Robot (name) VALUES ('x')").contains("unknown node type 'Robot'"));
    assert!(err("INSERT INTO User (name, age) VALUES ('x', 'old')").contains("expects INT"));
    assert!(err("INSERT INTO User (age) VALUES (3)").contains("NOT NULL"));
    assert!(err("INSERT INTO User (name) VALUES ('ada')").contains("UNIQUE"));
    assert!(err("INSERT INTO User (name, emb) VALUES ('v', [1, 2])").contains("VECTOR(3)"));
    assert!(
        err("SELECT p FROM (p:Post)-[:FOLLOWS]->(u)").contains("FOLLOWS goes from User to User")
    );
    assert!(
        err("INSERT (p)-[:FOLLOWS]->(u) FROM Post p, User u")
            .contains("FOLLOWS edges go from User to User")
    );
    assert!(err("SELECT x FROM (x)-[:LIKES]->(y)").contains("unknown edge type 'LIKES'"));
    assert!(matches!(
        db.execute("SELECT name FROM User WHERE").unwrap_err(),
        Error::Syntax { .. }
    ));
}

#[test]
fn writes_and_transactions() {
    let db = db();
    let r = db
        .execute("UPDATE User SET age = age + 1 WHERE name = 'ada' RETURNING name, age")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::from("ada"), Value::Int(37)]]);
    assert_eq!(r.stats.properties_set, 1);

    let r = db.execute("DELETE FROM User WHERE name = 'linus'").unwrap();
    assert_eq!((r.stats.nodes_deleted, r.stats.edges_deleted), (1, 2));
    assert_eq!(
        strings(&db, "SELECT count(*) FROM (a)-[:FOLLOWS]->(b)"),
        ["1"]
    );

    // A failing statement rolls back the whole script.
    let before = strings(&db, "SELECT count(*) FROM User");
    let failed = db
        .execute("INSERT INTO User (name) VALUES ('zed'); INSERT INTO User (name) VALUES ('ada')");
    assert!(failed.is_err());
    assert_eq!(strings(&db, "SELECT count(*) FROM User"), before);

    let r = db
        .execute("INSERT INTO User (name) VALUES ('new') RETURNING _id, name, joined")
        .unwrap();
    assert!(
        matches!(r.rows[0][2], Value::Int(t) if t > 1_600_000_000_000),
        "default now() applied"
    );
    let r = db
        .execute("DELETE e FROM (a)-[e:FOLLOWS]->(b) RETURNING e.since")
        .unwrap();
    assert_eq!(r.stats.edges_deleted, 1);
}

#[test]
fn ddl_evolution() {
    let db = db();
    db.execute("ALTER NODE User ADD country STRING DEFAULT 'earth'")
        .unwrap();
    assert_eq!(strings(&db, "SELECT DISTINCT country FROM User"), ["earth"]);
    db.execute("ALTER NODE User DROP country").unwrap();
    assert!(db.execute("SELECT country FROM User").is_err());
    assert!(
        db.execute("DROP NODE Post")
            .unwrap_err()
            .to_string()
            .contains("WROTE")
    );
    db.execute("DROP EDGE WROTE CASCADE; DROP NODE Post CASCADE")
        .unwrap();
    assert!(db.execute("SELECT * FROM Post").is_err());
    db.execute("CREATE INDEX ON User(bio)").unwrap();
    assert!(db.execute("CREATE UNIQUE INDEX ON User(age)").is_ok());
    db.execute("INSERT INTO User (name, age) VALUES ('dup', 99)")
        .unwrap();
    assert!(
        db.execute("INSERT INTO User (name, age) VALUES ('dup2', 99)")
            .is_err()
    );
    assert!(db.schema_ddl().contains("CREATE NODE User"));
}

#[test]
fn stored_queries() {
    let db = db();
    db.execute("CREATE QUERY followers($name STRING) AS SELECT a.name FROM (a)-[:FOLLOWS]->(b:User) WHERE b.name = $name")
        .unwrap();
    let mut p = Params::new();
    p.insert("name".into(), "grace".into());
    assert_eq!(
        db.call("followers", &p).unwrap().rows,
        vec![vec![Value::from("linus")]]
    );
    assert_eq!(strings(&db, "CALL followers('alan')"), ["grace"]);
    assert!(
        db.execute("CREATE QUERY bad($x INT) AS SELECT nope FROM User")
            .is_err()
    );
    assert!(
        db.execute("CREATE QUERY bad2() AS SELECT name FROM User WHERE age = $undeclared")
            .is_err()
    );
    db.execute("CREATE QUERY birthday($name STRING) AS UPDATE User SET age = age + 1 WHERE name = $name RETURNING age")
        .unwrap();
    assert_eq!(strings(&db, "CALL birthday('ada')"), ["37"]);
    // Validation ran in a rolled-back savepoint: the dummy call changed nothing.
    assert_eq!(
        strings(&db, "SELECT age FROM User WHERE name = 'ada'"),
        ["37"]
    );
}

#[test]
fn durability_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        db.execute("CREATE NODE Item (sku STRING UNIQUE, emb VECTOR(2)); CREATE EDGE NEXT FROM Item TO Item")
            .unwrap();
        db.execute(
            "INSERT (:Item {sku: 'a', emb: [1, 0]})-[:NEXT]->(:Item {sku: 'b', emb: [0, 1]})",
        )
        .unwrap();
        assert!(matches!(Database::open(dir.path()), Err(Error::Locked(_))));
    }
    {
        let db = Database::open(dir.path()).unwrap();
        assert_eq!(
            strings(&db, "SELECT b.sku FROM (a:Item {sku: 'a'})-[:NEXT]->(b)"),
            ["b"]
        );
        db.execute("UPDATE Item SET sku = 'c' WHERE sku = 'b'")
            .unwrap();
        db.snapshot().unwrap();
        db.execute("INSERT INTO Item (sku) VALUES ('d')").unwrap();
    }
    let db = Database::open(dir.path()).unwrap();
    assert_eq!(
        strings(&db, "SELECT sku FROM Item ORDER BY sku"),
        ["a", "c", "d"]
    );
    assert_eq!(
        strings(
            &db,
            "SELECT sku FROM Item ORDER BY similarity(emb, [0, 1]) DESC LIMIT 1"
        ),
        ["c"]
    );
    assert!(
        db.execute("INSERT INTO Item (sku) VALUES ('a')").is_err(),
        "unique index rebuilt on load"
    );
}

#[test]
fn survives_a_crash_without_clean_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path()).unwrap();
    db.execute("CREATE NODE T (v INT UNIQUE); INSERT INTO T (v) VALUES (1), (2)")
        .unwrap();
    // Simulate a crash: the handle is never closed, and we copy the file as
    // it sits on disk right now. Every acknowledged commit must be in it.
    let copy = tempfile::tempdir().unwrap();
    std::fs::copy(
        dir.path().join(nexadb::DATA_FILE),
        copy.path().join(nexadb::DATA_FILE),
    )
    .unwrap();
    std::mem::forget(db);
    let db = Database::open(copy.path()).unwrap();
    assert_eq!(strings(&db, "SELECT v FROM T ORDER BY v"), ["1", "2"]);
    assert!(
        db.execute("INSERT INTO T (v) VALUES (2)").is_err(),
        "index intact"
    );
}

#[test]
fn data_larger_than_the_page_cache() {
    let dir = tempfile::tempdir().unwrap();
    let options = nexadb::Options {
        cache_mb: 1,
        ..nexadb::Options::default()
    };
    let rows = 20_000;
    {
        let db = Database::open_with(dir.path(), options.clone()).unwrap();
        db.execute("CREATE NODE Doc (n INT UNIQUE, body STRING); CREATE EDGE NEXT FROM Doc TO Doc")
            .unwrap();
        let filler = "x".repeat(400);
        let values: Vec<String> = (0..rows).map(|n| format!("({n}, '{filler}{n}')")).collect();
        db.execute(&format!(
            "INSERT INTO Doc (n, body) VALUES {}",
            values.join(", ")
        ))
        .unwrap();
        db.execute("INSERT (a)-[:NEXT]->(b) FROM Doc a, Doc b WHERE a.n = 19998 AND b.n = 19999")
            .unwrap();
    }
    let size = std::fs::metadata(dir.path().join(nexadb::DATA_FILE))
        .unwrap()
        .len();
    assert!(
        size > 8 * 1024 * 1024,
        "the data ({size} bytes) is far larger than the 1 MiB cache"
    );
    let db = Database::open_with(dir.path(), options).unwrap();
    assert_eq!(strings(&db, "SELECT count(*) FROM Doc"), [rows.to_string()]);
    assert_eq!(
        strings(&db, "SELECT length(body) FROM Doc WHERE n = 12345"),
        ["405"]
    );
    assert_eq!(
        strings(&db, "SELECT b.n FROM (a:Doc {n: 19998})-[:NEXT]->(b)"),
        ["19999"]
    );
    assert_eq!(
        strings(&db, "SELECT count(*) FROM Doc WHERE n >= 19990"),
        ["10"]
    );
    assert_eq!(db.stats().unwrap().nodes, rows);
}

#[test]
fn failed_scripts_leave_nothing_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        db.execute("CREATE NODE T (v INT UNIQUE, s STRING SEARCHABLE)")
            .unwrap();
        let failed = db.execute(
            "INSERT INTO T (v, s) VALUES (1, 'alpha'); CREATE NODE U (); INSERT INTO T (v) VALUES (1)",
        );
        assert!(failed.is_err());
    }
    let db = Database::open(dir.path()).unwrap();
    assert_eq!(strings(&db, "SELECT count(*) FROM T"), ["0"]);
    assert!(
        db.execute("SELECT * FROM U").is_err(),
        "schema change rolled back too"
    );
    assert!(strings(&db, "SELECT v FROM T WHERE matches(s, 'alpha')").is_empty());
    db.execute("INSERT INTO T (v, s) VALUES (1, 'alpha')")
        .unwrap();
}

#[test]
fn json_fields_and_list_indexing() {
    let db = Database::in_memory();
    db.execute(
        "CREATE NODE Doc (name STRING, meta JSON, tags STRING[]);
         INSERT INTO Doc {name: 'a', meta: {owner: {team: 'core'}}, tags: ['x', 'y']}",
    )
    .unwrap();
    assert_eq!(
        strings(
            &db,
            "SELECT meta.owner.team, tags[1], tags[-1], length(tags) FROM Doc"
        ),
        ["core|y|y|2"]
    );
    assert!(
        db.execute("SELECT nope.field FROM Doc")
            .unwrap_err()
            .to_string()
            .contains("unknown variable 'nope'")
    );
}
