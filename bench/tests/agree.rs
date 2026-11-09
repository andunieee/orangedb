//! The engines answer every query the same way, so the benchmarks compare
//! the same work.

use orangedb_bench::*;

fn run<E: Engine>() -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let mut e = E::open(dir.path());
    let ids = e.load(0, 20);
    let extra = e.insert_user(20);
    let mut out = vec![format!("{ids:?} {extra}")];
    for &id in &ids[..5] {
        out.push(format!("{:?}", e.user(id)));
        out.push(format!("{:?}", e.user_by_email(&email(id - ids[0]))));
        out.push(format!("{:?}", e.posts_by_author(id)));
        let mut comments = e.comments_on_posts_by(id);
        comments.sort_by_key(|c| c.id);
        assert_eq!(comments.len(), POSTS_PER_USER * COMMENTS_PER_POST);
        out.push(format!("{comments:?}"));
    }
    e.delete_user(ids[3]);
    out.push(format!("{:?} {:?}", e.user(ids[3]), e.posts_by_author(ids[3])));
    // User 4 commented on user 3's posts, which are gone; user 3's comments
    // on user 4's posts are gone too.
    out.push(format!("{:?}", e.comments_on_posts_by(ids[4]).len()));
    out
}

#[test]
fn engines_agree() {
    let orange = run::<OrangeDb>();
    assert_eq!(orange.last().unwrap(), "0");
    assert_eq!(run::<MiniSqlite>(), orange);
    assert_eq!(run::<Turso>(), orange);
}
