<div align="center">

<img src="assets/logo.svg" alt="RatikDB" width="420">

### Graphs at the speed of thought.

**RatikDB** is a strictly-typed graph database with built-in **vector search** and **BM25 full-text search**,
queried with **RQL** — SQL you already know, plus graph patterns — or with a **Rust DSL that reads like English**.

It ships as **one binary**. No Docker, no JVM, no container runtime: download `ratik`, run it.

`macOS (Apple Silicon + Intel)` · `Linux` · `Rust 2024` · `Apache-2.0`

</div>

---

```console
$ ratik init && ratik start --apply schema.rql
◆ RatikDB is up
  api        http://127.0.0.1:7285
  console    http://127.0.0.1:7285/
  data       ~/Library/Application Support/RatikDB

$ ratik shell
ratik › SELECT f.name, k.since
   ...  FROM (p:Person {name: 'Ada Lovelace'})-[k:KNOWS]->(f);
╭─────────────────┬─────────╮
│ f.name          │ k.since │
├─────────────────┼─────────┤
│ Charles Babbage │    1833 │
╰─────────────────┴─────────╯
1 row · 0.41 ms
```

## Why RatikDB

| | |
|---|---|
| **One binary** | `ratik` is the server, the shell, the web console, and the process manager. Copy it anywhere and run it. |
| **Strict schema** | Every node type, edge type and property is declared first. Typos fail fast with a fix: *"User has no property 'agee' — did you mean 'age'?"* |
| **SQL-shaped** | `SELECT … FROM (u:User)-[:FOLLOWS]->(f) WHERE … GROUP BY … ORDER BY … LIMIT`. Plus `FROM User u` when you just want a table. |
| **Graph + vectors + text** | HNSW vector indexes, BM25 full-text indexes, and B-tree property indexes, all planned together in one query. |
| **Readable SDK** | `db.find("User").with("name", "Ada").outgoing("FOLLOWS").all()?` — remote or embedded, same API. |
| **Durable** | Write-ahead log with checksummed records, atomic multi-statement transactions, automatic snapshots, crash-safe recovery. `F_FULLFSYNC` on macOS. |
| **Made for the Mac** | Universal binary, data in `~/Library/Application Support`, logs in `~/Library/Logs`, a launchd service in one command, and a Homebrew formula. |

## Install

**Homebrew** (macOS):

```sh
brew tap ratikgambhir/ratikdb https://github.com/RatikGambhir/graphdb
brew install --HEAD ratikgambhir/ratikdb/ratikdb
```

**Install script** (macOS universal binary / Linux x86_64, from GitHub releases):

```sh
curl -fsSL https://raw.githubusercontent.com/RatikGambhir/graphdb/main/scripts/install.sh | bash
```

**From source** (any platform with Rust 1.89+):

```sh
cargo install --path crates/ratik-cli      # puts `ratik` in ~/.cargo/bin
scripts/build-macos.sh                     # on a Mac: arm64 + x86_64 + universal tarballs in dist/
```

## The `ratik` CLI

```text
ratik serve      run in the foreground (Ctrl-C stops gracefully)
ratik start      run in the background          ratik stop       graceful stop + snapshot
ratik restart                                   ratik status     address, uptime, graph size
ratik logs -f    follow the server log          ratik shell      interactive RQL prompt
ratik query      one statement, table or --json ratik run        execute an .rql script atomically
ratik schema     print the schema as RQL        ratik snapshot   compact the log now
ratik init       write an example schema.rql    ratik service install|uninstall|print
```

* `ratik service install` registers a **launchd agent** (`~/Library/LaunchAgents/com.ratikdb.ratik.plist`) that starts
  RatikDB at login and restarts it if it crashes — but not after a deliberate `ratik stop`. On Linux it writes a systemd
  user unit instead. Homebrew users can also use `brew services start ratikdb`.
* Flags can come from the environment: `RATIK_DATA`, `RATIK_PORT`, `RATIK_HOST`, `RATIK_TOKEN`, `RATIK_URL`, `RATIK_LOG`.
* By default the server listens only on `127.0.0.1`. `--token` turns on bearer-token auth (do this before using
  `--host 0.0.0.0`), and browser cross-origin access stays off unless you pass `--cors <origin>`.
