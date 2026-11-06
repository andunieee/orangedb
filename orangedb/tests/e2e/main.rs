//! End-to-end tests. Every scenario declared with [`backends!`] runs twice:
//! once against `Db::in_memory()` and once against a redb file on disk.
//! Scenarios that are about the file itself (reopening, schema checks) live
//! in `persistence`.

// Some tests pass `&id` on purpose, to cover the `Borrow` arguments.
#![allow(clippy::needless_borrows_for_generic_args)]

/// Declares each scenario as a module holding a `memory` and a `file` test,
/// so a failure reads as e.g. `blog::cascade::file`.
///
/// ```ignore
/// backends!(schema_module => fn name(db) { ... });
/// ```
macro_rules! backends {
    ($schema:path => $( $(#[$meta:meta])* fn $name:ident($db:ident) $body:block )*) => {$(
        $(#[$meta])*
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            use $schema as schema;

            fn run($db: &schema::Db) $body

            #[test]
            fn memory() {
                run(&schema::Db::in_memory().unwrap());
            }

            #[test]
            fn file() {
                let dir = tempfile::tempdir().unwrap();
                run(&schema::Db::open(dir.path().join("db.redb")).unwrap());
            }
        }
    )*};
}

mod blog;
mod fingerprints;
mod keys;
mod migrations;
mod persistence;
mod references;
mod transactions;
mod unique;
