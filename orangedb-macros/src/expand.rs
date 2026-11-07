//! Code generation for a parsed schema.
//!
//! Public items (id newtypes, table handles, `Db`, `ReadTx`, `WriteTx`) land
//! in the schema module itself; table definitions and the constraint logic
//! live in a private `__orangedb` submodule.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
use syn::{Ident, LitStr};

use crate::parse::{Entity, Field, IdKind, OnDelete, Schema, Shape, snake_case};

pub fn expand(schema: &Schema, source: &str) -> TokenStream {
    let source = LitStr::new(source, proc_macro2::Span::call_site());
    let entities = &schema.entities;
    let public = entities.iter().map(|e| public_items(schema, e));
    let internal = entities.iter().map(|e| internal_items(schema, e));
    let names: Vec<&Ident> = entities.iter().map(|e| &e.ident).collect();
    let id_tys: Vec<&Ident> = entities.iter().map(|e| &e.id.ty).collect();
    let accessors: Vec<&Ident> = entities.iter().map(|e| &e.accessor).collect();
    let tables: Vec<Ident> = entities.iter().map(|e| format_ident!("{}Table", e.ident)).collect();
    let autos: Vec<Ident> = entities.iter().map(|e| format_ident!("{}AutoTable", e.ident)).collect();
    let plans: Vec<Ident> = entities.iter().map(|e| fn_name("plan", e)).collect();
    let detaches: Vec<Ident> = entities.iter().map(|e| fn_name("detach", e)).collect();
    let erases: Vec<Ident> = entities.iter().map(|e| fn_name("erase", e)).collect();
    let creates = entities.iter().map(|e| {
        let rows = rows_const(e);
        let fks = e
            .fields
            .iter()
            .filter(|f| f.references.is_some())
            .map(|f| fk_const(e, f));
        let uniques = e.fields.iter().filter(|f| f.unique).map(|f| unique_const(e, f));
        quote! {
            rt::create_row_table(tx, #rows)?;
            #( rt::create_multimap_table(tx, #fks)?; )*
            #( rt::create_table(tx, #uniques)?; )*
        }
    });

    let verifies = entities
        .iter()
        .filter(|e| e.fields.iter().any(|f| f.references.is_some()))
        .map(|e| {
            let ident = &e.ident;
            let rows = rows_const(e);
            let check = fn_name("check", e);
            quote! {
                for bytes in rt::Store::rows(tx, #rows)? {
                    #check(tx, &rt::decode::<#ident>(&bytes)?, true)?;
                }
            }
        });
    let layouts = entities.iter().map(layout);
    let migrate = match &schema.migration {
        None => quote!(Ok(false)),
        Some(m) => {
            let function = &m.function;
            let old = &m.old;
            quote! {
                type Old = #old;
                type OldSchema = <Old as ::orangedb::__private::SchemaReadTx>::Schema;
                ::orangedb::__private::open::<OldSchema>(db)?;
                let old = <Old as ::orangedb::__private::SchemaReadTx>::from_redb(::orangedb::__private::begin_read(db)?);
                let tx = WriteTx { inner: db.begin_write()?, migrating: true };
                let result = (|| -> ::orangedb::Result<()> {
                    <Self as ::orangedb::__private::Schema>::create_tables(&tx.inner)?;
                    #function(&old, &tx)?;
                    ::orangedb::__private::finish_migration::<OldSchema, Self>(&tx.inner)
                })();
                drop(old);
                match result {
                    Ok(()) => {
                        tx.inner.commit()?;
                        Ok(true)
                    }
                    Err(e) => {
                        let _ = tx.inner.abort();
                        Err(e)
                    }
                }
            }
        }
    };

    quote! {
        #( #public )*

        /// A handle to the database. Every table method on it runs in its
        /// own transaction; use [`Db::write`] or [`Db::read`] to group calls.
        pub struct Db {
            inner: ::orangedb::redb::Database,
        }

        impl Db {
            /// Opens the database at `path`, creating it if needed.
            pub fn open(path: impl ::std::convert::AsRef<::std::path::Path>) -> ::orangedb::Result<Self> {
                Self::from_redb(::orangedb::redb::Database::create(path)?)
            }

            /// A database that lives in memory only, mostly for tests.
            pub fn in_memory() -> ::orangedb::Result<Self> {
                let backend = ::orangedb::redb::backends::InMemoryBackend::new();
                Self::from_redb(::orangedb::redb::Database::builder().create_with_backend(backend)?)
            }

            /// Wraps an already-open redb database, creating the schema's
            /// tables and checking that its stored schema matches this one.
            /// If it doesn't, the schema's `#[migrate]` function brings it
            /// up to date.
            pub fn from_redb(inner: ::orangedb::redb::Database) -> ::orangedb::Result<Self> {
                ::orangedb::__private::open::<Self>(&inner)?;
                Ok(Self { inner })
            }

            /// The schema source recorded for the stored layout of `E`, if
            /// this database has one. Use `E` from an older schema module to
            /// read what a migrated layout looked like.
            pub fn stored_source<E: ::orangedb::Entity>(
                &self,
            ) -> ::orangedb::Result<::std::option::Option<::std::string::String>> {
                self.stored_source_at(E::NAME, E::FINGERPRINT)
            }

            /// The schema source recorded for `entity` at `fingerprint`, if
            /// this database has one, even when the entity's Rust type is no
            /// longer declared.
            pub fn stored_source_at(
                &self,
                entity: &str,
                fingerprint: u64,
            ) -> ::orangedb::Result<::std::option::Option<::std::string::String>> {
                ::orangedb::__private::stored_source(&self.inner, entity, fingerprint)
            }

            /// What the database records about its layouts: the tables,
            /// references, unique fields and source of every entity at every
            /// fingerprint it has stored, current or migrated away from.
            pub fn stored_catalog(&self) -> ::orangedb::Result<::orangedb::catalog::Catalog> {
                use ::orangedb::redb::ReadableDatabase as _;
                ::orangedb::catalog::Catalog::read(&self.inner.begin_read()?)
            }

            /// Runs `f` in a write transaction, committing if it returns `Ok`
            /// and rolling everything back if it returns `Err` or panics.
            pub fn write<R, Er: ::std::convert::From<::orangedb::Error>>(
                &self,
                f: impl ::std::ops::FnOnce(&WriteTx) -> ::std::result::Result<R, Er>,
            ) -> ::std::result::Result<R, Er> {
                let tx = WriteTx {
                    inner: self.inner.begin_write().map_err(::orangedb::Error::from)?,
                    migrating: false,
                };
                match f(&tx) {
                    Ok(out) => {
                        tx.inner.commit().map_err(::orangedb::Error::from)?;
                        Ok(out)
                    }
                    Err(e) => {
                        let _ = tx.inner.abort();
                        Err(e)
                    }
                }
            }

            /// Runs `f` against a consistent snapshot of the database.
            pub fn read<R, Er: ::std::convert::From<::orangedb::Error>>(
                &self,
                f: impl ::std::ops::FnOnce(&ReadTx) -> ::std::result::Result<R, Er>,
            ) -> ::std::result::Result<R, Er> {
                use ::orangedb::redb::ReadableDatabase as _;
                let tx = ReadTx { inner: self.inner.begin_read().map_err(::orangedb::Error::from)? };
                f(&tx)
            }

            #(
                pub fn #accessors(&self) -> #autos<'_> {
                    #autos { db: self }
                }
            )*
        }

        /// A write transaction, see [`Db::write`]. In a `#[migrate]`
        /// function, references are only checked once it returns, so rows
        /// can be written in any order.
        pub struct WriteTx {
            inner: ::orangedb::redb::WriteTransaction,
            migrating: bool,
        }

        impl WriteTx {
            #(
                pub fn #accessors(&self) -> #tables<'_, ::orangedb::redb::WriteTransaction> {
                    #tables { tx: &self.inner, migrating: self.migrating }
                }
            )*
        }

        /// A read transaction, see [`Db::read`].
        pub struct ReadTx {
            inner: ::orangedb::redb::ReadTransaction,
        }

        impl ReadTx {
            #(
                pub fn #accessors(&self) -> #tables<'_, ::orangedb::redb::ReadTransaction> {
                    #tables { tx: &self.inner, migrating: false }
                }
            )*
        }

        impl ::orangedb::__private::SchemaReadTx for ReadTx {
            type Schema = Db;

            fn from_redb(inner: ::orangedb::redb::ReadTransaction) -> Self {
                ReadTx { inner }
            }
        }

        impl ::orangedb::__private::Schema for Db {
            const ENTITIES: &'static [(&'static str, u64)] = &[
                #( (stringify!(#names), <#names as ::orangedb::Entity>::FINGERPRINT), )*
            ];

            const SOURCE: &'static str = #source;

            fn layouts(source: u64) -> ::std::vec::Vec<::orangedb::catalog::Layout> {
                use ::orangedb::catalog as c;
                use ::orangedb::redb::{MultimapTableHandle as _, TableHandle as _};
                ::std::vec![ #( #layouts ),* ]
            }

            fn create_tables(tx: &::orangedb::redb::WriteTransaction) -> ::orangedb::Result<()> {
                __orangedb::create_tables(tx)
            }

            fn verify(tx: &::orangedb::redb::WriteTransaction) -> ::orangedb::Result<()> {
                __orangedb::verify(tx)
            }

            fn migrate(db: &::orangedb::redb::Database) -> ::orangedb::Result<bool> {
                #migrate
            }
        }

        #[doc(hidden)]
        #[allow(unused_variables, clippy::all)]
        mod __orangedb {
            use super::*;
            use ::orangedb::__private as rt;
            use ::orangedb::__private::Store as _;
            use ::orangedb::redb::{MultimapTableDefinition, TableDefinition, WriteTransaction};
            use ::orangedb::{Error, Result};

            #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
            pub enum AnyId {
                #( #names(#id_tys), )*
            }

            impl ::std::fmt::Debug for AnyId {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    match self {
                        #( AnyId::#names(id) => ::std::fmt::Debug::fmt(id, f), )*
                    }
                }
            }

            impl rt::AnyId for AnyId {
                fn entity(&self) -> &'static str {
                    match self {
                        #( AnyId::#names(_) => stringify!(#names), )*
                    }
                }
            }

            pub fn create_tables(tx: &WriteTransaction) -> Result<()> {
                #( #creates )*
                Ok(())
            }

            pub fn verify(tx: &WriteTransaction) -> Result<()> {
                #( #verifies )*
                Ok(())
            }

            /// Deletes `root` and everything its `on_delete` rules reach.
            pub fn delete(tx: &WriteTransaction, root: AnyId) -> Result<()> {
                let mut plan = rt::DeletePlan::new(root);
                while let Some(next) = plan.pop() {
                    match &next {
                        #( AnyId::#names(id) => #plans(tx, id, &mut plan)?, )*
                    }
                }
                let plan = plan.resolve()?;
                for (child, field, parent) in &plan.detach {
                    match child {
                        #( AnyId::#names(id) => #detaches(tx, id, field, parent)?, )*
                    }
                }
                for row in &plan.delete {
                    match row {
                        #( AnyId::#names(id) => #erases(tx, id)?, )*
                    }
                }
                Ok(())
            }

            #( #internal )*
        }
    }
}

