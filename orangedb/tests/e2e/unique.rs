//! `#[unique]` on plain, optional and integer fields, and on references
//! (one-to-one relations).

use orangedb::Error;

#[orangedb::schema]
pub mod accounts {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Account {
        #[id(auto)]
        pub id: AccountId,
        #[unique]
        pub handle: String,
        #[unique]
        pub phone: Option<String>,
        #[unique]
        pub badge: Option<u32>,
        /// Not unique: shared freely.
        pub display: String,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Profile {
        #[id(auto)]
        pub id: ProfileId,
        /// One profile per account, gone with it.
        #[references(Account, on_delete = cascade)]
        #[unique]
        pub account: AccountId,
        pub bio: String,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Session {
        #[id(uuid)]
        pub id: SessionId,
        #[references(Account, on_delete = cascade)]
        pub account: AccountId,
        #[unique]
        pub token: u64,
    }
}

use accounts::*;

fn account(db: &Db, handle: &str, phone: Option<&str>, badge: Option<u32>) -> Result<Account, Error> {
    db.accounts().create(|id| Account {
        id,
        handle: handle.into(),
        phone: phone.map(Into::into),
        badge,
        display: "same for everyone".into(),
    })
}

backends!(crate::unique::accounts =>
    fn every_unique_field_is_enforced(db) {
        let a = account(db, "a", Some("555-0001"), Some(1)).unwrap();
        let b = account(db, "b", None, None).unwrap();
        let c = account(db, "c", None, None).unwrap();
        assert_eq!(db.accounts().len().unwrap(), 3);

        let err = account(db, "a", None, None).unwrap_err();
        assert!(matches!(&err, Error::Unique { entity: "Account", field: "handle", value, owner }
            if value == r#""a""# && owner == "AccountId(1)"), "{err}");
        assert_eq!(
            err.to_string(),
            r#"Account.handle must be unique, but "a" is already used by Account AccountId(1)"#
        );
        let err = account(db, "d", Some("555-0001"), None).unwrap_err();
        assert!(matches!(err, Error::Unique { field: "phone", .. }), "{err}");
        let err = account(db, "d", None, Some(1)).unwrap_err();
        assert!(matches!(&err, Error::Unique { field: "badge", value, .. } if value == "1"), "{err}");
        assert_eq!(db.accounts().len().unwrap(), 3);

        // Lookups by every unique field; `Into` lets &str stand in for String.
        assert_eq!(db.accounts().by_handle("b").unwrap(), Some(b.clone()));
        assert_eq!(db.accounts().by_handle(String::from("c")).unwrap(), Some(c.clone()));
        assert_eq!(db.accounts().by_phone("555-0001").unwrap(), Some(a.clone()));
        assert_eq!(db.accounts().by_badge(1u32).unwrap(), Some(a.clone()));
        assert_eq!(db.accounts().by_badge(2u32).unwrap(), None);
        assert_eq!(db.accounts().by_handle("zz").unwrap(), None);

        // Updates and upserts are checked too, but a row never conflicts with itself.
        let stolen = Account { handle: "a".into(), ..b.clone() };
        assert!(matches!(db.accounts().update(&stolen), Err(Error::Unique { field: "handle", .. })));
        assert!(matches!(db.accounts().upsert(&stolen), Err(Error::Unique { field: "handle", .. })));
        let fresh = Account { id: AccountId(50), ..stolen.clone() };
        assert!(matches!(db.accounts().upsert(&fresh), Err(Error::Unique { .. })));
        assert!(!db.accounts().contains(AccountId(50)).unwrap());
        db.accounts().update(&a).unwrap();
        db.accounts().upsert(&a).unwrap();
        assert_eq!(db.accounts().by_handle("a").unwrap(), Some(a.clone()));
    }

    fn values_move_and_free_up(db) {
        let mut a = account(db, "a", Some("1"), Some(7)).unwrap();
        let mut b = account(db, "b", Some("2"), None).unwrap();

        // Clearing an optional value frees it.
        a.phone = None;
        a.badge = None;
        db.accounts().update(&a).unwrap();
        assert_eq!(db.accounts().by_phone("1").unwrap(), None);
        assert_eq!(db.accounts().by_badge(7u32).unwrap(), None);
        b.badge = Some(7);
        b.phone = Some("1".into());
        db.accounts().update(&b).unwrap();
        assert_eq!(db.accounts().by_badge(7u32).unwrap(), Some(b.clone()));
        assert_eq!(db.accounts().by_phone("2").unwrap(), None);

        // Swapping two values needs a free value in between, all in one transaction.
        db.write(|tx| {
            let accounts = tx.accounts();
            accounts.update(&Account { handle: "tmp".into(), ..a.clone() })?;
            accounts.update(&Account { handle: "a".into(), ..b.clone() })?;
            accounts.update(&Account { handle: "b".into(), ..a.clone() })?;
            Ok::<_, Error>(())
        })
        .unwrap();
        assert_eq!(db.accounts().by_handle("a").unwrap().unwrap().id, b.id);
        assert_eq!(db.accounts().by_handle("b").unwrap().unwrap().id, a.id);
        assert_eq!(db.accounts().by_handle("tmp").unwrap(), None);

        // Deleting frees every value the row held.
        db.accounts().delete(b.id).unwrap();
        account(db, "a", Some("1"), Some(7)).unwrap();
    }

    fn uniqueness_within_one_transaction(db) {
        let result = db.write(|tx| {
            tx.accounts().create(|id| Account {
                id,
                handle: "x".into(),
                phone: None,
                badge: None,
                display: "".into(),
            })?;
            assert_eq!(tx.accounts().by_handle("x")?.map(|a| a.id), Some(AccountId(1)));
            tx.accounts().create(|id| Account {
                id,
                handle: "x".into(),
                phone: None,
                badge: None,
                display: "".into(),
            })?;
            Ok::<_, Error>(())
        });
        assert!(matches!(result, Err(Error::Unique { field: "handle", .. })));
        assert!(db.accounts().is_empty().unwrap());
        assert_eq!(db.accounts().by_handle("x").unwrap(), None);
    }

    fn one_to_one_references(db) {
        let a = account(db, "a", None, None).unwrap();
        let b = account(db, "b", None, None).unwrap();
        let profile = db.profiles().create(|id| Profile { id, account: a.id, bio: "hi".into() }).unwrap();

        // A unique reference looks up a single row.
        assert_eq!(db.profiles().by_account(a.id).unwrap(), Some(profile.clone()));
        assert_eq!(db.profiles().by_account(b.id).unwrap(), None);

        let err = db.profiles().create(|id| Profile { id, account: a.id, bio: "again".into() }).unwrap_err();
        assert!(matches!(&err, Error::Unique { entity: "Profile", field: "account", .. }), "{err}");
        let err = db.profiles().create(|id| Profile { id, account: AccountId(9), bio: "".into() }).unwrap_err();
        assert!(matches!(err, Error::ForeignKey { field: "account", .. }), "{err}");

        // Moving the profile frees the old account's slot.
        let moved = Profile { account: b.id, ..profile.clone() };
        db.profiles().update(&moved).unwrap();
        assert_eq!(db.profiles().by_account(a.id).unwrap(), None);
        let second = db.profiles().create(|id| Profile { id, account: a.id, bio: "".into() }).unwrap();

        // Each cascade takes exactly its own profile.
        db.accounts().delete(b.id).unwrap();
        assert_eq!(db.profiles().all().unwrap(), vec![second.clone()]);
        db.accounts().delete(a.id).unwrap();
        assert!(db.profiles().is_empty().unwrap());
    }

    fn unique_alongside_a_plain_reference(db) {
        let a = account(db, "a", None, None).unwrap();
        let s1 = db.sessions().create(|id| Session { id, account: a.id, token: 11 }).unwrap();
        let s2 = db.sessions().create(|id| Session { id, account: a.id, token: 12 }).unwrap();
        assert!(matches!(
            db.sessions().create(|id| Session { id, account: a.id, token: 11 }),
            Err(Error::Unique { entity: "Session", field: "token", .. })
        ));
        assert_eq!(db.sessions().by_account(a.id).unwrap(), vec![s1.clone(), s2.clone()]);
        assert_eq!(db.sessions().by_token(12u64).unwrap(), Some(s2));

        db.accounts().delete(a.id).unwrap();
        assert!(db.sessions().is_empty().unwrap());
        assert_eq!(db.sessions().by_token(11u64).unwrap(), None);
        let b = account(db, "b", None, None).unwrap();
        db.sessions().create(|id| Session { id, account: b.id, token: 11 }).unwrap();
    }
);
