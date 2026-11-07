//! Migrating a file database from one schema to the next. A forum goes
//! through three schemas:
//!
//! - v1: users, posts, threaded comments and drafts.
//! - v2: user names become structured, comments switch to uuid ids, drafts
//!   are dropped. Posts don't change.
//! - v3: posts get a unique slug.

use std::collections::HashMap;
use std::path::Path;

use orangedb::redb::{MultimapTableHandle, ReadableDatabase, TableHandle};
use orangedb::{Entity, Error};

pub mod forum {
    #[orangedb::schema]
    pub mod v1 {
        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct User {
            #[id(auto)]
            pub id: UserId,
            #[unique]
            pub email: String,
            pub name: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Post {
            #[id(auto)]
            pub id: PostId,
            #[references(User, on_delete = cascade)]
            pub author: UserId,
            pub title: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Comment {
            #[id(auto)]
            pub id: CommentId,
            #[references(Post, on_delete = cascade)]
            pub post: PostId,
            #[references(Comment, on_delete = cascade)]
            pub parent: Option<CommentId>,
            pub body: String,
        }

        #[entity]
        pub struct Draft {
            #[id(auto)]
            pub id: DraftId,
            pub body: String,
        }
    }

    #[orangedb::schema]
    pub mod v2 {
        use std::collections::HashMap;

        use super::v1;

        thread_local! {
            /// How often `from_v1` ran on this thread, i.e. in this test.
            pub static RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }

        #[migrate]
        fn from_v1(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
            RUNS.set(RUNS.get() + 1);
            for user in old.users().all()? {
                let (first, last) = user.name.split_once(' ').unwrap_or((&user.name, ""));
                new.users().insert(&User {
                    id: UserId(user.id.0),
                    email: user.email,
                    name: Name {
                        first: first.into(),
                        last: last.into(),
                    },
                })?;
            }
            // Comments get new ids, and reference each other by them.
            let comments = old.comments().all()?;
            let ids: HashMap<u64, CommentId> = comments.iter().map(|c| (c.id.0, CommentId::generate())).collect();
            // Replies before the comments they reply to: references are only
            // checked once the migration is done.
            for c in comments.into_iter().rev() {
                new.comments().insert(&Comment {
                    id: ids[&c.id.0],
                    post: PostId(c.post.0),
                    parent: c.parent.map(|p| ids[&p.0]),
                    body: c.body,
                })?;
            }
            Ok(())
        }

        #[embed]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Name {
            pub first: String,
            pub last: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct User {
            #[id(auto)]
            pub id: UserId,
            #[unique]
            pub email: String,
            pub name: Name,
        }

        /// Unchanged, so it keeps its table through the migration.
        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Post {
            #[id(auto)]
            pub id: PostId,
            #[references(User, on_delete = cascade)]
            pub author: UserId,
            pub title: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Comment {
            #[id(uuid)]
            pub id: CommentId,
            #[references(Post, on_delete = cascade)]
            pub post: PostId,
            #[references(Comment, on_delete = cascade)]
            pub parent: Option<CommentId>,
            pub body: String,
        }
    }

    #[orangedb::schema]
    pub mod v3 {
        use super::v2;

        #[migrate]
        fn from_v2(old: &v2::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
            for post in old.posts().all()? {
                new.posts().insert(&Post {
                    id: PostId(post.id.0),
                    author: UserId(post.author.0),
                    slug: format!("{}-{}", post.id.0, post.title.to_lowercase().replace(' ', "-")),
                    title: post.title,
                })?;
            }
            Ok(())
        }

        #[embed]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Name {
            pub first: String,
            pub last: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct User {
            #[id(auto)]
            pub id: UserId,
            #[unique]
            pub email: String,
            pub name: Name,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Post {
            #[id(auto)]
            pub id: PostId,
            #[references(User, on_delete = cascade)]
            pub author: UserId,
            pub title: String,
            #[unique]
            pub slug: String,
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Comment {
            #[id(uuid)]
            pub id: CommentId,
            #[references(Post, on_delete = cascade)]
            pub post: PostId,
            #[references(Comment, on_delete = cascade)]
            pub parent: Option<CommentId>,
            pub body: String,
        }
    }
}

use forum::{v1, v2, v3};

/// Two users, a post, a comment with a reply, a draft.
fn seed(path: &Path) {
    let db = v1::Db::open(path).unwrap();
    let ada = db
        .users()
        .create(|id| v1::User {
            id,
            email: "ada@example.com".into(),
            name: "Ada Lovelace".into(),
        })
        .unwrap();
    db.users()
        .create(|id| v1::User {
            id,
            email: "plato@example.com".into(),
            name: "Plato".into(),
        })
        .unwrap();
    let post = db
        .posts()
        .create(|id| v1::Post {
            id,
            author: ada.id,
            title: "Notes on the Engine".into(),
        })
        .unwrap();
    let top = db
        .comments()
        .create(|id| v1::Comment {
            id,
            post: post.id,
            parent: None,
            body: "first".into(),
        })
        .unwrap();
    db.comments()
        .create(|id| v1::Comment {
            id,
            post: post.id,
            parent: Some(top.id),
            body: "reply".into(),
        })
        .unwrap();
    db.drafts().create(|id| v1::Draft { id, body: "wip".into() }).unwrap();
}

pub fn table_names(path: &Path) -> Vec<String> {
    let db = orangedb::redb::Database::create(path).unwrap();
    let tx = db.begin_read().unwrap();
    let mut names: Vec<String> = tx.list_tables().unwrap().map(|t| t.name().to_string()).collect();
    names.extend(tx.list_multimap_tables().unwrap().map(|t| t.name().to_string()));
    names.sort();
    names
}

fn tables_of(names: &[String], entity: &str, fingerprint: u64) -> usize {
    let prefix = format!("{entity}@{fingerprint:016x}");
    names.iter().filter(|n| n.starts_with(&prefix)).count()
}

#[test]
fn a_migration_rewrites_the_changed_entities() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forum.redb");
    seed(&path);

    let db = v2::Db::open(&path).unwrap();
    let users = db.users().all().unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(
        users[0].name,
        v2::Name {
            first: "Ada".into(),
            last: "Lovelace".into()
        }
    );
    assert_eq!(users[1].name.last, "");
    // Indexes are rebuilt by the writes.
    assert_eq!(
        db.users().by_email("plato@example.com").unwrap().unwrap().id,
        users[1].id
    );

    // Comments were renumbered and still thread.
    let post = db.posts().all().unwrap().remove(0);
    let comments = db.comments().by_post(post.id).unwrap();
    assert_eq!(comments.len(), 2);
    let top = comments.iter().find(|c| c.parent.is_none()).unwrap();
    let reply = comments.iter().find(|c| c.parent.is_some()).unwrap();
    assert_eq!(reply.parent, Some(top.id));
    assert_eq!(db.comments().by_parent(top.id).unwrap(), vec![reply.clone()]);

    // Sequences carry over: no id is handed out twice.
    assert_eq!(db.users().next_id().unwrap(), v2::UserId(3));
    // Cascades work across migrated and carried-over tables.
    db.users().delete(users[0].id).unwrap();
    assert!(db.posts().is_empty().unwrap());
    assert!(db.comments().is_empty().unwrap());
    drop(db);

    // Old layouts are gone, unchanged ones stayed, dropped entities left.
    let names = table_names(&path);
    for (entity, old, new) in [
        ("User", v1::User::FINGERPRINT, v2::User::FINGERPRINT),
        ("Comment", v1::Comment::FINGERPRINT, v2::Comment::FINGERPRINT),
    ] {
        assert_eq!(tables_of(&names, entity, old), 0, "{entity}: {names:?}");
        assert!(tables_of(&names, entity, new) > 0, "{entity}: {names:?}");
    }
    assert_eq!(v1::Post::FINGERPRINT, v2::Post::FINGERPRINT);
    assert_eq!(tables_of(&names, "Post", v2::Post::FINGERPRINT), 2, "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("Draft")), "{names:?}");

    // A database at v2 doesn't go back.
    assert!(matches!(
        v1::Db::open(&path).err().unwrap(),
        Error::SchemaMismatch { entity: "User", .. }
    ));
}

#[test]
fn migrated_layouts_keep_their_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forum.redb");
    seed(&path);

    let db = v2::Db::open(&path).unwrap();
    let old = db.stored_source::<v1::User>().unwrap().unwrap();
    let new = db.stored_source::<v2::User>().unwrap().unwrap();
    assert_ne!(v1::User::FINGERPRINT, v2::User::FINGERPRINT);
    assert!(old.contains("pub struct Draft"), "{old}");
    assert!(new.contains("pub struct Name"), "{new}");
    assert!(!new.contains("pub struct Draft"), "{new}");
}

#[test]
fn the_catalog_keeps_migrated_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forum.redb");
    seed(&path);

    let catalog = v3::Db::open(&path).unwrap().stored_catalog().unwrap();
    // Every layout the file has been at, current or not.
    let mut seen: Vec<(&str, u64, bool)> = catalog
        .layouts
        .iter()
        .map(|l| (l.entity.as_str(), l.fingerprint, catalog.is_current(l)))
        .collect();
    seen.sort();
    let mut expected = vec![
        ("Comment", v1::Comment::FINGERPRINT, false),
        ("Comment", v2::Comment::FINGERPRINT, true),
        ("Draft", v1::Draft::FINGERPRINT, false),
        ("Post", v1::Post::FINGERPRINT, false),
        ("Post", v3::Post::FINGERPRINT, true),
        ("User", v1::User::FINGERPRINT, false),
        ("User", v2::User::FINGERPRINT, true),
    ];
    expected.sort();
    assert_eq!(seen, expected);
    assert!(!catalog.current.contains_key("Draft"));

    // A migrated layout still knows which module declared it.
    let draft = catalog.layout("Draft", v1::Draft::FINGERPRINT).unwrap();
    assert!(catalog.source(draft).unwrap().contains("pub struct Draft"));
    // A layout several schemas declare records the latest of them: v2 kept
    // v1's posts, and v3 kept v2's users and comments.
    let post = catalog.layout("Post", v1::Post::FINGERPRINT).unwrap();
    assert!(catalog.source(post).unwrap().contains("pub mod v2"));
    let user = catalog.layout("User", v2::User::FINGERPRINT).unwrap();
    assert!(catalog.source(user).unwrap().contains("pub mod v3"));
    assert_eq!(catalog.sources.len(), 3);
}

#[test]
fn migrations_chain_and_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forum.redb");
    seed(&path);

    let db = v3::Db::open(&path).unwrap();
    assert_eq!(v2::RUNS.get(), 1);
    let post = db.posts().by_slug("1-notes-on-the-engine").unwrap().unwrap();
    assert_eq!(db.comments().by_post(post.id).unwrap().len(), 2);
    assert_eq!(db.users().all().unwrap()[0].name.first, "Ada");
    drop(db);

    // Reopening at the current schema runs nothing.
    v3::Db::open(&path).unwrap();
    assert_eq!(v2::RUNS.get(), 1);
    // Nor does a fresh database.
    v2::Db::open(dir.path().join("fresh.redb")).unwrap();
    assert_eq!(v2::RUNS.get(), 1);
}

/// `v2` with a broken migration.
mod broken {
    use super::v1;

