//! Thin, owned-value wrappers around redb tables.
//!
//! Every helper opens the table, does one thing and closes it again. redb
//! refuses to open a table twice in the same write transaction, so keeping the
//! borrow this short is what lets cascades recurse freely across tables.

use redb::{
    Key, MultimapTableDefinition, ReadTransaction, ReadableMultimapTable, ReadableTable, ReadableTableMetadata,
    TableDefinition, Value, WriteTransaction,
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

    fn len<K: OwnedKey>(&self, def: RowTable<K>) -> Result<u64>;

    fn get_all<K: OwnedKey, V: OwnedKey>(&self, def: MultimapTableDefinition<'static, K, V>, key: &K)
    -> Result<Vec<V>>;
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

fn get_all<K: OwnedKey, V: OwnedKey>(t: &impl ReadableMultimapTable<K, V>, key: &K) -> Result<Vec<V>> {
    t.get(key)?.map(|v| Ok(v?.value())).collect()
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

pub fn create_multimap_table<K: OwnedKey, V: OwnedKey>(
    tx: &WriteTransaction,
    def: MultimapTableDefinition<'static, K, V>,
) -> Result<()> {
    tx.open_multimap_table(def)?;
    Ok(())
}
