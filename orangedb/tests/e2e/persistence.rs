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

    /// Only `Item` exists here: the stored `Order` is simply ignored.
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
        /// `name` becomes unique, which would need an index the file doesn't have.
        made_unique {
            #[entity]
            pub struct Item {
                #[id(auto)]
                pub id: ItemId,
                #[unique]
                pub name: String,
                pub price: u32,
            }
        }
        /// The id switches from auto to uuid.
        uuid_id {
            #[entity]
            pub struct Item {
                #[id(uuid)]
                pub id: ItemId,
                pub name: String,
                pub price: u32,
            }
        }
        /// The layout is unchanged but the version is bumped.
        bumped {
            #[entity(version = 2)]
            pub struct Item {
                #[id(auto)]
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
    {
        let db = shop::subset::Db::open(&path).unwrap();
        assert_eq!(db.items().len().unwrap(), 1);
    }
    // And back again: the original program still opens it, and its new
    // tables don't get in the way.
    let db = shop::v1::Db::open(&path).unwrap();
    assert_eq!(db.items().len().unwrap(), 1);
    assert_eq!(db.orders().len().unwrap(), 1);
    assert!(matches!(
        db.items().delete(db.items().all().unwrap()[0].id),
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
        ("made_unique", shop::made_unique::Db::open(&path).err()),
        ("uuid_id", shop::uuid_id::Db::open(&path).err()),
        ("bumped", shop::bumped::Db::open(&path).err()),
    ];
    for (variant, err) in errors {
        let err = err.unwrap_or_else(|| panic!("{variant} opened"));
        let expected_version = if variant == "bumped" { 2 } else { 1 };
        assert!(
            matches!(
                err,
                Error::SchemaMismatch { entity: "Item", stored_version: 1, version, .. } if version == expected_version
            ),
            "{variant}: {err}"
        );
        assert!(
            err.to_string()
                .starts_with("Item schema mismatch: the database holds version 1"),
            "{err}"
        );
    }

    // With `Order` declared first, its index on `ItemId` is opened before
    // `Item`'s schema is checked, so redb rejects the new key type itself.
    #[orangedb::schema]
    mod uuid_id_order_first {
        #[entity]
        pub struct Order {
            #[id(auto)]
            pub id: OrderId,
            #[references(Item, on_delete = restrict)]
            pub item: ItemId,
        }

        #[entity]
        pub struct Item {
            #[id(uuid)]
            pub id: ItemId,
            pub name: String,
            pub price: u32,
        }
    }
    let err = uuid_id_order_first::Db::open(&path).err().unwrap();
    assert!(matches!(err, Error::Storage(_)), "{err}");

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
