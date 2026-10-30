//! A small blog: users write posts, and replies thread under posts and under
//! each other.
//!
//! Run with `cargo run --example blog`.

#[orangedb::schema]
pub mod blog {
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

use blog::*;

fn main() -> orangedb::Result<()> {
    let db = Db::in_memory()?;

    // Single calls each run in their own transaction.
    let alice = db.users().create(|id| User {
        id,
        email: "alice@example.com".into(),
        name: "Alice".into(),
    })?;
    let bob = db.users().create(|id| User {
        id,
        email: "bob@example.com".into(),
        name: "Bob".into(),
    })?;

    // Or group them: all of this commits together, or not at all.
    let post = db.write(|tx| {
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
        tx.replies().create(|id| Reply {
            id,
            post: post.id,
            author: alice.id,
            parent: Some(question.id),
            body: "It's small and fast.".into(),
        })?;
        Ok::<_, orangedb::Error>(post)
    })?;

    println!("{} replies on {:?}", db.replies().by_post(post.id)?.len(), post.title);
    println!(
        "found by email: {:?}",
        db.users().by_email("bob@example.com")?.map(|u| u.name)
    );

    // Constraints are checked on every write.
    let dangling = db.posts().create(|id| Post {
        id,
        author: UserId(99),
        title: "?".into(),
        liked_by: vec![],
    });
    println!("dangling author: {}", dangling.unwrap_err());
    let duplicate = db.users().create(|id| User {
        id,
        email: "bob@example.com".into(),
        name: "Bob 2".into(),
    });
    println!("duplicate email: {}", duplicate.unwrap_err());

    // Bob has a reply on Alice's post that would outlive him.
    println!("delete bob: {}", db.users().delete(bob.id).unwrap_err());

    // Deleting Alice cascades to her post, which cascades to every reply on
    // it, including Bob's, so nothing is left to restrict.
    db.users().delete(alice.id)?;
    println!(
        "after deleting alice: {} users, {} posts, {} replies",
        db.users().len()?,
        db.posts().len()?,
        db.replies().len()?
    );

    // With nothing referencing him any more, Bob can go too.
    db.users().delete(bob.id)?;
    println!("after deleting bob: {} users", db.users().len()?);
    Ok(())
}
