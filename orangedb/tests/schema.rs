//! Natural keys, nullable references, and schema checks and migrations on
//! reopen.

#[orangedb::schema]
mod v1 {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Team {
        #[id(String)]
        pub slug: TeamSlug,
        /// Unique and nullable: rows without one don't conflict.
        #[unique]
        pub domain: Option<String>,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Member {
        #[id(auto)]
        pub id: MemberId,
        #[references(Team, on_delete = set_null)]
        pub team: Option<TeamSlug>,
        /// A unique reference, i.e. one-to-one: `by_mentor` returns one row.
        #[references(Member, on_delete = set_null)]
        #[unique]
        pub mentor: Option<MemberId>,
    }
}

/// Same tables, one more field on `Team`.
#[orangedb::schema]
mod v1_changed {
    #[entity]
    pub struct Team {
        #[id(String)]
        pub slug: TeamSlug,
        #[unique]
        pub domain: Option<String>,
        pub motto: String,
    }

    #[entity]
    pub struct Member {
        #[id(auto)]
        pub id: MemberId,
        #[references(Team, on_delete = set_null)]
        pub team: Option<TeamSlug>,
        #[references(Member, on_delete = set_null)]
        #[unique]
        pub mentor: Option<MemberId>,
    }
}

/// The same change, with a migration from `v1`.
#[orangedb::schema]
mod v2 {
    use super::v1;

    #[migrate]
    fn from_v1(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
        for team in old.teams().all()? {
            new.teams().insert(&Team {
                motto: format!("{} forever", team.slug.0),
                slug: TeamSlug(team.slug.0),
                domain: team.domain,
            })?;
        }
        Ok(())
    }

    #[entity]
    pub struct Team {
        #[id(String)]
        pub slug: TeamSlug,
        #[unique]
        pub domain: Option<String>,
        pub motto: String,
    }

    #[entity]
    pub struct Member {
        #[id(auto)]
        pub id: MemberId,
        #[references(Team, on_delete = set_null)]
        pub team: Option<TeamSlug>,
        #[references(Member, on_delete = set_null)]
        #[unique]
        pub mentor: Option<MemberId>,
    }
}

use orangedb::Error;
use v1::*;

fn team(slug: &str, domain: Option<&str>) -> Team {
    Team {
        slug: TeamSlug::new(slug),
        domain: domain.map(Into::into),
    }
}

#[test]
fn natural_keys_and_nullable_references() {
    let db = Db::in_memory().unwrap();
    db.teams().insert(&team("core", Some("core.example.com"))).unwrap();
    db.teams().insert(&team("infra", None)).unwrap();
    db.teams().insert(&team("web", None)).unwrap();
    assert!(matches!(
        db.teams().insert(&team("core", None)),
        Err(Error::AlreadyExists { .. })
    ));
    assert!(matches!(
        db.teams().insert(&team("ops", Some("core.example.com"))),
        Err(Error::Unique { .. })
    ));
    assert_eq!(
        db.teams().by_domain("core.example.com").unwrap().unwrap().slug,
        TeamSlug::new("core")
    );

    let lead = db
        .members()
        .create(|id| Member {
            id,
            team: Some(TeamSlug::new("core")),
            mentor: None,
        })
        .unwrap();
    let junior = db
        .members()
        .create(|id| Member {
            id,
            team: Some(TeamSlug::new("core")),
            mentor: Some(lead.id),
        })
        .unwrap();
    let err = db.members().create(|id| Member {
        id,
        team: None,
        mentor: Some(lead.id),
    });
    assert!(matches!(err, Err(Error::Unique { field: "mentor", .. })));
    assert_eq!(db.members().by_mentor(lead.id).unwrap(), Some(junior.clone()));

    db.teams().delete(TeamSlug::new("core")).unwrap();
    assert_eq!(db.members().get(lead.id).unwrap().unwrap().team, None);
    assert!(db.members().by_team(TeamSlug::new("core")).unwrap().is_empty());

    db.members().delete(lead.id).unwrap();
    assert_eq!(
        db.members().all().unwrap(),
        vec![Member {
            team: None,
            mentor: None,
            ..junior
        }]
    );
}

#[test]
fn schema_is_checked_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.redb");

    {
        let db = v1::Db::open(&path).unwrap();
        db.teams().insert(&team("core", None)).unwrap();
    }
    let db = v1::Db::open(&path).unwrap();
    assert_eq!(db.teams().len().unwrap(), 1);
    drop(db);

    let err = v1_changed::Db::open(&path).err().unwrap();
    assert!(
        matches!(err, Error::SchemaMismatch { entity: "Team", stored, declared }
            if stored == <v1::Team as orangedb::Entity>::FINGERPRINT
                && declared == <v1_changed::Team as orangedb::Entity>::FINGERPRINT),
        "{err}"
    );
    // A rejected open leaves the stored schema alone.
    v1::Db::open(&path).unwrap();

    let db = v2::Db::open(&path).unwrap();
    assert_eq!(
        db.teams().get(v2::TeamSlug::new("core")).unwrap().unwrap().motto,
        "core forever"
    );
    drop(db);
    // Now the database is at v2, and v1 can't open it.
    assert!(matches!(
        v1::Db::open(&path).err().unwrap(),
        Error::SchemaMismatch { entity: "Team", .. }
    ));
}
