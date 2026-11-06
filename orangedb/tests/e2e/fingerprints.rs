//! What does and doesn't change an entity's fingerprint. Each variant
//! module is the baseline with one change.

use std::collections::BTreeSet;

use orangedb::{Entity, Fingerprint};

/// Declared outside any schema.
#[orangedb::embed]
#[derive(Debug, Clone, PartialEq)]
pub struct Money {
    pub cents: u64,
    pub currency: String,
}

#[orangedb::embed]
#[derive(Debug, Clone, PartialEq)]
pub struct Pair<T> {
    pub left: T,
    pub right: T,
}

/// Stores `Money` and a `Pair` of it, by way of an embedded enum declared
/// inside the schema.
macro_rules! shop {
    ($( $(#[$doc:meta])* $module:ident { $($item:tt)* } )*) => {$(
        $(#[$doc])*
        #[orangedb::schema]
        #[allow(dead_code)]
        pub mod $module {
            #[allow(unused_imports)]
            use super::*;

            $($item)*
        }
    )*};
}

shop! {
    baseline {
        #[embed]
        #[derive(Debug, Clone, PartialEq)]
        pub enum Status {
            Draft,
            Live { since: u64 },
            Retired(String),
        }

        #[entity]
        #[derive(Debug, Clone, PartialEq)]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            pub tags: Vec<String>,
        }
    }

    /// Cosmetic: derives, docs, renames that serde doesn't store.
    cosmetic {
        /// Docs.
        #[embed]
        #[serde(rename_all = "camelCase")]
        pub enum Status {
            #[serde(rename = "draft")]
            Draft,
            Live {
                #[serde(alias = "from")]
                since: u64,
            },
            Retired(String),
        }

        #[entity(accessor = catalog)]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            #[serde(rename = "cost")]
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            /// Stored the same way as a `Vec`.
            pub tags: BTreeSet<String>,
        }
    }

    /// A variant moves, which changes every index after it.
    reordered_variant {
        #[embed]
        pub enum Status {
            Live { since: u64 },
            Draft,
            Retired(String),
        }

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            pub tags: Vec<String>,
        }
    }

    /// A field deep inside an embedded type changes type.
    nested_type {
        #[embed]
        pub enum Status {
            Draft,
            Live { since: i64 },
            Retired(String),
        }

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            pub tags: Vec<String>,
        }
    }

    /// A field of an embedded type is renamed.
    nested_rename {
        #[embed]
        pub enum Status {
            Draft,
            Live { started: u64 },
            Retired(String),
        }

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            pub tags: Vec<String>,
        }
    }

    /// A generic embedded type gets a different argument.
    generic_argument {
        #[embed]
        pub enum Status {
            Draft,
            Live { since: u64 },
            Retired(String),
        }

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<u64>,
            pub status: Status,
            pub tags: Vec<String>,
        }
    }

    /// `serde(skip)` takes a field out of the stored row.
    skipped {
        #[embed]
        pub enum Status {
            Draft,
            Live { since: u64 },
            Retired(String),
        }

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            #[serde(skip)]
            pub tags: Vec<String>,
        }
    }

    /// A field with a declared stored format instead of a `Fingerprint`.
    labelled {
        #[embed]
        pub enum Status {
            Draft,
            Live { since: u64 },
            Retired(String),
        }

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            #[fingerprint = "comma-separated/1"]
            #[serde(with = "super::comma_separated")]
            pub tags: Vec<String>,
        }
    }

    /// The same, with the label bumped after changing the format.
    relabelled {
        #[embed]
        pub enum Status {
            Draft,
            Live { since: u64 },
            Retired(String),
        }

        #[entity]
        pub struct Item {
            #[id(auto)]
            pub id: ItemId,
            pub price: Money,
            pub range: Pair<Money>,
            pub status: Status,
            #[fingerprint = "comma-separated/2"]
            #[serde(with = "super::comma_separated")]
            pub tags: Vec<String>,
        }
    }
}

/// Stores a list of strings as one string.
pub mod comma_separated {
    use orangedb::__private::serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(tags: &[String], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&tags.join(","))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
        let joined = String::deserialize(d)?;
        Ok(joined.split(',').filter(|t| !t.is_empty()).map(Into::into).collect())
    }
}

#[test]
fn what_changes_a_fingerprint() {
    let baseline = baseline::Item::FINGERPRINT;
    assert_eq!(cosmetic::Item::FINGERPRINT, baseline);
    let changed = [
        ("reordered_variant", reordered_variant::Item::FINGERPRINT),
        ("nested_type", nested_type::Item::FINGERPRINT),
        ("nested_rename", nested_rename::Item::FINGERPRINT),
        ("generic_argument", generic_argument::Item::FINGERPRINT),
        ("skipped", skipped::Item::FINGERPRINT),
        ("labelled", labelled::Item::FINGERPRINT),
        ("relabelled", relabelled::Item::FINGERPRINT),
    ];
    for (i, (variant, print)) in changed.iter().enumerate() {
        assert_ne!(*print, baseline, "{variant}");
        for (other, other_print) in &changed[i + 1..] {
            assert_ne!(print, other_print, "{variant} vs {other}");
        }
    }
}

#[test]
fn fingerprints_of_embedded_types() {
    assert_ne!(Pair::<u32>::FINGERPRINT, Pair::<u64>::FINGERPRINT);
    assert_ne!(Money::FINGERPRINT, Pair::<u64>::FINGERPRINT);
    // Ids are stored as their key.
    assert_eq!(baseline::ItemId::FINGERPRINT, u64::FINGERPRINT);
    // Boxes are transparent, collections of one kind are alike.
    assert_eq!(Box::<Money>::FINGERPRINT, Money::FINGERPRINT);
    assert_eq!(Vec::<u8>::FINGERPRINT, BTreeSet::<u8>::FINGERPRINT);
    assert_ne!(Vec::<u8>::FINGERPRINT, Option::<u8>::FINGERPRINT);
}

#[test]
fn embedded_values_round_trip() {
    let db = baseline::Db::in_memory().unwrap();
    let usd = |cents| Money {
        cents,
        currency: "USD".into(),
    };
    let item = db
        .items()
        .create(|id| baseline::Item {
            id,
            price: usd(500),
            range: Pair {
                left: usd(100),
                right: usd(900),
            },
            status: baseline::Status::Live { since: 7 },
            tags: vec!["a".into()],
        })
        .unwrap();
    assert_eq!(db.items().get(item.id).unwrap(), Some(item));

    let db = labelled::Db::in_memory().unwrap();
    let item = db
        .items()
        .create(|id| labelled::Item {
            id,
            price: usd(1),
            range: Pair {
                left: usd(1),
                right: usd(2),
            },
            status: labelled::Status::Retired("old".into()),
            tags: vec!["x".into(), "y".into()],
        })
        .unwrap();
    assert_eq!(db.items().get(item.id).unwrap().unwrap().tags, ["x", "y"]);
}
