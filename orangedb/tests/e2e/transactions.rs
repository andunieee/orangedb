//! `Db::write` and `Db::read`: atomicity, isolation, custom error types,
//! panics and concurrent writers.

use std::panic::{AssertUnwindSafe, catch_unwind};

use orangedb::Error;

#[orangedb::schema]
pub mod ledger {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Account {
        #[id(String)]
        pub name: AccountName,
        pub balance: i64,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Transfer {
        #[id(auto)]
        pub id: TransferId,
        #[references(Account, on_delete = restrict)]
        pub from: AccountName,
        #[references(Account, on_delete = restrict)]
        pub to: AccountName,
        pub amount: i64,
    }
}

use ledger::*;

#[derive(Debug)]
enum BankError {
    Db(Error),
    Insufficient { name: String, balance: i64 },
}

impl From<Error> for BankError {
    fn from(e: Error) -> Self {
        BankError::Db(e)
    }
}

fn open(db: &Db, name: &str, balance: i64) -> AccountName {
    let name = AccountName::new(name);
    db.accounts()
        .insert(&Account {
            name: name.clone(),
            balance,
        })
        .unwrap();
    name
}

fn balance(db: &Db, name: &AccountName) -> i64 {
    db.accounts().get(name).unwrap().unwrap().balance
}

/// Records the transfer first and checks the balance last, so a failure
/// always has something to roll back.
fn transfer(db: &Db, from: &AccountName, to: &AccountName, amount: i64) -> Result<Transfer, BankError> {
    db.write(|tx| {
        let t = tx.transfers().create(|id| Transfer {
            id,
            from: from.clone(),
            to: to.clone(),
            amount,
        })?;
        let mut dst = tx.accounts().get(to)?.expect("checked by the foreign key");
        dst.balance += amount;
        tx.accounts().update(&dst)?;
        let mut src = tx.accounts().get(from)?.expect("checked by the foreign key");
        src.balance -= amount;
        if src.balance < 0 {
            return Err(BankError::Insufficient {
                name: src.name.0,
                balance: src.balance + amount,
            });
        }
        tx.accounts().update(&src)?;
        Ok(t)
    })
}

backends!(crate::transactions::ledger =>
    fn commit_and_rollback(db) {
        let (a, b) = (open(db, "a", 100), open(db, "b", 0));

        let t = transfer(db, &a, &b, 30).unwrap();
        assert_eq!(t.id, TransferId(1));
        assert_eq!((balance(db, &a), balance(db, &b)), (70, 30));

        // A custom error rolls back the transfer row and the credit.
        match transfer(db, &a, &b, 1000) {
            Err(BankError::Insufficient { name, balance }) => assert_eq!((name.as_str(), balance), ("a", 70)),
            other => panic!("expected Insufficient, got {other:?}"),
        }
        assert_eq!((balance(db, &a), balance(db, &b)), (70, 30));
        assert_eq!(db.transfers().all().unwrap(), vec![t.clone()]);

        // So does a library error, wrapped in the custom type.
        match transfer(db, &a, &AccountName::new("nobody"), 5) {
            Err(BankError::Db(Error::ForeignKey { field: "to", .. })) => {}
            other => panic!("expected a foreign key error, got {other:?}"),
        }

        // The rolled-back transfers didn't use up ids.
        assert_eq!(transfer(db, &b, &a, 10).unwrap().id, TransferId(2));
        assert_eq!((balance(db, &a), balance(db, &b)), (80, 20));
        assert_eq!(db.transfers().by_from(&b).unwrap().len(), 1);
        assert_eq!(db.transfers().by_to(&b).unwrap(), vec![t]);

        // Restrict from either side of a transfer.
        assert!(matches!(db.accounts().delete(&a), Err(Error::Restricted { referenced_by: "Transfer.from", .. })));
        assert!(matches!(db.accounts().delete(&b), Err(Error::Restricted { .. })));
    }

    fn reserved_ids_roll_back(db) {
        let result: Result<(), Error> = db.write(|tx| {
            assert_eq!(tx.transfers().next_id()?, TransferId(1));
            assert_eq!(tx.transfers().next_id()?, TransferId(2));
            Err(Error::NotFound { entity: "Account", id: "test".into() })
        });
        assert!(result.is_err());
        assert_eq!(db.transfers().next_id().unwrap(), TransferId(1));
    }

    fn a_panic_rolls_back(db) {
        let a = open(db, "a", 1);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            db.write(|tx| {
                tx.accounts().update(&Account { name: a.clone(), balance: 1_000_000 })?;
                tx.accounts().insert(&Account { name: AccountName::new("b"), balance: 0 })?;
                panic!("boom");
                #[allow(unreachable_code)]
                Ok::<_, Error>(())
            })
        }));
        assert!(outcome.is_err());
        assert_eq!(db.accounts().all().unwrap(), vec![Account { name: a.clone(), balance: 1 }]);

        // The database is still usable for both reads and writes.
        open(db, "b", 0);
        assert_eq!(db.accounts().len().unwrap(), 2);
    }

    fn a_handled_error_keeps_the_transaction(db) {
        let (a, b) = (open(db, "a", 10), open(db, "b", 10));
        let made = db
            .write(|tx| {
                let t = tx.transfers().create(|id| Transfer { id, from: a.clone(), to: b.clone(), amount: 1 })?;
                // Each of these fails before writing anything, and is ignored.
                assert!(tx.accounts().insert(&Account { name: a.clone(), balance: 0 }).is_err());
                assert!(tx.accounts().update(&Account { name: AccountName::new("c"), balance: 0 }).is_err());
                assert!(tx.accounts().delete(&a).is_err());
                assert!(tx
                    .transfers()
                    .insert(&Transfer { id: TransferId(9), from: a.clone(), to: AccountName::new("c"), amount: 1 })
                    .is_err());
                Ok::<_, Error>(t)
            })
            .unwrap();
        assert_eq!(db.transfers().all().unwrap(), vec![made]);
        assert_eq!((balance(db, &a), balance(db, &b)), (10, 10));
        // The failed insert of TransferId(9) didn't move the sequence.
        assert_eq!(db.transfers().next_id().unwrap(), TransferId(2));
    }

    fn isolation(db) {
        let a = open(db, "a", 1);

        // Readers see a snapshot: neither an open write nor a later commit shows up in it.
        db.read(|snapshot| {
            db.write(|tx| {
                tx.accounts().update(&Account { name: a.clone(), balance: 2 })?;
                assert_eq!(tx.accounts().get(&a)?.unwrap().balance, 2);
                assert_eq!(balance(db, &a), 1);
                assert_eq!(snapshot.accounts().get(&a)?.unwrap().balance, 1);
                Ok::<_, Error>(())
            })?;
            assert_eq!(snapshot.accounts().get(&a)?.unwrap().balance, 1);
            assert_eq!(balance(db, &a), 2);
            Ok::<_, Error>(())
        })
        .unwrap();

        // Read results and errors pass straight through.
        let total: i64 = db.read(|tx| Ok::<_, Error>(tx.accounts().all()?.iter().map(|a| a.balance).sum())).unwrap();
        assert_eq!(total, 2);
        let err = db
            .read(|_| Err::<(), _>(BankError::Insufficient { name: "x".into(), balance: 0 }))
            .unwrap_err();
        assert!(matches!(err, BankError::Insufficient { .. }));
    }

    fn concurrent_writers(db) {
        let names: Vec<AccountName> = (0..4).map(|i| open(db, &format!("acct{i}"), 1_000)).collect();
        std::thread::scope(|s| {
            for t in 0..4 {
                let names = &names;
                s.spawn(move || {
                    for i in 0..25 {
                        let from = &names[(t + i) % 4];
                        let to = &names[(t + i + 1) % 4];
                        transfer(db, from, to, 7).unwrap();
                    }
                });
            }
        });
        // Money is conserved and every transfer got its own id.
        let total: i64 = names.iter().map(|n| balance(db, n)).sum();
        assert_eq!(total, 4_000);
        let ids: Vec<u64> = db.transfers().all().unwrap().into_iter().map(|t| t.id.0).collect();
        assert_eq!(ids, (1..=100).collect::<Vec<_>>());
    }
);
