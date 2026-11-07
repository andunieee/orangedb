//! Runs the `orangedb` binary against a shop database that went through a
//! migration.

use std::path::Path;
use std::process::Command;

use orangedb::Entity;

pub mod shop {
    #[orangedb::schema]
    pub mod v1 {
        #[entity]
        pub struct Customer {
            #[id(auto)]
            pub id: CustomerId,
            #[unique]
            pub email: String,
        }

        #[entity]
        pub struct Note {
            #[id(auto)]
            pub id: NoteId,
            pub text: String,
        }
    }

    #[orangedb::schema]
    pub mod v2 {
        use super::v1;

        #[migrate]
        fn from_v1(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
            for c in old.customers().all()? {
                new.customers().insert(&Customer {
                    id: CustomerId(c.id.0),
                    email: c.email,
                    nickname: None,
                    vip: false,
                })?;
            }
            Ok(())
        }

        #[entity]
        pub struct Customer {
            #[id(auto)]
            pub id: CustomerId,
            #[unique]
            pub email: String,
            #[unique]
            pub nickname: Option<String>,
            pub vip: bool,
        }

        #[embed]
        pub struct Address {
            pub city: String,
        }

        /// Orders keep the customers who placed them.
        #[entity]
        pub struct Order {
            #[id(uuid)]
            pub id: OrderId,
            #[references(Customer, on_delete = restrict)]
            pub customer: CustomerId,
            #[references(Customer, on_delete = set_null)]
            pub gift_for: Option<CustomerId>,
            pub skus: Vec<SkuId>,
            pub ship_to: Address,
            pub total_cents: i64,
        }

        #[entity]
        pub struct Sku {
            #[id(String)]
            pub code: SkuId,
            pub name: String,
            #[references(Sku, on_delete = cascade)]
            pub bundled_in: Vec<SkuId>,
        }
    }
}

use shop::v2;

fn seed(path: &Path) {
    let db = shop::v1::Db::open(path).unwrap();
    db.customers()
        .create(|id| shop::v1::Customer {
            id,
            email: "ada@example.com".into(),
        })
        .unwrap();
    db.notes()
        .create(|id| shop::v1::Note {
            id,
            text: "dropped by v2".into(),
        })
        .unwrap();
    drop(db);

    let db = v2::Db::open(path).unwrap();
    let grace = db
        .customers()
        .create(|id| v2::Customer {
            id,
            email: "grace@example.com".into(),
            nickname: Some("amazing grace".into()),
            vip: true,
        })
        .unwrap();
    db.skus()
        .insert(&v2::Sku {
            code: v2::SkuId::new("LAMP"),
            name: "Desk lamp".into(),
            bundled_in: vec![],
        })
        .unwrap();
    db.orders()
        .create(|id| v2::Order {
            id,
            customer: grace.id,
            gift_for: Some(v2::CustomerId(1)),
            skus: vec![v2::SkuId::new("LAMP")],
            ship_to: v2::Address {
                city: "Arlington".into(),
            },
            total_cents: -250,
        })
        .unwrap();
    drop(db);

    // A table orangedb doesn't own.
    let db = orangedb::redb::Database::open(path).unwrap();
    let tx = db.begin_write().unwrap();
    tx.open_table(orangedb::redb::TableDefinition::<u32, &str>::new("app.settings"))
        .unwrap()
        .insert(7, "dark mode")
        .unwrap();
    tx.commit().unwrap();
}

fn run(path: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_orangedb"))
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    (out.status.success(), stdout, stderr)
}

fn orangedb(path: &Path, args: &[&str]) -> String {
    let (ok, stdout, stderr) = run(path, args);
    assert!(ok, "orangedb {args:?} failed:\n{stderr}");
    stdout
}

fn fails(path: &Path, args: &[&str]) -> String {
    let (ok, stdout, stderr) = run(path, args);
    assert!(!ok, "orangedb {args:?} succeeded:\n{stdout}");
    stderr
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shop.redb");
    seed(&path);
    (dir, path)
}

/// The line of `out` that starts with `start`.
fn line<'a>(out: &'a str, start: &str) -> &'a str {
    out.lines()
        .find(|l| l.trim_start().starts_with(start))
        .unwrap_or_else(|| panic!("no line starting with {start:?} in:\n{out}"))
}

/// The cells of a table line, split at runs of two spaces or more.
fn cells(line: &str) -> Vec<&str> {
    line.split("  ").map(str::trim).filter(|c| !c.is_empty()).collect()
}

fn rows(entity: &str, fingerprint: u64) -> String {
    format!("{entity}@{fingerprint:016x}")
}

