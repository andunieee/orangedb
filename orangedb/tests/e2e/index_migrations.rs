//! Schema changes whose rows keep their layout and only gain or lose
//! indexes. A shop goes from no indexes, to a unique `sku` and a foreign key
//! from orders to items, and back to no indexes.
//!
//! A unique index isn't part of the fingerprint: `Item` keeps its layout and
//! opening the database builds or drops its index. A reference is, so each
//! step migrates `Order` by copying its rows. The writes rebuild its index
//! and the end of the migration checks every reference.

use std::path::Path;

use orangedb::{Entity, Error};

use crate::migrations::table_names;

pub mod shop {
    #[orangedb::schema]
    pub mod plain {
        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub sku: String,
        }

        /// `item` holds an item id, but nothing checks or indexes it.
        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            pub item: u64,
            pub quantity: u32,
        }
    }

    #[orangedb::schema]
    pub mod indexed {
        use super::plain;

        /// Items keep their layout, so only orders are copied.
        #[migrate]
        fn from_plain(old: &plain::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
            for order in old.orders().all()? {
                new.orders().insert(&Order {
                    id: OrderId(order.id.0),
                    item: ItemId(order.item),
                    quantity: order.quantity,
                })?;
            }
            Ok(())
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            #[unique]
            pub sku: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            #[references(Item, on_delete = cascade)]
            pub item: ItemId,
            pub quantity: u32,
        }
    }

    /// The same layout as `plain` again, reached from `indexed`.
    #[orangedb::schema]
    pub mod unindexed {
        use super::indexed;

        #[migrate]
        fn from_indexed(old: &indexed::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
            for order in old.orders().all()? {
                new.orders().insert(&Order {
                    id: OrderId(order.id.0),
                    item: order.item.0,
                    quantity: order.quantity,
                })?;
            }
            Ok(())
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub sku: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            pub item: u64,
            pub quantity: u32,
        }
    }
}

use shop::{indexed, plain, unindexed};

/// Items with the given skus, and one order of each of `orders` item ids.
fn seed(path: &Path, skus: &[&str], orders: &[u64]) {
    let db = plain::Db::open(path).unwrap();
    for sku in skus {
        db.items()
            .create(|id| plain::Item {
                id,
                sku: sku.to_string(),
            })
            .unwrap();
    }
    for &item in orders {
        db.orders().create(|id| plain::Order { id, item, quantity: 1 }).unwrap();
    }
}

/// The names of the index tables, with fingerprints taken out. The database
/// must be closed.
pub fn index_tables(path: &Path) -> Vec<String> {
    table_names(path)
        .into_iter()
        .filter(|n| !n.starts_with("__orangedb."))
        .map(|n| {
            // `@` and 16 hex digits after the entity, and after an index.
            let mut parts = n.split('@');
            let mut out = parts.next().unwrap().to_string();
            for part in parts {
                out.push_str(&part[16..]);
            }
            out
        })
        .filter(|n| n.contains('.'))
        .collect()
}

#[test]
fn a_migration_can_add_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path, &["lamp", "desk"], &[1, 1, 2]);
    assert!(index_tables(&path).is_empty());

    // A unique index leaves the layout as it was; a reference doesn't.
    assert_eq!(plain::Item::FINGERPRINT, indexed::Item::FINGERPRINT);
    assert_ne!(plain::Order::FINGERPRINT, indexed::Order::FINGERPRINT);

    drop(indexed::Db::open(&path).unwrap());
    assert_eq!(index_tables(&path), ["Item.sku.unique", "Order.item.fk"]);
    let db = indexed::Db::open(&path).unwrap();

    // The new indexes cover the migrated rows.
    let lamp = db.items().by_sku("lamp").unwrap().unwrap();
    assert_eq!(lamp.id, indexed::ItemId(1));
    assert_eq!(
        db.orders().ids_by_item(lamp.id).unwrap(),
        [indexed::OrderId(1), indexed::OrderId(2)]
    );
    assert_eq!(db.orders().by_item(indexed::ItemId(2)).unwrap().len(), 1);

    // And their constraints apply from now on.
    assert!(matches!(
        db.items().create(|id| indexed::Item { id, sku: "lamp".into() }),
        Err(Error::Unique { field: "sku", .. })
    ));
    assert!(matches!(
        db.orders().create(|id| indexed::Order {
            id,
            item: indexed::ItemId(99),
            quantity: 1
        }),
        Err(Error::ForeignKey { field: "item", .. })
    ));
    // Including on_delete, through the rebuilt foreign key index.
    db.items().delete(lamp.id).unwrap();
    assert_eq!(db.orders().len().unwrap(), 1);
    // Sequences carry over.
    assert_eq!(db.items().next_id().unwrap(), indexed::ItemId(3));
    assert_eq!(db.orders().next_id().unwrap(), indexed::OrderId(4));
}

