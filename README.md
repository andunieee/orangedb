# orangedb

Indexing layer for structs with foreign keys, unique constraints and computed indexes on top of [redb].

You declare a schema as a module of plain structs. The `#[orangedb::schema]`
macro turns it into a `Db` type with one typed table handle per entity. Every
write checks foreign keys and unique constraints, deletes follow each
reference's `on_delete` rule.

[redb]: https://github.com/cberner/redb

## Usage

```toml
[dependencies]
orangedb = "0.1"
```

### Declare a schema

```rust
#[orangedb::schema]
pub mod bistro {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Waiter {
        #[id(auto)]
        pub id: WaiterId,
        #[unique]
        pub email: String,
        pub name: String,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Table {
        #[id(auto)]
        pub id: TableId,
        #[unique]
        pub number: u32,
        pub seats: u8,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    #[index(table_at: (TableId, u64) = |r| (r.table, r.at))]
    pub struct Reservation {
        #[id(auto)]
        pub id: ReservationId,
        #[unique]
        pub code: String,
        #[references(Table, on_delete = cascade)]
        pub table: TableId,
        #[references(Waiter, on_delete = set_null)]
        pub waiter: Option<WaiterId>,
        pub party_size: u8,
        pub at: u64,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Order {
        #[id(auto)]
        pub id: OrderId,
        #[references(Table, on_delete = cascade)]
        pub table: TableId,
        #[references(Waiter, on_delete = set_null)]
        pub waiter: Option<WaiterId>,
        #[references(Reservation, on_delete = set_null)]
        pub reservation: Option<ReservationId>,
        pub items: Vec<String>,
        #[index]
        pub open: bool,
    }
}

use bistro::*;
```

The macro generates the id newtypes (`WaiterId`, `TableId`, `ReservationId`,
`OrderId`) and adds the serde derives itself, so don't add your own.

### Open a database and write to it

```rust
let db = Db::open("bistro.redb")?; // or Db::in_memory()?

// A single call runs in its own transaction.
let sam = db.waiters().create(|id| Waiter {
    id,
    email: "sam@example.com".into(),
    name: "Sam".into(),
})?;

let table = db.tables().create(|id| Table {
    id,
    number: 12,
    seats: 4,
})?;

// A closure groups writes: they all commit together, or none do.
let (reservation, order) = db.write(|tx| {
    let reservation = tx.reservations().create(|id| Reservation {
        id,
        code: "R-1042".into(),
        table: table.id,
        waiter: Some(sam.id),
        party_size: 3,
        at: 1_790_000_000,
    })?;
    let order = tx.orders().create(|id| Order {
        id,
        table: table.id,
        waiter: Some(sam.id),
        reservation: Some(reservation.id),
        items: vec!["Soup".into(), "Pasta".into()],
        open: true,
    })?;
    Ok::<_, orangedb::Error>((reservation, order))
})?;
```

Each table handle has `create`, `insert`, `update`, `upsert`, `get`,
`contains`, `all`, `len`, `is_empty` and `delete`. You get the same handles
inside `db.write(|tx| ...)` and `db.read(|tx| ...)`.

### Query

```rust
db.tables().by_number(12)?;                 // Option<Table>, from #[unique]
db.reservations().by_table(table.id)?;      // Vec<Reservation>
db.orders().by_table(table.id)?;            // Vec<Order>, all orders on one table
db.orders().by_waiter(sam.id)?;             // Vec<Order>, every order Sam owns
db.reservations().by_waiter(sam.id)?;       // Vec<Reservation>
db.orders().by_open(true)?;                 // Vec<Order>, from #[index]

// Table 12's reservations from a time on, in time order, from the computed index.
db.reservations().range_by_table_at((table.id, 1_790_000_000)..(table.id, u64::MAX))?;
```

`#[index]` indexes a field's value, `#[unique]` does the same and also
rejects duplicates, and `#[index(name: Key = expr)]` on the struct indexes
whatever the expression computes from the row. The expression returns the
key, an `Option` of it or a `Vec` of them, and must depend on nothing but the
row. Each index gets `by_name`, and `range_by_name` for any range of keys;
the non-unique ones also get `ids_by_name`. Keys are redb key types, so
tuples work and sort by their parts.

A table is active while it has an open order, so filter `by_table(...)` on
`Order::open` to find the tables currently in service.

### Constraints

```rust
// A missing referenced row:        Error::ForeignKey
// A duplicate unique value:        Error::Unique
// A delete blocked by `restrict`:  Error::Restricted
db.tables().delete(table.id)?; // cascades to the table's reservations and orders
db.waiters().delete(sam.id)?;  // detaches Sam from reservations and orders
```

