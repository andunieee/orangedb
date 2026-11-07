//! What survives closing a file database, and what reopening it checks.
//! These only make sense on disk, so they don't go through `backends!`.

use std::path::Path;

use orangedb::Error;
use orangedb::redb::{ReadableDatabase, TableDefinition};

use crate::blog::schema::{self as blog, Post, Reply, User, UserId};

fn alice() -> User {
    User {
        id: UserId(1),
        email: "alice@example.com".into(),
        name: "Alice".into(),
    }
}

#[test]
fn everything_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blog.redb");

    let (alice, bob, post, reply) = {
        let db = blog::Db::open(&path).unwrap();
        let alice = db.users().create(|id| User { id, ..alice() }).unwrap();
        let bob = db
            .users()
            .create(|id| User {
                id,
                email: "bob@example.com".into(),
                name: "Bob".into(),
            })
            .unwrap();
        let post = db
            .posts()
            .create(|id| Post {
                id,
                author: alice.id,
                title: "persisted".into(),
                liked_by: vec![bob.id],
            })
            .unwrap();
        let reply = db
            .replies()
            .create(|id| Reply {
                id,
                post: post.id,
                author: bob.id,
                parent: None,
                body: "still here?".into(),
            })
            .unwrap();
        db.users().next_id().unwrap(); // reserves 3
        (alice, bob, post, reply)
    };

    let db = blog::Db::open(&path).unwrap();
    // Rows.
    assert_eq!(db.users().all().unwrap(), vec![alice.clone(), bob.clone()]);
    assert_eq!(db.posts().get(post.id).unwrap(), Some(post.clone()));
    assert_eq!(db.replies().all().unwrap(), vec![reply.clone()]);
    // Indexes.
    assert_eq!(db.users().by_email("bob@example.com").unwrap(), Some(bob.clone()));
    assert_eq!(db.posts().by_author(alice.id).unwrap(), vec![post.clone()]);
    assert_eq!(db.posts().by_liked_by(bob.id).unwrap(), vec![post.clone()]);
    assert_eq!(db.replies().by_post(post.id).unwrap(), vec![reply.clone()]);
    // Constraints, using those indexes.
    assert!(matches!(
        db.users().create(|id| User { id, ..alice.clone() }),
        Err(Error::Unique { .. })
    ));
    assert!(matches!(db.users().delete(bob.id), Err(Error::Restricted { .. })));
    // Sequences.
    assert_eq!(db.users().next_id().unwrap(), UserId(4));
    assert_eq!(db.replies().next_id().unwrap().0, 2);

    // A cascade that runs after the reopen.
    db.users().delete(alice.id).unwrap();
    drop(db);
    let db = blog::Db::open(&path).unwrap();
    assert_eq!(db.users().all().unwrap(), vec![bob]);
    assert!(db.posts().is_empty().unwrap());
    assert!(db.replies().is_empty().unwrap());
}

#[test]
fn schema_source_is_recorded_with_each_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blog.redb");

    let db = blog::Db::open(&path).unwrap();
    let source = db.stored_source::<User>().unwrap().unwrap();
    assert!(source.contains("pub mod schema"), "{source}");
    // The snapshot is the module as written, doc comments included.
    assert!(source.contains("/// Deleting a user deletes their posts."), "{source}");
    assert!(source.contains("#[unique]"), "{source}");
    // Every entity of the module records the same source at its fingerprint.
    assert_eq!(db.stored_source::<Post>().unwrap().as_deref(), Some(source.as_str()));
    assert_eq!(db.stored_source::<Reply>().unwrap().as_deref(), Some(source.as_str()));
    drop(db);

    let db = blog::Db::open(&path).unwrap();
    assert_eq!(db.stored_source::<User>().unwrap().as_deref(), Some(source.as_str()));
}

