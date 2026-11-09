# orangedb-bench

Benchmarks orangedb against [minisqlite](https://github.com/cursor/minisqlite)
and [turso](https://github.com/tursodatabase/turso) on a three-table forum
schema: users (unique email), posts (references a user) and comments
(reference a post and a user), every reference cascading on delete.

```console
cargo test --release   # checks the engines all return the same answers
cargo bench            # full run; add `-- --quick` for a fast pass, or a filter like `-- read/`
```

The crate is outside the main workspace so turso's dependencies stay out of
its lockfile.

How the engines are set up so they do the same work:

- All three write to a file in a temp dir and sync on commit: redb's default
  durability, and WAL with the default `synchronous` for the SQL engines.
- The SQL engines run with `PRAGMA foreign_keys = ON` and have indexes on every
  referencing column, the same ones orangedb maintains for `#[references]`.
- Every result is decoded into the same plain structs.
- turso uses cached prepared statements. minisqlite has no prepared statements,
  so each call formats SQL that it parses and plans again.
- turso is async; each call blocks on a current-thread tokio runtime.

The lookups and single writes run against 1,000 users, 10,000 posts and 50,000
comments.
