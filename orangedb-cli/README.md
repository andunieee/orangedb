# orangedb-cli

`orangedb`, a command-line tool that inspects
[`orangedb`](https://crates.io/crates/orangedb) databases without the
schema's Rust types, from the catalog every database records when it is
opened.

```sh
cargo install orangedb-cli
orangedb shop.redb [COMMAND]
```

| Command | Shows |
| --- | --- |
| `entities` (default) | each entity at its current layout: id, row count, references and unique fields, with their tables |
| `source [ENTITY[@FP]] [--all]` | the schema module source recorded for each layout, once per module |
| `history` | every layout the file has recorded, current or migrated away from |
| `tables` | every redb table in the file: kind, key and value types, entries, and the entity it belongs to |
| `count [ENTITY...]` | rows per entity |
| `refs [ENTITY]` | foreign keys from or to an entity: target, `on_delete`, links, index table |
| `indexes [ENTITY]` | foreign key and unique index tables, with their entries |
| `dump TARGET [-n N] [--full]` | entries of an entity's rows (`Entity`), an index (`Entity.field`) or any table by name |

The tool opens the file read-only, and any redb file works: tables orangedb
doesn't own are listed and dumped too. Rows are postcard bytes without type
information, so `dump` decodes them field by field with the types the
catalog records and shows the rest of a row in hex from the first type it
can't decode, such as an `#[embed]` struct.

## License

MIT OR Apache-2.0