#[test]
fn schema_source_of_a_macro_generated_module_is_expanded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");

    let db = shop::price_type::Db::open(&path).unwrap();
    let source = db.stored_source::<shop::price_type::Item>().unwrap().unwrap();
    assert!(source.contains("pub struct Item"), "{source}");
    assert!(source.contains("price : u64"), "{source}");
    // Not the `$item` template that `item_variant!` expanded from.
    assert!(!source.contains('$'), "{source}");
}

#[test]
fn the_catalog_describes_every_table() {
    use orangedb::Entity;
    use orangedb::catalog::{IdKind, Indexed, OnDelete, Shape};
    use orangedb::redb::{MultimapTableHandle, TableHandle};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blog.redb");
    let db = blog::Db::open(&path).unwrap();
    db.users().create(|id| User { id, ..alice() }).unwrap();

    let catalog = db.stored_catalog().unwrap();
    let names: Vec<&str> = catalog.layouts.iter().map(|l| l.entity.as_str()).collect();
    assert_eq!(names, ["Post", "Reply", "User"]);
    assert_eq!(catalog.current["User"], User::FINGERPRINT);
    assert_eq!(catalog.next_ids["User"], 2);
    assert!(catalog.layouts.iter().all(|l| catalog.is_current(l)));

    let user = catalog.layout("User", User::FINGERPRINT).unwrap();
    assert_eq!(user.rows, format!("User@{:016x}", User::FINGERPRINT));
    assert_eq!(user.id.field, "id");
    assert_eq!(user.id.ty, "UserId");
    assert_eq!(user.id.kind, IdKind::Auto);
    let fields: Vec<(&str, Option<&str>)> = user.fields.iter().map(|f| (f.name.as_str(), f.ty.as_deref())).collect();
    assert_eq!(
        fields,
        [
            ("id", Some("UserId")),
            ("email", Some("String")),
            ("name", Some("String"))
        ]
    );
    assert!(user.references.is_empty());
    assert_eq!(user.indexes.len(), 1);
    let email = &user.indexes[0];
    assert_eq!(email.name, "email");
    assert!(email.unique);
    assert_eq!(
        email.on,
        Indexed::Field {
            field: "email".into(),
            shape: Shape::One
        }
    );
    assert_eq!(email.ty, "String");
    assert_eq!(
        email.table,
        format!("{}.email@{:016x}.unique", user.rows, email.fingerprint)
    );

    let post = catalog.layout("Post", Post::FINGERPRINT).unwrap();
    assert_eq!(post.id.kind, IdKind::Auto);
    let liked_by = &post.references[1];
    assert_eq!(liked_by.field, "liked_by");
    assert_eq!(liked_by.shape, Shape::Vec);
    assert_eq!(liked_by.target, "User");
    assert_eq!(liked_by.on_delete, OnDelete::SetNull);
    assert_eq!(liked_by.index, format!("{}.liked_by.fk", post.rows));

    // Every table the catalog names exists.
    drop(db);
    let redb = orangedb::redb::Database::open(&path).unwrap();
    let tx = redb.begin_read().unwrap();
    let tables: Vec<String> = tx.list_tables().unwrap().map(|t| t.name().to_string()).collect();
    let multimaps: Vec<String> = tx
        .list_multimap_tables()
        .unwrap()
        .map(|t| t.name().to_string())
        .collect();
    for layout in &catalog.layouts {
        assert!(tables.contains(&layout.rows), "{}", layout.rows);
        for r in &layout.references {
            assert!(multimaps.contains(&r.index), "{}", r.index);
        }
        for i in &layout.indexes {
            let kind = if i.unique { &tables } else { &multimaps };
            assert!(kind.contains(&i.table), "{}", i.table);
        }
        // The module's source is stored once, for all of its entities.
        assert!(catalog.source(layout).unwrap().contains("pub mod schema"));
    }
    assert_eq!(catalog.sources.len(), 1);
}

