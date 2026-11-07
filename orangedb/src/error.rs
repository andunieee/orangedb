/// Errors returned by orangedb operations.
///
/// Every constraint error is raised before the operation writes anything, so a
/// failed call leaves the transaction exactly as it was.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage error: {0}")]
    Storage(#[from] redb::Error),

    #[error("failed to encode or decode a {entity} row: {source}")]
    Codec {
        entity: &'static str,
        #[source]
        source: postcard::Error,
    },

    #[error("{entity} {id} does not exist")]
    NotFound { entity: &'static str, id: String },

    #[error("{entity} {id} already exists")]
    AlreadyExists { entity: &'static str, id: String },

    #[error("{entity}.{field} references {target} {id}, which does not exist")]
    ForeignKey {
        entity: &'static str,
        field: &'static str,
        target: &'static str,
        id: String,
    },

    #[error("{entity}.{field} must be unique, but {value} is already used by {entity} {owner}")]
    Unique {
        entity: &'static str,
        field: &'static str,
        value: String,
        owner: String,
    },

    #[error("cannot delete {entity} {id}: it is still referenced by {referenced_by} {child}")]
    Restricted {
        entity: &'static str,
        id: String,
        referenced_by: &'static str,
        child: String,
    },

    #[error(
        "{entity} schema mismatch: the database stores fingerprint {stored:016x}, the code declares \
         {declared:016x}, and no #[migrate] function leads from one to the other"
    )]
    SchemaMismatch {
        entity: &'static str,
        stored: u64,
        declared: u64,
    },

    #[error("the database's catalog uses format {format}, which this version of orangedb can't read")]
    CatalogFormat { format: u32 },

    /// Returned by a `#[migrate]` function that gives up, see
    /// [`Error::migration`]. The database stays at the old schema.
    #[error("migration failed: {0}")]
    Migration(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    /// Fails a migration with your own error or message.
    pub fn migration(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Error::Migration(error.into())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

macro_rules! storage_from {
    ($($ty:ty),* $(,)?) => {$(
        impl From<$ty> for Error {
            fn from(e: $ty) -> Self {
                Error::Storage(e.into())
            }
        }
    )*};
}

storage_from!(
    redb::StorageError,
    redb::TableError,
    redb::TransactionError,
    redb::CommitError,
    redb::DatabaseError,
);
