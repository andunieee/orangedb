//! One small forum schema (users, posts, comments) implemented on each engine
//! behind the same [`Engine`] trait, so the benchmarks run the same workload
//! against all of them.
//!
//! Every engine stores to a file and enforces the same constraints: a unique
//! email, foreign keys from posts and comments, and cascading deletes. The SQL
//! engines get the indexes orangedb creates for each reference.

use std::path::Path;

pub const POSTS_PER_USER: usize = 10;
pub const COMMENTS_PER_POST: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: u64,
    pub email: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub id: u64,
    pub author: u64,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub id: u64,
    pub post: u64,
    pub author: u64,
    pub body: String,
}

pub fn email(n: u64) -> String {
    format!("user{n}@example.com")
}

fn name(n: u64) -> String {
    format!("User {n}")
}

fn title(n: u64, p: usize) -> String {
    format!("Post {p} by user {n}")
}

const POST_BODY: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt.";
const COMMENT_BODY: &str = "Ut enim ad minim veniam, quis nostrud exercitation.";

pub trait Engine: Sized {
    const NAME: &'static str;

    fn open(dir: &Path) -> Self;

    /// Inserts users `first..first + count`, each with [`POSTS_PER_USER`]
    /// posts that each get [`COMMENTS_PER_POST`] comments by the user who
    /// signed up just before, all in one transaction. Returns the user ids.
    fn load(&mut self, first: u64, count: u64) -> Vec<u64>;

    /// Inserts user `n` alone, in its own transaction.
    fn insert_user(&mut self, n: u64) -> u64;

    fn user(&mut self, id: u64) -> Option<User>;

    fn user_by_email(&mut self, email: &str) -> Option<User>;

    fn posts_by_author(&mut self, author: u64) -> Vec<Post>;

    /// Every comment on the posts `author` wrote.
    fn comments_on_posts_by(&mut self, author: u64) -> Vec<Comment>;

    /// Deletes a user, cascading to their posts and to every comment on
    /// those posts or written by them.
    fn delete_user(&mut self, id: u64);
}

// --- orangedb ---------------------------------------------------------------

#[orangedb::schema]
pub mod forum {
    #[entity]
    pub struct User {
        #[id(auto)]
        pub id: UserId,
        #[unique]
        pub email: String,
        pub name: String,
    }

    #[entity]
    pub struct Post {
        #[id(auto)]
        pub id: PostId,
        #[references(User, on_delete = cascade)]
        pub author: UserId,
        pub title: String,
        pub body: String,
    }

    #[entity]
    pub struct Comment {
        #[id(auto)]
        pub id: CommentId,
        #[references(Post, on_delete = cascade)]
        pub post: PostId,
        #[references(User, on_delete = cascade)]
        pub author: UserId,
        pub body: String,
    }
}

pub struct OrangeDb(forum::Db);

impl From<forum::User> for User {
    fn from(u: forum::User) -> Self {
        User { id: u.id.0, email: u.email, name: u.name }
    }
}

impl From<forum::Post> for Post {
    fn from(p: forum::Post) -> Self {
        Post { id: p.id.0, author: p.author.0, title: p.title, body: p.body }
    }
}

impl From<forum::Comment> for Comment {
    fn from(c: forum::Comment) -> Self {
        Comment { id: c.id.0, post: c.post.0, author: c.author.0, body: c.body }
    }
}

impl Engine for OrangeDb {
    const NAME: &'static str = "orangedb";

    fn open(dir: &Path) -> Self {
        OrangeDb(forum::Db::open(dir.join("bench.redb")).unwrap())
    }