* `ratik shell --data DIR` or `--memory` opens a database directly, without a server.

Open **http://127.0.0.1:7285/** for the built-in console: a query editor, a schema browser, result tables, and a live
graph view.

## RQL in five minutes

The full reference is in [docs/RQL.md](docs/RQL.md).

```sql
-- 1. Declare everything first. Nothing undeclared can be written or read.
CREATE NODE User (
  name      STRING NOT NULL UNIQUE,
  age       INT INDEX,
  bio       STRING SEARCHABLE,          -- BM25 full-text index
  embedding VECTOR(384),                -- HNSW vector index (cosine)
  joined    TIMESTAMP DEFAULT now()
);
CREATE EDGE FOLLOWS FROM User TO User (since INT);

-- 2. Write — SQL-style rows, or graph patterns.
INSERT INTO User (name, age) VALUES ('Ada', 36), ('Alan', 41);
INSERT (a)-[:FOLLOWS {since: 2024}]->(b)
FROM User a, User b WHERE a.name = 'Ada' AND b.name = 'Alan';

-- 3. Read like SQL...
SELECT name, age FROM User WHERE age > 30 ORDER BY age DESC LIMIT 10;

-- ...walk the graph...
SELECT f.name FROM (u:User {name: 'Ada'})-[:FOLLOWS*1..3]->(f);

-- ...aggregate...
SELECT u.name, count(f) AS following
FROM (u:User)-[:FOLLOWS]->(f) GROUP BY u.name HAVING count(f) > 10;

-- ...search by meaning or by words.
SELECT name FROM User ORDER BY similarity(embedding, $vector) DESC LIMIT 5;
SELECT name FROM User WHERE matches(bio, 'graph theory') ORDER BY bm25(bio, 'graph theory') DESC;

-- 4. Change and remove.
UPDATE User SET age = age + 1 WHERE name = 'Ada' RETURNING name, age;
DELETE FROM User WHERE age < 13;                  -- also removes their edges

-- 5. Name a query once, call it from anywhere (CLI, HTTP, SDK).
CREATE QUERY followers($name STRING) AS
  SELECT a.name FROM (a)-[:FOLLOWS]->(b:User) WHERE b.name = $name;
CALL followers('Alan');

EXPLAIN SELECT * FROM User WHERE age > 40;          -- see which index is used
```

## The Rust SDK

```toml
[dependencies]
ratik = { git = "https://github.com/RatikGambhir/graphdb" }                          # talk to a server
# ratik = { git = "...", default-features = false, features = ["embedded"] }        # or run in-process
```

```rust
use ratik::prelude::*;

fn main() -> ratik::Result<()> {
    let db = Ratik::connect("http://127.0.0.1:7285")?;      // or Ratik::open("./data")? for embedded

    // Schema, declared in Rust.
    db.define_all([
        node("User").string("name").required().unique()
                    .int("age").indexed()
                    .text("bio")
                    .vector("embedding", 384),
        edge("FOLLOWS").from("User").to("User").int("since"),
    ])?;

    // Writes.
    let ada  = db.create("User").set("name", "Ada").set("age", 36).run()?;
    let alan = db.create("User").set("name", "Alan").set("age", 41).run()?;
    db.link(&ada, "FOLLOWS", &alan).set("since", 2024).run()?;

    // Reads that read like sentences.
    let following = db.find("User").with("name", "Ada").outgoing("FOLLOWS").all()?;
    let network: Vec<String> = db.find("User").with("name", "Ada")
        .outgoing("FOLLOWS").hops(1..=3).distinct().pluck("name")?;
    let adults = db.find("User").filter(prop("age").gte(18) & !prop("name").starts_with("Bot")).count()?;
    let similar = db.find("User").nearest("embedding", query_vector()).limit(5).all()?;
    let experts = db.find("User").search("bio", "graph databases").limit(10).all()?;

    // Typed results.
    #[derive(serde::Deserialize)]
    struct User { name: String, age: Option<i64> }
    let users: Vec<User> = db.find("User").order_by("name").all_as()?;

    // Updates and deletes.
    db.find("User").with("name", "Ada").update().increment("age", 1).run()?;
    db.find("User").filter(prop("age").lt(13)).delete()?;

    // Any RQL, any time.
    let rows = db.query("SELECT f.name FROM (u:User)-[:FOLLOWS]->(f) WHERE u.name = $who")
        .bind("who", "Ada")
        .fetch()?;
    for row in &rows {
        println!("{}", row.get::<String>("f.name")?);
    }
    Ok(())
}

fn query_vector() -> Vec<f32> {
    vec![0.0; 384] // your embedding model goes here
}
```

