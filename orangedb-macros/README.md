# orangedb-macros

Procedural macros for [`orangedb`](https://crates.io/crates/orangedb), an
indexing layer for structs with foreign keys and unique constraints on top of
[redb](https://github.com/cberner/redb).

This crate only implements `#[orangedb::schema]`, `#[orangedb::embed]`,
`#[entity]`, `#[id]`, `#[references]`, `#[unique]`, `#[fingerprint]` and
`#[migrate]`. You do not depend on it directly — use
`orangedb`, which re-exports everything you need. See the
[`orangedb` documentation](https://docs.rs/orangedb) for the full attribute
reference.

## License

MIT OR Apache-2.0
