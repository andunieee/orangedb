use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use orangedb_bench::*;

/// Users in the database the lookups and single writes run against, each
/// with their posts and comments.
const USERS: u64 = 1_000;
/// Users written by one `load` transaction.
const BATCH: u64 = 100;

/// Picks ids spread over `0..n` without a dependency on `rand`.
struct Pick(u64);

impl Pick {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn bench<E: Engine>(c: &mut Criterion) {
    let name = E::NAME;

    c.benchmark_group("load")
        .sample_size(10)
        .bench_function(format!("{name}/{BATCH} users, {} rows", BATCH * (1 + 10 + 50)), |b| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let e = E::open(dir.path());
                    (dir, e)
                },
                |(dir, mut e)| {
                    black_box(e.load(0, BATCH));
                    (dir, e)
                },
                BatchSize::PerIteration,
            )
        });

    let dir = tempfile::tempdir().unwrap();
    let mut e = E::open(dir.path());
    let mut ids = Vec::new();
    for first in (0..USERS).step_by(BATCH as usize) {
        ids.extend(e.load(first, BATCH));
    }
    let mut pick = Pick(0);

    let mut g = c.benchmark_group("read");
    g.bench_function(format!("{name}/user by id"), |b| {
        b.iter(|| black_box(e.user(ids[pick.next(USERS) as usize]).unwrap()))
    });
    g.bench_function(format!("{name}/user by unique email"), |b| {
        b.iter(|| black_box(e.user_by_email(&email(pick.next(USERS))).unwrap()))
    });
    g.bench_function(format!("{name}/posts by author (10)"), |b| {
        b.iter(|| black_box(e.posts_by_author(ids[pick.next(USERS) as usize])))
    });
    g.bench_function(format!("{name}/comments on posts by author (50)"), |b| {
        b.iter(|| black_box(e.comments_on_posts_by(ids[pick.next(USERS) as usize])))
    });
    g.finish();

    let mut g = c.benchmark_group("write");
    g.sample_size(20);
    let mut next = USERS;
    g.bench_function(format!("{name}/insert user (1 tx)"), |b| {
        b.iter(|| {
            next += 1;
            black_box(e.insert_user(next))
        })
    });
    g.bench_function(format!("{name}/delete user, cascade 60 rows (1 tx)"), |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                next += 1;
                let id = e.load(next, 1)[0];
                let start = Instant::now();
                e.delete_user(id);
                total += start.elapsed();
            }
            total
        })
    });
    g.finish();
}

fn engines(c: &mut Criterion) {
    bench::<OrangeDb>(c);
    bench::<MiniSqlite>(c);
    bench::<Turso>(c);
}

criterion_group!(benches, engines);
criterion_main!(benches);
