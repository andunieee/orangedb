//! A company directory: employees live in cities.
//!
//! A city is found by its name, which is unique. Employee names repeat, so
//! `Employee::name` is a plain per-field index, and the custom `(city, name)`
//! index narrows a repeated name down to the person in one city.

use orangedb::Error;

#[orangedb::schema]
mod directory {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct City {
        #[id(auto)]
        pub id: CityId,
        #[unique]
        pub name: String,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    #[index(city_name: (CityId, String) = |e| (e.city, e.name.clone()))]
    pub struct Employee {
        #[id(auto)]
        pub id: EmployeeId,
        #[references(City, on_delete = cascade)]
        pub city: CityId,
        #[index]
        pub name: String,
        pub title: String,
    }
}

use directory::*;

fn city(db: &Db, name: &str) -> City {
    db.cities().create(|id| City { id, name: name.into() }).unwrap()
}

fn employee(db: &Db, city: &City, name: &str, title: &str) -> Employee {
    db.employees()
        .create(|id| Employee {
            id,
            city: city.id,
            name: name.into(),
            title: title.into(),
        })
        .unwrap()
}

fn names(employees: Vec<Employee>) -> Vec<String> {
    employees.into_iter().map(|e| e.name).collect()
}

#[test]
fn lookups_by_every_key() {
    let db = Db::in_memory().unwrap();
    let (london, paris) = (city(&db, "London"), city(&db, "Paris"));
    let alice = employee(&db, &london, "Alice", "Engineer");
    let bob = employee(&db, &london, "Bob", "Designer");
    let carol = employee(&db, &paris, "Carol", "Engineer");

    // A city from its name.
    assert_eq!(db.cities().by_name("London").unwrap(), Some(london.clone()));
    assert_eq!(db.cities().by_name("Berlin").unwrap(), None);

    // An employee by id.
    assert_eq!(db.employees().get(bob.id).unwrap(), Some(bob.clone()));
    assert_eq!(db.employees().get(EmployeeId(99)).unwrap(), None);

    // An employee by name.
    assert_eq!(db.employees().by_name("Alice").unwrap(), vec![alice.clone()]);
    assert!(db.employees().by_name("Dave").unwrap().is_empty());

    // Employees from a city, in id order.
    assert_eq!(
        db.employees().by_city(london.id).unwrap(),
        vec![alice.clone(), bob.clone()]
    );
    assert_eq!(db.employees().ids_by_city(paris.id).unwrap(), vec![carol.id]);

    // An employee by city and name.
    assert_eq!(
        db.employees().by_city_name((london.id, "Alice".into())).unwrap(),
        vec![alice.clone()]
    );
    assert_eq!(
        db.employees().ids_by_city_name((paris.id, "Carol".into())).unwrap(),
        [carol.id]
    );
    assert!(
        db.employees()
            .by_city_name((paris.id, "Alice".into()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_repeated_name_is_disambiguated_by_the_city() {
    let db = Db::in_memory().unwrap();
    let (london, paris) = (city(&db, "London"), city(&db, "Paris"));
    let london_sam = employee(&db, &london, "Sam", "Engineer");
    let paris_sam = employee(&db, &paris, "Sam", "Designer");

    // The name alone finds both Sams.
    assert_eq!(
        db.employees().by_name("Sam").unwrap(),
        vec![london_sam.clone(), paris_sam.clone()]
    );

    // With the city, it finds one.
    assert_eq!(
        db.employees().by_city_name((london.id, "Sam".into())).unwrap(),
        vec![london_sam]
    );
    assert_eq!(
        db.employees().by_city_name((paris.id, "Sam".into())).unwrap(),
        vec![paris_sam]
    );
}

#[test]
fn ranges_over_the_custom_key_stay_in_key_order() {
    let db = Db::in_memory().unwrap();
    let (london, paris) = (city(&db, "London"), city(&db, "Paris"));
    for name in ["Zoe", "Alice", "Mia"] {
        employee(&db, &london, name, "Engineer");
    }
    let paris_alice = employee(&db, &paris, "Alice", "Engineer");

    // Across cities the key sorts by city first, then by name.
    assert_eq!(
        names(db.employees().range_by_city_name(..).unwrap()),
        ["Alice", "Mia", "Zoe", "Alice"]
    );

    // A range of the key within one city is that city's names in order.
    assert_eq!(
        names(
            db.employees()
                .range_by_city_name((london.id, String::new())..(paris.id, String::new()))
                .unwrap()
        ),
        ["Alice", "Mia", "Zoe"]
    );
    assert_eq!(
        names(
            db.employees()
                .range_by_city_name((london.id, "M".to_string())..(london.id, "Z".to_string()))
                .unwrap()
        ),
        ["Mia"]
    );
    assert_eq!(
        db.employees().range_by_city_name((paris.id, String::new())..).unwrap(),
        vec![paris_alice]
    );
}

#[test]
fn indexes_follow_every_write() {
    let db = Db::in_memory().unwrap();
    let (london, paris) = (city(&db, "London"), city(&db, "Paris"));
    let mut alice = employee(&db, &london, "Alice", "Engineer");
    let bob = employee(&db, &london, "Alice", "Designer");
    let carol = employee(&db, &london, "Carol", "Manager");

    // Two people called Alice in London.
    assert_eq!(
        db.employees().by_city_name((london.id, "Alice".into())).unwrap(),
        vec![alice.clone(), bob.clone()]
    );

    // Renaming moves only that row's entries.
    alice.name = "Alicia".into();
    db.employees().update(&alice).unwrap();
    assert_eq!(db.employees().by_name("Alice").unwrap(), vec![bob.clone()]);
    assert_eq!(db.employees().by_name("Alicia").unwrap(), vec![alice.clone()]);
    assert_eq!(
        db.employees().by_city_name((london.id, "Alice".into())).unwrap(),
        vec![bob.clone()]
    );

    // Moving her to Paris moves both the city and the composite key.
    alice.city = paris.id;
    db.employees().update(&alice).unwrap();
    assert_eq!(
        db.employees().by_city(london.id).unwrap(),
        vec![bob.clone(), carol.clone()]
    );
    assert_eq!(db.employees().by_city(paris.id).unwrap(), vec![alice.clone()]);
    assert!(
        db.employees()
            .by_city_name((london.id, "Alicia".into()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.employees().by_city_name((paris.id, "Alicia".into())).unwrap(),
        vec![alice.clone()]
    );

    // An upsert replaces the old row's entries too.
    alice.name = "Alice".into();
    alice.city = london.id;
    db.employees().upsert(&alice).unwrap();
    assert!(db.employees().by_city(paris.id).unwrap().is_empty());
    assert_eq!(
        db.employees().by_city_name((london.id, "Alice".into())).unwrap(),
        vec![alice.clone(), bob.clone()]
    );

    // Deleting clears them.
    db.employees().delete(bob.id).unwrap();
    assert_eq!(
        db.employees().by_city_name((london.id, "Alice".into())).unwrap(),
        vec![alice.clone()]
    );
    assert_eq!(db.employees().by_name("Alice").unwrap(), vec![alice]);
}

#[test]
fn constraints_and_cascades() {
    let db = Db::in_memory().unwrap();
    let mut london = city(&db, "London");
    let alice = employee(&db, &london, "Alice", "Engineer");

    // City names are unique.
    let err = db
        .cities()
        .create(|id| City {
            id,
            name: "London".into(),
        })
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Unique {
                entity: "City",
                field: "name",
                ..
            }
        ),
        "{err}"
    );

    // Renaming frees the old name and keeps the employees attached.
    london.name = "Greater London".into();
    db.cities().update(&london).unwrap();
    assert_eq!(db.cities().by_name("London").unwrap(), None);
    assert_eq!(db.cities().by_name("Greater London").unwrap(), Some(london.clone()));
    assert_eq!(db.employees().by_city(london.id).unwrap(), vec![alice.clone()]);

    // An employee needs a city that exists.
    let err = db
        .employees()
        .create(|id| Employee {
            id,
            city: CityId(99),
            name: "Ghost".into(),
            title: String::new(),
        })
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::ForeignKey {
                entity: "Employee",
                field: "city",
                target: "City",
                ..
            }
        ),
        "{err}"
    );

    // Deleting a city takes its employees and their index entries with it.
    db.cities().delete(london.id).unwrap();
    assert_eq!(db.employees().get(alice.id).unwrap(), None);
    assert!(db.employees().by_name("Alice").unwrap().is_empty());
    assert!(
        db.employees()
            .by_city_name((london.id, "Alice".into()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.cities().by_name("Greater London").unwrap(), None);
}
