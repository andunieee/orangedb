//! Typed entities with foreign keys and unique indexes on top of [redb].
//!
//! Declare a schema as a module of structs; the [`schema`] macro generates a
//! `Db` type with a table handle per entity:
//!
//! ```ignore
//! #[orangedb::schema]
//! pub mod blog {
//!     #[entity]
//!     pub struct User {
//!         #[id(auto)]
//!         pub id: UserId,
//!         #[unique]
//!         pub email: String,
//!     }
//!
//!     #[entity]
//!     pub struct Post {
//!         #[id(auto)]
//!         pub id: PostId,
//!         #[references(User, on_delete = cascade)]
//!         pub author: UserId,
//!     }
//! }
//!
//! let db = blog::Db::open("blog.redb")?;
//! let alice = db.users().create(|id| User { id, email: "a@example.com".into() })?;
//! db.write(|tx| {
//!     tx.posts().create(|id| Post { id, author: alice.id })?;
//!     tx.users().delete(alice.id) // cascades to her posts
//! })?;
//! ```
//!
//! See the `schema` macro documentation for the full attribute reference.

pub mod catalog;
mod error;
mod fingerprint;
mod migrate;
mod store;

use std::collections::BTreeSet;
use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use error::{Error, Result};
pub use fingerprint::Fingerprint;
pub use orangedb_macros::{embed, schema};
pub use redb;
pub use store::{Owned, OwnedKey};

/// Implemented by every `#[entity]` struct.
pub trait Entity: Serialize + DeserializeOwned + 'static {
    type Id: OwnedKey + Clone + Ord + Debug;

    /// The struct name, which also names its redb tables.
    const NAME: &'static str;
    /// A hash of everything that decides how rows are stored and indexed:
    /// field names, order and types (through their own [`Fingerprint`]s),
    /// serde attributes that change the encoding, the id kind and
    /// references. It acts as the entity's version: a database that stores
    /// a different one has to be migrated. Unique and plain indexes aren't
    /// part of it: opening a database builds the ones it lacks.
    const FINGERPRINT: u64;

    fn id(&self) -> &Self::Id;
}

