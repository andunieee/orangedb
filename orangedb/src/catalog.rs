//! What a database records about its own layout, for tools that inspect a
//! file without the schema's Rust types.
//!
//! Opening a database records a [`Layout`] for every entity next to its
//! fingerprint: the tables its rows and indexes live in, its references and
//! indexes, and the source of the schema module that declared it.
//! Layouts of earlier fingerprints stay after a migration, so the catalog is
//! also the database's schema history. Indexes aren't part of a fingerprint,
//! so a layout lists the indexes of the schema that opened it last.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use redb::{ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, TableHandle, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::__private::{Fp, META};
use crate::migrate::table_prefix;
use crate::store::{RowTable, Store, put, put_row};
use crate::{Error, Result};

/// Each layout, keyed by its rows table's name (`Entity@fingerprint`).
pub(crate) const LAYOUTS: RowTable<String> = TableDefinition::new("__orangedb.layout");

/// Schema module sources, keyed by [`Layout::source`]. Entities declared by
/// the same module share one.
pub(crate) const SOURCES: TableDefinition<'static, u64, String> = TableDefinition::new("__orangedb.source");

/// Stored layouts start with this, so that the format can change later.
const FORMAT: u32 = 1;

/// How one entity is stored at one fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub entity: String,
    pub fingerprint: u64,
    pub id: Id,
    /// The fields a row stores, in the order postcard encodes them.
    pub fields: Vec<Field>,
    /// The table of rows: id to postcard bytes.
    pub rows: String,
    pub references: Vec<Reference>,
    pub indexes: Vec<Index>,
    /// The key of the schema module's source in [`Catalog::sources`].
    pub source: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Id {
    pub field: String,
    /// The id newtype, which is stored as the key it wraps.
    pub ty: String,
    pub kind: IdKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    /// The type as written, or `None` for a field with a `#[fingerprint]`
    /// label, which is stored by its own serializer.
    pub ty: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdKind {
    /// A `u64` from the entity's sequence.
    Auto,
    /// A caller-supplied key, with its type as written.
    Key(String),
}

/// A `#[references]` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub field: String,
    pub shape: Shape,
    /// The target entity. Its layout isn't recorded: schemas that share
    /// this layout can store the target at different fingerprints.
    pub target: String,
    pub on_delete: OnDelete,
    /// The multimap table from a target id to the ids that reference it.
    pub index: String,
}

/// A `#[unique]` or `#[index]` field, or an `#[index(...)]` on the entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// The name its lookups go by, `by_{name}`.
    pub name: String,
    pub unique: bool,
    pub on: Indexed,
    /// The key type as written.
    pub ty: String,
    /// Changes whenever what the index holds does, see [`Index::table`].
    pub fingerprint: u64,
    /// For a unique index, the table from a key to the id that holds it;
    /// otherwise the multimap table from a key to the ids that hold it. Its
    /// name ends in the index's fingerprint, so a changed index gets a new
    /// table.
    pub table: String,
}

/// What an index's keys come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Indexed {
    /// The values of a field: `T`, the `Some` of an `Option<T>` or each
    /// element of a `Vec<T>`.
    Field { field: String, shape: Shape },
    /// The keys an expression returns for each row, with the expression as
    /// written.
    Expr(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shape {
    One,
    Option,
    Vec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnDelete {
    Restrict,
    Cascade,
    SetNull,
}

impl fmt::Display for OnDelete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OnDelete::Restrict => "restrict",
            OnDelete::Cascade => "cascade",
            OnDelete::SetNull => "set_null",
        })
    }
}

impl fmt::Display for IdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdKind::Auto => f.write_str("auto"),
            IdKind::Key(ty) => f.write_str(ty),
        }
    }
}

/// Everything a database records about its layouts.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Every recorded layout, in entity and then fingerprint order.
    pub layouts: Vec<Layout>,
    /// The fingerprint each entity is stored at now.
    pub current: BTreeMap<String, u64>,
    /// The id the next auto-increment allocation of each entity returns.
    pub next_ids: BTreeMap<String, u64>,
    /// Schema module sources, by [`Layout::source`].
    pub sources: BTreeMap<u64, String>,
}

