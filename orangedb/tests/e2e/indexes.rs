//! `#[index]` fields and `#[index(name: K = expr)]` on entities: lookups,
//! ranges, and what keeps them in step with the rows. Then, on a file, how
//! opening a database builds, rebuilds and drops index tables without a
//! migration.

use std::path::Path;

use orangedb::{Entity, Error};

use crate::index_migrations::index_tables;
use crate::migrations::table_names;

#[orangedb::schema]
pub mod library {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    #[index(title_lower: String = |b| b.title.to_lowercase())]
    #[index(word: String = |b| b.title.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>())]
    #[index(isbn: String = |b| b.isbn.as_ref().map(|i| i.replace('-', "")), unique)]
    #[index(shelf_year: (u32, u16) = |b| (b.shelf, b.year))]
    #[index(initial: char = initial)]
    pub struct Book {
        #[id(auto)]
        pub id: BookId,
        pub title: String,
        /// As printed, with or without dashes; the `isbn` index has none.
        pub isbn: Option<String>,
        #[index]
        pub year: u16,
        #[index]
        pub tags: Vec<String>,
        #[index]
        pub series: Option<String>,
        pub shelf: u32,
    }

    /// An index expression can be a function, private or not.
    fn initial(book: &Book) -> Option<char> {
        book.title.chars().next().map(|c| c.to_ascii_uppercase())
    }
}

use library::*;

fn book(title: &str, year: u16, shelf: u32) -> impl FnOnce(BookId) -> Book {
    move |id| Book {
        id,
        title: title.into(),
        isbn: None,
        year,
        tags: Vec::new(),
        series: None,
        shelf,
    }
}

fn ids(books: Vec<Book>) -> Vec<u64> {
    books.into_iter().map(|b| b.id.0).collect()
}