#[test]
fn rolled_back_writes_are_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blog.redb");
    {
        let db = blog::Db::open(&path).unwrap();
        let _ = db.write(|tx| {
            tx.users().insert(&alice())?;
            Err::<(), _>(Error::NotFound {
                entity: "User",
                id: "abort".into(),
            })
        });
    }
    let db = blog::Db::open(&path).unwrap();
    assert!(db.users().is_empty().unwrap());
    assert_eq!(db.users().next_id().unwrap(), UserId(1));
}

#[test]
fn wrapping_an_existing_redb_database() {
    const NOTES: TableDefinition<&str, &str> = TableDefinition::new("notes");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.redb");

    // A database with a table of its own...
    let raw = orangedb::redb::Database::create(&path).unwrap();
    let tx = raw.begin_write().unwrap();
    tx.open_table(NOTES).unwrap().insert("hello", "world").unwrap();
    tx.commit().unwrap();

    // ...can host a schema next to it.
    let db = blog::Db::from_redb(raw).unwrap();
    db.users().insert(&alice()).unwrap();
    drop(db);

    let raw = orangedb::redb::Database::create(&path).unwrap();
    let tx = raw.begin_read().unwrap();
    assert_eq!(
        tx.open_table(NOTES).unwrap().get("hello").unwrap().unwrap().value(),
        "world"
    );
    drop(tx);
    let db = blog::Db::from_redb(raw).unwrap();
    assert_eq!(db.users().all().unwrap(), vec![alice()]);

    // from_redb works with any backend.
    let backend = orangedb::redb::backends::InMemoryBackend::new();
    let raw = orangedb::redb::Database::builder()
        .create_with_backend(backend)
        .unwrap();
    let db = blog::Db::from_redb(raw).unwrap();
    assert!(db.users().is_empty().unwrap());
}

#[test]
fn opening_garbage_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-a-db");
    std::fs::write(&path, vec![0xab; 8192]).unwrap();
    let err = blog::Db::open(&path).err().unwrap();
    assert!(matches!(err, Error::Storage(_)), "{err}");
}

/// The baseline schema, and variations on it. Each module stands for the
/// same program at a later point in its life.
mod shop {
    #[orangedb::schema]
    pub mod v1 {
        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub name: String,
            pub price: u32,
        }