#[test]
fn entities_describe_the_current_layouts() {
    let (_dir, path) = fixture();
    let out = orangedb(&path, &[]);
    assert_eq!(out, orangedb(&path, &["entities"]));

    let customer = rows("Customer", v2::Customer::FINGERPRINT);
    assert_eq!(
        line(&out, "Customer @"),
        format!("Customer @{:016x}: 2 rows", v2::Customer::FINGERPRINT)
    );
    assert_eq!(cells(line(&out, "id  ")), ["id", "id auto, next 3", customer.as_str()]);
    assert_eq!(
        cells(line(&out, "nickname")),
        [
            "nickname",
            "unique Option<String>",
            &format!("{customer}.nickname.unique")
        ]
    );
    assert_eq!(
        cells(line(&out, "gift_for")),
        [
            "gift_for",
            "-> Option<Customer> on_delete = set_null",
            &format!("{}.gift_for.fk", rows("Order", v2::Order::FINGERPRINT))
        ]
    );
    assert_eq!(
        cells(line(&out, "code")),
        ["code", "id String", &rows("Sku", v2::Sku::FINGERPRINT)]
    );
    // Note was dropped by the migration.
    assert!(!out.contains("Note"), "{out}");
}

#[test]
fn source_prints_each_module_once() {
    let (_dir, path) = fixture();
    let out = orangedb(&path, &["source"]);
    assert_eq!(out.matches("pub mod v2 {").count(), 1, "{out}");
    assert!(out.starts_with("// Customer@"), "{out}");
    assert!(out.contains("/// Orders keep the customers who placed them."), "{out}");
    // Nested in `shop`, but printed as if it weren't.
    assert!(out.contains("\n    #[entity]\n    pub struct Customer {"), "{out}");
    assert!(!out.contains("pub mod v1"), "{out}");

    let all = orangedb(&path, &["source", "--all"]);
    assert!(all.contains("pub mod v1 {"), "{all}");
    assert!(all.contains("// Note@") && all.contains("(migrated)"), "{all}");
    // The current schema comes last.
    assert!(
        all.find("pub mod v1").unwrap() < all.find("pub mod v2").unwrap(),
        "{all}"
    );

    let note = orangedb(&path, &["source", "Note", "--all"]);
    assert!(note.contains("pub struct Note"), "{note}");
    assert_eq!(
        note,
        orangedb(&path, &["source", &rows("Note", shop::v1::Note::FINGERPRINT)])
    );
}

#[test]
fn history_lists_every_layout() {
    let (_dir, path) = fixture();
    let out = orangedb(&path, &["history"]);
    let fp = |f: u64| format!("{f:016x}");
    let row = |entity: &str, f: u64| {
        out.lines()
            .map(cells)
            .find(|c| c[0] == entity && c[1] == fp(f))
            .unwrap_or_else(|| panic!("no {entity}@{} in:\n{out}", fp(f)))
    };
    assert_eq!(row("Customer", shop::v1::Customer::FINGERPRINT)[2..3], ["migrated"]);
    assert!(row("Customer", shop::v1::Customer::FINGERPRINT)[3].starts_with("mod v1"));
    assert_eq!(row("Customer", v2::Customer::FINGERPRINT)[2..3], ["current"]);
    assert_eq!(row("Note", shop::v1::Note::FINGERPRINT)[2..3], ["migrated"]);
    assert!(row("Order", v2::Order::FINGERPRINT)[3].starts_with("mod v2"));
}

#[test]
fn tables_name_every_table_and_what_it_holds() {
    let (_dir, path) = fixture();
    let out = orangedb(&path, &["tables"]);
    let order = rows("Order", v2::Order::FINGERPRINT);
    assert_eq!(
        cells(line(&out, &format!("{order}.customer.fk"))),
        [
            format!("{order}.customer.fk").as_str(),
            "multimap",
            "orangedb::Customer.id",
            "orangedb::Order.id",
            "1",
            "Order.customer -> Customer index"
        ]
    );
    assert_eq!(
        cells(line(&out, "__orangedb.source")),
        ["__orangedb.source", "table", "u64", "String", "2", "orangedb metadata"]
    );
    assert_eq!(
        cells(line(&out, "app.settings")),
        ["app.settings", "table", "u32", "&str", "1", "-"]
    );
    // The migration dropped every table of the old layouts.
    assert!(
        !out.contains(&rows("Customer", shop::v1::Customer::FINGERPRINT)),
        "{out}"
    );
}