    fn load(&mut self, first: u64, count: u64) -> Vec<u64> {
        use forum::*;
        self.0
            .write(|tx| {
                let mut ids = Vec::with_capacity(count as usize);
                let mut prev = None;
                for n in first..first + count {
                    let user = tx.users().create(|id| User { id, email: email(n), name: name(n) })?;
                    let commenter = prev.unwrap_or(user.id);
                    for p in 0..POSTS_PER_USER {
                        let post = tx.posts().create(|id| Post {
                            id,
                            author: user.id,
                            title: title(n, p),
                            body: POST_BODY.into(),
                        })?;
                        for _ in 0..COMMENTS_PER_POST {
                            tx.comments().create(|id| Comment {
                                id,
                                post: post.id,
                                author: commenter,
                                body: COMMENT_BODY.into(),
                            })?;
                        }
                    }
                    prev = Some(user.id);
                    ids.push(user.id.0);
                }
                Ok::<_, orangedb::Error>(ids)
            })
            .unwrap()
    }

    fn insert_user(&mut self, n: u64) -> u64 {
        use forum::*;
        self.0.users().create(|id| User { id, email: email(n), name: name(n) }).unwrap().id.0
    }

    fn user(&mut self, id: u64) -> Option<User> {
        self.0.users().get(forum::UserId(id)).unwrap().map(Into::into)
    }

    fn user_by_email(&mut self, email: &str) -> Option<User> {
        self.0.users().by_email(email.to_string()).unwrap().map(Into::into)
    }

    fn posts_by_author(&mut self, author: u64) -> Vec<Post> {
        self.0.posts().by_author(forum::UserId(author)).unwrap().into_iter().map(Into::into).collect()
    }

    fn comments_on_posts_by(&mut self, author: u64) -> Vec<Comment> {
        self.0
            .read(|tx| {
                let mut out = Vec::new();
                for post in tx.posts().by_author(forum::UserId(author))? {
                    out.extend(tx.comments().by_post(post.id)?.into_iter().map(Comment::from));
                }
                Ok::<_, orangedb::Error>(out)
            })
            .unwrap()
    }

    fn delete_user(&mut self, id: u64) {
        self.0.users().delete(forum::UserId(id)).unwrap();
    }
}

// --- SQL engines ------------------------------------------------------------

const SCHEMA: &str = "
    CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        email TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL
    );
    CREATE TABLE posts (
        id INTEGER PRIMARY KEY,
        author INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        title TEXT NOT NULL,
        body TEXT NOT NULL
    );
    CREATE INDEX posts_author ON posts(author);
    CREATE TABLE comments (
        id INTEGER PRIMARY KEY,
        post INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
        author INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        body TEXT NOT NULL
    );
    CREATE INDEX comments_post ON comments(post);
    CREATE INDEX comments_author ON comments(author);
";

const COMMENTS_ON_POSTS_BY: &str =
    "SELECT c.id, c.post, c.author, c.body FROM posts p JOIN comments c ON c.post = p.id WHERE p.author = ";

// --- minisqlite -------------------------------------------------------------

/// minisqlite has no prepared statements or parameters, so every call
/// formats its SQL and the engine parses and plans it again.
pub struct MiniSqlite(minisqlite::Connection);