/// Support code for the generated items. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::catalog::stored_source;
    pub use crate::fingerprint::Fp;
    pub use crate::migrate::*;
    pub use crate::store::*;
    pub use orangedb_macros::SchemaAttributes;
    pub use serde;

    /// Keeps [`Fingerprint`] to the types orangedb and its macros declare.
    pub trait Sealed {}

    use super::*;

    pub const META: redb::TableDefinition<'static, String, u64> = redb::TableDefinition::new("__orangedb.meta");

    pub fn encode<E: Entity>(row: &E) -> Result<Vec<u8>> {
        postcard::to_allocvec(row).map_err(|source| Error::Codec {
            entity: E::NAME,
            source,
        })
    }

    pub fn decode<E: Entity>(bytes: &[u8]) -> Result<E> {
        postcard::from_bytes(bytes).map_err(|source| Error::Codec {
            entity: E::NAME,
            source,
        })
    }

    /// Compiles only for types an index can use as its key.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` can't be an index key",
        label = "not a redb key type",
        note = "index keys are redb key types: integers, `bool`, `char`, `String`, ids, \
                and `Option`s, tuples and arrays of those"
    )]
    pub trait IndexKey: OwnedKey {}
    impl<K: OwnedKey> IndexKey for K {}

    pub fn index_key<K: IndexKey>() {}

    /// What an `#[index(name: K = expr)]` expression returns: one key, an
    /// optional one, or any number of them.
    #[diagnostic::on_unimplemented(
        message = "an index of `{K}` keys can't use a `{Self}`",
        label = "the index expression returns this",
        note = "an index expression returns its key type `{K}`, an `Option<{K}>` or a `Vec<{K}>`"
    )]
    pub trait IndexKeys<K> {
        fn into_keys(self) -> Vec<K>;
    }

    impl<K: OwnedKey> IndexKeys<K> for K {
        fn into_keys(self) -> Vec<K> {
            vec![self]
        }
    }

    impl<K: OwnedKey> IndexKeys<K> for Option<K> {
        fn into_keys(self) -> Vec<K> {
            self.into_iter().collect()
        }
    }

    impl<K: OwnedKey> IndexKeys<K> for Vec<K> {
        fn into_keys(self) -> Vec<K> {
            self
        }
    }

    /// The keys `index` gives `row`. Taking the expression as an argument
    /// with this bound is what lets a closure leave its parameter's type out.
    pub fn index_keys<E, K, R: IndexKeys<K>>(index: impl FnOnce(&E) -> R, row: &E) -> Vec<K> {
        index(row).into_keys()
    }

    pub fn fmt_id(id: &impl Debug) -> String {
        format!("{id:?}")
    }

    fn seq_key<E: Entity>() -> String {
        crate::migrate::seq_key(E::NAME)
    }

    /// The id the next auto-increment allocation would return. Ids start at 1.
    pub fn peek_seq<E: Entity>(tx: &redb::WriteTransaction) -> Result<u64> {
        Ok(tx.get(META, &seq_key::<E>())?.unwrap_or(1))
    }

    /// Allocates the next auto-increment id.
    pub fn next_seq<E: Entity>(tx: &redb::WriteTransaction) -> Result<u64> {
        let next = peek_seq::<E>(tx)?;
        put(tx, META, &seq_key::<E>(), &after::<E>(next)?)?;
        Ok(next)
    }

    /// Keeps the sequence ahead of ids that were inserted explicitly.
    pub fn observe_seq<E: Entity>(tx: &redb::WriteTransaction, used: u64) -> Result<()> {
        let key = seq_key::<E>();
        if tx.get(META, &key)?.unwrap_or(1) <= used {
            put(tx, META, &key, &after::<E>(used)?)?;
        }
        Ok(())
    }

    /// The sequence value after `id`. `u64::MAX` has none, so it can't be
    /// used as an auto-increment id: the sequence could never move past it.
    fn after<E: Entity>(id: u64) -> Result<u64> {
        id.checked_add(1).ok_or(Error::IdsExhausted { entity: E::NAME })
    }

    /// A schema-wide id: one variant per entity. Generated by the macro.
    pub trait AnyId: Ord + Clone + Debug {
        fn entity(&self) -> &'static str;
    }

    /// The set of rows a delete touches, computed before anything is written
    /// so that a `restrict` anywhere in the cascade fails the whole delete
    /// cleanly.
    pub struct DeletePlan<A> {
        pending: Vec<A>,
        delete: BTreeSet<A>,
        order: Vec<A>,
        restrict: Vec<(A, A, &'static str)>,
        detach: Vec<(A, &'static str, A)>,
    }

    pub struct ResolvedPlan<A> {
        /// `(child, field, parent)`: remove `parent` from `child.field`.
        pub detach: Vec<(A, &'static str, A)>,
        pub delete: Vec<A>,
    }

    impl<A: AnyId> DeletePlan<A> {
        pub fn new(root: A) -> Self {
            Self {
                pending: vec![root],
                delete: BTreeSet::new(),
                order: Vec::new(),
                restrict: Vec::new(),
                detach: Vec::new(),
            }
        }

        /// The next row to delete whose inbound references have not been
        /// visited yet.
        pub fn pop(&mut self) -> Option<A> {
            while let Some(id) = self.pending.pop() {
                if self.delete.insert(id.clone()) {
                    self.order.push(id.clone());
                    return Some(id);
                }
            }
            None
        }

        pub fn cascade(&mut self, child: A) {
            self.pending.push(child);
        }

        pub fn restrict(&mut self, parent: A, child: A, referenced_by: &'static str) {
            self.restrict.push((parent, child, referenced_by));
        }

        pub fn detach(&mut self, child: A, field: &'static str, parent: A) {
            self.detach.push((child, field, parent));
        }

        /// A restricting reference only blocks the delete if the referencing
        /// row survives it; rows that are deleted anyway need no detaching.
        pub fn resolve(self) -> Result<ResolvedPlan<A>> {
            if let Some((parent, child, referenced_by)) = self
                .restrict
                .into_iter()
                .find(|(_, child, _)| !self.delete.contains(child))
            {
                return Err(Error::Restricted {
                    entity: parent.entity(),
                    id: format!("{parent:?}"),
                    referenced_by,
                    child: format!("{child:?}"),
                });
            }
            let detach = self
                .detach
                .into_iter()
                .filter(|(child, _, _)| !self.delete.contains(child))
                .collect();
            Ok(ResolvedPlan {
                detach,
                delete: self.order,
            })
        }
    }
}