A delete works out the whole cascade before it writes anything. If a
`restrict` reference blocks any part of it, nothing is deleted.

## Attribute reference

| Attribute | Where | Meaning |
| --- | --- | --- |
| `#[entity]` | struct | A table. Option: `accessor = name` (default: plural snake case). |
| `#[id(auto)]` | field | A `u64` id from a per-table sequence. |
| `#[id(Type)]` | field | A key you supply, of any owned redb key type (`String`, `u32`, …). |
| `#[references(Entity, on_delete = …)]` | `EntityId`, `Option<EntityId>` or `Vec<EntityId>` field | A foreign key plus a `by_field` index. `on_delete` is `restrict` (the default), `cascade` or `set_null`. |
| `#[unique]` | field of a redb key type, or an `Option` or `Vec` of one | Rejects duplicate values and adds `by_field` and `range_by_field` lookups. |
| `#[index]` | field of a redb key type, or an `Option` or `Vec` of one | Adds `by_field`, `ids_by_field` and `range_by_field` lookups. |
| `#[index(name: Key = expr)]` | struct | An index of keys computed from each row, see [Query](#query). Options: `unique`, and `fingerprint = "label"`, to change when a function the expression calls changes what it returns. |
| `#[embed]` / `#[orangedb::embed]` | struct or enum | A type entities can store in a field. `#[embed]` inside the schema module, `#[orangedb::embed]` anywhere else. |
| `#[fingerprint = "label"]` | field | Declares the stored format of a field whose type has no fingerprint, e.g. one stored with `#[serde(with = ...)]`. Change the label when the format changes. |
| `#[migrate]` | fn in the schema module | Migrates one entity, or the whole database, from the previous schema, see [Fingerprints and migrations](#fingerprints-and-migrations). |

## Stored types

Every field of an entity needs a type orangedb knows how to fingerprint:
integers, floats, `bool`, `char`, `String`, `Duration`,
`SystemTime`, ids, and `Option`, `Box`, tuples, arrays, sequences and maps of
those. Your own structs and enums get a fingerprint from `#[embed]`, which
also derives serde for them:

```rust
#[orangedb::schema]
pub mod shop {
    #[embed]
    pub enum Price {
        Fixed { cents: u64 },
        OnRequest,
    }

    #[entity]
    pub struct Item {
        #[id(auto)]
        pub id: ItemId,
        pub price: Price,
    }
}
```

Any other type is a compile error. Recursive embedded types aren't supported.

Rows are stored with postcard, which writes fields by position and has no
field names or type tags. So serde attributes that only rename things are
fine, `#[serde(skip)]` is fine, and the ones that need a self-describing
format (`flatten`, `untagged`, `tag`) or that read rows back differently from
how they were written (`skip_serializing_if`, ...) are rejected.

## Fingerprints and migrations

Each entity has a fingerprint, computed at compile time from everything that
decides how its rows are stored and indexed: field names, order and types
(through the fingerprints of embedded types, all the way down), enum variants,
the serde attributes that change the encoding, the id kind and key type, and
references. `on_delete`, docs, derives, accessors and indexes don't count.
The database records each entity's fingerprint, and it is the entity's
version: there is no number to bump.

Indexes, unique or not, have fingerprints of their own, in the names of their
tables: of the field, or of the expression's tokens and key type. Opening a
database builds the index tables it doesn't have from the rows and drops the
ones the schema doesn't declare, so adding, changing or removing an index
needs no migration, and reopening with the same indexes builds nothing. If a
new unique index finds a duplicate, opening fails with `Error::Unique` and
changes nothing.

Opening a database that stores a different fingerprint runs the schema's
`#[migrate]` functions, which name the previous schema by their argument
types. Keep the old schema module around for them. The usual kind migrates
one entity, a row at a time:

```rust
#[orangedb::schema]
pub mod v1 {
    #[entity]
    pub struct User {
        #[id(auto)]
        pub id: UserId,
        pub name: String,
    }
}

#[orangedb::schema]
pub mod v2 {
    use super::v1;

    #[migrate]
    fn user_from_v1(old: v1::User) -> orangedb::Result<User> {
        let (first, last) = old.name.split_once(' ').unwrap_or((&old.name, ""));
        Ok(User {
            id: UserId(old.id.0),
            name: Name { first: first.into(), last: last.into() },
        })
    }

    #[embed]
    pub struct Name {
        pub first: String,
        pub last: String,
    }

    #[entity]
    pub struct User {
        #[id(auto)]
        pub id: UserId,
        pub name: Name,
    }
}

let db = v2::Db::open("app.redb")?; // migrates a v1 file, opens a v2 one
```

Every entity that changed needs one, named after it, from the old entity of
the same name to the new one. It's a compile error to leave one out, or to
write one for an entity that didn't change. Rows are read in pages and
written one by one, so a migration doesn't hold a whole table in memory.

Some migrations can't be done one row at a time: dropping or renaming an
entity, or giving rows new ids that other entities' rows have to follow.
For those, there's a function for the whole schema:

```rust
#[migrate]
fn from_v1(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
    for draft in old.drafts().all()? {
        new.notes().create(|id| Note { id, body: draft.body })?;
    }
    Ok(())
}
```

It reads the whole database as the old schema left it and writes it at the
new one. A schema has at most one, and it is required when an entity of the
old schema is gone (one that just returns `Ok(())` drops it). With it,
changed entities don't need functions of their own: the global function
migrates whichever of them have none. Prefer the per-entity functions
anyway. They say exactly what changed, they don't load whole tables like
`all()` does, and the compiler checks that they cover every change. When a
schema has both kinds, the per-entity functions run first, so the global
function finds their rows already in `new`.

- It all happens in one transaction: if a function returns an error (for
  instance `Error::migration("...")`) or a constraint fails, the database
  stays at the old schema.
- Each entity's tables are named after its fingerprint, so entities that
  didn't change are already there, and the changed ones start out empty.
  Afterwards the old layouts' tables are dropped, along with entities the
  new schema no longer declares. Auto-increment sequences carry over.
- References are checked once the migration is done, not on every write,
  so rows can be written in any order, which matters for ids that change and
  for entities that reference themselves.
- Entities the new schema adds start out empty and need no function.
- Migrations chain: if the database is older than `v1`, `v1`'s own
  `#[migrate]` functions bring it to `v1` first. Each step commits on its own.
- Without a `#[migrate]` function, or if no chain of them leads from the
  stored schema, opening fails with `Error::SchemaMismatch`.

Adding an entity needs no migration, and neither does opening a database
with a schema that only declares some of its entities, as long as none of
the ones it leaves out reference one it declares: deleting a row would skip
their `on_delete` rules. Opening then fails with `Error::Undeclared`, unless
a global `#[migrate]` function drops the entity (one that just returns
`Ok(())` will do, if nothing else changed).

## Inspecting a database

Opening a database records a catalog next to the fingerprints: for every
entity layout, the tables its rows and indexes live in, its id, stored
fields, references and indexes, and the source of the schema module
that declared it. Layouts the database was migrated away from stay in the
catalog, so it is also the file's schema history. Read it with
`db.stored_catalog()?`, or just the source with
`db.stored_source::<Entity>()?`; pass an entity from an older schema module
to see what a migrated layout looked like, or use
`db.stored_source_at(name, fingerprint)?` when that type is gone.

The source is captured at compile time as written (comments included), and
as the token stream for modules generated by `macro_rules!`. Note that it is
compiled into your binary, `#[migrate]` functions and all. It is
informational only: changing docs or derives changes the stored text without
changing the fingerprint, and embedded types declared outside the schema
module with `#[orangedb::embed]` are not part of it. A layout that several
schema modules declare, because an entity didn't change between them,
records the source of the last one that opened the database.

The `orangedb` command-line tool reads the catalog, so it needs none of your
types:

```sh
cargo install orangedb-cli
orangedb shop.redb                   # entities: ids, rows, references, indexes
orangedb shop.redb source --all      # the schema source of every layout
orangedb shop.redb history           # every layout, current or migrated away from
orangedb shop.redb tables            # every redb table, its types, entries and owner
orangedb shop.redb refs Customer     # foreign keys from or to Customer
orangedb shop.redb indexes           # foreign key, unique and plain index tables
orangedb shop.redb count             # rows per entity
orangedb shop.redb dump Customer     # rows, decoded as far as the recorded types allow
orangedb shop.redb dump Order.customer   # an index, or any table by name
```

## A complete example

The end-to-end suite in [`orangedb/tests/e2e`](https://github.com/andunieee/orangedb/tree/master/orangedb/tests/e2e) covers the
whole lifecycle: every id kind and reference shape, each `on_delete` rule,
index lookups, constraint failures, transactions, and reopening a database
file. Run it with:

```sh
cargo test --test e2e
```

## License

MIT OR Apache-2.0
