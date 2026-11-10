#[orangedb::schema]
mod blog {
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
        #[references(User, on_delete = set_null)]
        pub liked_by: Vec<UserId>,
    }

    #[entity(accessor = replies)]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Reply {
        #[id(auto)]
        pub id: ReplyId,
        #[references(Post, on_delete = cascade)]
        pub post: PostId,
        #[references(User, on_delete = restrict)]
        pub author: UserId,
        #[references(Reply, on_delete = cascade)]
        pub parent: Option<ReplyId>,
        pub body: String,
    }
}

use blog::*;
use orangedb::Error;

fn user(db: &Db, name: &str) -> User {
    db.users()
        .create(|id| User {
            id,
            email: format!("{name}@example.com"),
            name: name.into(),
        })
        .unwrap()
}

fn post(db: &Db, author: &User, liked_by: &[&User]) -> Post {
    db.posts()
        .create(|id| Post {
            id,
            author: author.id,
            title: "title".into(),
            liked_by: liked_by.iter().map(|u| u.id).collect(),
        })
        .unwrap()
}

fn reply(db: &Db, post: &Post, author: &User, parent: Option<&Reply>) -> Reply {
    db.replies()
        .create(|id| Reply {
            id,
            post: post.id,
            author: author.id,
            parent: parent.map(|r| r.id),
            body: "hi".into(),
        })
        .unwrap()
}

#[test]
fn crud() {
    let db = Db::in_memory().unwrap();
    let mut alice = user(&db, "alice");
    assert_eq!(db.users().get(alice.id).unwrap(), Some(alice.clone()));
    assert!(matches!(db.users().insert(&alice), Err(Error::AlreadyExists { .. })));

    alice.name = "Alice".into();
    db.users().update(&alice).unwrap();
    assert_eq!(db.users().get(alice.id).unwrap().unwrap().name, "Alice");

    let ghost = User {
        id: UserId(42),
        email: "ghost@example.com".into(),
        name: "ghost".into(),
    };
    assert!(matches!(db.users().update(&ghost), Err(Error::NotFound { .. })));
    db.users().upsert(&ghost).unwrap();
    assert_eq!(db.users().len().unwrap(), 2);

    assert!(db.users().delete(ghost.id).unwrap());
    assert!(!db.users().delete(ghost.id).unwrap());
}

#[test]
fn auto_ids_skip_past_explicit_inserts() {
    let db = Db::in_memory().unwrap();
    assert_eq!(user(&db, "a").id, UserId(1));
    db.users()
        .insert(&User {
            id: UserId(10),
            email: "b@example.com".into(),
            name: "b".into(),
        })
        .unwrap();
    assert_eq!(user(&db, "c").id, UserId(11));
    assert_eq!(db.write(|tx| tx.users().next_id()).unwrap(), UserId(12));
    assert_eq!(user(&db, "d").id, UserId(13));
}

#[test]
fn failed_create_does_not_consume_an_id() {
    let db = Db::in_memory().unwrap();
    user(&db, "a");
    assert!(
        db.users()
            .create(|id| User {
                id,
                email: "a@example.com".into(),
                name: "dup".into()
            })
            .is_err()
    );
    assert_eq!(user(&db, "b").id, UserId(2));
}

#[test]
fn foreign_keys_are_checked_on_write() {
    let db = Db::in_memory().unwrap();
    let alice = user(&db, "alice");
    let missing = UserId(99);

    let err = db.posts().create(|id| Post {
        id,
        author: missing,
        title: "t".into(),
        liked_by: vec![],
    });
    assert!(matches!(err, Err(Error::ForeignKey { field: "author", .. })));
    let err = db.posts().create(|id| Post {
        id,
        author: alice.id,
        title: "t".into(),
        liked_by: vec![alice.id, missing],
    });
    assert!(matches!(err, Err(Error::ForeignKey { field: "liked_by", .. })));

    let mut p = post(&db, &alice, &[]);
    p.author = missing;
    assert!(matches!(db.posts().update(&p), Err(Error::ForeignKey { .. })));

    let r = reply(&db, &post(&db, &alice, &[]), &alice, None);
    let err = db.replies().create(|id| Reply {
        parent: Some(ReplyId(r.id.0 + 100)),
        id,
        ..r.clone()
    });
    assert!(matches!(
        err,
        Err(Error::ForeignKey {
            field: "parent",
            target: "Reply",
            ..
        })
    ));
}

#[test]
fn indexes_follow_updates() {
    let db = Db::in_memory().unwrap();
    let (alice, bob) = (user(&db, "alice"), user(&db, "bob"));
    let mut p = post(&db, &alice, &[&alice]);
    assert_eq!(db.posts().ids_by_author(alice.id).unwrap(), vec![p.id]);
    assert_eq!(db.posts().by_liked_by(alice.id).unwrap(), vec![p.clone()]);

    p.author = bob.id;
    p.liked_by.push(bob.id);
    db.posts().update(&p).unwrap();
    assert!(db.posts().by_author(alice.id).unwrap().is_empty());
    assert_eq!(db.posts().by_author(bob.id).unwrap(), vec![p.clone()]);
    assert_eq!(db.posts().by_liked_by(bob.id).unwrap(), vec![p.clone()]);
    assert_eq!(db.posts().by_liked_by(alice.id).unwrap(), vec![p.clone()]);
}