        #[entity]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            #[references(Item, on_delete = restrict)]
            pub item: ItemId,
        }
    }

    /// Cosmetic changes: docs, derives, accessor and on_delete.
    #[orangedb::schema]
    pub mod cosmetic {
        /// Now with docs.
        #[entity(accessor = catalog)]
        #[derive(Debug, Clone)]
        pub struct Item {
            /// The id.
            #[id(auto)]
            pub id: ItemId,
            pub name: String,
            pub price: u32,
        }

        #[entity]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            #[references(Item, on_delete = cascade)]
            pub item: ItemId,
        }
    }

    /// A new entity next to the existing ones.
    #[orangedb::schema]
    pub mod extended {
        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub name: String,
            pub price: u32,
        }

        #[entity]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            #[references(Item, on_delete = restrict)]
            pub item: ItemId,
        }

        #[entity]
        pub struct Review {
            #[id(auto)]
            pub id: ReviewId,
            #[references(Item, on_delete = cascade)]
            pub item: ItemId,
        }
    }

    /// Only `Item` exists here. It can't ignore the stored `Order`, which
    /// references it: deleting an item would skip the order's `restrict`.
    #[orangedb::schema]
    pub mod subset {
        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub name: String,
            pub price: u32,
        }
    }

    /// `Order` dropped with a migration, which is how a schema stops
    /// declaring an entity that references one it keeps.
    #[orangedb::schema]
    pub mod dropped {
        use super::v1;

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub name: String,
            pub price: u32,
        }

        #[migrate]
        fn drop_orders(_old: &v1::ReadTx, _new: &WriteTx) -> orangedb::Result<()> {
            // Item's layout didn't change, so its rows stay where they are.
            Ok(())
        }
    }

    /// `Item`'s id field is renamed. `Order` doesn't change, so it keeps its
    /// index of items, which is keyed by `ItemId`.
    #[orangedb::schema]
    pub mod renamed_id {
        use super::v1;

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub key: ItemId,
            pub name: String,
            pub price: u32,
        }

        #[entity]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            #[references(Item, on_delete = restrict)]
            pub item: ItemId,
        }

        #[migrate]
        fn rename_id(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
            for item in old.items().all()? {
                new.items().insert(&Item {
                    key: ItemId(item.id.0),
                    name: item.name,
                    price: item.price,
                })?;
            }
            Ok(())
        }
    }

    macro_rules! item_variant {
        ($( $(#[$doc:meta])* $module:ident { $($item:tt)* } )*) => {$(
            $(#[$doc])*
            #[orangedb::schema]
            pub mod $module {
                $($item)*

                #[entity]
                pub struct Order {
                    #[id(auto)]
                    pub id: OrderId,
                    #[references(Item, on_delete = restrict)]
                    pub item: ItemId,
                }
            }
        )*};
    }

    item_variant! {
        /// `price` changes type.
        price_type {
            #[entity]
            pub struct Item {
                #[id(auto)]
                pub id: ItemId,
                pub name: String,
                pub price: u64,
            }
        }
        /// `name` is renamed.
        renamed {
            #[entity]
            pub struct Item {
                #[id(auto)]
                pub id: ItemId,
                pub title: String,
                pub price: u32,
            }
        }
        /// Fields are reordered, which changes the encoding.
        reordered {
            #[entity]
            pub struct Item {
                #[id(auto)]
                pub id: ItemId,
                pub price: u32,
                pub name: String,
            }
        }
        /// The id switches from auto to a natural key.
        key_id {
            #[entity]
            pub struct Item {
                #[id(String)]
                pub id: ItemId,
                pub name: String,
                pub price: u32,
            }
        }
    }
}

fn seed(path: &Path) {
    let db = shop::v1::Db::open(path).unwrap();
    let item = db
        .items()
        .create(|id| shop::v1::Item {
            id,
            name: "lamp".into(),
            price: 30,
        })
        .unwrap();
    db.orders().create(|id| shop::v1::Order { id, item: item.id }).unwrap();
}

#[test]
fn schema_changes_that_keep_the_layout_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path);

    {
        let db = shop::cosmetic::Db::open(&path).unwrap();
        assert_eq!(db.catalog().all().unwrap()[0].name, "lamp");
        // on_delete is behaviour, not layout: the new rule applies to old rows.
        db.catalog().delete(shop::cosmetic::ItemId(1)).unwrap();
        assert!(db.orders().is_empty().unwrap());
    }
    seed(&path);
    {
        let db = shop::extended::Db::open(&path).unwrap();
        assert!(db.reviews().is_empty().unwrap());
        let item = db.items().all().unwrap()[0].id;
        db.reviews().create(|id| shop::extended::Review { id, item }).unwrap();
    }
    // A schema that doesn't declare an entity referencing one of its own
    // would delete items without applying that entity's on_delete rule.
    let err = shop::subset::Db::open(&path).err().unwrap();
    assert!(
        matches!(&err, Error::Undeclared { entity, field, target: "Item" } if entity == "Order" && field == "item"),
        "{err}"
    );
    let err = shop::v1::Db::open(&path).err().unwrap();
    assert!(
        matches!(&err, Error::Undeclared { entity, .. } if entity == "Review"),
        "{err}"
    );

    // The refused opens left the file as it was.
    let db = shop::extended::Db::open(&path).unwrap();
    assert_eq!(db.items().len().unwrap(), 1);
    assert_eq!(db.orders().len().unwrap(), 1);
    assert_eq!(db.reviews().len().unwrap(), 1);
    assert!(matches!(
        db.items().delete(db.items().all().unwrap()[0].id),
        Err(Error::Restricted { .. })
    ));
}