impl Catalog {
    /// Reads the catalog. A database orangedb never opened has an empty one.
    pub fn read(tx: &ReadTransaction) -> Result<Self> {
        let mut catalog = Catalog::default();
        if exists(tx, META.name())? {
            for entry in tx.open_table(META)?.iter()? {
                let (key, value) = entry?;
                let key = key.value();
                if let Some(entity) = key.strip_suffix(".fingerprint") {
                    catalog.current.insert(entity.to_string(), value.value());
                } else if let Some(entity) = key.strip_suffix(".next_id") {
                    catalog.next_ids.insert(entity.to_string(), value.value());
                }
            }
        }
        if exists(tx, LAYOUTS.name())? {
            for entry in tx.open_table(LAYOUTS)?.iter()? {
                catalog.layouts.push(decode(entry?.1.value())?);
            }
        }
        if exists(tx, SOURCES.name())? {
            for entry in tx.open_table(SOURCES)?.iter()? {
                let (key, value) = entry?;
                catalog.sources.insert(key.value(), value.value());
            }
        }
        Ok(catalog)
    }

    pub fn layout(&self, entity: &str, fingerprint: u64) -> Option<&Layout> {
        self.layouts
            .iter()
            .find(|l| l.entity == entity && l.fingerprint == fingerprint)
    }

    /// The layouts the database stores its entities at now.
    pub fn current_layouts(&self) -> impl Iterator<Item = &Layout> {
        self.layouts.iter().filter(|l| self.is_current(l))
    }

    pub fn is_current(&self, layout: &Layout) -> bool {
        self.current.get(&layout.entity) == Some(&layout.fingerprint)
    }

    pub fn source(&self, layout: &Layout) -> Option<&str> {
        self.sources.get(&layout.source).map(String::as_str)
    }
}

fn exists(tx: &ReadTransaction, name: &str) -> Result<bool> {
    Ok(tx.list_tables()?.any(|t| t.name() == name))
}

pub(crate) fn source_key(source: &str) -> u64 {
    Fp::new("source").str(source).finish()
}

fn encode(layout: &Layout) -> Result<Vec<u8>> {
    let codec = |source| Error::Codec {
        entity: "catalog",
        source,
    };
    let bytes = postcard::to_extend(&FORMAT, Vec::new()).map_err(codec)?;
    postcard::to_extend(layout, bytes).map_err(codec)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Layout> {
    let codec = |source| Error::Codec {
        entity: "catalog",
        source,
    };
    let (format, rest) = postcard::take_from_bytes::<u32>(bytes).map_err(codec)?;
    if format != FORMAT {
        return Err(Error::CatalogFormat { format });
    }
    postcard::from_bytes(rest).map_err(codec)
}

/// Records `layouts` and the `source` they came from, writing only what
/// changed so that reopening a database doesn't rewrite it.
///
/// A layout belongs to every schema that declares it, so it records the
/// source of whichever of them opened the database last. Sources that no
/// layout records any more are removed.
pub(crate) fn record(tx: &WriteTransaction, layouts: &[Layout], source: &str) -> Result<()> {
    let mut changed = false;
    for layout in layouts {
        let key = table_prefix(&layout.entity, layout.fingerprint);
        let bytes = encode(layout)?;
        if tx.get_row(LAYOUTS, &key)?.as_deref() != Some(bytes.as_slice()) {
            put_row(tx, LAYOUTS, &key, &bytes)?;
            changed = true;
        }
    }
    let Some(key) = layouts.first().map(|l| l.source) else {
        return Ok(());
    };
    if tx.get(SOURCES, &key)?.is_none() {
        put(tx, SOURCES, &key, &source.to_string())?;
    }
    if changed {
        let mut used = BTreeSet::new();
        for entry in tx.open_table(LAYOUTS)?.iter()? {
            used.insert(decode(entry?.1.value())?.source);
        }
        tx.open_table(SOURCES)?.retain(|key, _| used.contains(&key))?;
    }
    Ok(())
}

/// The schema source recorded for `entity` at `fingerprint`, if any.
pub fn stored_source(db: &impl ReadableDatabase, entity: &str, fingerprint: u64) -> Result<Option<String>> {
    let tx = db.begin_read()?;
    if !exists(&tx, LAYOUTS.name())? {
        return Ok(None);
    }
    let Some(bytes) = tx.get_row(LAYOUTS, &table_prefix(entity, fingerprint))? else {
        return Ok(None);
    };
    tx.get(SOURCES, &decode(&bytes)?.source)
}
