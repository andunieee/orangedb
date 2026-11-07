//! Every `on_delete` rule in combination: self-references, several
//! references from one row to the same parent, and cascades that restrict or
//! detach far from where they started.

use orangedb::Error;

#[orangedb::schema]
pub mod org {
    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Company {
        #[id(String)]
        pub slug: CompanySlug,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Desk {
        #[id(u32)]
        pub number: DeskNumber,
        #[references(Company, on_delete = restrict)]
        pub company: CompanySlug,
    }

    #[entity(accessor = staff)]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Employee {
        #[id(auto)]
        pub id: EmployeeId,
        #[references(Company, on_delete = cascade)]
        pub company: CompanySlug,
        #[references(Employee, on_delete = set_null)]
        pub manager: Option<EmployeeId>,
        /// One buddy each, cleared when they leave.
        #[references(Employee, on_delete = set_null)]
        #[unique]
        pub buddy: Option<EmployeeId>,
        /// An employee at a desk keeps it from being removed.
        #[references(Desk)]
        pub desk: Option<DeskNumber>,
    }

    #[entity]
    #[derive(Debug, Clone, PartialEq)]
    pub struct Project {
        #[id(auto)]
        pub id: ProjectId,
        #[references(Company, on_delete = cascade)]
        pub company: CompanySlug,
        #[references(Employee, on_delete = set_null)]
        pub lead: Option<EmployeeId>,
        #[references(Employee, on_delete = set_null)]
        pub members: Vec<EmployeeId>,
    }
}

use org::*;

fn company(db: &Db, slug: &str) -> CompanySlug {
    let slug = CompanySlug::new(slug);
    db.companies().insert(&Company { slug: slug.clone() }).unwrap();
    slug
}

fn employee(db: &Db, company: &CompanySlug, manager: Option<EmployeeId>) -> Employee {
    db.staff()
        .create(|id| Employee {
            id,
            company: company.clone(),
            manager,
            buddy: None,
            desk: None,
        })
        .unwrap()
}

fn project(db: &Db, company: &CompanySlug, lead: Option<&Employee>, members: &[&Employee]) -> Project {
    db.projects()
        .create(|id| Project {
            id,
            company: company.clone(),
            lead: lead.map(|e| e.id),
            members: members.iter().map(|e| e.id).collect(),
        })
        .unwrap()
}