#[test]
fn adding_a_unique_index_over_duplicates_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path, &["lamp", "desk", "lamp"], &[]);
    let before = table_names(&path);

    let err = indexed::Db::open(&path).err().unwrap();
    assert!(
        matches!(&err, Error::Unique { entity: "Item", field: "sku", value, .. } if value == "\"lamp\""),
        "{err}"
    );

    // Nothing changed: plain still opens it, duplicates and all.
    assert_eq!(table_names(&path), before);
    let db = plain::Db::open(&path).unwrap();
    assert_eq!(db.items().len().unwrap(), 3);
}

#[test]
fn adding_a_reference_over_dangling_values_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    // Order 2 points at an item that was never there.
    seed(&path, &["lamp"], &[1, 7]);
    let before = table_names(&path);

    // The migration itself writes fine, since references are only checked
    // at the end, and the check at the end is what fails.
    let err = indexed::Db::open(&path).err().unwrap();
    assert!(
        matches!(&err, Error::ForeignKey { entity: "Order", field: "item", target: "Item", id } if id == "ItemId(7)"),
        "{err}"
    );
    assert_eq!(table_names(&path), before);
    assert_eq!(plain::Db::open(&path).unwrap().orders().len().unwrap(), 2);
}

#[test]
fn a_migration_can_remove_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path, &["lamp", "desk"], &[1, 2]);
    indexed::Db::open(&path).unwrap();

    drop(unindexed::Db::open(&path).unwrap());
    assert!(index_tables(&path).is_empty(), "{:?}", table_names(&path));
    let db = unindexed::Db::open(&path).unwrap();
    assert_eq!(db.items().all().unwrap().len(), 2);
    assert_eq!(db.orders().all().unwrap()[1].item, 2);

    // No constraints any more.
    db.items()
        .create(|id| unindexed::Item { id, sku: "lamp".into() })
        .unwrap();
    db.orders()
        .create(|id| unindexed::Order {
            id,
            item: 99,
            quantity: 1,
        })
        .unwrap();
    drop(db);

    // `unindexed` has `plain`'s layout, so `plain` opens the result without
    // a migration, whichever schema wrote it.
    assert_eq!(unindexed::Item::FINGERPRINT, plain::Item::FINGERPRINT);
    assert_eq!(unindexed::Order::FINGERPRINT, plain::Order::FINGERPRINT);
    let db = plain::Db::open(&path).unwrap();
    assert_eq!(db.items().len().unwrap(), 3);
    assert_eq!(db.orders().len().unwrap(), 3);
}

/// More orders than the reference check reads at once, with the dangling
/// one last.
#[test]
fn the_reference_check_reads_every_page() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    let mut orders = vec![1; 600];
    orders.push(7);
    seed(&path, &["lamp"], &orders);

    let err = indexed::Db::open(&path).err().unwrap();
    assert!(
        matches!(&err, Error::ForeignKey { entity: "Order", field: "item", id, .. } if id == "ItemId(7)"),
        "{err}"
    );
    assert_eq!(plain::Db::open(&path).unwrap().orders().len().unwrap(), 601);
}
