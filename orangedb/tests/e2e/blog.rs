//! A small blog: users write posts, and replies thread under posts and under
//! each other. `walkthrough` tells its story; the rest cover the corners
//! around it.

use orangedb::Error;

#[orangedb::schema]
pub mod schema {
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
        #[id(uuid)]
        pub id: PostId,
        /// Deleting a user deletes their posts.
        #[references(User, on_delete = cascade)]
        pub author: UserId,
        pub title: String,
        /// Deleting a user removes them from the posts they liked.
        #[references(User, on_delete = set_null)]
        pub liked_by: Vec<UserId>,
    }

    #[entity(accessor = replies)]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Reply {
        #[id(auto)]
        pub id: ReplyId,
        /// Deleting a post deletes its replies.
        #[references(Post, on_delete = cascade)]
        pub post: PostId,
        /// A user can't be deleted while their replies on other people's
        /// posts would survive.
        #[references(User, on_delete = restrict)]
        pub author: UserId,
        /// Deleting a reply deletes the replies under it.
        #[references(Reply, on_delete = cascade)]
        pub parent: Option<ReplyId>,
        pub body: String,
    }
}

use schema::*;

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
            title: format!("by {}", author.name),
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
            body: format!("from {}", author.name),
        })
        .unwrap()
}

backends!(crate::blog::schema =>
    /// The tour: build a blog, query it, hit each constraint, tear it down.
    fn walkthrough(db) {
        let alice = user(db, "alice");
        let bob = user(db, "bob");
        assert_eq!((alice.id, bob.id), (UserId(1), UserId(2)));

        let (post, question, answer) = db
            .write(|tx| {
                let post = tx.posts().create(|id| Post {
                    id,
                    author: alice.id,
                    title: "Hello, redb".into(),
                    liked_by: vec![bob.id],
                })?;
                let question = tx.replies().create(|id| Reply {
                    id,
                    post: post.id,
                    author: bob.id,
                    parent: None,
                    body: "Why redb?".into(),
                })?;
                let answer = tx.replies().create(|id| Reply {
                    id,
                    post: post.id,
                    author: alice.id,
                    parent: Some(question.id),
                    body: "It's small and fast.".into(),
                })?;
                Ok::<_, Error>((post, question, answer))
            })
            .unwrap();

        assert_eq!(db.posts().get(post.id).unwrap(), Some(post.clone()));
        assert_eq!(db.replies().by_post(post.id).unwrap(), vec![question.clone(), answer.clone()]);
        assert_eq!(db.replies().by_parent(question.id).unwrap(), vec![answer.clone()]);
        assert_eq!(db.replies().by_author(bob.id).unwrap(), vec![question.clone()]);
        assert_eq!(db.posts().by_author(alice.id).unwrap(), vec![post.clone()]);
        assert_eq!(db.posts().by_liked_by(bob.id).unwrap(), vec![post.clone()]);
        assert_eq!(db.users().by_email("bob@example.com").unwrap(), Some(bob.clone()));
        assert_eq!(db.users().by_email("carol@example.com").unwrap(), None);

        let err = db
            .posts()
            .create(|id| Post {
                id,
                author: UserId(99),
                title: "?".into(),
                liked_by: vec![],
            })
            .unwrap_err();
        assert!(
            matches!(&err, Error::ForeignKey { entity: "Post", field: "author", target: "User", id } if id == "UserId(99)"),
            "{err}"
        );
        let err = db
            .users()
            .create(|id| User {
                id,
                email: "bob@example.com".into(),
                name: "Bob 2".into(),
            })
            .unwrap_err();
        assert!(
            matches!(&err, Error::Unique { entity: "User", field: "email", owner, .. } if owner == "UserId(2)"),
            "{err}"
        );

        // Bob's question survives on Alice's post, so he can't go yet.
        let err = db.users().delete(bob.id).unwrap_err();
        assert!(
            matches!(&err, Error::Restricted { entity: "User", referenced_by: "Reply.author", child, .. }
                if *child == format!("{:?}", question.id)),
            "{err}"
        );
        assert_eq!(db.users().len().unwrap(), 2);
        assert_eq!(db.replies().len().unwrap(), 2);

        // Alice -> her post -> every reply on it, Bob's included.
        assert!(db.users().delete(alice.id).unwrap());
        assert_eq!(db.users().all().unwrap(), vec![bob.clone()]);
        assert!(db.posts().is_empty().unwrap());
        assert!(db.replies().is_empty().unwrap());
        assert!(db.replies().by_author(bob.id).unwrap().is_empty());
        assert!(db.posts().by_liked_by(bob.id).unwrap().is_empty());

        assert!(db.users().delete(bob.id).unwrap());
        assert!(db.users().is_empty().unwrap());
        // The email is free again.
        assert_eq!(db.users().by_email("bob@example.com").unwrap(), None);
        user(db, "bob");
    }

    fn crud(db) {
        let mut alice = user(db, "alice");
        assert!(db.users().contains(alice.id).unwrap());
        assert!(db.users().contains(&alice.id).unwrap());
        assert_eq!(db.users().get(&alice.id).unwrap(), Some(alice.clone()));

        let err = db.users().insert(&alice).unwrap_err();
        assert!(matches!(&err, Error::AlreadyExists { entity: "User", id } if id == "UserId(1)"), "{err}");

        alice.name = "Alice".into();
        db.users().update(&alice).unwrap();
        assert_eq!(db.users().get(alice.id).unwrap(), Some(alice.clone()));

        let ghost = User {
            id: UserId(7),
            email: "ghost@example.com".into(),
            name: "ghost".into(),
        };
        let err = db.users().update(&ghost).unwrap_err();
        assert!(matches!(&err, Error::NotFound { entity: "User", id } if id == "UserId(7)"), "{err}");
        assert!(!db.users().contains(ghost.id).unwrap());

        // upsert inserts, then replaces.
        db.users().upsert(&ghost).unwrap();
        let renamed = User {
            name: "casper".into(),
            ..ghost.clone()
        };
        db.users().upsert(&renamed).unwrap();
        assert_eq!(db.users().get(ghost.id).unwrap(), Some(renamed.clone()));
        assert_eq!(db.users().all().unwrap(), vec![alice.clone(), renamed.clone()]);
        assert_eq!(db.users().len().unwrap(), 2);

        assert!(db.users().delete(ghost.id).unwrap());
        assert!(!db.users().delete(ghost.id).unwrap());
        assert!(!db.users().delete(UserId(1234)).unwrap());
        assert_eq!(db.users().get(ghost.id).unwrap(), None);
        assert_eq!(db.users().len().unwrap(), 1);
    }

    fn auto_ids(db) {
        assert_eq!(user(db, "a").id, UserId(1));
        assert_eq!(user(db, "b").id, UserId(2));

        // Deleting the newest row doesn't hand its id out again.
        db.users().delete(UserId(2)).unwrap();
        assert_eq!(user(db, "c").id, UserId(3));

        // Explicit inserts and upserts push the sequence past themselves...
        db.users()
            .insert(&User {
                id: UserId(10),
                email: "d@example.com".into(),
                name: "d".into(),
            })
            .unwrap();
        assert_eq!(user(db, "e").id, UserId(11));
        db.users()
            .upsert(&User {
                id: UserId(20),
                email: "f@example.com".into(),
                name: "f".into(),
            })
            .unwrap();
        assert_eq!(user(db, "g").id, UserId(21));

        // ...but explicit ids below it don't pull it back.
        db.users()
            .insert(&User {
                id: UserId(5),
                email: "h@example.com".into(),
                name: "h".into(),
            })
            .unwrap();
        assert_eq!(user(db, "i").id, UserId(22));

        // A reserved id is used up even if nothing is inserted with it.
        let reserved = db.users().next_id().unwrap();
        assert_eq!(reserved, UserId(23));
        assert!(!db.users().contains(reserved).unwrap());
        assert_eq!(user(db, "j").id, UserId(24));

        // And it can be filled in later.
        db.users()
            .insert(&User {
                id: reserved,
                email: "k@example.com".into(),
                name: "k".into(),
            })
            .unwrap();
        assert_eq!(user(db, "l").id, UserId(25));

        // Each table counts on its own.
        let p = post(db, &db.users().get(UserId(1)).unwrap().unwrap(), &[]);
        let alice = db.users().get(UserId(1)).unwrap().unwrap();
        assert_eq!(reply(db, &p, &alice, None).id, ReplyId(1));

        // A failed create gives its id back.
        assert!(
            db.users()
                .create(|id| User {
                    id,
                    email: "a@example.com".into(),
                    name: "dup".into(),
                })
                .is_err()
        );
        assert_eq!(user(db, "m").id, UserId(26));
    }

    fn uuid_ids(db) {
        let alice = user(db, "alice");
        let posts: Vec<Post> = (0..20).map(|_| post(db, &alice, &[])).collect();
        // v7 ids are time-ordered, so id order is creation order.
        assert_eq!(db.posts().all().unwrap(), posts);
        assert_eq!(posts[0].id.0.get_version_num(), 7);

        // Generated ids can be used for explicit inserts too.
        let id = PostId::generate();
        assert_ne!(id, PostId::generate());
        let explicit = Post {
            id,
            author: alice.id,
            title: "explicit".into(),
            liked_by: vec![],
        };
        db.posts().insert(&explicit).unwrap();
        assert_eq!(db.posts().get(id).unwrap(), Some(explicit.clone()));
        assert!(matches!(db.posts().insert(&explicit), Err(Error::AlreadyExists { entity: "Post", .. })));

        // As can any other uuid.
        let nil = Post {
            id: PostId::from(orangedb::__private::uuid::Uuid::nil()),
            ..explicit
        };
        db.posts().insert(&nil).unwrap();
        assert_eq!(db.posts().all().unwrap()[0], nil);
        assert_eq!(db.posts().len().unwrap(), 22);
    }

    fn indexes_follow_every_write(db) {
        let (alice, bob, carol) = (user(db, "alice"), user(db, "bob"), user(db, "carol"));
        let mut p = post(db, &alice, &[&bob, &carol]);
        let other = post(db, &bob, &[&bob]);
        assert_eq!(db.posts().ids_by_author(alice.id).unwrap(), vec![p.id]);
        assert_eq!(db.posts().ids_by_liked_by(bob.id).unwrap(), vec![p.id, other.id]);
        assert_eq!(db.posts().by_liked_by(carol.id).unwrap(), vec![p.clone()]);

        // update moves entries.
        p.author = carol.id;
        p.liked_by = vec![alice.id];
        db.posts().update(&p).unwrap();
        assert!(db.posts().by_author(alice.id).unwrap().is_empty());
        assert_eq!(db.posts().by_author(carol.id).unwrap(), vec![p.clone()]);
        assert_eq!(db.posts().by_liked_by(alice.id).unwrap(), vec![p.clone()]);
        assert_eq!(db.posts().ids_by_liked_by(bob.id).unwrap(), vec![other.id]);
        assert!(db.posts().by_liked_by(carol.id).unwrap().is_empty());

        // upsert over an existing row moves them too.
        p.author = bob.id;
        p.liked_by = vec![];
        db.posts().upsert(&p).unwrap();
        assert!(db.posts().by_author(carol.id).unwrap().is_empty());
        assert_eq!(db.posts().by_author(bob.id).unwrap(), vec![p.clone(), other.clone()]);
        assert!(db.posts().by_liked_by(alice.id).unwrap().is_empty());

        // A repeated id in a Vec is one index entry, and survives losing one copy.
        p.liked_by = vec![carol.id, carol.id];
        db.posts().update(&p).unwrap();
        assert_eq!(db.posts().ids_by_liked_by(carol.id).unwrap(), vec![p.id]);

        // delete clears them.
        db.posts().delete(p.id).unwrap();
        assert!(db.posts().by_liked_by(carol.id).unwrap().is_empty());
        assert_eq!(db.posts().by_author(bob.id).unwrap(), vec![other.clone()]);

        // Unique values follow the same rules.
        let mut alice = alice;
        alice.email = "alice@new.example.com".into();
        db.users().update(&alice).unwrap();
        assert_eq!(db.users().by_email("alice@example.com").unwrap(), None);
        assert_eq!(db.users().by_email(alice.email.clone()).unwrap(), Some(alice.clone()));
        db.users().delete(alice.id).unwrap();
        assert_eq!(db.users().by_email("alice@new.example.com").unwrap(), None);
    }

    fn foreign_keys_are_checked_on_every_write(db) {
        let alice = user(db, "alice");
        let missing = UserId(99);
        let p = post(db, &alice, &[]);

        let dangling_like = Post {
            liked_by: vec![alice.id, missing],
            ..p.clone()
        };
        for result in [
            db.posts().update(&dangling_like),
            db.posts().upsert(&dangling_like),
            db.posts().insert(&Post {
                id: PostId::generate(),
                ..dangling_like.clone()
            }),
        ] {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, Error::ForeignKey { entity: "Post", field: "liked_by", id, .. } if id == "UserId(99)"),
                "{err}"
            );
        }
        assert_eq!(db.posts().all().unwrap(), vec![p.clone()]);

        let err = db
            .replies()
            .create(|id| Reply {
                id,
                post: PostId::generate(),
                author: alice.id,
                parent: None,
                body: "".into(),
            })
            .unwrap_err();
        assert!(matches!(err, Error::ForeignKey { field: "post", target: "Post", .. }), "{err}");

        let err = db
            .replies()
            .create(|id| Reply {
                id,
                post: p.id,
                author: alice.id,
                parent: Some(ReplyId(5)),
                body: "".into(),
            })
            .unwrap_err();
        assert!(matches!(err, Error::ForeignKey { field: "parent", target: "Reply", .. }), "{err}");

        // A reply can't be its own parent before it exists...
        let err = db
            .replies()
            .create(|id| Reply {
                id,
                post: p.id,
                author: alice.id,
                parent: Some(id),
                body: "".into(),
            })
            .unwrap_err();
        assert!(matches!(err, Error::ForeignKey { field: "parent", .. }), "{err}");

        // ...but can once it does.
        let mut r = reply(db, &p, &alice, None);
        r.parent = Some(r.id);
        db.replies().update(&r).unwrap();
        assert_eq!(db.replies().by_parent(r.id).unwrap(), vec![r.clone()]);
        db.replies().delete(r.id).unwrap();
        assert!(db.replies().is_empty().unwrap());
    }

    fn cascade_runs_through_every_level(db) {
        let (alice, bob) = (user(db, "alice"), user(db, "bob"));
        let p = post(db, &alice, &[]);
        let root = reply(db, &p, &alice, None);
        let child = reply(db, &p, &alice, Some(&root));
        let sibling = reply(db, &p, &alice, Some(&root));
        let grandchild = reply(db, &p, &alice, Some(&child));
        let unrelated = reply(db, &p, &alice, None);
        let elsewhere = post(db, &bob, &[]);
        let bobs = reply(db, &elsewhere, &bob, None);

        // A subtree of replies.
        assert!(db.replies().delete(child.id).unwrap());
        assert_eq!(db.replies().all().unwrap(), vec![root.clone(), sibling.clone(), unrelated.clone(), bobs.clone()]);
        assert!(!db.replies().contains(grandchild.id).unwrap());
        assert_eq!(db.replies().by_parent(root.id).unwrap(), vec![sibling.clone()]);

        // A post takes its replies, however deep.
        db.posts().delete(p.id).unwrap();
        assert_eq!(db.replies().all().unwrap(), vec![bobs.clone()]);
        assert!(db.replies().by_post(p.id).unwrap().is_empty());
        assert!(db.replies().by_author(alice.id).unwrap().is_empty());

        // A cycle of parents terminates.
        let p = post(db, &alice, &[]);
        let mut a = reply(db, &p, &alice, None);
        let b = reply(db, &p, &alice, Some(&a));
        a.parent = Some(b.id);
        db.replies().update(&a).unwrap();
        db.replies().delete(a.id).unwrap();
        assert!(db.replies().by_post(p.id).unwrap().is_empty());

        // A user takes their posts and the replies under them.
        db.users().delete(bob.id).unwrap();
        assert!(db.replies().is_empty().unwrap());
        assert_eq!(db.posts().all().unwrap(), vec![p]);
    }

    fn restrict_blocks_the_whole_delete(db) {
        let (alice, bob, carol) = (user(db, "alice"), user(db, "bob"), user(db, "carol"));
        let alices = post(db, &alice, &[&carol]);
        let alices_reply = reply(db, &alices, &alice, None);
        let bobs = post(db, &bob, &[&alice]);
        let r = reply(db, &bobs, &alice, None);
        let before = (db.users().all().unwrap(), db.posts().all().unwrap(), db.replies().all().unwrap());

        let err = db.users().delete(alice.id).unwrap_err();
        assert!(
            matches!(&err, Error::Restricted { entity: "User", referenced_by: "Reply.author", id, child }
                if id == "UserId(1)" && *child == format!("{:?}", r.id)),
            "{err}"
        );
        assert_eq!(err.to_string(), format!("cannot delete User UserId(1): it is still referenced by Reply.author {:?}", r.id));

        // Neither the cascade to her post nor the set_null on Bob's ran.
        assert_eq!((db.users().all().unwrap(), db.posts().all().unwrap(), db.replies().all().unwrap()), before);
        assert_eq!(db.posts().by_liked_by(alice.id).unwrap(), vec![bobs.clone()]);
        assert_eq!(db.replies().by_post(alices.id).unwrap(), vec![alices_reply]);

        // Removing the blocker unblocks it.
        db.replies().delete(r.id).unwrap();
        db.users().delete(alice.id).unwrap();
        assert_eq!(db.posts().all().unwrap(), vec![Post { liked_by: vec![], ..bobs }]);
        assert_eq!(db.users().all().unwrap(), vec![bob, carol]);
    }

    fn set_null_scrubs_vecs(db) {
        let (alice, bob, carol) = (user(db, "alice"), user(db, "bob"), user(db, "carol"));
        let p = post(db, &alice, &[&bob, &carol, &bob]);
        let q = post(db, &carol, &[&bob]);

        db.users().delete(bob.id).unwrap();
        assert_eq!(db.posts().get(p.id).unwrap().unwrap().liked_by, vec![carol.id]);
        assert!(db.posts().get(q.id).unwrap().unwrap().liked_by.is_empty());
        assert!(db.posts().by_liked_by(bob.id).unwrap().is_empty());
        assert_eq!(db.posts().ids_by_liked_by(carol.id).unwrap(), vec![p.id]);

        // Other fields of a detached row are left alone.
        let p = db.posts().get(p.id).unwrap().unwrap();
        assert_eq!((p.author, p.title.as_str()), (alice.id, "by alice"));

        // A row that likes its own author: the cascade wins over set_null.
        let mine = post(db, &carol, &[&carol, &alice]);
        db.users().delete(carol.id).unwrap();
        assert!(!db.posts().contains(mine.id).unwrap());
        assert!(!db.posts().contains(q.id).unwrap());
        assert!(db.posts().by_liked_by(alice.id).unwrap().is_empty());
    }

    fn table_handles_inside_transactions(db) {
        let alice = user(db, "alice");
        db.write(|tx| {
            let users = tx.users();
            assert_eq!(users.get(alice.id)?, Some(alice.clone()));
            assert_eq!(users.by_email("alice@example.com")?, Some(alice.clone()));
            let bob_id = users.next_id()?;
            users.insert(&User {
                id: bob_id,
                email: "bob@example.com".into(),
                name: "bob".into(),
            })?;
            assert_eq!(users.len()?, 2);
            let p = tx.posts().create(|id| Post {
                id,
                author: bob_id,
                title: "t".into(),
                liked_by: vec![alice.id],
            })?;
            assert_eq!(tx.posts().ids_by_liked_by(alice.id)?, vec![p.id]);
            assert!(users.delete(bob_id)?);
            assert!(tx.posts().is_empty()?);
            assert!(tx.posts().by_liked_by(alice.id)?.is_empty());
            Ok::<_, Error>(())
        })
        .unwrap();

        db.read(|tx| {
            assert_eq!(tx.users().all()?, vec![alice.clone()]);
            assert!(tx.users().contains(alice.id)?);
            assert!(!tx.users().is_empty()?);
            assert_eq!(tx.users().by_email("bob@example.com")?, None);
            assert!(tx.posts().by_author(alice.id)?.is_empty());
            assert!(tx.replies().ids_by_author(alice.id)?.is_empty());
            Ok::<_, Error>(())
        })
        .unwrap();
    }
);