    pub mod dangling {
        use super::v1;

        /// Leaves out the user who wrote the post, which isn't migrated.
        #[orangedb::schema]
        pub mod schema {
            use super::v1;

            #[migrate]
            fn from_v1(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
                for user in old.users().all()?.into_iter().skip(1) {
                    new.users().insert(&User {
                        id: UserId(user.id.0),
                        email: user.email,
                        nick: user.name,
                    })?;
                }
                Ok(())
            }

            #[entity]
            pub struct User {
                #[id(auto)]
                pub id: UserId,
                #[unique]
                pub email: String,
                pub nick: String,
            }

            #[entity]
            pub struct Post {
                #[id(auto)]
                pub id: PostId,
                #[references(User, on_delete = cascade)]
                pub author: UserId,
                pub title: String,
            }
        }
    }

    pub mod failing {
        use super::v1;

        /// Gives up halfway through.
        #[orangedb::schema]
        pub mod schema {
            use super::v1;

            #[migrate]
            fn from_v1(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
                for user in old.users().all()? {
                    if user.name == "Plato" {
                        return Err(orangedb::Error::migration(format!("can't migrate {}", user.name)));
                    }
                    new.users().insert(&User {
                        id: UserId(user.id.0),
                        email: user.email,
                        nick: user.name,
                    })?;
                }
                Ok(())
            }