fn public_items(schema: &Schema, e: &Entity) -> TokenStream {
    let ident = &e.ident;
    let vis = &e.vis;
    let name = LitStr::new(&ident.to_string(), ident.span());
    let id_ty = &e.id.ty;
    let id_field = &e.id.ident;
    let inner = e.id.kind.inner();
    let fingerprint = &e.fingerprint;
    let inner_print = crate::print::type_print(&inner);
    let table = format_ident!("{}Table", ident);
    let auto = format_ident!("{}AutoTable", ident);
    let accessor = &e.accessor;
    let type_name = LitStr::new(&format!("orangedb::{ident}.{id_field}"), ident.span());

    let copy = match e.id.kind {
        IdKind::Auto | IdKind::Uuid => quote!(Copy,),
        IdKind::Natural(_) => quote!(),
    };
    let constructor = match &e.id.kind {
        IdKind::Auto => quote!(),
        IdKind::Uuid => quote! {
            impl #id_ty {
                /// A new time-ordered (v7) UUID, so that recent rows sit
                /// together in the table.
                pub fn generate() -> Self {
                    Self(::orangedb::__private::uuid::Uuid::now_v7())
                }
            }
        },
        IdKind::Natural(_) => quote! {
            impl #id_ty {
                pub fn new(value: impl ::std::convert::Into<#inner>) -> Self {
                    Self(value.into())
                }
            }
        },
    };

    let methods = methods(schema, e);
    let read = methods.iter().filter(|m| !m.write).map(|m| m.definition());
    let write = methods.iter().filter(|m| m.write).map(|m| m.definition());
    let forwarded = methods.iter().map(|m| m.forward(accessor));

    quote! {
        #[derive(Clone, #copy Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
                 ::orangedb::__private::serde::Serialize, ::orangedb::__private::serde::Deserialize)]
        #[serde(crate = "::orangedb::__private::serde", transparent)]
        #vis struct #id_ty(pub #inner);

        impl ::std::convert::From<#inner> for #id_ty {
            fn from(value: #inner) -> Self {
                Self(value)
            }
        }

        #constructor

        impl ::orangedb::__private::Sealed for #id_ty {}

        /// Stored as the key it wraps.
        impl ::orangedb::Fingerprint for #id_ty {
            const FINGERPRINT: u64 = #inner_print;
        }

        impl ::orangedb::redb::Value for #id_ty {
            type SelfType<'a> = #id_ty where Self: 'a;
            type AsBytes<'a> = <#inner as ::orangedb::redb::Value>::AsBytes<'a> where Self: 'a;

            fn fixed_width() -> ::std::option::Option<usize> {
                <#inner as ::orangedb::redb::Value>::fixed_width()
            }

            fn from_bytes<'a>(data: &'a [u8]) -> #id_ty where Self: 'a {
                #id_ty(<#inner as ::orangedb::redb::Value>::from_bytes(data))
            }

            fn as_bytes<'a, 'b: 'a>(value: &'a #id_ty) -> Self::AsBytes<'a> where Self: 'b {
                <#inner as ::orangedb::redb::Value>::as_bytes(&value.0)
            }

            fn type_name() -> ::orangedb::redb::TypeName {
                ::orangedb::redb::TypeName::new(#type_name)
            }
        }

        impl ::orangedb::redb::Key for #id_ty {
            fn compare(a: &[u8], b: &[u8]) -> ::std::cmp::Ordering {
                <#inner as ::orangedb::redb::Key>::compare(a, b)
            }

            fn separator<'a>(left: &'a [u8], right: &'a [u8]) -> ::std::borrow::Cow<'a, [u8]> {
                <#inner as ::orangedb::redb::Key>::separator(left, right)
            }

            fn min_encoded_key() -> ::std::option::Option<::std::borrow::Cow<'static, [u8]>> {
                <#inner as ::orangedb::redb::Key>::min_encoded_key()
            }
        }

        impl ::orangedb::Entity for #ident {
            type Id = #id_ty;
            const NAME: &'static str = #name;
            const FINGERPRINT: u64 = #fingerprint;

            fn id(&self) -> &#id_ty {
                &self.#id_field
            }
        }

        /// Table handle inside a transaction. Reads work in both
        /// transaction kinds, writes only in a [`WriteTx`].
        pub struct #table<'t, S> {
            tx: &'t S,
            migrating: bool,
        }

        impl<'t, S: ::orangedb::__private::Store> #table<'t, S> {
            #( #read )*
        }

        impl<'t> #table<'t, ::orangedb::redb::WriteTransaction> {
            #( #write )*
        }

        /// Table handle on [`Db`]: each call runs in its own transaction.
        pub struct #auto<'db> {
            db: &'db Db,
        }

        impl<'db> #auto<'db> {
            #( #forwarded )*
        }
    }
}

