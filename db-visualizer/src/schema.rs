//! The schema catalog as served by `GET /v1/schema`.

use serde::Deserialize;
use serde_json::Value;

/// A declared property.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PropertyInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub indexed: bool,
    #[serde(default)]
    pub searchable: bool,
}

impl PropertyInfo {
    /// `true` for `VECTOR(n)` properties, which always carry an HNSW index.
    #[must_use]
    pub fn is_vector(&self) -> bool {
        self.ty.to_ascii_uppercase().starts_with("VECTOR")
    }

    /// Index kinds written the way `SHOW INDEXES` prints them.
    #[must_use]
    pub fn index_summary(&self) -> String {
        let mut kinds = Vec::new();
        if self.unique {
            kinds.push("UNIQUE");
        } else if self.indexed {
            kinds.push("BTREE");
        }
        if self.searchable {
            kinds.push("BM25");
        }
        if self.is_vector() {
            kinds.push("HNSW");
        }
        if kinds.is_empty() {
            "—".to_owned()
        } else {
            kinds.join(" · ")
        }
    }

    /// A compact modifier list such as `NOT NULL UNIQUE`.
    #[must_use]
    pub fn modifiers(&self) -> String {
        let mut parts = Vec::new();
        if !self.nullable {
            parts.push("NOT NULL");
        }
        if self.unique {
            parts.push("UNIQUE");
        } else if self.indexed {
            parts.push("INDEX");
        }
        if self.searchable {
            parts.push("SEARCHABLE");
        }
        parts.join(" ")
    }
}

/// A node or edge type.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct TypeInfo {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub from: Vec<String>,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub properties: Vec<PropertyInfo>,
}

impl TypeInfo {
    /// `true` for node types.
    #[must_use]
    pub fn is_node(&self) -> bool {
        self.kind == "node"
    }

    /// `User | Bot → Post` for edges.
    #[must_use]
    pub fn endpoints(&self) -> String {
        format!("{} → {}", self.from.join(" | "), self.to.join(" | "))
    }

    /// Vector-indexed properties.
    #[must_use]
    pub fn vector_properties(&self) -> Vec<&PropertyInfo> {
        self.properties.iter().filter(|p| p.is_vector()).collect()
    }
}

/// A stored query's parameter.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct StoredParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// A `CREATE QUERY`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct StoredQuery {
    pub name: String,
    #[serde(default)]
    pub params: Vec<StoredParam>,
    #[serde(default)]
    pub body: String,
}

impl StoredQuery {
    /// `name($a STRING, $b INT)`.
    #[must_use]
    pub fn signature(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|p| format!("${} {}", p.name, p.ty))
            .collect();
        format!("{}({})", self.name, params.join(", "))
    }
}

/// The whole catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Schema {
    #[serde(default)]
    pub types: Vec<TypeInfo>,
    #[serde(default)]
    pub queries: Vec<StoredQuery>,
    #[serde(default)]
    pub ddl: String,
}

impl Schema {
    /// Decodes the server JSON.
    ///
    /// # Errors
    /// When the payload does not have the expected shape.
    pub fn from_json(value: Value) -> Result<Self, String> {
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    /// Node types in declaration order.
    pub fn node_types(&self) -> impl Iterator<Item = &TypeInfo> {
        self.types.iter().filter(|t| t.is_node())
    }

    /// Edge types in declaration order.
    pub fn edge_types(&self) -> impl Iterator<Item = &TypeInfo> {
        self.types.iter().filter(|t| !t.is_node())
    }

    /// Every vector-indexed property as `(type, property)`.
    #[must_use]
    pub fn vector_indexes(&self) -> Vec<(&TypeInfo, &PropertyInfo)> {
        self.types
            .iter()
            .flat_map(|t| t.vector_properties().into_iter().map(move |p| (t, p)))
            .collect()
    }

    /// Case-insensitive label filter.
    pub fn filter<'a>(
        entries: impl Iterator<Item = &'a TypeInfo>,
        search: &str,
    ) -> Vec<&'a TypeInfo> {
        let needle = search.trim().to_lowercase();
        entries
            .filter(|t| needle.is_empty() || t.name.to_lowercase().contains(&needle))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_server_payload() {
        let json = serde_json::json!({
            "types": [
                {"name": "User", "kind": "node", "count": 3, "from": [], "to": [],
                 "properties": [{"name": "name", "type": "STRING", "nullable": false, "unique": true, "indexed": true, "searchable": false},
                                {"name": "embedding", "type": "VECTOR(3)", "nullable": true, "unique": false, "indexed": false, "searchable": false}]},
                {"name": "FOLLOWS", "kind": "edge", "count": 1, "from": ["User"], "to": ["User"], "properties": []}
            ],
            "queries": [{"name": "friends", "params": [{"name": "who", "type": "STRING"}], "body": "SELECT 1"}],
            "ddl": "CREATE NODE User (...)"
        });
        let schema = Schema::from_json(json).unwrap();
        assert_eq!(schema.node_types().count(), 1);
        assert_eq!(
            schema.edge_types().next().unwrap().endpoints(),
            "User → User"
        );
        assert_eq!(schema.vector_indexes().len(), 1);
        assert_eq!(schema.queries[0].signature(), "friends($who STRING)");
        assert_eq!(schema.types[0].properties[0].index_summary(), "UNIQUE");
    }
}