#[test]
fn count_refs_and_indexes() {
    let (_dir, path) = fixture();
    assert_eq!(orangedb(&path, &["count"]), "Customer  2\nOrder     1\nSku       1\n");
    assert_eq!(orangedb(&path, &["count", "Sku"]), "Sku  1\n");

    let refs = orangedb(&path, &["refs"]);
    assert_eq!(
        cells(line(&refs, "Order.customer"))[..4],
        ["Order.customer", "Customer", "restrict", "1"]
    );
    assert_eq!(
        cells(line(&refs, "Sku.bundled_in"))[..4],
        ["Sku.bundled_in", "Vec<Sku>", "cascade", "0"]
    );
    // References into Customer, from Order.
    let into = orangedb(&path, &["refs", "Customer"]);
    assert_eq!(into.lines().count(), 3, "{into}");
    assert_eq!(orangedb(&path, &["refs", "Sku"]).lines().count(), 2);

    let indexes = orangedb(&path, &["indexes", "Customer"]);
    assert_eq!(
        cells(line(&indexes, "Customer@"))[1..],
        ["unique", "Customer.email", "2"]
    );
    assert_eq!(indexes.lines().count(), 3, "{indexes}");
}

#[test]
fn dump_decodes_rows() {
    let (_dir, path) = fixture();
    let out = orangedb(&path, &["dump", "Customer"]);
    assert_eq!(
        cells(line(&out, "2  ")),
        [
            "2",
            r#"{ id: 2, email: "grace@example.com", nickname: Some("amazing grace"), vip: true }"#
        ]
    );

    let out = orangedb(&path, &["dump", "Sku"]);
    assert_eq!(
        cells(line(&out, "\"LAMP\"")),
        ["\"LAMP\"", r#"{ code: "LAMP", name: "Desk lamp", bundled_in: [] }"#]
    );

    // Decoding stops at the embedded type, whose layout isn't recorded.
    let out = orangedb(&path, &["dump", "Order"]);
    let row = cells(out.lines().nth(1).unwrap())[1];
    assert!(
        row.contains(r#"customer: 2, gift_for: Some(1), skus: ["LAMP"], ship_to: ?<Address>, … 0x"#),
        "{row}"
    );
}

#[test]
fn dump_reads_indexes_and_other_tables() {
    let (_dir, path) = fixture();
    let out = orangedb(&path, &["dump", "Customer.email"]);
    assert!(
        out.starts_with("Customer@") && out.contains(".email.unique (String -> orangedb::Customer.id)\n"),
        "{out}"
    );
    assert_eq!(cells(line(&out, "\"ada@")), ["\"ada@example.com\"", "1"]);

    let out = orangedb(&path, &["dump", "Order.gift_for"]);
    assert!(cells(out.lines().nth(1).unwrap())[0] == "1", "{out}");

    let out = orangedb(&path, &["dump", "app.settings"]);
    assert_eq!(out, "app.settings (u32 -> &str)\n7  \"dark mode\"\n");

    let out = orangedb(&path, &["dump", "__orangedb.meta", "-n", "1"]);
    assert!(
        out.ends_with("… showing 1 of 4 entries; pass --limit to see more\n"),
        "{out}"
    );
}

#[test]
fn mistakes_are_explained() {
    let (_dir, path) = fixture();
    let err = fails(&path, &["dump", "Nope"]);
    assert!(
        err.contains("no entity layout `Nope`; the entities are: Customer, Order, Sku"),
        "{err}"
    );
    let err = fails(&path, &["dump", "Customer.vip"]);
    assert!(
        err.contains("Customer.vip is neither a reference nor a unique field"),
        "{err}"
    );
    let old = rows("Customer", shop::v1::Customer::FINGERPRINT);
    let err = fails(&path, &["dump", &old]);
    assert!(err.contains("migrated away from"), "{err}");
    let err = fails(&path, &["source", "Customer@xyz"]);
    assert!(err.contains("isn't a fingerprint"), "{err}");
}

#[test]
fn a_plain_redb_file_has_tables_but_no_entities() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.redb");
    let db = orangedb::redb::Database::create(&path).unwrap();
    let tx = db.begin_write().unwrap();
    tx.open_table(orangedb::redb::TableDefinition::<&str, u64>::new("hits"))
        .unwrap()
        .insert("/", 3)
        .unwrap();
    tx.commit().unwrap();
    drop(db);

    let err = fails(&path, &[]);
    assert!(err.contains("this isn't an orangedb database"), "{err}");
    assert_eq!(
        cells(line(&orangedb(&path, &["tables"]), "hits")),
        ["hits", "table", "&str", "u64", "1", "-"]
    );
    assert_eq!(orangedb(&path, &["dump", "hits"]), "hits (&str -> u64)\n\"/\"  3\n");
}
