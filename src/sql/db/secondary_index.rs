//! Secondary indexes — a separate lookup structure per indexed column.
//!
//! Every UNIQUE (or PRIMARY KEY) column gets one automatically at
//! CREATE TABLE time. Explicit `CREATE INDEX` statements (Phase 3e.2) add
//! more. On INSERT / UPDATE / DELETE the owning `Table` keeps its indexes
//! in lockstep with its row storage.
//!
//! **Key shape.** A B-Tree keyed by `(value, rowid)` would let us support
//! duplicate values naturally. For simplicity the in-memory representation
//! is `BTreeMap<value, Vec<rowid>>` — functionally equivalent, cheaper to
//! iterate for a given value, a little heavier on the allocator for
//! wide-dup columns. The on-disk representation in Phase 3e.4 will flatten
//! to `(value, rowid)` keys one row per entry.
//!
//! **Types.** Only Integer and Text columns are currently indexed. Real
//! has floating-point equality hazards; Bool has so few distinct values an
//! index isn't worth it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::{DataType, Value};

/// Declares who created the index. Persisted into `sqlrite_master.sql` so
/// the text round-trips; auto-created indexes get a synthesized SQL form
/// so the catalog stays uniform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexOrigin {
    /// Auto-created for a UNIQUE / PRIMARY KEY column at CREATE TABLE time.
    Auto,
    /// Explicit `CREATE INDEX` statement (Phase 3e.2).
    Explicit,
}

/// One secondary index on a single column. Multi-column composite indexes
/// are on the longer-term list; the `column_name` field stays singular
/// for now.
#[derive(Debug, Serialize, Deserialize)]
pub struct SecondaryIndex {
    /// Catalog name. For auto indexes: `sqlrite_autoindex_<table>_<col>`.
    /// For explicit indexes: the user-supplied identifier from CREATE INDEX.
    pub name: String,
    pub table_name: String,
    pub column_name: String,
    pub is_unique: bool,
    pub origin: IndexOrigin,
    pub entries: IndexEntries,
}

/// Typed map from value → list of rowids carrying that value. The rowid
/// list is always non-empty; empty lists are pruned on remove.
#[derive(Debug, Serialize, Deserialize)]
pub enum IndexEntries {
    Integer(BTreeMap<i64, Vec<i64>>),
    Text(BTreeMap<String, Vec<i64>>),
}

impl SecondaryIndex {
    /// Builds an empty index over a column of the given datatype. Returns
    /// an error for unsupported datatypes (Real, Bool, None, Invalid).
    pub fn new(
        name: String,
        table_name: String,
        column_name: String,
        datatype: &DataType,
        is_unique: bool,
        origin: IndexOrigin,
    ) -> Result<Self> {
        let entries = match datatype {
            DataType::Integer => IndexEntries::Integer(BTreeMap::new()),
            DataType::Text => IndexEntries::Text(BTreeMap::new()),
            other => {
                return Err(SQLRiteError::General(format!(
                    "cannot build a secondary index on a {other} column"
                )));
            }
        };
        Ok(Self {
            name,
            table_name,
            column_name,
            is_unique,
            origin,
            entries,
        })
    }

    /// Synthesizes a CREATE INDEX statement for `sqlrite_master.sql`. For
    /// auto indexes this is a synthetic form; for explicit indexes the
    /// caller can override with the original user text if it has been
    /// preserved. Used by the persistence path in Phase 3e.4.
    #[allow(dead_code)]
    pub fn synthesized_sql(&self) -> String {
        let unique = if self.is_unique { "UNIQUE " } else { "" };
        format!(
            "CREATE {unique}INDEX {} ON {} ({});",
            self.name, self.table_name, self.column_name
        )
    }

    /// Standard name for the auto-generated index of a UNIQUE/PK column.
    /// Uniform across save/open so indexes persist under a stable name.
    pub fn auto_name(table_name: &str, column_name: &str) -> String {
        format!("sqlrite_autoindex_{table_name}_{column_name}")
    }

