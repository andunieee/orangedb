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
        pub title: String,
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
        pub title: String,
    }
}

/// The same tables as `v1`, with a per-field index and an arbitrary one
/// added. Indexes aren't part of the fingerprint, so this opens a `v1`
/// database without a migration and builds them from its rows.
#[orangedb::schema]
mod v1_indexed {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    #[index(slug_upper: String = |t| t.slug.0.to_uppercase())]
    pub struct Team {
        #[id(String)]
        pub slug: TeamSlug,
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
        #[references(Member, on_delete = set_null)]
        #[unique]
        pub mentor: Option<MemberId>,
        #[index]
        pub title: String,
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
        pub title: String,
    }
}

use orangedb::{Entity, Error};
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
            title: "Lead".into(),
        })
        .unwrap();
    let junior = db
        .members()
        .create(|id| Member {
            id,
            team: Some(TeamSlug::new("core")),
            mentor: Some(lead.id),
            title: "Junior".into(),
        })
        .unwrap();
    let err = db.members().create(|id| Member {
        id,
        team: None,
        mentor: Some(lead.id),
        title: "Other".into(),
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
fn indexes_are_not_part_of_the_schema() {
    let dir = tempdir();
    let path = dir.path().join("db.redb");

    {
        let db = v1::Db::open(&path).unwrap();
        db.teams().insert(&team("core", Some("core.example.com"))).unwrap();
        db.teams().insert(&team("infra", None)).unwrap();
        let lead = db
            .members()
            .create(|id| Member {
                id,
                team: Some(TeamSlug::new("core")),
                mentor: None,
                title: "Lead".into(),
            })
            .unwrap();
        db.members()
            .create(|id| Member {
                id,
                team: Some(TeamSlug::new("infra")),
                mentor: Some(lead.id),
                title: "Junior".into(),
            })
            .unwrap();
    }

    // Neither a field index nor an arbitrary one changes the fingerprint.
    assert_eq!(v1::Team::FINGERPRINT, v1_indexed::Team::FINGERPRINT);
    assert_eq!(v1::Member::FINGERPRINT, v1_indexed::Member::FINGERPRINT);

    // So `v1_indexed` opens v1's file and builds both from its rows.
    let db = v1_indexed::Db::open(&path).unwrap();
    assert_eq!(db.members().ids_by_title("Lead").unwrap(), [v1_indexed::MemberId(1)]);
    assert_eq!(db.members().by_title("Junior").unwrap()[0].id, v1_indexed::MemberId(2));
    assert_eq!(
        db.members()
            .range_by_title("A".to_string().."Z".to_string())
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        db.teams().ids_by_slug_upper("CORE").unwrap(),
        [v1_indexed::TeamSlug::new("core")]
    );
    assert_eq!(
        db.teams().by_slug_upper("INFRA").unwrap()[0].slug,
        v1_indexed::TeamSlug::new("infra")
    );
}

#[test]
fn schema_is_checked_on_open() {
    let dir = tempdir();
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

/// A temporary directory under Cargo's target directory.
///
/// `tempfile::tempdir()` panics on wasm, where `std::env::temp_dir` is
/// unsupported, so tests go through this instead.
fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}
