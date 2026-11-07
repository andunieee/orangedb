//! Thin, owned-value wrappers around redb tables.
//!
//! Every helper opens the table, does one thing and closes it again. redb
//! refuses to open a table twice in the same write transaction, so keeping the
//! borrow this short is what lets cascades recurse freely across tables.

use std::collections::BTreeSet;
use std::ops::{Bound, RangeBounds};

use redb::{
    Key, MultimapTableDefinition, MultimapTableHandle, ReadTransaction, ReadableMultimapTable, ReadableTable,
    ReadableTableMetadata, TableDefinition, TableHandle, Value, WriteTransaction,
};

use crate::Result;

/// A key or value type whose decoded form is the type itself, with no
/// borrowed lifetime. Ids, `String` and integers all qualify.
pub trait Owned: Value + 'static + for<'a> Value<SelfType<'a> = Self> {}
impl<T> Owned for T where T: Value + 'static + for<'a> Value<SelfType<'a> = T> {}

pub trait OwnedKey: Owned + Key {}
impl<T: Owned + Key> OwnedKey for T {}

/// Rows are stored as postcard bytes.
pub type RowTable<K> = TableDefinition<'static, K, &'static [u8]>;

/// Read access shared by read and write transactions.
pub trait Store {
    fn get<K: OwnedKey, V: Owned>(&self, def: TableDefinition<'static, K, V>, key: &K) -> Result<Option<V>>;

    fn get_row<K: OwnedKey>(&self, def: RowTable<K>, key: &K) -> Result<Option<Vec<u8>>>;

    fn rows<K: OwnedKey>(&self, def: RowTable<K>) -> Result<Vec<Vec<u8>>>;

    /// Up to `limit` rows with their ids, in id order, starting after the
    /// id `after`, or at the first row without one.
    fn rows_page<K: OwnedKey>(&self, def: RowTable<K>, after: Option<K>, limit: usize) -> Result<Vec<(K, Vec<u8>)>>;

    fn len<K: OwnedKey>(&self, def: RowTable<K>) -> Result<u64>;

    fn get_all<K: OwnedKey, V: OwnedKey>(&self, def: MultimapTableDefinition<'static, K, V>, key: &K)
    -> Result<Vec<V>>;

    /// The values of the keys in `range`, in key order.
    fn range<K: OwnedKey, V: Owned>(
        &self,
        def: TableDefinition<'static, K, V>,
        range: impl RangeBounds<K>,
    ) -> Result<Vec<V>>;

    /// The values of the keys in `range`, in key order and then value order.
    fn range_all<K: OwnedKey, V: OwnedKey>(
        &self,
        def: MultimapTableDefinition<'static, K, V>,
        range: impl RangeBounds<K>,
    ) -> Result<Vec<V>>;
}

fn get<K: OwnedKey, V: Owned>(t: &impl ReadableTable<K, V>, key: &K) -> Result<Option<V>> {
    Ok(t.get(key)?.map(|g| g.value()))
}

fn get_row<K: OwnedKey>(t: &impl ReadableTable<K, &'static [u8]>, key: &K) -> Result<Option<Vec<u8>>> {
    Ok(t.get(key)?.map(|g| g.value().to_vec()))
}

fn rows<K: OwnedKey>(t: &impl ReadableTable<K, &'static [u8]>) -> Result<Vec<Vec<u8>>> {
    t.iter()?.map(|entry| Ok(entry?.1.value().to_vec())).collect()
}

fn rows_page<K: OwnedKey>(
    t: &impl ReadableTable<K, &'static [u8]>,
    after: Option<K>,
    limit: usize,
) -> Result<Vec<(K, Vec<u8>)>> {
    let start = match after {
        Some(id) => Bound::Excluded(id),
        None => Bound::Unbounded,
    };
    t.range::<K>((start, Bound::Unbounded))?
        .take(limit)
        .map(|entry| {
            let (id, row) = entry?;
            Ok((id.value(), row.value().to_vec()))
        })
        .collect()
}

fn get_all<K: OwnedKey, V: OwnedKey>(t: &impl ReadableMultimapTable<K, V>, key: &K) -> Result<Vec<V>> {
    t.get(key)?.map(|v| Ok(v?.value())).collect()
}

fn range<K: OwnedKey, V: Owned>(t: &impl ReadableTable<K, V>, range: impl RangeBounds<K>) -> Result<Vec<V>> {
    t.range::<K>(range)?.map(|entry| Ok(entry?.1.value())).collect()
}

fn range_all<K: OwnedKey, V: OwnedKey>(
    t: &impl ReadableMultimapTable<K, V>,
    range: impl RangeBounds<K>,
) -> Result<Vec<V>> {
    let mut out = Vec::new();
    for entry in t.range::<K>(range)? {
        for value in entry?.1 {
            out.push(value?.value());
        }
    }
    Ok(out)
}

impl Store for ReadTransaction {
    fn get<K: OwnedKey, V: Owned>(&self, def: TableDefinition<'static, K, V>, key: &K) -> Result<Option<V>> {
        get(&self.open_table(def)?, key)
    }

    fn get_row<K: OwnedKey>(&self, def: RowTable<K>, key: &K) -> Result<Option<Vec<u8>>> {
        get_row(&self.open_table(def)?, key)
    }

    fn rows<K: OwnedKey>(&self, def: RowTable<K>) -> Result<Vec<Vec<u8>>> {
        rows(&self.open_table(def)?)
    }

    fn rows_page<K: OwnedKey>(&self, def: RowTable<K>, after: Option<K>, limit: usize) -> Result<Vec<(K, Vec<u8>)>> {
        rows_page(&self.open_table(def)?, after, limit)
    }

    fn len<K: OwnedKey>(&self, def: RowTable<K>) -> Result<u64> {
        Ok(self.open_table(def)?.len()?)
    }

    fn get_all<K: OwnedKey, V: OwnedKey>(
        &self,
        def: MultimapTableDefinition<'static, K, V>,
        key: &K,
    ) -> Result<Vec<V>> {
        get_all(&self.open_multimap_table(def)?, key)
    }

    fn range<K: OwnedKey, V: Owned>(
        &self,
        def: TableDefinition<'static, K, V>,
        range: impl RangeBounds<K>,
    ) -> Result<Vec<V>> {
        self::range(&self.open_table(def)?, range)
    }

    fn range_all<K: OwnedKey, V: OwnedKey>(
        &self,
        def: MultimapTableDefinition<'static, K, V>,
        range: impl RangeBounds<K>,
    ) -> Result<Vec<V>> {
        self::range_all(&self.open_multimap_table(def)?, range)
    }
}

impl Store for WriteTransaction {
    fn get<K: OwnedKey, V: Owned>(&self, def: TableDefinition<'static, K, V>, key: &K) -> Result<Option<V>> {
        get(&self.open_table(def)?, key)
    }

    fn get_row<K: OwnedKey>(&self, def: RowTable<K>, key: &K) -> Result<Option<Vec<u8>>> {
        get_row(&self.open_table(def)?, key)
    }

    fn rows<K: OwnedKey>(&self, def: RowTable<K>) -> Result<Vec<Vec<u8>>> {
        rows(&self.open_table(def)?)
    }

    fn rows_page<K: OwnedKey>(&self, def: RowTable<K>, after: Option<K>, limit: usize) -> Result<Vec<(K, Vec<u8>)>> {
        rows_page(&self.open_table(def)?, after, limit)
    }

    fn len<K: OwnedKey>(&self, def: RowTable<K>) -> Result<u64> {
        Ok(self.open_table(def)?.len()?)
    }

    fn get_all<K: OwnedKey, V: OwnedKey>(
        &self,
        def: MultimapTableDefinition<'static, K, V>,
        key: &K,
    ) -> Result<Vec<V>> {
        get_all(&self.open_multimap_table(def)?, key)
    }

    fn range<K: OwnedKey, V: Owned>(
        &self,
        def: TableDefinition<'static, K, V>,
        range: impl RangeBounds<K>,
    ) -> Result<Vec<V>> {
        self::range(&self.open_table(def)?, range)
    }

    fn range_all<K: OwnedKey, V: OwnedKey>(
        &self,
        def: MultimapTableDefinition<'static, K, V>,
        range: impl RangeBounds<K>,
    ) -> Result<Vec<V>> {
        self::range_all(&self.open_multimap_table(def)?, range)
    }
}

pub fn put<K: OwnedKey, V: Owned>(
    tx: &WriteTransaction,
    def: TableDefinition<'static, K, V>,
    key: &K,
    value: &V,
) -> Result<()> {
    tx.open_table(def)?.insert(key, value)?;
    Ok(())
}

pub fn remove<K: OwnedKey, V: Owned>(
    tx: &WriteTransaction,
    def: TableDefinition<'static, K, V>,
    key: &K,
) -> Result<()> {
    tx.open_table(def)?.remove(key)?;
    Ok(())
}

pub fn put_row<K: OwnedKey>(tx: &WriteTransaction, def: RowTable<K>, key: &K, row: &[u8]) -> Result<()> {
    tx.open_table(def)?.insert(key, row)?;
    Ok(())
}

pub fn remove_row<K: OwnedKey>(tx: &WriteTransaction, def: RowTable<K>, key: &K) -> Result<()> {
    tx.open_table(def)?.remove(key)?;
    Ok(())
}

pub fn link<K: OwnedKey, V: OwnedKey>(
    tx: &WriteTransaction,
    def: MultimapTableDefinition<'static, K, V>,
    key: &K,
    value: &V,
) -> Result<()> {
    tx.open_multimap_table(def)?.insert(key, value)?;
    Ok(())
}

pub fn unlink<K: OwnedKey, V: OwnedKey>(
    tx: &WriteTransaction,
    def: MultimapTableDefinition<'static, K, V>,
    key: &K,
    value: &V,
) -> Result<()> {
    tx.open_multimap_table(def)?.remove(key, value)?;
    Ok(())
}

/// Opening a table in a write transaction creates it. Read transactions fail
/// on tables that were never created, so `Db::open` touches every table once.
pub fn create_table<K: OwnedKey, V: Owned>(tx: &WriteTransaction, def: TableDefinition<'static, K, V>) -> Result<()> {
    tx.open_table(def)?;
    Ok(())
}

pub fn create_row_table<K: OwnedKey>(tx: &WriteTransaction, def: RowTable<K>) -> Result<()> {
    tx.open_table(def)?;
    Ok(())
}

/// The names of every table and multimap table.
pub fn table_names(tx: &WriteTransaction) -> Result<BTreeSet<String>> {
    let tables = tx.list_tables()?.map(|t| t.name().to_string());
    let multimaps = tx.list_multimap_tables()?.map(|t| t.name().to_string());
    Ok(tables.chain(multimaps).collect())
}

/// Rows read per page by [`for_each_row`].
const PAGE: usize = 256;

/// Calls `f` with every row's bytes, in id order, holding at most one page
/// of rows in memory. The table is closed while `f` runs, so `f` can read
/// any table, this one included, and write to any other.
pub fn for_each_row<K: OwnedKey + Clone>(
    tx: &impl Store,
    def: RowTable<K>,
    mut f: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let mut after = None;
    loop {
        let page = tx.rows_page(def, after.take(), PAGE)?;
        let full = page.len() == PAGE;
        for (id, row) in page {
            f(&row)?;
            after = Some(id);
        }
        if !full {
            return Ok(());
        }
    }
}

/// `values` without repeats, each where it first appears. A row whose index
/// yields several keys in a range is listed once.
pub fn first_seen<T: Ord + Clone>(values: Vec<T>) -> Vec<T> {
    let mut seen = BTreeSet::new();
    values.into_iter().filter(|v| seen.insert(v.clone())).collect()
}

pub fn create_multimap_table<K: OwnedKey, V: OwnedKey>(
    tx: &WriteTransaction,
    def: MultimapTableDefinition<'static, K, V>,
) -> Result<()> {
    tx.open_multimap_table(def)?;
    Ok(())
}