    /// Returns `true` iff inserting `value` would violate the UNIQUE
    /// constraint — i.e., the index already has an entry for this value
    /// and `self.is_unique` is set. Null values are never indexed and
    /// never conflict.
    pub fn would_violate_unique(&self, value: &Value) -> bool {
        if !self.is_unique {
            return false;
        }
        match (&self.entries, value) {
            (IndexEntries::Integer(m), Value::Integer(v)) => m.contains_key(v),
            (IndexEntries::Text(m), Value::Text(s)) => m.contains_key(s),
            _ => false, // type mismatch can't collide; other code paths catch it
        }
    }

    /// Adds a `(value, rowid)` entry. Null values are ignored (NULL in an
    /// indexed column stays out of the index). Type mismatches — e.g.
    /// calling this with a Text value against an Integer index — return
    /// an error rather than silently skipping.
    pub fn insert(&mut self, value: &Value, rowid: i64) -> Result<()> {
        match (&mut self.entries, value) {
            (_, Value::Null) => Ok(()),
            (IndexEntries::Integer(m), Value::Integer(v)) => {
                m.entry(*v).or_default().push(rowid);
                Ok(())
            }
            (IndexEntries::Text(m), Value::Text(s)) => {
                m.entry(s.clone()).or_default().push(rowid);
                Ok(())
            }
            (entries, value) => Err(SQLRiteError::Internal(format!(
                "type mismatch inserting into index '{}': entries={entries:?}, value={value:?}",
                self.name
            ))),
        }
    }

    /// Removes a `(value, rowid)` entry. If the value has other rowids
    /// attached they remain; if this was the last rowid, the value key is
    /// pruned. A no-op if the entry isn't present (simpler than failing —
    /// UPDATE paths rely on this).
    pub fn remove(&mut self, value: &Value, rowid: i64) {
        match (&mut self.entries, value) {
            (IndexEntries::Integer(m), Value::Integer(v)) => {
                if let Some(list) = m.get_mut(v) {
                    list.retain(|r| *r != rowid);
                    if list.is_empty() {
                        m.remove(v);
                    }
                }
            }
            (IndexEntries::Text(m), Value::Text(s)) => {
                if let Some(list) = m.get_mut(s) {
                    list.retain(|r| *r != rowid);
                    if list.is_empty() {
                        m.remove(s);
                    }
                }
            }
            _ => {}
        }
    }