#[test]
fn dropping_a_referencing_entity_takes_a_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path);

    {
        let db = shop::dropped::Db::open(&path).unwrap();
        // Order's restrict went with it.
        assert!(db.items().delete(shop::dropped::ItemId(1)).unwrap());
    }
    let catalog = shop::dropped::Db::open(&path).unwrap().stored_catalog().unwrap();
    assert_eq!(catalog.current.keys().collect::<Vec<_>>(), ["Item"]);
    // With Order gone, a subset schema has nothing to trip over.
    shop::subset::Db::open(&path).unwrap();
}

#[test]
fn renaming_a_referenced_id_field_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path);

    let db = shop::renamed_id::Db::open(&path).unwrap();
    assert_eq!(db.items().all().unwrap()[0].key, shop::renamed_id::ItemId(1));
    let orders = db.orders().by_item(shop::renamed_id::ItemId(1)).unwrap();
    assert_eq!(orders.len(), 1);
    assert!(matches!(
        db.items().delete(shop::renamed_id::ItemId(1)),
        Err(Error::Restricted { .. })
    ));
}

#[test]
fn schema_changes_that_break_the_layout_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path);

    let errors = [
        ("price_type", shop::price_type::Db::open(&path).err()),
        ("renamed", shop::renamed::Db::open(&path).err()),
        ("reordered", shop::reordered::Db::open(&path).err()),
        ("key_id", shop::key_id::Db::open(&path).err()),
    ];
    let stored = <shop::v1::Item as orangedb::Entity>::FINGERPRINT;
    for (variant, err) in errors {
        let err = err.unwrap_or_else(|| panic!("{variant} opened"));
        assert!(
            matches!(err, Error::SchemaMismatch { entity: "Item", stored: s, declared, .. } if s == stored && declared != stored),
            "{variant}: {err}"
        );
        assert!(
            err.to_string().starts_with(&format!(
                "Item schema mismatch: the database stores fingerprint {stored:016x}"
            )),
            "{err}"
        );
    }

    // The id's key type is part of `Order`'s fingerprint too, through
    // `ItemId`, so whichever entity is checked first is the one reported.
    #[orangedb::schema]
    mod key_id_order_first {
        #[entity]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            #[references(Item, on_delete = restrict)]
            pub item: ItemId,
        }

        #[entity]
        pub struct Item {
            #[id(String)]
            pub id: ItemId,
            pub name: String,
            pub price: u32,
        }
    }
    let err = key_id_order_first::Db::open(&path).err().unwrap();
    assert!(matches!(err, Error::SchemaMismatch { entity: "Order", .. }), "{err}");

    // None of the rejected opens touched the file.
    let db = shop::v1::Db::open(&path).unwrap();
    assert_eq!(db.items().all().unwrap()[0].name, "lamp");
    assert_eq!(db.orders().len().unwrap(), 1);
}

#[test]
fn every_open_checks_every_entity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path);

    // A schema that is wrong about an entity it shares with the file fails,
    // even if it has another, new entity that is fine.
    #[orangedb::schema]
    mod mixed {
        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub name: String,
        }

        #[entity]
        pub struct Coupon {
            #[id(String)]
            pub code: CouponCode,
        }
    }
    assert!(matches!(
        mixed::Db::open(&path).err().unwrap(),
        Error::SchemaMismatch { entity: "Item", .. }
    ));

    // The failed open didn't record Coupon, so a schema with a different
    // Coupon can still claim it.
    #[orangedb::schema]
    mod other_coupon {
        #[entity]
        pub struct Coupon {
            #[id(u32)]
            pub code: CouponCode,
            pub percent: u8,
        }
    }
    let db = other_coupon::Db::open(&path).unwrap();
    db.coupons()
        .insert(&other_coupon::Coupon {
            code: other_coupon::CouponCode(10),
            percent: 10,
        })
        .unwrap();
}
