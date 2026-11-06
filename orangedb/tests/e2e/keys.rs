//! Every kind of id, generated accessor names and the `Entity` impl.

use orangedb::{Entity, Error};

#[orangedb::schema]
pub mod catalog {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Category {
        #[id(String)]
        pub slug: CategorySlug,
        pub title: String,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Batch {
        #[id(u32)]
        pub number: BatchNumber,
        /// No `on_delete`: restrict.
        #[references(Category)]
        pub category: CategorySlug,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Address {
        #[id((u32, u32))]
        pub cell: AddressCell,
        pub label: String,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct BlogPost {
        #[id(uuid)]
        pub id: BlogPostId,
        #[references(Category, on_delete = set_null)]
        pub tags: Vec<CategorySlug>,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Day {
        #[id(i64)]
        pub ordinal: DayOrdinal,
    }

    #[entity(accessor = entries)]
    #[derive(Debug, Clone, PartialEq)]
    pub struct LogEntry {
        #[id(auto)]
        pub id: LogEntryId,
        #[references(Batch, on_delete = cascade)]
        pub batch: Option<BatchNumber>,
    }
}

use catalog::*;

fn category(slug: &str) -> Category {
    Category {
        slug: CategorySlug::new(slug),
        title: slug.to_uppercase(),
    }
}

#[test]
fn entity_impl() {
    assert_eq!(Category::NAME, "Category");
    assert_eq!(BlogPost::NAME, "BlogPost");
    assert_eq!(LogEntry::NAME, "LogEntry");
    let prints = [
        Category::FINGERPRINT,
        Batch::FINGERPRINT,
        Address::FINGERPRINT,
        BlogPost::FINGERPRINT,
        Day::FINGERPRINT,
        LogEntry::FINGERPRINT,
    ];
    for (i, a) in prints.iter().enumerate() {
        for b in &prints[i + 1..] {
            assert_ne!(a, b);
        }
    }

    let c = category("tools");
    assert_eq!(c.id(), &CategorySlug::new("tools"));
    let e = LogEntry {
        id: LogEntryId(4),
        batch: None,
    };
    assert_eq!(*e.id(), LogEntryId(4));
}

#[test]
fn id_newtypes() {
    // Auto and uuid ids are Copy; all of them convert from their inner type.
    fn is_copy<T: Copy>(_: T) {}
    is_copy(LogEntryId(1));
    is_copy(BlogPostId::generate());
    assert_eq!(LogEntryId::from(3), LogEntryId(3));
    assert_eq!(BatchNumber::from(7), BatchNumber::new(7u32));
    assert_eq!(CategorySlug::from("x".to_string()), CategorySlug::new("x"));
    assert_eq!(AddressCell::new((1, 2)).0, (1, 2));
    assert!(LogEntryId(1) < LogEntryId(2));
    assert_eq!(format!("{:?}", CategorySlug::new("x")), r#"CategorySlug("x")"#);

    let set: std::collections::HashSet<_> = [DayOrdinal(1), DayOrdinal(1), DayOrdinal(2)].into();
    assert_eq!(set.len(), 2);
}

backends!(crate::keys::catalog =>
    fn accessor_names(db) {
        // Plural snake case, or the `accessor` override.
        assert!(db.categories().is_empty().unwrap());
        assert!(db.batches().is_empty().unwrap());
        assert!(db.addresses().is_empty().unwrap());
        assert!(db.blog_posts().is_empty().unwrap());
        assert!(db.days().is_empty().unwrap());
        assert!(db.entries().is_empty().unwrap());
    }

    fn string_keys(db) {
        for slug in ["tools", "garden", "art", "arts"] {
            db.categories().insert(&category(slug)).unwrap();
        }
        let slugs: Vec<_> = db.categories().all().unwrap().into_iter().map(|c| c.slug.0).collect();
        assert_eq!(slugs, ["art", "arts", "garden", "tools"]);

        let err = db.categories().insert(&category("art")).unwrap_err();
        assert!(matches!(&err, Error::AlreadyExists { entity: "Category", id } if id == r#"CategorySlug("art")"#), "{err}");
        assert_eq!(err.to_string(), r#"Category CategorySlug("art") already exists"#);

        let slug = CategorySlug::new("garden");
        assert_eq!(db.categories().get(&slug).unwrap(), Some(category("garden")));
        assert_eq!(db.categories().get(CategorySlug::new("nope")).unwrap(), None);
        db.categories()
            .update(&Category {
                title: "Garden & Outdoors".into(),
                ..category("garden")
            })
            .unwrap();
        assert_eq!(db.categories().get(&slug).unwrap().unwrap().title, "Garden & Outdoors");
        assert!(db.categories().delete(&slug).unwrap());
        assert!(!db.categories().contains(slug).unwrap());

        // Empty and unicode keys are keys like any other.
        db.categories().insert(&category("")).unwrap();
        db.categories().insert(&category("café")).unwrap();
        assert!(db.categories().contains(CategorySlug::new("")).unwrap());
        assert_eq!(db.categories().get(CategorySlug::new("café")).unwrap().unwrap().title, "CAFÉ");
    }

    fn integer_and_tuple_keys(db) {
        db.categories().insert(&category("tools")).unwrap();
        for n in [300, 2, 10, 0, u32::MAX] {
            db.batches()
                .insert(&Batch {
                    number: BatchNumber(n),
                    category: CategorySlug::new("tools"),
                })
                .unwrap();
        }
        let numbers: Vec<u32> = db.batches().all().unwrap().into_iter().map(|b| b.number.0).collect();
        assert_eq!(numbers, [0, 2, 10, 300, u32::MAX]);

        for n in [5, -3, 0, i64::MIN] {
            db.days().insert(&Day { ordinal: DayOrdinal(n) }).unwrap();
        }
        let days: Vec<i64> = db.days().all().unwrap().into_iter().map(|d| d.ordinal.0).collect();
        assert_eq!(days, [i64::MIN, -3, 0, 5]);

        for cell in [(2, 1), (1, 9), (1, 2)] {
            db.addresses()
                .insert(&Address {
                    cell: AddressCell(cell),
                    label: format!("{cell:?}"),
                })
                .unwrap();
        }
        let cells: Vec<_> = db.addresses().all().unwrap().into_iter().map(|a| a.cell.0).collect();
        assert_eq!(cells, [(1, 2), (1, 9), (2, 1)]);
        assert_eq!(db.addresses().get(AddressCell((1, 9))).unwrap().unwrap().label, "(1, 9)");
        assert!(matches!(
            db.addresses().insert(&Address {
                cell: AddressCell((2, 1)),
                label: "again".into()
            }),
            Err(Error::AlreadyExists { entity: "Address", .. })
        ));
    }

    fn natural_keys_as_references(db) {
        db.categories().insert(&category("tools")).unwrap();
        db.categories().insert(&category("toys")).unwrap();
        let batch = Batch {
            number: BatchNumber(1),
            category: CategorySlug::new("tools"),
        };
        db.batches().insert(&batch).unwrap();
        let err = db
            .batches()
            .insert(&Batch {
                number: BatchNumber(2),
                category: CategorySlug::new("food"),
            })
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"Batch.category references Category CategorySlug("food"), which does not exist"#
        );
        assert_eq!(db.batches().by_category(CategorySlug::new("tools")).unwrap(), vec![batch.clone()]);
        assert_eq!(db.batches().ids_by_category(&CategorySlug::new("tools")).unwrap(), vec![batch.number.clone()]);

        // Restrict is the default.
        let err = db.categories().delete(CategorySlug::new("tools")).unwrap_err();
        assert!(matches!(err, Error::Restricted { entity: "Category", referenced_by: "Batch.category", .. }), "{err}");
        assert!(db.categories().delete(CategorySlug::new("toys")).unwrap());

        // An optional reference to a natural key, cascading.
        let e1 = db.entries().create(|id| LogEntry { id, batch: Some(batch.number.clone()) }).unwrap();
        let e2 = db.entries().create(|id| LogEntry { id, batch: None }).unwrap();
        assert_eq!((e1.id, e2.id), (LogEntryId(1), LogEntryId(2)));
        assert_eq!(db.entries().by_batch(BatchNumber(1)).unwrap(), vec![e1]);
        db.batches().delete(BatchNumber(1)).unwrap();
        assert_eq!(db.entries().all().unwrap(), vec![e2]);
        db.categories().delete(CategorySlug::new("tools")).unwrap();

        // A Vec of natural keys, set to null.
        for slug in ["a", "b", "c"] {
            db.categories().insert(&category(slug)).unwrap();
        }
        let tags = |slugs: &[&str]| slugs.iter().map(|s| CategorySlug::new(*s)).collect::<Vec<_>>();
        let post = db.blog_posts().create(|id| BlogPost { id, tags: tags(&["c", "a", "b"]) }).unwrap();
        db.categories().delete(CategorySlug::new("a")).unwrap();
        assert_eq!(db.blog_posts().get(post.id).unwrap().unwrap().tags, tags(&["c", "b"]));
        assert_eq!(db.blog_posts().by_tags(CategorySlug::new("b")).unwrap().len(), 1);
        assert!(db.blog_posts().by_tags(CategorySlug::new("a")).unwrap().is_empty());
    }
);
