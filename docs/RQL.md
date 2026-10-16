# RQL reference

RQL (Ratik Query Language) is SQL with graph patterns. Keywords are case-insensitive, while type and property
names are case-sensitive. Statements are separated by `;`, and a script sent in one request runs as **one atomic
transaction**. Comments are `-- …`, `// …` and `/* … */`.

- [Schema](#schema)
- [Patterns](#patterns)
- [SELECT](#select)
- [INSERT](#insert) · [UPDATE](#update) · [DELETE](#delete)
- [Vectors and full-text](#vectors-and-full-text)
- [Stored queries](#stored-queries)
- [Introspection](#introspection)
- [Expressions and functions](#expressions-and-functions)

---

## Schema

Every node type, edge type and property must be declared before it can be written **or read**. If you use an
undeclared name, the query fails and the error suggests the closest declared one.

```sql
CREATE NODE [IF NOT EXISTS] User (
  name      STRING NOT NULL UNIQUE,
  email     STRING UNIQUE,
  age       INT INDEX,
  score     FLOAT DEFAULT 0,
  bio       STRING SEARCHABLE,
  tags      STRING[],
  profile   JSON,
  embedding VECTOR(384) METRIC COSINE,
  joined    TIMESTAMP DEFAULT now()
);

CREATE EDGE [IF NOT EXISTS] FOLLOWS FROM User TO User (since INT);
CREATE EDGE LIKES FROM User | Bot TO Post | Comment;      -- several endpoint types
```

| Type | Values |
|---|---|
| `BOOL` | `true`, `false` |
| `INT` | 64-bit integers |
| `FLOAT` | 64-bit floats (`INT` values are accepted and widened) |
| `STRING` (`TEXT`, `VARCHAR`) | UTF-8 text |
| `TIMESTAMP` | milliseconds since the Unix epoch (`now()`) |
| `VECTOR(n)` | exactly `n` floats; always indexed with HNSW |
| `JSON` | any value: lists, maps, numbers… |
| `T[]` | list of `T`, e.g. `STRING[]`, `INT[]` |

| Modifier | Meaning |
|---|---|
| `NOT NULL` | a value is required on insert and may not be set to `NULL` |
| `UNIQUE` | no two elements of the type share a value (indexed) |
| `PRIMARY KEY` | `NOT NULL UNIQUE` |
| `INDEX` | B-tree index for `=`, `IN`, `<`, `>`, `BETWEEN` lookups |
| `SEARCHABLE` | BM25 full-text index (`STRING` only) |
| `DEFAULT <literal>` / `DEFAULT now()` | value used when the property is omitted |
| `METRIC COSINE \| EUCLIDEAN \| DOT` | distance used by a `VECTOR` index (default `COSINE`) |

Evolving the schema:

```sql
ALTER NODE User ADD country STRING DEFAULT 'unknown';
ALTER NODE User DROP country;
CREATE INDEX ON User(age);            CREATE UNIQUE INDEX ON User(email);
CREATE SEARCH INDEX ON User(bio);     DROP INDEX ON User(age);
DROP EDGE FOLLOWS;                    -- fails if edges exist…
DROP NODE User CASCADE;               -- …unless CASCADE deletes them too
```

Names starting with `_` are reserved. Every node and edge exposes `_id` and `_label`, and edges also expose `_from`
and `_to`. Quote names that collide with keywords using backticks, for example `` `order` ``.

## Patterns

Patterns describe shapes in the graph and appear after `FROM`:

```sql
(u:User)                                  -- a node of type User, bound to u
(u:User {name: 'Ada'})                    -- with inline equality filters
(u)-[:FOLLOWS]->(f)                       -- outgoing edge
(u)<-[:FOLLOWS]-(f)                       -- incoming edge
(u)-[:FOLLOWS]-(f)                        -- either direction
(u)-[e:FOLLOWS {since: 2024}]->(f)        -- bind and filter the edge
(u)-[:FOLLOWS|BLOCKS]->(f)                -- several edge types
(u)-[:FOLLOWS*1..3]->(f)                  -- 1 to 3 hops   (*2 exactly, *2.. at least, * = 1..∞)
(u)-[]->(x)                               -- any edge type
```

SQL-style table references work too. `FROM User` means `FROM (User:User)`, and `FROM User u` means `FROM (u:User)`.
Several comma-separated patterns are joined on shared variables, or as a cross product when they share none.

Unlabeled nodes get their possible types from the edges around them. In `(u:User)-[:WROTE]->(p)`, `p` must be a
`Post` if `WROTE` is declared `FROM User TO Post`. A pattern the schema makes impossible is rejected before it runs.

A variable-length step (`*`) returns each reachable node once, at its shortest distance, and cannot be bound to a
variable.

## SELECT

```sql
SELECT [DISTINCT] <expr> [AS alias], ... | *
[FROM <pattern>, ...]
[WHERE <condition>]
[GROUP BY <expr>, ...]
[HAVING <condition>]
[ORDER BY <expr> [ASC|DESC], ...]
[LIMIT <n>] [OFFSET <n>]
```

* Plain names resolve to an alias, then a variable, then a property of the only variable that declares it. So in
  `SELECT name FROM User WHERE age > 30`, `name` means `User.name`. An ambiguous name produces an error asking you to
  qualify it.
* `SELECT *` with a single variable spreads its properties into columns. With several variables, you get one column
  per variable holding the whole node or edge as a map with `_id` and `_label` keys.
* Aggregates: `count(*)`, `count(x)`, `count(DISTINCT x)`, `sum`, `avg`, `min`, `max`, `collect` (`array_agg`).
* `NULL`s sort last in both directions.
* `LIMIT` and `OFFSET` take integers or parameters.

```sql
SELECT u.name, count(f) AS following, collect(f.name) AS who
FROM (u:User)-[:FOLLOWS]->(f)
WHERE u.age >= 18
GROUP BY u.name
HAVING count(f) > 2
ORDER BY following DESC
LIMIT 10;
```

## INSERT

```sql
-- Rows, SQL style
INSERT INTO User (name, age) VALUES ('Ada', 36), ('Alan', 41) RETURNING _id, name;
INSERT INTO User {name: 'Grace', age: 45}, {name: 'Edsger'};

-- Patterns: unbound labeled nodes are created, bound variables are reused
INSERT (:User {name: 'Ada'})-[:FOLLOWS {since: 2020}]->(:User {name: 'Alan'});
INSERT (a)-[:FOLLOWS]->(b)
FROM User a, User b
WHERE a.name = 'Ada' AND b.name = 'Grace'
RETURNING a.name, b.name;
```

Edges must have exactly one type and a direction, and their endpoint types must match the declaration.

## UPDATE

```sql
UPDATE User SET age = age + 1, score = score * 1.1 WHERE name = 'Ada' RETURNING *;
UPDATE (u:User)-[:FOLLOWS]->(f) SET f.score = f.score + 1 WHERE u.name = 'Ada';
UPDATE (a)-[e:FOLLOWS]->(b) SET e.since = 2025 WHERE a.name = 'Ada';
```

All right-hand sides read the state from before the statement. Each element is updated at most once, even if the
pattern reaches it several times.

## DELETE

```sql
DELETE FROM User WHERE age < 13;                           -- removes their edges as well
DELETE e FROM (a:User)-[e:FOLLOWS]->(b) WHERE e.since < 2000;
DELETE a, b FROM (a)-[:DUPLICATE_OF]->(b) RETURNING a.name; -- RETURNING sees the deleted data
```

## Vectors and full-text

```sql
-- k nearest neighbours (uses the HNSW index; exact re-ranking of the candidates)
SELECT name, similarity(embedding, $q) AS score
FROM User
ORDER BY similarity(embedding, $q) DESC
LIMIT 10;

-- ...combined with filters and traversals
SELECT p.title FROM (u:User {name: 'Ada'})-[:WROTE]->(p)
ORDER BY similarity(p.embedding, $q) DESC LIMIT 5;

-- BM25 full-text: matches() filters, bm25() ranks
SELECT title, bm25(body, 'graph databases') AS relevance
FROM Post
WHERE matches(body, 'graph databases')
ORDER BY relevance DESC;
```

| Function | Index used when ordering… |
|---|---|
| `similarity(a, b)` — cosine similarity | `METRIC COSINE` with `DESC` |
| `distance(a, b)` — Euclidean distance | `METRIC EUCLIDEAN` with `ASC` |
| `dot(a, b)` — inner product | `METRIC DOT` with `DESC` |
| `matches(prop, 'text')` | BM25 candidates (any of the words) |
| `bm25(prop, 'text')` | relevance score |

Text is lower-cased and split on non-alphanumeric characters, and common English stopwords are dropped.

## Stored queries

```sql
CREATE [OR REPLACE] QUERY friends_of($name STRING, $limit INT) AS
  SELECT f.name FROM (u:User {name: $name})-[:FOLLOWS]->(f) LIMIT $limit;

CALL friends_of('Ada', 10);
DROP QUERY [IF EXISTS] friends_of;
```

The body is checked against the schema when it is created. The check runs inside a transaction that is always rolled
back, so it never changes data. Arguments are type-checked on every call. Over HTTP, call it with
`POST /v1/call/friends_of {"params": {"name": "Ada", "limit": 10}}`.

## Introspection

```sql
SHOW SCHEMA;    SHOW NODES;    SHOW EDGES;    SHOW INDEXES;    SHOW QUERIES;    SHOW STATS;
DESCRIBE User;
EXPLAIN SELECT ...;     -- which index starts the match, and where filters run
```

## Expressions and functions

**Operators**, from lowest to highest precedence:

1. `OR`
2. `AND`
3. `NOT`
4. Comparisons: `=`, `!=`/`<>`, `<`, `<=`, `>`, `>=`, `IS [NOT] NULL`, `[NOT] IN`, `[NOT] LIKE`/`ILIKE`,
   `[NOT] BETWEEN … AND …`
5. `+`, `-`, `||`
6. `*`, `/`, `%`
7. Unary `-`
8. `.prop` and `[index]`

Logic is SQL three-valued: comparisons with `NULL` are `NULL`, and `WHERE` keeps only rows that are `TRUE`. Integer
overflow is an error, not a silent wrap. `LIKE` uses `%` for any run of characters and `_` for exactly one.

**Literals:** `42`, `-1.5e3`, `'text'` (or `"text"`, with `''` to escape a quote), `true`, `false`, `NULL`,
`[1, 2, 3]`, `{key: 'value'}`, and parameters `$name`. Use `CASE WHEN … THEN … ELSE … END` for conditionals.

| Category | Functions |
|---|---|
| Elements | `id(x)`, `label(x)`, `degree(n)`, `out_degree(n)`, `in_degree(n)`, `keys(x)` |
| Strings | `lower`, `upper`, `trim`, `length`, `substring(s, start [, len])`, `replace(s, from, to)`, `split(s, sep)`, `starts_with`, `ends_with`, `contains` |
| Numbers | `abs`, `round(x [, digits])`, `floor`, `ceil`, `sqrt`, `pow`, `greatest(...)`, `least(...)` |
| Conversion | `str(x)`, `int(x)`, `float(x)` |
| Nulls | `coalesce(a, b, ...)` |
| Time | `now()` |
| Vectors | `similarity`, `distance`, `dot` |
| Text | `matches`, `bm25` |