backends!(crate::references::org =>
    fn set_null_on_self_references(db) {
        let acme = company(db, "acme");
        let ceo = employee(db, &acme, None);
        let vp = employee(db, &acme, Some(ceo.id));
        let dev = employee(db, &acme, Some(vp.id));
        let ops = employee(db, &acme, Some(vp.id));
        assert_eq!(db.staff().ids_by_manager(vp.id).unwrap(), vec![dev.id, ops.id]);

        // Only direct reports lose their manager.
        db.staff().delete(vp.id).unwrap();
        assert_eq!(db.staff().get(dev.id).unwrap().unwrap().manager, None);
        assert_eq!(db.staff().get(ops.id).unwrap().unwrap().manager, None);
        assert_eq!(db.staff().get(ceo.id).unwrap(), Some(ceo.clone()));
        assert!(db.staff().by_manager(vp.id).unwrap().is_empty());

        // Being your own manager is allowed once you exist, and harmless to delete.
        let mut me = employee(db, &acme, None);
        me.manager = Some(me.id);
        db.staff().update(&me).unwrap();
        assert_eq!(db.staff().by_manager(me.id).unwrap(), vec![me.clone()]);
        db.staff().delete(me.id).unwrap();
        assert!(db.staff().ids_by_manager(me.id).unwrap().is_empty());
    }

    fn unique_self_reference(db) {
        let acme = company(db, "acme");
        let mut a = employee(db, &acme, None);
        let mut b = employee(db, &acme, None);
        let c = employee(db, &acme, None);
        a.buddy = Some(b.id);
        b.buddy = Some(a.id);
        db.staff().update(&a).unwrap();
        db.staff().update(&b).unwrap();
        assert_eq!(db.staff().by_buddy(b.id).unwrap(), Some(a.clone()));
        assert_eq!(db.staff().by_buddy(a.id).unwrap(), Some(b.clone()));

        let taken = Employee { buddy: Some(b.id), ..c.clone() };
        assert!(matches!(db.staff().update(&taken), Err(Error::Unique { field: "buddy", .. })));

        // Leaving frees the buddy slot that pointed at you.
        db.staff().delete(b.id).unwrap();
        assert_eq!(db.staff().get(a.id).unwrap().unwrap().buddy, None);
        assert_eq!(db.staff().by_buddy(b.id).unwrap(), None);
        db.staff().update(&Employee { buddy: Some(a.id), ..c }).unwrap();
    }

    fn two_set_null_fields_to_the_same_parent(db) {
        let acme = company(db, "acme");
        let (x, y) = (employee(db, &acme, None), employee(db, &acme, None));
        let p = project(db, &acme, Some(&x), &[&y, &x]);
        let q = project(db, &acme, Some(&y), &[&x]);

        db.staff().delete(x.id).unwrap();
        let p2 = db.projects().get(p.id).unwrap().unwrap();
        assert_eq!((p2.lead, p2.members), (None, vec![y.id]));
        let q2 = db.projects().get(q.id).unwrap().unwrap();
        assert_eq!((q2.lead, q2.members), (Some(y.id), vec![]));
        assert!(db.projects().by_lead(x.id).unwrap().is_empty());
        assert!(db.projects().by_members(x.id).unwrap().is_empty());
        assert_eq!(db.projects().ids_by_members(y.id).unwrap(), vec![p.id]);
    }

    fn one_cascade_detaches_a_row_many_times(db) {
        let (acme, globex) = (company(db, "acme"), company(db, "globex"));
        let boss = employee(db, &globex, None);
        let a1 = employee(db, &acme, Some(boss.id));
        let a2 = employee(db, &acme, Some(a1.id));
        let g1 = employee(db, &globex, Some(a1.id));
        let mut g2 = employee(db, &globex, None);
        g2.buddy = Some(a2.id);
        db.staff().update(&g2).unwrap();

        // A globex project staffed partly from acme.
        let joint = project(db, &globex, Some(&a2), &[&a1, &g1, &a2, &boss]);
        let acmes = project(db, &acme, Some(&g1), &[&g1]);

        // acme -> its staff and projects -> scrub every globex row pointing at them.
        db.companies().delete(acme.clone()).unwrap();
        assert_eq!(
            db.staff().all().unwrap(),
            vec![
                boss.clone(),
                Employee { manager: None, ..g1.clone() },
                Employee { buddy: None, ..g2.clone() },
            ]
        );
        let joint = db.projects().get(joint.id).unwrap().unwrap();
        assert_eq!(joint.lead, None);
        assert_eq!(joint.members, vec![g1.id, boss.id]);
        assert!(!db.projects().contains(acmes.id).unwrap());
        assert!(db.staff().by_company(&acme).unwrap().is_empty());
        assert!(db.staff().by_manager(a1.id).unwrap().is_empty());
        assert_eq!(db.staff().by_buddy(a2.id).unwrap(), None);
        assert_eq!(db.projects().ids_by_members(g1.id).unwrap(), vec![joint.id]);
    }

    fn restrict_far_down_a_cascade(db) {
        let acme = company(db, "acme");
        let desk = DeskNumber(101);
        db.desks().insert(&Desk { number: desk.clone(), company: acme.clone() }).unwrap();
        let mut e = employee(db, &acme, None);
        e.desk = Some(desk.clone());
        db.staff().update(&e).unwrap();
        let p = project(db, &acme, Some(&e), &[&e]);

        // The desk restricts its company, even though everything else would cascade.
        let err = db.companies().delete(acme.clone()).unwrap_err();
        assert!(matches!(&err, Error::Restricted { entity: "Company", referenced_by: "Desk.company", child, .. }
            if child == "DeskNumber(101)"), "{err}");
        assert_eq!(db.staff().all().unwrap(), vec![e.clone()]);
        assert_eq!(db.projects().all().unwrap(), vec![p.clone()]);

        // The employee sitting at it restricts the desk, through an Option.
        let err = db.desks().delete(desk.clone()).unwrap_err();
        assert!(matches!(err, Error::Restricted { entity: "Desk", referenced_by: "Employee.desk", .. }), "{err}");
        assert!(db.desks().contains(&desk).unwrap());

        // A None doesn't restrict anything; once the desk is free, it can go,
        // and then so can the company.
        db.staff().update(&Employee { desk: None, ..e.clone() }).unwrap();
        assert!(db.staff().by_desk(&desk).unwrap().is_empty());
        db.desks().delete(&desk).unwrap();
        db.companies().delete(&acme).unwrap();
        assert!(db.staff().is_empty().unwrap());
        assert!(db.projects().is_empty().unwrap());
        assert!(db.companies().is_empty().unwrap());
    }

    fn deletes_inside_a_transaction_compose(db) {
        let acme = company(db, "acme");
        let a = employee(db, &acme, None);
        let b = employee(db, &acme, Some(a.id));
        let p = project(db, &acme, Some(&a), &[&a, &b]);

        db.write(|tx| {
            assert!(tx.staff().delete(a.id)?);
            // The detach is visible straight away.
            assert_eq!(tx.staff().get(b.id)?.unwrap().manager, None);
            assert_eq!(tx.projects().get(p.id)?.unwrap().members, vec![b.id]);
            // Re-adding under the same id is a new row: nothing comes back.
            tx.staff().insert(&a)?;
            assert!(tx.staff().by_manager(a.id)?.is_empty());
            assert!(tx.projects().by_members(a.id)?.is_empty());
            assert!(!tx.staff().delete(EmployeeId(77))?);
            Ok::<_, Error>(())
        })
        .unwrap();
        assert_eq!(db.staff().len().unwrap(), 2);
        assert_eq!(db.projects().get(p.id).unwrap().unwrap().lead, None);
    }
);
