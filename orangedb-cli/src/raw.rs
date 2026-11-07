//! Reading redb tables without their Rust types.
//!
//! redb checks a table's key and value type names and widths when it is
//! opened. [`Raw`] reports whatever a thread-local holds, and opening a table
//! learns the stored ones from the errors that opening it with the wrong
//! ones returns. Since a table reads the widths again while it is in use,
//! each function here opens one table, reads what it needs and closes it.

use std::cell::RefCell;
use std::cmp::Ordering;

use orangedb::redb::{
    Key, MultimapTableDefinition, ReadOnlyMultimapTable, ReadOnlyTable, ReadTransaction, ReadableMultimapTable,
    ReadableTable, ReadableTableMetadata, TableDefinition, TableError, TypeName, Value,
};

/// A stored key or value type.
#[derive(Debug, Clone)]
pub struct Type {
    pub name: TypeName,
    pub width: Option<usize>,
}

impl Type {
    fn unknown() -> Self {
        Type {
            name: TypeName::new("orangedb-cli::unknown"),
            width: None,
        }
    }
}

thread_local! {
    static TYPES: RefCell<[Type; 2]> = RefCell::new([Type::unknown(), Type::unknown()]);
}

/// Bytes, of the type in slot `SLOT` of [`TYPES`]: 0 for keys, 1 for values.
#[derive(Debug)]
pub struct Raw<const SLOT: usize>;

impl<const SLOT: usize> Value for Raw<SLOT> {
    type SelfType<'a>
        = &'a [u8]
    where
        Self: 'a;
    type AsBytes<'a>
        = &'a [u8]
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        TYPES.with_borrow(|t| t[SLOT].width)
    }

    fn from_bytes<'a>(data: &'a [u8]) -> &'a [u8]
    where
        Self: 'a,
    {
        data
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a &'b [u8]) -> &'a [u8]
    where
        Self: 'b,
    {
        value
    }

    fn type_name() -> TypeName {
        TYPES.with_borrow(|t| t[SLOT].name.clone())
    }
}

/// Byte order, which only matters for lookups; reading a whole table in
/// stored order never compares keys.
impl<const SLOT: usize> Key for Raw<SLOT> {
    fn compare(a: &[u8], b: &[u8]) -> Ordering {
        a.cmp(b)
    }
}

type K = Raw<0>;
type V = Raw<1>;

/// Opens a table with `open`, retrying with the stored types each failure
/// reveals.
fn probe<T>(open: impl Fn() -> Result<T, TableError>) -> Result<(T, [Type; 2]), TableError> {
    TYPES.set([Type::unknown(), Type::unknown()]);
    // Names, then the key's width, then the value's.
    let mut tries = 3;
    loop {
        let error = match open() {
            Ok(table) => return Ok((table, TYPES.with_borrow(Clone::clone))),
            Err(e) if tries == 0 => return Err(e),
            Err(e) => e,
        };
        tries -= 1;
        match &error {
            TableError::TableTypeMismatch { key, value, .. } => TYPES.with_borrow_mut(|t| {
                t[0].name = key.clone();
                t[1].name = value.clone();
            }),
            // A type has one width, so set it on every slot of that type.
            TableError::TypeDefinitionChanged { name, width, .. } => TYPES.with_borrow_mut(|t| {
                for slot in t.iter_mut().filter(|s| s.name == *name) {
                    slot.width = *width;
                }
            }),
            _ => return Err(error),
        }
    }
}

fn open_table(tx: &ReadTransaction, name: &str) -> Result<(ReadOnlyTable<K, V>, [Type; 2]), TableError> {
    probe(|| tx.open_table(TableDefinition::<K, V>::new(name)))
}

fn open_multimap(tx: &ReadTransaction, name: &str) -> Result<(ReadOnlyMultimapTable<K, V>, [Type; 2]), TableError> {
    probe(|| tx.open_multimap_table(MultimapTableDefinition::<K, V>::new(name)))
}

/// A table's types and number of entries; for a multimap, of key-value
/// pairs.
pub struct Stat {
    pub key: Type,
    pub value: Type,
    pub len: u64,
}

pub fn stat(tx: &ReadTransaction, name: &str, multimap: bool) -> Result<Stat, TableError> {
    let (len, [key, value]) = if multimap {
        let (table, types) = open_multimap(tx, name)?;
        (table.len()?, types)
    } else {
        let (table, types) = open_table(tx, name)?;
        (table.len()?, types)
    };
    Ok(Stat { key, value, len })
}

/// Up to `limit` entries of a table, in key order, with each key's values.
pub struct Entries {
    pub stat: Stat,
    pub entries: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
}

pub fn entries(tx: &ReadTransaction, name: &str, multimap: bool, limit: usize) -> Result<Entries, TableError> {
    let mut entries = Vec::new();
    let (len, [key, value]) = if multimap {
        let (table, types) = open_multimap(tx, name)?;
        for entry in table.iter()?.take(limit) {
            let (k, values) = entry?;
            let values = values
                .map(|v| Ok(v?.value().to_vec()))
                .collect::<Result<_, TableError>>()?;
            entries.push((k.value().to_vec(), values));
        }
        (table.len()?, types)
    } else {
        let (table, types) = open_table(tx, name)?;
        for entry in table.iter()?.take(limit) {
            let (k, v) = entry?;
            entries.push((k.value().to_vec(), vec![v.value().to_vec()]));
        }
        (table.len()?, types)
    };
    Ok(Entries {
        stat: Stat { key, value, len },
        entries,
    })
}