fn text(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

mod mini {
    use minisqlite::Value;

    pub fn int(v: &Value) -> u64 {
        match v {
            Value::Integer(i) => *i as u64,
            v => panic!("expected an integer, got {v:?}"),
        }
    }

    pub fn text(v: &Value) -> String {
        match v {
            Value::Text(s) => s.clone(),
            v => panic!("expected text, got {v:?}"),
        }
    }
}

impl MiniSqlite {
    fn insert(&mut self, sql: String) -> u64 {
        let r = self.0.query(&sql).unwrap();
        mini::int(&r.rows[0][0])
    }

    fn users(&mut self, sql: &str) -> Vec<User> {
        let r = self.0.query(sql).unwrap();
        r.rows
            .iter()
            .map(|r| User { id: mini::int(&r[0]), email: mini::text(&r[1]), name: mini::text(&r[2]) })
            .collect()
    }
}

impl Engine for MiniSqlite {
    const NAME: &'static str = "minisqlite";

    fn open(dir: &Path) -> Self {
        let mut c = minisqlite::Connection::open(&dir.join("bench.minisqlite")).unwrap();
        c.execute("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;").unwrap();
        c.execute(SCHEMA).unwrap();
        MiniSqlite(c)
    }

    fn load(&mut self, first: u64, count: u64) -> Vec<u64> {
        self.0.execute("BEGIN").unwrap();
        let mut ids = Vec::with_capacity(count as usize);
        let mut prev = None;
        for n in first..first + count {
            let user = self.insert(format!(
                "INSERT INTO users (email, name) VALUES ({}, {}) RETURNING id",
                text(&email(n)),
                text(&name(n))
            ));
            let commenter = prev.unwrap_or(user);
            for p in 0..POSTS_PER_USER {
                let post = self.insert(format!(
                    "INSERT INTO posts (author, title, body) VALUES ({user}, {}, {}) RETURNING id",
                    text(&title(n, p)),
                    text(POST_BODY)
                ));
                for _ in 0..COMMENTS_PER_POST {
                    self.0
                        .execute(&format!(
                            "INSERT INTO comments (post, author, body) VALUES ({post}, {commenter}, {})",
                            text(COMMENT_BODY)
                        ))
                        .unwrap();
                }
            }
            prev = Some(user);
            ids.push(user);
        }
        self.0.execute("COMMIT").unwrap();
        ids
    }

    fn insert_user(&mut self, n: u64) -> u64 {
        self.insert(format!(
            "INSERT INTO users (email, name) VALUES ({}, {}) RETURNING id",
            text(&email(n)),
            text(&name(n))
        ))
    }

    fn user(&mut self, id: u64) -> Option<User> {
        self.users(&format!("SELECT id, email, name FROM users WHERE id = {id}")).pop()
    }

    fn user_by_email(&mut self, email: &str) -> Option<User> {
        self.users(&format!("SELECT id, email, name FROM users WHERE email = {}", text(email))).pop()
    }

    fn posts_by_author(&mut self, author: u64) -> Vec<Post> {
        let r = self.0.query(&format!("SELECT id, author, title, body FROM posts WHERE author = {author}")).unwrap();
        r.rows
            .iter()
            .map(|r| Post {
                id: mini::int(&r[0]),
                author: mini::int(&r[1]),
                title: mini::text(&r[2]),
                body: mini::text(&r[3]),
            })
            .collect()
    }

    fn comments_on_posts_by(&mut self, author: u64) -> Vec<Comment> {
        let r = self.0.query(&format!("{COMMENTS_ON_POSTS_BY}{author}")).unwrap();
        r.rows
            .iter()
            .map(|r| Comment {
                id: mini::int(&r[0]),
                post: mini::int(&r[1]),
                author: mini::int(&r[2]),
                body: mini::text(&r[3]),
            })
            .collect()
    }

    fn delete_user(&mut self, id: u64) {
        self.0.execute(&format!("DELETE FROM users WHERE id = {id}")).unwrap();
    }
}

// --- turso ------------------------------------------------------------------

/// turso's API is async; each call blocks on a current-thread runtime. Its
/// statements are prepared once and cached.
pub struct Turso {
    rt: tokio::runtime::Runtime,
    _db: turso::Database,
    conn: turso::Connection,
}

impl Turso {
    fn run<T>(&self, f: impl Future<Output = turso::Result<T>>) -> T {
        self.rt.block_on(f).unwrap()
    }

    async fn rows<T>(
        conn: &turso::Connection,
        sql: &str,
        params: impl turso::IntoParams,
        row: impl Fn(&turso::Row) -> turso::Result<T>,
    ) -> turso::Result<Vec<T>> {
        let mut stmt = conn.prepare_cached(sql).await?;
        let mut rows = stmt.query(params).await?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().await? {
            out.push(row(&r)?);
        }
        Ok(out)
    }

    async fn insert_user(conn: &turso::Connection, n: u64) -> turso::Result<u64> {
        let mut stmt = conn.prepare_cached("INSERT INTO users (email, name) VALUES (?1, ?2)").await?;
        stmt.execute((email(n), name(n))).await?;
        Ok(conn.last_insert_rowid() as u64)
    }
}

fn user_row(r: &turso::Row) -> turso::Result<User> {
    Ok(User { id: r.get::<i64>(0)? as u64, email: r.get(1)?, name: r.get(2)? })
}

impl Engine for Turso {
    const NAME: &'static str = "turso";

    fn open(dir: &Path) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let path = dir.join("bench.turso");
        let (db, conn) = rt
            .block_on(async {
                let db = turso::Builder::new_local(path.to_str().unwrap()).build().await?;
                let conn = db.connect()?;
                conn.execute("PRAGMA foreign_keys = ON", ()).await?;
                conn.execute_batch(SCHEMA).await?;
                Ok::<_, turso::Error>((db, conn))
            })
            .unwrap();
        Turso { rt, _db: db, conn }
    }

    fn load(&mut self, first: u64, count: u64) -> Vec<u64> {
        let conn = &self.conn;
        self.rt
            .block_on(async {
                conn.execute("BEGIN", ()).await?;
                let mut post_stmt = conn.prepare_cached("INSERT INTO posts (author, title, body) VALUES (?1, ?2, ?3)").await?;
                let mut comment_stmt =
                    conn.prepare_cached("INSERT INTO comments (post, author, body) VALUES (?1, ?2, ?3)").await?;
                let mut ids = Vec::with_capacity(count as usize);
                let mut prev = None;
                for n in first..first + count {
                    let user = Turso::insert_user(conn, n).await?;
                    let commenter = prev.unwrap_or(user);
                    for p in 0..POSTS_PER_USER {
                        post_stmt.execute((user as i64, title(n, p), POST_BODY)).await?;
                        let post = conn.last_insert_rowid();
                        for _ in 0..COMMENTS_PER_POST {
                            comment_stmt.execute((post, commenter as i64, COMMENT_BODY)).await?;
                        }
                    }
                    prev = Some(user);
                    ids.push(user);
                }
                conn.execute("COMMIT", ()).await?;
                Ok::<_, turso::Error>(ids)
            })
            .unwrap()
    }

    fn insert_user(&mut self, n: u64) -> u64 {
        self.run(Turso::insert_user(&self.conn, n))
    }

    fn user(&mut self, id: u64) -> Option<User> {
        self.run(Turso::rows(&self.conn, "SELECT id, email, name FROM users WHERE id = ?1", [id as i64], user_row))
            .pop()
    }

    fn user_by_email(&mut self, email: &str) -> Option<User> {
        self.run(Turso::rows(&self.conn, "SELECT id, email, name FROM users WHERE email = ?1", [email], user_row))
            .pop()
    }

    fn posts_by_author(&mut self, author: u64) -> Vec<Post> {
        self.run(Turso::rows(
            &self.conn,
            "SELECT id, author, title, body FROM posts WHERE author = ?1",
            [author as i64],
            |r| {
                Ok(Post {
                    id: r.get::<i64>(0)? as u64,
                    author: r.get::<i64>(1)? as u64,
                    title: r.get(2)?,
                    body: r.get(3)?,
                })
            },
        ))
    }

    fn comments_on_posts_by(&mut self, author: u64) -> Vec<Comment> {
        self.run(Turso::rows(&self.conn, &format!("{COMMENTS_ON_POSTS_BY}?1"), [author as i64], |r| {
            Ok(Comment {
                id: r.get::<i64>(0)? as u64,
                post: r.get::<i64>(1)? as u64,
                author: r.get::<i64>(2)? as u64,
                body: r.get(3)?,
            })
        }))
    }

    fn delete_user(&mut self, id: u64) {
        let conn = &self.conn;
        self.run(async {
            let mut stmt = conn.prepare_cached("DELETE FROM users WHERE id = ?1").await?;
            stmt.execute([id as i64]).await
        });
    }
}