backends!(crate::indexes::library =>
    fn field_indexes(db) {
        let dune = db.books().create(|id| Book {
            tags: vec!["sf".into(), "classic".into()],
            series: Some("Dune".into()),
            ..book("Dune", 1965, 1)(id)
        }).unwrap();
        let messiah = db.books().create(|id| Book {
            tags: vec!["sf".into()],
            series: Some("Dune".into()),
            ..book("Dune Messiah", 1969, 1)(id)
        }).unwrap();
        let emma = db.books().create(|id| Book {
            tags: vec!["classic".into()],
            ..book("Emma", 1815, 2)(id)
        }).unwrap();
        db.books().create(book("Solaris", 1965, 3)).unwrap();

        // Several rows per key, in id order.
        assert_eq!(ids(db.books().by_year(1965u16).unwrap()), [1, 4]);
        assert_eq!(db.books().ids_by_year(1969u16).unwrap(), [messiah.id]);
        assert!(db.books().by_year(2000u16).unwrap().is_empty());
        // Each element of a Vec, and the Some of an Option.
        assert_eq!(ids(db.books().by_tags("classic").unwrap()), [dune.id.0, emma.id.0]);
        assert_eq!(ids(db.books().by_series("Dune").unwrap()), [1, 2]);

        // Ranges go in key order, then id order.
        assert_eq!(ids(db.books().range_by_year(1900u16..1970).unwrap()), [1, 4, 2]);
        assert_eq!(ids(db.books().range_by_year(..1900u16).unwrap()), [3]);
        assert_eq!(ids(db.books().range_by_year(1965u16..=1965).unwrap()), [1, 4]);
        assert_eq!(ids(db.books().range_by_tags("a".to_string().."d".to_string()).unwrap()), [1, 3]);
    }

    fn computed_indexes(db) {
        let dune = db.books().create(|id| Book {
            isbn: Some("978-0-441-17271-9".into()),
            ..book("Dune", 1965, 1)(id)
        }).unwrap();
        db.books().create(book("The Left Hand of Darkness", 1969, 3)).unwrap();
        db.books().create(book("the dispossessed", 1974, 3)).unwrap();
        db.books().create(book("Solaris", 1961, 2)).unwrap();

        // The key the expression computes, not the field.
        assert_eq!(ids(db.books().by_title_lower("dune").unwrap()), [dune.id.0]);
        assert!(db.books().by_title_lower("Dune").unwrap().is_empty());
        // A Vec of keys: every word of every title.
        assert_eq!(ids(db.books().by_word("the").unwrap()), [2, 3]);
        assert_eq!(ids(db.books().by_word("darkness").unwrap()), [2]);
        // In key order: "darkness", "dispossessed", "dune"; book 2's "hand"
        // and "left" are in the range too, but it is listed once.
        assert_eq!(ids(db.books().range_by_word("d".to_string().."m".to_string()).unwrap()), [2, 3, 1]);

        // A unique computed key: the same ISBN with other dashes is taken.
        assert_eq!(db.books().by_isbn("9780441172719").unwrap(), Some(dune.clone()));
        let err = db.books().create(|id| Book {
            isbn: Some("9780-441-172719".into()),
            ..book("Dune (reprint)", 2005, 1)(id)
        }).unwrap_err();
        assert!(matches!(&err, Error::Unique { entity: "Book", field: "isbn", value, owner }
            if value == r#""9780441172719""# && owner == "BookId(1)"), "{err}");
        // No ISBN, no key: rows without one don't conflict.
        db.books().create(book("Untitled", 2001, 9)).unwrap();
        assert_eq!(db.books().len().unwrap(), 5);

        // A tuple key orders by its parts, so a range over one shelf is
        // that shelf's books by year.
        assert_eq!(
            ids(db.books().range_by_shelf_year((3u32, 0u16)..(4, 0)).unwrap()),
            [2, 3]
        );
        assert_eq!(ids(db.books().by_shelf_year((2u32, 1961u16)).unwrap()), [4]);
        assert_eq!(ids(db.books().by_initial('T').unwrap()), [2, 3]);
    }

    /// A non-unique index holds (key, id) pairs: changing or deleting one
    /// row removes its own pairs and leaves other rows with the same keys.
    fn writes_move_only_their_own_entries(db) {
        let a = db.books().create(book("The Hobbit", 1937, 1)).unwrap();
        let b = db.books().create(book("The Silmarillion", 1977, 1)).unwrap();
        let c = db.books().create(book("The Road", 1937, 2)).unwrap();

        // Change a's year and title: its old keys go, b's and c's stay.
        db.books().update(&Book { year: 1938, title: "Hobbit".into(), ..a.clone() }).unwrap();
        assert_eq!(ids(db.books().by_year(1937u16).unwrap()), [c.id.0]);
        assert_eq!(ids(db.books().by_year(1938u16).unwrap()), [a.id.0]);
        assert_eq!(ids(db.books().by_word("the").unwrap()), [b.id.0, c.id.0]);
        assert_eq!(ids(db.books().by_word("hobbit").unwrap()), [a.id.0]);

        // Deleting b removes it from the keys it shares with c.
        db.books().delete(b.id).unwrap();
        assert_eq!(ids(db.books().by_word("the").unwrap()), [c.id.0]);
        assert_eq!(ids(db.books().by_shelf_year((1u32, 1938u16)).unwrap()), [a.id.0]);
        assert!(db.books().by_word("silmarillion").unwrap().is_empty());

        // A failed write leaves the indexes as they were.
        let _ = db.write(|tx| {
            tx.books().delete(c.id)?;
            Err::<(), _>(Error::migration("changed my mind"))
        });
        assert_eq!(ids(db.books().by_word("the").unwrap()), [c.id.0]);
    }
);

/// The same `Book` layout with different indexes. None of them changes the
/// fingerprint, so each opens the others' files without a migration.
pub mod shelves {
    macro_rules! shelf {
        ($name:ident $(#[$index:meta])*) => {
            #[orangedb::schema]
            pub mod $name {
                #[entity]
                #[derive(Debug, Clone, PartialEq)]
                $(#[$index])*
                pub struct Book {
                    #[id(auto)]
                    pub id: BookId,
                    pub title: String,
                }
            }
        };
    }

    shelf!(plain);
    shelf!(lower #[index(key: String = |b| b.title.to_lowercase())]);
    shelf!(upper #[index(key: String = |b| b.title.to_uppercase())]);
    // `lower`'s expression, but with a label: as if a function it called
    // had changed.
    shelf!(relabelled #[index(key: String = |b| b.title.to_lowercase(), fingerprint = "2")]);
    shelf!(unique #[index(key: String = |b| b.title.to_lowercase(), unique)]);
}

use shelves::{lower, plain, relabelled, unique, upper};

fn seed(path: &Path, titles: &[&str]) {
    let db = plain::Db::open(path).unwrap();
    for title in titles {
        db.books()
            .create(|id| plain::Book {
                id,
                title: title.to_string(),
            })
            .unwrap();
    }
}

fn table(path: &Path) -> Option<String> {
    table_names(path).into_iter().find(|n| n.contains(".key@"))
}

#[test]
fn indexes_are_not_part_of_the_fingerprint() {
    let fingerprints = [
        plain::Book::FINGERPRINT,
        lower::Book::FINGERPRINT,
        upper::Book::FINGERPRINT,
        relabelled::Book::FINGERPRINT,
        unique::Book::FINGERPRINT,
    ];
    assert!(fingerprints.iter().all(|&f| f == plain::Book::FINGERPRINT));
    assert_eq!(library::Book::FINGERPRINT, {
        #[orangedb::schema]
        mod unindexed {
            #[entity]
            pub struct Book {
                #[id(auto)]
                pub id: BookId,
                pub title: String,
                pub isbn: Option<String>,
                pub year: u16,
                pub tags: Vec<String>,
                pub series: Option<String>,
                pub shelf: u32,
            }
        }
        unindexed::Book::FINGERPRINT
    });
}

#[test]
fn opening_builds_a_new_index_from_the_rows() {
    let dir = crate::tempdir();
    let path = dir.path().join("books.redb");
    seed(&path, &["Dune", "dune", "Emma"]);
    assert!(index_tables(&path).is_empty());

    let db = lower::Db::open(&path).unwrap();
    assert_eq!(
        db.books().ids_by_key("dune").unwrap(),
        [lower::BookId(1), lower::BookId(2)]
    );
    assert_eq!(db.books().ids_by_key("emma").unwrap(), [lower::BookId(3)]);
    drop(db);
    assert_eq!(index_tables(&path), ["Book.key.index"]);

    // Going back drops it: `plain` wouldn't keep it up to date.
    drop(plain::Db::open(&path).unwrap());
    assert!(index_tables(&path).is_empty());
}

#[test]
fn reopening_trusts_the_index_it_finds() {
    use orangedb::redb::MultimapTableDefinition;

    let dir = crate::tempdir();
    let path = dir.path().join("books.redb");
    seed(&path, &["Dune"]);
    drop(lower::Db::open(&path).unwrap());
    let name = table(&path).unwrap();

    // Plant an entry no row has. A rebuild would lose it.
    {
        let redb = orangedb::redb::Database::create(&path).unwrap();
        let tx = redb.begin_write().unwrap();
        let def: MultimapTableDefinition<String, lower::BookId> = MultimapTableDefinition::new(&name);
        tx.open_multimap_table(def)
            .unwrap()
            .insert("planted".to_string(), lower::BookId(1))
            .unwrap();
        tx.commit().unwrap();
    }
    let db = lower::Db::open(&path).unwrap();
    assert_eq!(db.books().ids_by_key("planted").unwrap(), [lower::BookId(1)]);
}

#[test]
fn a_changed_index_is_rebuilt() {
    let dir = crate::tempdir();
    let path = dir.path().join("books.redb");
    seed(&path, &["Dune"]);
    drop(lower::Db::open(&path).unwrap());
    let lower_table = table(&path).unwrap();

    // Another expression: another table, built afresh, and the old one gone.
    let db = upper::Db::open(&path).unwrap();
    assert_eq!(db.books().ids_by_key("DUNE").unwrap(), [upper::BookId(1)]);
    assert!(db.books().ids_by_key("dune").unwrap().is_empty());
    // Written while `lower`'s index doesn't exist.
    db.books()
        .create(|id| upper::Book {
            id,
            title: "Emma".into(),
        })
        .unwrap();
    drop(db);
    let upper_table = table(&path).unwrap();
    assert_ne!(upper_table, lower_table);
    assert_eq!(index_tables(&path), ["Book.key.index"]);

    // So going back rebuilds `lower`'s, and finds the row `upper` wrote.
    let db = lower::Db::open(&path).unwrap();
    assert_eq!(db.books().ids_by_key("emma").unwrap(), [lower::BookId(2)]);
    drop(db);
    assert_eq!(table(&path).unwrap(), lower_table);

    // A label changes the table as the expression would.
    drop(relabelled::Db::open(&path).unwrap());
    let relabelled_table = table(&path).unwrap();
    assert_ne!(relabelled_table, lower_table);
    assert_ne!(relabelled_table, upper_table);
}

#[test]
fn a_unique_index_over_duplicates_fails_to_open() {
    let dir = crate::tempdir();
    let path = dir.path().join("books.redb");
    seed(&path, &["Dune", "Emma", "DUNE"]);
    let before = table_names(&path);

    let err = unique::Db::open(&path).err().unwrap();
    assert!(
        matches!(&err, Error::Unique { entity: "Book", field: "key", value, owner }
            if value == r#""dune""# && owner == "BookId(1)"),
        "{err}"
    );
    assert_eq!(table_names(&path), before);

    // Fix the duplicate and it opens, with the index checking from then on.
    let db = plain::Db::open(&path).unwrap();
    db.books()
        .update(&plain::Book {
            id: plain::BookId(3),
            title: "Dune II".into(),
        })
        .unwrap();
    drop(db);
    let db = unique::Db::open(&path).unwrap();
    assert_eq!(db.books().by_key("dune ii").unwrap().unwrap().id, unique::BookId(3));
    assert!(matches!(
        db.books().create(|id| unique::Book {
            id,
            title: "emma".into()
        }),
        Err(Error::Unique { field: "key", .. })
    ));
}

/// More rows than a rebuild reads at once: every page is indexed, and a
/// duplicate in the last one is still found.
#[test]
fn a_rebuild_reads_every_page() {
    let dir = crate::tempdir();
    let path = dir.path().join("books.redb");
    let titles: Vec<String> = (0..600).map(|i| format!("Book {i}")).collect();
    let mut seeded: Vec<&str> = titles.iter().map(String::as_str).collect();
    seed(&path, &seeded);

    let db = unique::Db::open(&path).unwrap();
    for i in [0, 255, 256, 511, 512, 599] {
        let found = db.books().by_key(format!("book {i}")).unwrap().unwrap();
        assert_eq!(found.id, unique::BookId(i + 1));
    }
    drop(db);

    // A fresh file whose only duplicate is the last row.
    let dir = crate::tempdir();
    let path = dir.path().join("books.redb");
    seeded.push("BOOK 0");
    seed(&path, &seeded);
    let err = unique::Db::open(&path).err().unwrap();
    assert!(
        matches!(&err, Error::Unique { field: "key", owner, .. } if owner == "BookId(1)"),
        "{err}"
    );
}