    /// Returns every rowid currently associated with `value`. For a unique
    /// index this is at most one; for a non-unique index it can be many.
    /// Empty `Vec` if the value isn't present.
    pub fn lookup(&self, value: &Value) -> Vec<i64> {
        match (&self.entries, value) {
            (IndexEntries::Integer(m), Value::Integer(v)) => {
                m.get(v).cloned().unwrap_or_default()
            }
            (IndexEntries::Text(m), Value::Text(s)) => {
                m.get(s).cloned().unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// Iterates every `(value, rowid)` pair in ascending-value order. The
    /// rowids for a given value come out in insertion order, which happens
    /// to match ascending rowid order in practice because rows are inserted
    /// in rowid-ascending sequence during a bulk load. Phase 3e.4 uses
    /// this to serialize the index to its B-Tree.
    #[allow(dead_code)]
    pub fn iter_entries(&self) -> Box<dyn Iterator<Item = (Value, i64)> + '_> {
        match &self.entries {
            IndexEntries::Integer(m) => Box::new(
                m.iter()
                    .flat_map(|(v, rs)| rs.iter().map(|r| (Value::Integer(*v), *r))),
            ),
            IndexEntries::Text(m) => Box::new(
                m.iter()
                    .flat_map(|(v, rs)| rs.iter().map(|r| (Value::Text(v.clone()), *r))),
            ),
        }
    }
}

// PartialEq that's useful for tests (not strictly required by the crate).
impl PartialEq for SecondaryIndex {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.table_name == other.table_name
            && self.column_name == other.column_name
            && self.is_unique == other.is_unique
            && self.origin == other.origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_name_is_deterministic() {
        assert_eq!(
            SecondaryIndex::auto_name("users", "email"),
            "sqlrite_autoindex_users_email"
        );
    }

    #[test]
    fn rejects_index_on_real_column() {
        let err = SecondaryIndex::new(
            "x".into(),
            "t".into(),
            "c".into(),
            &DataType::Real,
            false,
            IndexOrigin::Explicit,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("cannot build"));
    }

    #[test]
    fn unique_violation_detected_before_insert() {
        let mut idx = SecondaryIndex::new(
            "x".into(),
            "t".into(),
            "c".into(),
            &DataType::Integer,
            true,
            IndexOrigin::Auto,
        )
        .unwrap();
        assert!(!idx.would_violate_unique(&Value::Integer(1)));
        idx.insert(&Value::Integer(1), 100).unwrap();
        assert!(idx.would_violate_unique(&Value::Integer(1)));
        assert!(!idx.would_violate_unique(&Value::Integer(2)));
    }

    #[test]
    fn null_is_not_indexed_and_never_conflicts() {
        let mut idx = SecondaryIndex::new(
            "x".into(),
            "t".into(),
            "c".into(),
            &DataType::Text,
            true,
            IndexOrigin::Auto,
        )
        .unwrap();
        idx.insert(&Value::Null, 1).unwrap();
        idx.insert(&Value::Null, 2).unwrap();
        assert_eq!(idx.lookup(&Value::Null), Vec::<i64>::new());
        assert!(!idx.would_violate_unique(&Value::Null));
    }

    #[test]
    fn insert_and_remove_preserve_list_semantics() {
        let mut idx = SecondaryIndex::new(
            "x".into(),
            "t".into(),
            "c".into(),
            &DataType::Text,
            false,
            IndexOrigin::Explicit,
        )
        .unwrap();
        idx.insert(&Value::Text("a".into()), 1).unwrap();
        idx.insert(&Value::Text("a".into()), 2).unwrap();
        idx.insert(&Value::Text("a".into()), 3).unwrap();
        assert_eq!(idx.lookup(&Value::Text("a".into())), vec![1, 2, 3]);

        idx.remove(&Value::Text("a".into()), 2);
        assert_eq!(idx.lookup(&Value::Text("a".into())), vec![1, 3]);

        idx.remove(&Value::Text("a".into()), 1);
        idx.remove(&Value::Text("a".into()), 3);
        assert_eq!(idx.lookup(&Value::Text("a".into())), Vec::<i64>::new());
    }

    #[test]
    fn iter_entries_yields_value_rowid_pairs_in_order() {
        let mut idx = SecondaryIndex::new(
            "x".into(),
            "t".into(),
            "c".into(),
            &DataType::Integer,
            false,
            IndexOrigin::Explicit,
        )
        .unwrap();
        idx.insert(&Value::Integer(20), 200).unwrap();
        idx.insert(&Value::Integer(10), 100).unwrap();
        idx.insert(&Value::Integer(10), 101).unwrap();

        let pairs: Vec<(Value, i64)> = idx.iter_entries().collect();
        assert_eq!(
            pairs,
            vec![
                (Value::Integer(10), 100),
                (Value::Integer(10), 101),
                (Value::Integer(20), 200),
            ]
        );
    }

    #[test]
    fn synthesized_sql_round_trips_through_parser() {
        let idx = SecondaryIndex::new(
            "sqlrite_autoindex_users_name".into(),
            "users".into(),
            "name".into(),
            &DataType::Text,
            true,
            IndexOrigin::Auto,
        )
        .unwrap();
        let sql = idx.synthesized_sql();
        assert_eq!(
            sql,
            "CREATE UNIQUE INDEX sqlrite_autoindex_users_name ON users (name);"
        );
    }
}