Every builder can show you what it will run: `.to_rql()` returns the generated RQL and its parameters. Values are
always sent as parameters, never pasted into query text.

## HTTP API

```sh
curl -s localhost:7285/v1/query -H 'content-type: application/json' \
  -d '{"query": "SELECT name FROM User WHERE age > $min", "params": {"min": 30}}'
# {"columns":["name"],"rows":[["Alan"],["Ada"]],"stats":{...},"time_ms":0.31}
```

| | |
|---|---|
| `POST /v1/query` | `{"query", "params"}` → `{columns, rows, stats, time_ms}` |
| `POST /v1/call/{name}` | call a stored query with `{"params"}` |
| `GET /v1/schema` · `GET /v1/stats` · `GET /v1/health` | introspection |
| `POST /v1/admin/snapshot` · `POST /v1/admin/shutdown` | operations |

Errors are always `{"error": {"code", "message"}}`, with codes like `syntax_error`, `schema_error`, `type_error` and
`constraint_violation` (HTTP 409).

## How it works

```
crates/
  ratikdb/     the engine — embeddable, no async, no network
    rql/       lexer → parser → AST
    exec/      name resolution against the schema, planner, pattern matcher, SELECT pipeline, writes, DDL
    index/     B-tree property indexes · BM25 inverted index · HNSW vector index
    graph.rs   in-memory adjacency store + invertible primitive ops
    storage.rs write-ahead log + snapshots
  ratik-sdk/   the `ratik` Rust crate (DSL + RQL, remote or embedded)
  ratik-cli/   the `ratik` binary (axum server, web console, shell, process manager)
```

* **Storage.** The graph lives in memory with adjacency lists on every node (index-free adjacency), which keeps traversals
  fast. Every change is a small invertible *op*. A transaction applies its ops and appends them to the write-ahead log
  as one CRC-checked record before acknowledging. If anything fails, the inverse ops roll it back. Snapshots compact the
  log, and recovery replays the exact code paths that ran live. A torn record at the tail (from power loss mid-write) is
  detected and discarded.
* **Planning.** Each pattern starts at its cheapest variable. The planner picks a unique or B-tree index lookup, an index
  range, a BM25 candidate set, an HNSW top-k (for `ORDER BY similarity(...) LIMIT k`), an edge-type scan, or a label
  scan. It then expands along adjacency lists. Every `WHERE` conjunct runs at the earliest step where its variables are
  bound. `EXPLAIN` shows the plan.
* **Concurrency.** Many readers, one writer. Scripts are atomic.

### Compared with HelixDB

RatikDB is inspired by [HelixDB](https://github.com/HelixDB/helix-db) and shares its goal of one engine for graph,
vector and text. It differs in a few deliberate ways:

* **Runs as a plain binary.** Helix's local instance needs Docker or Podman.
* **SQL-shaped query language.** Queries are SQL with graph patterns, instead of a traversal DSL.
* **Mandatory schema.** It is enforced on both reads and writes.
* **Built-in tooling.** A console and a process manager ship inside the same executable.

## Development

```sh
cargo test --workspace --all-features       # engine, SDK (embedded) and server tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p ratik-cli -- serve --memory    # hack on it with a throwaway database
```

CI runs on macOS Apple Silicon, macOS Intel and Linux. Pushing a `v*` tag builds release binaries, including an ad-hoc
signed universal macOS binary.

---

<div align="center"><sub>RatikDB · Graphs at the speed of thought.</sub></div>