            #[entity]
            pub struct User {
                #[id(auto)]
                pub id: UserId,
                #[unique]
                pub email: String,
                pub nick: String,
            }
        }
    }
}

#[test]
fn a_failed_migration_leaves_the_database_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forum.redb");
    seed(&path);
    let before = table_names(&path);

    let err = broken::dangling::schema::Db::open(&path).err().unwrap();
    assert!(
        matches!(
            err,
            Error::ForeignKey {
                entity: "Post",
                field: "author",
                ..
            }
        ),
        "{err}"
    );
    let err = broken::failing::schema::Db::open(&path).err().unwrap();
    assert!(matches!(&err, Error::Migration(_)), "{err}");
    assert_eq!(err.to_string(), "migration failed: can't migrate Plato");

    assert_eq!(table_names(&path), before);
    let db = v1::Db::open(&path).unwrap();
    assert_eq!(db.users().len().unwrap(), 2);
    assert_eq!(db.drafts().len().unwrap(), 1);
    drop(db);

    // And the real migration still works afterwards.
    let db = v2::Db::open(&path).unwrap();
    let ids: HashMap<_, _> = db.users().all().unwrap().into_iter().map(|u| (u.email, u.id)).collect();
    assert_eq!(ids.len(), 2);
}

#[test]
fn a_database_from_elsewhere_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forum.redb");

    // Neither v2 nor v1 (which v2 migrates from) matches this `User`.
    #[orangedb::schema]
    mod stranger {
        #[entity]
        pub struct User {
            #[id(String)]
            pub handle: UserHandle,
        }
    }
    stranger::Db::open(&path).unwrap();
    let err = v2::Db::open(&path).err().unwrap();
    assert!(matches!(err, Error::SchemaMismatch { entity: "User", .. }), "{err}");
}