/// A table method, emitted once on the transaction handle and once,
/// forwarding through its own transaction, on the `Db` handle.
struct Method {
    write: bool,
    doc: String,
    name: Ident,
    args: Vec<(Ident, TokenStream)>,
    ret: TokenStream,
    body: TokenStream,
}

impl Method {
    fn signature(&self) -> TokenStream {
        let Method { doc, name, ret, .. } = self;
        let args = self.args.iter().map(|(a, ty)| quote!(#a: #ty));
        quote! {
            #[doc = #doc]
            pub fn #name(&self, #( #args ),*) -> ::orangedb::Result<#ret>
        }
    }

    fn definition(&self) -> TokenStream {
        let sig = self.signature();
        let body = &self.body;
        quote!(#sig { #body })
    }

    fn forward(&self, accessor: &Ident) -> TokenStream {
        let sig = self.signature();
        let name = &self.name;
        let args = self.args.iter().map(|(a, _)| a);
        let run = if self.write { quote!(write) } else { quote!(read) };
        quote! {
            #sig {
                self.db.#run(|tx| tx.#accessor().#name(#( #args ),*))
            }
        }
    }
}

fn methods(schema: &Schema, e: &Entity) -> Vec<Method> {
    let ident = &e.ident;
    let id_ty = &e.id.ty;
    let id_field = &e.id.ident;
    let rows = rows_const(e);
    let rows = quote!(__orangedb::#rows);
    let store = fn_name("store", e);
    let check = fn_name("check", e);
    let id_arg = quote!(impl ::std::borrow::Borrow<#id_ty>);
    let m =
        |write: bool, doc: &str, name: &str, args: Vec<(&str, TokenStream)>, ret: TokenStream, body: TokenStream| {
            Method {
                write,
                doc: doc.to_string(),
                name: format_ident!("{}", name),
                args: args.into_iter().map(|(a, ty)| (format_ident!("{}", a), ty)).collect(),
                ret,
                body,
            }
        };

    let mut out = vec![
        m(
            false,
            "The row with this id, if any.",
            "get",
            vec![("id", id_arg.clone())],
            quote!(::std::option::Option<#ident>),
            quote! {
                match ::orangedb::__private::Store::get_row(self.tx, #rows, id.borrow())? {
                    Some(bytes) => Ok(Some(::orangedb::__private::decode(&bytes)?)),
                    None => Ok(None),
                }
            },
        ),
        m(
            false,
            "Whether a row with this id exists.",
            "contains",
            vec![("id", id_arg.clone())],
            quote!(bool),
            quote! {
                Ok(::orangedb::__private::Store::get_row(self.tx, #rows, id.borrow())?.is_some())
            },
        ),
        m(
            false,
            "Every row, in id order.",
            "all",
            vec![],
            quote!(::std::vec::Vec<#ident>),
            quote! {
                ::orangedb::__private::Store::rows(self.tx, #rows)?
                    .iter()
                    .map(|bytes| ::orangedb::__private::decode(bytes))
                    .collect()
            },
        ),
        m(
            false,
            "The number of rows.",
            "len",
            vec![],
            quote!(u64),
            quote! {
                ::orangedb::__private::Store::len(self.tx, #rows)
            },
        ),
        m(
            false,
            "Whether the table has no rows.",
            "is_empty",
            vec![],
            quote!(bool),
            quote! {
                Ok(::orangedb::__private::Store::len(self.tx, #rows)? == 0)
            },
        ),
    ];

    for f in &e.fields {
        let field = &f.ident;
        if f.unique {
            let inner = &f.inner;
            let uq = unique_const(e, f);
            out.push(m(
                false,
                &format!("The row whose unique `{field}` equals `value`, if any."),
                &format!("by_{field}"),
                vec![("value", quote!(impl ::std::convert::Into<#inner>))],
                quote!(::std::option::Option<#ident>),
                quote! {
                    match ::orangedb::__private::Store::get(self.tx, __orangedb::#uq, &value.into())? {
                        Some(id) => self.get(id),
                        None => Ok(None),
                    }
                },
            ));
        } else if let Some(r) = &f.references {
            let target_id = &schema.entity(&r.target).id.ty;
            let fk = fk_const(e, f);
            let ids_by = format_ident!("ids_by_{}", field);
            out.push(m(
                false,
                &format!("Ids of the rows whose `{field}` references `parent`."),
                &format!("ids_by_{field}"),
                vec![("parent", quote!(impl ::std::borrow::Borrow<#target_id>))],
                quote!(::std::vec::Vec<#id_ty>),
                quote! {
                    ::orangedb::__private::Store::get_all(self.tx, __orangedb::#fk, parent.borrow())
                },
            ));
            out.push(m(
                false,
                &format!("The rows whose `{field}` references `parent`, in id order."),
                &format!("by_{field}"),
                vec![("parent", quote!(impl ::std::borrow::Borrow<#target_id>))],
                quote!(::std::vec::Vec<#ident>),
                // A listed id with no row means a corrupt index: report it.
                quote! {
                    self.#ids_by(parent)?
                        .into_iter()
                        .map(|id| {
                            self.get(&id)?.ok_or_else(|| ::orangedb::Error::NotFound {
                                entity: <#ident as ::orangedb::Entity>::NAME,
                                id: ::orangedb::__private::fmt_id(&id),
                            })
                        })
                        .collect()
                },
            ));
        }
    }

    let observe = match e.id.kind {
        IdKind::Auto => quote!(::orangedb::__private::observe_seq::<#ident>(self.tx, row.#id_field.0)?;),
        _ => quote!(),
    };
    out.extend([
        m(
            true,
            "Inserts a new row. Fails if the id is taken or a constraint is violated.",
            "insert",
            vec![("row", quote!(&#ident))],
            quote!(()),
            quote! {
                if ::orangedb::__private::Store::get_row(self.tx, #rows, &row.#id_field)?.is_some() {
                    return Err(::orangedb::Error::AlreadyExists {
                        entity: <#ident as ::orangedb::Entity>::NAME,
                        id: ::orangedb::__private::fmt_id(&row.#id_field),
                    });
                }
                __orangedb::#check(self.tx, row, !self.migrating)?;
                #observe
                __orangedb::#store(self.tx, row, None)
            },
        ),
        m(
            true,
            "Replaces an existing row. Fails if it does not exist or a constraint is violated.",
            "update",
            vec![("row", quote!(&#ident))],
            quote!(()),
            quote! {
                let old = self.get(&row.#id_field)?.ok_or_else(|| ::orangedb::Error::NotFound {
                    entity: <#ident as ::orangedb::Entity>::NAME,
                    id: ::orangedb::__private::fmt_id(&row.#id_field),
                })?;
                __orangedb::#check(self.tx, row, !self.migrating)?;
                __orangedb::#store(self.tx, row, Some(&old))
            },
        ),
        m(
            true,
            "Inserts the row, or replaces it if the id already exists.",
            "upsert",
            vec![("row", quote!(&#ident))],
            quote!(()),
            quote! {
                let old = self.get(&row.#id_field)?;
                __orangedb::#check(self.tx, row, !self.migrating)?;
                #observe
                __orangedb::#store(self.tx, row, old.as_ref())
            },
        ),
        m(
            true,
            "Deletes the row, applying `on_delete` to everything that references it. \
             Returns `false` if there was no such row.",
            "delete",
            vec![("id", id_arg)],
            quote!(bool),
            quote! {
                let id = id.borrow();
                if !self.contains(id)? {
                    return Ok(false);
                }
                __orangedb::delete(self.tx, __orangedb::AnyId::#ident(::std::clone::Clone::clone(id)))?;
                Ok(true)
            },
        ),
    ]);

    let fresh_id = match e.id.kind {
        IdKind::Auto => Some(quote!(#id_ty(::orangedb::__private::peek_seq::<#ident>(self.tx)?))),
        IdKind::Uuid => Some(quote!(#id_ty::generate())),
        IdKind::Natural(_) => None,
    };
    if let IdKind::Auto = e.id.kind {
        out.push(m(
            true,
            "Reserves the next auto-increment id without inserting anything.",
            "next_id",
            vec![],
            quote!(#id_ty),
            quote!(Ok(#id_ty(::orangedb::__private::next_seq::<#ident>(self.tx)?))),
        ));
    }
    if let Some(fresh_id) = fresh_id {
        out.push(m(
            true,
            "Builds a row around a freshly generated id and inserts it.",
            "create",
            vec![("build", quote!(impl ::std::ops::FnOnce(#id_ty) -> #ident))],
            quote!(#ident),
            quote! {
                let row = build(#fresh_id);
                self.insert(&row)?;
                Ok(row)
            },
        ));
    }
    out
}

fn internal_items(schema: &Schema, e: &Entity) -> TokenStream {
    let ident = &e.ident;
    let id_ty = &e.id.ty;
    let id_field = &e.id.ident;
    let rows = rows_const(e);
    let store = fn_name("store", e);
    let check = fn_name("check", e);
    let erase = fn_name("erase", e);
    let detach = fn_name("detach", e);
    let plan = fn_name("plan", e);

    let fks: Vec<&Field> = e.fields.iter().filter(|f| f.references.is_some()).collect();
    let uniques: Vec<&Field> = e.fields.iter().filter(|f| f.unique).collect();

    let consts = fks
        .iter()
        .map(|f| {
            let target_id = &schema.entity(&f.references.as_ref().unwrap().target).id.ty;
            table_const(
                e,
                &fk_const(e, f),
                quote!(MultimapTableDefinition<'static, #target_id, #id_ty>),
                &format!(".{}.fk", f.ident),
            )
        })
        .chain(uniques.iter().map(|f| {
            let inner = &f.inner;
            table_const(
                e,
                &unique_const(e, f),
                quote!(TableDefinition<'static, #inner, #id_ty>),
                &format!(".{}.unique", f.ident),
            )
        }));
    let rows_table = table_const(e, &rows, quote!(rt::RowTable<#id_ty>), "");

    let fk_names: Vec<Ident> = fks.iter().map(|f| fk_const(e, f)).collect();
    let uq_names: Vec<Ident> = uniques.iter().map(|f| unique_const(e, f)).collect();
    let fk_old: Vec<TokenStream> = fks.iter().map(|f| values(quote!(old), f)).collect();
    let fk_new: Vec<TokenStream> = fks.iter().map(|f| values(quote!(row), f)).collect();
    let uq_old: Vec<TokenStream> = uniques.iter().map(|f| values(quote!(old), f)).collect();
    let uq_new: Vec<TokenStream> = uniques.iter().map(|f| values(quote!(row), f)).collect();
    let unlink_old = quote! {
        #( for parent in #fk_old { rt::unlink(tx, #fk_names, parent, &old.#id_field)?; } )*
        #( for value in #uq_old { rt::remove(tx, #uq_names, value)?; } )*
    };

    let fk_checks = fks.iter().map(|f| {
        let target = &f.references.as_ref().unwrap().target;
        let target_rows = rows_const(schema.entity(target));
        let field = LitStr::new(&f.ident.to_string(), f.ident.span());
        let iter = values(quote!(row), f);
        quote! {
            for parent in #iter {
                if rt::Store::get_row(tx, #target_rows, parent)?.is_none() {
                    return Err(Error::ForeignKey {
                        entity: stringify!(#ident),
                        field: #field,
                        target: stringify!(#target),
                        id: rt::fmt_id(parent),
                    });
                }
            }
        }
    });
    let uq_checks = uniques.iter().map(|f| {
        let name = unique_const(e, f);
        let field = LitStr::new(&f.ident.to_string(), f.ident.span());
        let iter = values(quote!(row), f);
        quote! {
            for value in #iter {
                if let Some(owner) = rt::Store::get(tx, #name, value)? {
                    if owner != row.#id_field {
                        return Err(Error::Unique {
                            entity: stringify!(#ident),
                            field: #field,
                            value: rt::fmt_id(value),
                            owner: rt::fmt_id(&owner),
                        });
                    }
                }
            }
        }
    });

    let detach_arms: Vec<TokenStream> = fks
        .iter()
        .filter_map(|f| {
            let r = f.references.as_ref().unwrap();
            if r.on_delete != OnDelete::SetNull {
                return None;
            }
            let target = &r.target;
            let field = &f.ident;
            let name = LitStr::new(&field.to_string(), field.span());
            let clear = match f.shape {
                Shape::Option => quote! {
                    if row.#field.as_ref() == Some(parent) {
                        row.#field = None;
                    }
                },
                Shape::Vec => quote!(row.#field.retain(|p| p != parent);),
                Shape::One => unreachable!("rejected while parsing"),
            };
            Some(quote!((#name, AnyId::#target(parent)) => { #clear }))
        })
        .collect();
    let detach_body = if detach_arms.is_empty() {
        quote!(unreachable!("{} has no set_null references", stringify!(#ident)))
    } else {
        quote! {
            let Some(bytes) = rt::Store::get_row(tx, #rows, id)? else {
                return Ok(());
            };
            let old: #ident = rt::decode(&bytes)?;
            let mut row: #ident = rt::decode(&bytes)?;
            #[allow(unreachable_patterns)]
            match (field, parent) {
                #( #detach_arms )*
                _ => unreachable!("no set_null reference {}.{} to {:?}", stringify!(#ident), field, parent),
            }
            #store(tx, &row, Some(&old))
        }
    };

    let inbound = schema.entities.iter().flat_map(|child| {
        child.fields.iter().filter_map(move |f| {
            let r = f.references.as_ref()?;
            if r.target != *ident {
                return None;
            }
            let child_ident = &child.ident;
            let fk = fk_const(child, f);
            let field = LitStr::new(&f.ident.to_string(), f.ident.span());
            let referenced_by = LitStr::new(&format!("{child_ident}.{}", f.ident), f.ident.span());
            let action = match r.on_delete {
                OnDelete::Cascade => quote!(plan.cascade(AnyId::#child_ident(child))),
                OnDelete::Restrict => {
                    quote!(plan.restrict(AnyId::#ident(id.clone()), AnyId::#child_ident(child), #referenced_by))
                }
                OnDelete::SetNull => quote!(plan.detach(AnyId::#child_ident(child), #field, AnyId::#ident(id.clone()))),
            };
            Some(quote! {
                for child in rt::Store::get_all(tx, #fk, id)? {
                    #action;
                }
            })
        })
    });

    quote! {
        #rows_table
        #( #consts )*

        /// Rejects `row` if a unique value is taken or, with `fks`, if a
        /// reference dangles.
        pub fn #check(tx: &WriteTransaction, row: &#ident, fks: bool) -> Result<()> {
            if fks {
                #( #fk_checks )*
            }
            #( #uq_checks )*
            Ok(())
        }

        /// Writes `row` and moves its index entries over from `old`.
        pub fn #store(tx: &WriteTransaction, row: &#ident, old: Option<&#ident>) -> Result<()> {
            // Encode first: a codec error must not leave `old` unlinked.
            let bytes = rt::encode(row)?;
            if let Some(old) = old {
                #unlink_old
            }
            rt::put_row(tx, #rows, &row.#id_field, &bytes)?;
            #( for parent in #fk_new { rt::link(tx, #fk_names, parent, &row.#id_field)?; } )*
            #( for value in #uq_new { rt::put(tx, #uq_names, value, &row.#id_field)?; } )*
            Ok(())
        }

        /// Removes the row and its index entries.
        pub fn #erase(tx: &WriteTransaction, id: &#id_ty) -> Result<()> {
            let Some(bytes) = rt::Store::get_row(tx, #rows, id)? else {
                return Ok(());
            };
            let old: #ident = rt::decode(&bytes)?;
            rt::remove_row(tx, #rows, id)?;
            #unlink_old
            Ok(())
        }

        /// Clears the `set_null` reference `field` that points at `parent`.
        pub fn #detach(tx: &WriteTransaction, id: &#id_ty, field: &'static str, parent: &AnyId) -> Result<()> {
            #detach_body
        }

        /// Adds the effects of deleting `id` on the rows that reference it.
        pub fn #plan(tx: &WriteTransaction, id: &#id_ty, plan: &mut rt::DeletePlan<AnyId>) -> Result<()> {
            #( #inbound )*
            Ok(())
        }
    }
}

/// The catalog entry for `e`, an expression of type `catalog::Layout`.
fn layout(e: &Entity) -> TokenStream {
    let ident = &e.ident;
    let name = LitStr::new(&ident.to_string(), ident.span());
    let id_field = LitStr::new(&e.id.ident.to_string(), e.id.ident.span());
    let id_ty = LitStr::new(&e.id.ty.to_string(), e.id.ty.span());
    let fields = e.stored.iter().map(|(name, ty)| {
        let name = LitStr::new(&name.to_string(), name.span());
        let ty = match ty {
            Some(ty) => {
                let ty = type_text(ty);
                quote!(::std::option::Option::Some(#ty.into()))
            }
            None => quote!(::std::option::Option::None),
        };
        quote!(c::Field { name: #name.into(), ty: #ty })
    });
    let id_kind = match &e.id.kind {
        IdKind::Auto => quote!(c::IdKind::Auto),
        IdKind::Uuid => quote!(c::IdKind::Uuid),
        IdKind::Natural(ty) => {
            let ty = type_text(ty);
            quote!(c::IdKind::Key(#ty.into()))
        }
    };
    let rows = rows_const(e);
    let references = e.fields.iter().filter_map(|f| {
        let r = f.references.as_ref()?;
        let field = LitStr::new(&f.ident.to_string(), f.ident.span());
        let shape = shape(f.shape);
        let target = LitStr::new(&r.target.to_string(), r.target.span());
        let on_delete = match r.on_delete {
            OnDelete::Restrict => quote!(c::OnDelete::Restrict),
            OnDelete::Cascade => quote!(c::OnDelete::Cascade),
            OnDelete::SetNull => quote!(c::OnDelete::SetNull),
        };
        let fk = fk_const(e, f);
        Some(quote! {
            c::Reference {
                field: #field.into(),
                shape: #shape,
                target: #target.into(),
                on_delete: #on_delete,
                index: __orangedb::#fk.name().into(),
            }
        })
    });
    let uniques = e.fields.iter().filter(|f| f.unique).map(|f| {
        let field = LitStr::new(&f.ident.to_string(), f.ident.span());
        let shape = shape(f.shape);
        let ty = type_text(&f.inner);
        let uq = unique_const(e, f);
        quote! {
            c::Unique {
                field: #field.into(),
                shape: #shape,
                ty: #ty.into(),
                index: __orangedb::#uq.name().into(),
            }
        }
    });
    quote! {
        c::Layout {
            entity: #name.into(),
            fingerprint: <#ident as ::orangedb::Entity>::FINGERPRINT,
            id: c::Id { field: #id_field.into(), ty: #id_ty.into(), kind: #id_kind },
            fields: ::std::vec![ #( #fields ),* ],
            rows: __orangedb::#rows.name().into(),
            references: ::std::vec![ #( #references ),* ],
            uniques: ::std::vec![ #( #uniques ),* ],
            source,
        }
    }
}

fn shape(shape: Shape) -> TokenStream {
    match shape {
        Shape::One => quote!(c::Shape::One),
        Shape::Option => quote!(c::Shape::Option),
        Shape::Vec => quote!(c::Shape::Vec),
    }
}

/// A type as written, without the spaces token printing puts in it.
fn type_text(ty: &syn::Type) -> LitStr {
    LitStr::new(
        &quote!(#ty).to_string().replace(' ', ""),
        proc_macro2::Span::call_site(),
    )
}

/// An iterator over the values in field `f` of `row`, whatever its shape.
fn values(row: TokenStream, f: &Field) -> TokenStream {
    let field = &f.ident;
    match f.shape {
        Shape::One => quote!(::std::iter::once(&#row.#field)),
        Shape::Option | Shape::Vec => quote!(#row.#field.iter()),
    }
}

/// A table definition named `{Entity}@{fingerprint}{suffix}`, so that each
/// layout of an entity has tables of its own.
fn table_const(e: &Entity, name: &Ident, ty: TokenStream, suffix: &str) -> TokenStream {
    let ident = &e.ident;
    let bytes = format_ident!("{}_NAME", name);
    let len = Literal::usize_unsuffixed(ident.to_string().len() + 1 + 16 + suffix.len());
    let def = if suffix.ends_with(".fk") {
        quote!(MultimapTableDefinition)
    } else {
        quote!(TableDefinition)
    };
    quote! {
        const #bytes: [u8; #len] =
            rt::table_name(stringify!(#ident), <#ident as ::orangedb::Entity>::FINGERPRINT, #suffix);
        pub const #name: #ty = #def::new(rt::utf8(&#bytes));
    }
}

fn upper(e: &Entity) -> String {
    snake_case(&e.ident.to_string()).to_uppercase()
}

fn rows_const(e: &Entity) -> Ident {
    format_ident!("{}_ROWS", upper(e))
}

fn fk_const(e: &Entity, f: &Field) -> Ident {
    format_ident!("{}_{}_FK", upper(e), f.ident.to_string().to_uppercase())
}

fn unique_const(e: &Entity, f: &Field) -> Ident {
    format_ident!("{}_{}_UNIQUE", upper(e), f.ident.to_string().to_uppercase())
}

fn fn_name(prefix: &str, e: &Entity) -> Ident {
    format_ident!("{}_{}", prefix, snake_case(&e.ident.to_string()))
}

impl Schema {
    fn entity(&self, ident: &Ident) -> &Entity {
        self.entities
            .iter()
            .find(|e| e.ident == *ident)
            .expect("references are validated while parsing")
    }
}