#[test]
fn unique_values() {
    let db = Db::in_memory().unwrap();
    let mut alice = user(&db, "alice");
    let bob = user(&db, "bob");

    let mut taken = bob.clone();
    taken.email = alice.email.clone();
    assert!(matches!(
        db.users().update(&taken),
        Err(Error::Unique { field: "email", .. })
    ));

    // Re-saving a row with its own value is fine.
    db.users().update(&alice).unwrap();

    // Changing the value frees the old one.
    alice.email = "alice@new.example.com".into();
    db.users().update(&alice).unwrap();
    assert_eq!(db.users().by_email("alice@new.example.com").unwrap(), Some(alice));
    assert_eq!(db.users().by_email("alice@example.com").unwrap(), None);
    taken.email = "alice@example.com".into();
    db.users().update(&taken).unwrap();
}

#[test]
fn cascade_reaches_nested_replies() {
    let db = Db::in_memory().unwrap();
    let alice = user(&db, "alice");
    let p = post(&db, &alice, &[]);
    let root = reply(&db, &p, &alice, None);
    let child = reply(&db, &p, &alice, Some(&root));
    let grandchild = reply(&db, &p, &alice, Some(&child));
    let other_post = post(&db, &alice, &[]);
    let elsewhere = reply(&db, &other_post, &alice, None);

    db.replies().delete(root.id).unwrap();
    assert_eq!(db.replies().all().unwrap(), vec![elsewhere]);
    assert!(db.replies().by_parent(child.id).unwrap().is_empty());
    assert!(!db.replies().contains(grandchild.id).unwrap());

    db.users().delete(alice.id).unwrap();
    assert!(db.posts().is_empty().unwrap());
    assert!(db.replies().is_empty().unwrap());
}

#[test]
fn restrict_leaves_everything_in_place() {
    let db = Db::in_memory().unwrap();
    let (alice, bob) = (user(&db, "alice"), user(&db, "bob"));
    let bobs_post = post(&db, &bob, &[&alice]);
    let alices_post = post(&db, &alice, &[]);
    let r = reply(&db, &bobs_post, &alice, None);

    // Alice's reply lives under Bob's post, so deleting her would leave it dangling.
    let err = db.users().delete(alice.id).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Restricted {
                entity: "User",
                referenced_by: "Reply.author",
                ..
            }
        ),
        "{err}"
    );

    // Nothing was touched, not even the parts of the cascade before the restrict.
    assert!(db.users().contains(alice.id).unwrap());
    assert_eq!(db.posts().get(alices_post.id).unwrap(), Some(alices_post.clone()));
    assert_eq!(db.posts().by_liked_by(alice.id).unwrap(), vec![bobs_post.clone()]);
    assert_eq!(db.replies().by_author(alice.id).unwrap(), vec![r.clone()]);

    // A restricting row that the same delete cascades away doesn't block it.
    db.users().delete(bob.id).unwrap();
    assert!(!db.replies().contains(r.id).unwrap());
    db.users().delete(alice.id).unwrap();
}

#[test]
fn set_null_removes_the_id_from_vecs() {
    let db = Db::in_memory().unwrap();
    let (alice, bob, carol) = (user(&db, "alice"), user(&db, "bob"), user(&db, "carol"));
    let p = post(&db, &alice, &[&bob, &carol, &bob]);

    db.users().delete(bob.id).unwrap();
    let p = db.posts().get(p.id).unwrap().unwrap();
    assert_eq!(p.liked_by, vec![carol.id]);
    assert!(db.posts().by_liked_by(bob.id).unwrap().is_empty());
    assert_eq!(db.posts().by_liked_by(carol.id).unwrap(), vec![p]);
}

#[test]
fn write_block_commits_or_rolls_back_as_one() {
    let db = Db::in_memory().unwrap();
    let result: Result<(), Error> = db.write(|tx| {
        let alice = tx.users().create(|id| User {
            id,
            email: "a@example.com".into(),
            name: "a".into(),
        })?;
        tx.posts().create(|id| Post {
            id,
            author: alice.id,
            title: "t".into(),
            liked_by: vec![],
        })?;
        tx.posts().create(|id| Post {
            id,
            author: UserId(99),
            title: "t".into(),
            liked_by: vec![],
        })?;
        Ok(())
    });
    assert!(matches!(result, Err(Error::ForeignKey { .. })));
    assert!(db.users().is_empty().unwrap());
    assert!(db.posts().is_empty().unwrap());

    // Writes are visible to reads later in the same transaction.
    db.write(|tx| {
        let alice = tx.users().create(|id| User {
            id,
            email: "a@example.com".into(),
            name: "a".into(),
        })?;
        let p = tx.posts().create(|id| Post {
            id,
            author: alice.id,
            title: "t".into(),
            liked_by: vec![],
        })?;
        assert_eq!(tx.posts().by_author(alice.id)?, vec![p]);
        Ok::<_, Error>(())
    })
    .unwrap();
    assert_eq!(db.posts().len().unwrap(), 1);
}

#[test]
fn a_caught_constraint_error_does_not_spoil_the_transaction() {
    let db = Db::in_memory().unwrap();
    let alice = user(&db, "alice");
    let p = post(&db, &alice, &[]);
    reply(&db, &p, &alice, None);
    let bob = user(&db, "bob");
    reply(&db, &p, &bob, None);

    db.write(|tx| {
        assert!(tx.users().delete(bob.id).is_err());
        tx.users().create(|id| User {
            id,
            email: "c@example.com".into(),
            name: "c".into(),
        })?;
        Ok::<_, Error>(())
    })
    .unwrap();
    assert_eq!(db.users().len().unwrap(), 3);
    assert_eq!(db.replies().len().unwrap(), 2);
}

#[test]
fn reads_see_a_snapshot() {
    let db = Db::in_memory().unwrap();
    user(&db, "alice");
    db.read(|tx| {
        user(&db, "bob");
        assert_eq!(tx.users().len()?, 1);
        Ok::<_, Error>(())
    })
    .unwrap();
    assert_eq!(db.users().len().unwrap(), 2);
}
