//! Writes that fail after their constraint checks, here on encoding, must not
//! leave the indexes out of step with the rows.

use orangedb::Error;

#[orangedb::schema]
pub mod shelf {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Owner {
        #[id(auto)]
        pub id: OwnerId,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Item {
        #[id(auto)]
        pub id: ItemId,
        #[references(Owner, on_delete = cascade)]
        pub owner: OwnerId,
        #[unique]
        pub code: String,
        /// Refuses to encode the value "poison".
        #[fingerprint = "refusing string"]
        #[serde(serialize_with = "super::refuse_poison")]
        pub note: String,
    }
}

use shelf::*;

pub fn refuse_poison<S: serde::Serializer>(note: &str, s: S) -> Result<S::Ok, S::Error> {
    if note == "poison" {
        return Err(serde::ser::Error::custom("poisoned note"));
    }
    s.serialize_str(note)
}

backends!(crate::failed_writes::shelf =>
    fn a_failed_encode_keeps_the_indexes(db) {
        let (owner, item) = db
            .write(|tx| {
                let owner = tx.owners().create(|id| Owner { id })?;
                let item = tx.items().create(|id| Item { id, owner: owner.id, code: "a".into(), note: "fine".into() })?;
                // The update fails while encoding, and the caller carries on.
                let poisoned = Item { note: "poison".into(), ..item.clone() };
                assert!(matches!(tx.items().update(&poisoned), Err(Error::Codec { .. })));
                Ok::<_, Error>((owner, item))
            })
            .unwrap();

        assert_eq!(db.items().get(item.id).unwrap(), Some(item.clone()));
        assert_eq!(db.items().by_code("a").unwrap(), Some(item.clone()));
        assert_eq!(db.items().by_owner(owner.id).unwrap(), vec![item]);
        assert_eq!(db.items().next_id().unwrap(), ItemId(2));
    }

    fn a_failed_encode_can_be_aborted(db) {
        let result: Result<(), Error> = db.write(|tx| {
            let owner = tx.owners().create(|id| Owner { id })?;
            let item = tx.items().create(|id| Item { id, owner: owner.id, code: "a".into(), note: "fine".into() })?;
            // The update fails while encoding; the caller catches the error and aborts.
            let poisoned = Item { note: "poison".into(), ..item };
            match tx.items().update(&poisoned) {
                Ok(()) => panic!("the poisoned note was encoded"),
                Err(e @ Error::Codec { .. }) => Err(e),
                Err(e) => panic!("expected a codec error, got {e:?}"),
            }
        });
        assert!(matches!(result, Err(Error::Codec { .. })));

        // The abort leaves neither the rows nor their index entries behind.
        assert!(db.owners().is_empty().unwrap());
        assert!(db.items().is_empty().unwrap());
        assert_eq!(db.items().by_code("a").unwrap(), None);
        assert_eq!(db.items().by_owner(OwnerId(1)).unwrap(), vec![]);
        assert_eq!(db.items().next_id().unwrap(), ItemId(1));
    }
);
