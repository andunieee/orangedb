mod embed;
mod expand;
mod parse;
mod print;

use proc_macro::TokenStream;
use syn::{Item, ItemMod, parse_macro_input};

/// Turns a module of structs into a typed database schema.
///
/// ```ignore
/// #[orangedb::schema]
/// pub mod blog {
///     #[entity]
///     pub struct User {
///         #[id(auto)]
///         pub id: UserId,
///         #[unique]
///         pub email: String,
///     }
///
///     #[entity(accessor = replies)]
///     pub struct Reply {
///         #[id(auto)]
///         pub id: ReplyId,
///         #[references(User, on_delete = restrict)]
///         pub author: UserId,
///         #[references(Reply, on_delete = cascade)]
///         pub parent: Option<ReplyId>,
///     }
/// }
/// ```
///
/// # Entities
///
/// `#[entity]` marks a struct as a table. It accepts `accessor = name` (the
/// method on `Db`/`ReadTx`/`WriteTx`, by default the plural snake case of the
/// struct name). Serde derives are added for you, so don't add your own.
///
/// Every field's type must have an `orangedb::Fingerprint`: std types and
/// ids do, and your own structs and enums get one from `#[embed]` (or
/// `#[orangedb::embed]` outside the schema module). For a type you can't
/// annotate, such as one from another crate stored through
/// `#[serde(with = ...)]`, give the field `#[fingerprint = "label"]` and
/// change the label whenever its stored format changes.
///
/// # Ids
///
/// Exactly one field is the `#[id]`. Its type names a newtype that the macro
/// generates, and the attribute picks what it wraps:
///
/// - `#[id(auto)]`: `u64`, allocated from a per-table sequence by `create`.
/// - `#[id(KeyType)]`: a caller-supplied key of any redb key type whose
///   decoded form is owned, such as `String` or `u32`.
///
/// # References
///
/// `#[references(Entity, on_delete = ...)]` on a field of type `EntityId`,
/// `Option<EntityId>` or `Vec<EntityId>` makes it a foreign key: writes fail
/// if a referenced row is missing, and an index answers `by_field(parent)`.
/// `on_delete` is `restrict` (the default), `cascade` or `set_null`, which
/// sets an `Option` to `None` or removes the id from a `Vec`.
///
/// # Indexes
///
/// `#[index]` on a field of a redb key type, or an `Option` or `Vec` of one,
/// indexes its value, the `Some` or each element. `by_field(value)` returns
/// the rows that hold `value` in id order, `ids_by_field(value)` their ids,
/// and `range_by_field(range)` the rows whose value falls in a range, in key
/// order. `#[unique]` does the same but rejects a value another row already
/// holds, and its `by_field(value)` returns an `Option`.
///
/// An index on the struct computes its keys:
///
/// ```ignore
/// #[entity]
/// #[index(email_lower: String = |u| u.email.to_lowercase(), unique)]
/// #[index(tag: String = |u| u.tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>())]
/// #[index(country_age: (String, u8) = |u| (u.country.clone(), u.age))]
/// pub struct User { /* ... */ }
/// ```
///
/// The expression takes `&Entity` and returns the key type, an `Option` of
/// it (no key) or a `Vec` of it (any number of keys); it is a closure or the
/// path of a function. It must depend on nothing but the row, since a write
/// recomputes the old row's keys to remove them. A tuple key orders by its
/// parts, so `range_by_country_age(("FR".into(), 0)..("FR".into(), 30))`
/// finds the French users under 30, youngest first.
///
/// Indexes aren't part of the fingerprint. Each has a fingerprint of its
/// own, of what it indexes and how: for a computed index, the expression's
/// tokens and the key type. Opening a database builds an index it doesn't
/// have yet from the rows (failing with `Error::Unique` if a unique one
/// finds a duplicate, and changing nothing), and drops the index tables the
/// schema doesn't declare. So adding, changing or removing an index needs no
/// migration, and reopening with the same indexes builds nothing. The macro
/// can't see into the functions the expression calls: when one of them
/// changes the keys it returns, add or change a label with
/// `#[index(..., fingerprint = "2")]` to have the index rebuilt.
///
/// # Fingerprints and migrations
///
/// Each entity has a fingerprint of how its rows are stored and how they
/// reference others, which the database records. Opening a database that stores a different
/// fingerprint for an entity runs the schema's `#[migrate]` functions, or
/// fails with `Error::SchemaMismatch` if it has none. The usual kind
/// migrates one entity that changed, a row at a time:
///
/// ```ignore
/// #[orangedb::schema]
/// pub mod v2 {
///     use super::v1;
///
///     #[migrate]
///     fn user_from_v1(old: v1::User) -> orangedb::Result<User> {
///         Ok(User { id: UserId(old.id.0), email: old.email, verified: false })
///     }
///
///     #[entity]
///     pub struct User { /* ... */ }
/// }
/// ```
///
/// Every entity that changed needs one, from the old entity of the same name,
/// and none that didn't change may have one: both are compile errors.
///
/// A schema can also have one function for the whole database,
/// `fn(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()>`, which
/// reads it as the old schema saw it and writes it at the new one. It is
/// needed when an entity of the old schema is gone, and it migrates the
/// changed entities that have no function of their own. Prefer the
/// per-entity functions where they do the job: they don't load whole tables
/// and the compiler checks that they cover every change. The per-entity
/// functions run first.
///
/// It all happens in a single transaction. Entities whose fingerprint didn't
/// change are already there; the others start out empty, and their old
/// tables are dropped afterwards, as are entities the new schema no longer
/// declares. References are checked once the migration is done, so rows can
/// be written in any order. If the database is older still, the old schema's
/// own `#[migrate]` functions run first.
///
/// The database also records a catalog of each entity's layout (its tables,
/// fields, references and indexes) and this module's source, which
/// `Db::stored_catalog()`, `Db::stored_source::<Entity>()` and the `orangedb`
/// command-line tool read. Neither is used to decide compatibility, and
/// `#[orangedb::embed]` types declared outside the module are not part of
/// the source.
#[proc_macro_attribute]
pub fn schema(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(proc_macro2::Span::call_site(), "#[orangedb::schema] takes no arguments")
            .into_compile_error()
            .into();
    }
    let mut module = parse_macro_input!(item as ItemMod);
    let source = module_source(&module);
    let schema = match parse::schema(&mut module) {
        Ok(schema) => schema,
        Err(e) => return e.into_compile_error().into(),
    };

    let items = &mut module.content.as_mut().expect("checked while parsing").1;
    let mut embedded = Vec::new();
    for item in items.iter_mut() {
        let is_embedded = match item {
            Item::Struct(s) => s.attrs.iter().any(|a| a.path().is_ident("embed")),
            Item::Enum(e) => e.attrs.iter().any(|a| a.path().is_ident("embed")),
            _ => false,
        };
        if is_embedded {
            // Keep `#[embed]` too: the helper derive below declares it, so
            // editors resolve it while the fingerprint impl is generated here.
            match embed::expand(item, false) {
                Ok(tokens) => embedded.push(tokens),
                Err(e) => return e.into_compile_error().into(),
            }
            helper_derive(item);
        }
    }
    items.extend(embedded.into_iter().map(Item::Verbatim));
    for item in items.iter_mut() {
        if let Item::Struct(s) = item
            && schema.entities.iter().any(|e| e.ident == s.ident)
        {
            // Ahead of the struct's own attributes, which may use `serde`.
            s.attrs.insert(
                0,
                syn::parse_quote! {
                    #[derive(
                        ::orangedb::__private::serde::Serialize,
                        ::orangedb::__private::serde::Deserialize,
                        ::orangedb::__private::SchemaAttributes
                    )]
                },
            );
            s.attrs
                .insert(1, syn::parse_quote!(#[serde(crate = "::orangedb::__private::serde")]));
        }
    }
    items.push(Item::Verbatim(expand::expand(&schema, &source)));
    quote::quote!(#module).into()
}

/// Adds the derive that declares the schema DSL attributes as helpers, so
/// that editors can resolve `#[entity]`, `#[id]`, `#[references]` and the
/// rest. It generates nothing; `schema` reads the attributes itself.
fn helper_derive(item: &mut Item) {
    let attrs = match item {
        Item::Struct(s) => &mut s.attrs,
        Item::Enum(e) => &mut e.attrs,
        _ => return,
    };
    attrs.insert(0, syn::parse_quote!(#[derive(::orangedb::__private::SchemaAttributes)]));
}

/// The module as it was written, before `schema` consumes `#[migrate]`.
/// Falls back to the token stream when the text isn't available or doesn't
/// round-trip, as for modules generated by `macro_rules!`.
///
/// Stable Rust can't join spans, so the text is assembled from spans that
/// cover one token tree each: every outer attribute, the header, and the
/// braced body, which is a single group.
fn module_source(module: &ItemMod) -> String {
    let tokens = quote::quote!(#module).to_string();
    written_source(module)
        .filter(|text| syn::parse_str::<ItemMod>(text).is_ok_and(|parsed| quote::quote!(#parsed).to_string() == tokens))
        .unwrap_or(tokens)
}

fn written_source(module: &ItemMod) -> Option<String> {
    let mut out = String::new();
    for attr in &module.attrs {
        // A doc comment's brackets span the comment itself.
        let text = attr.bracket_token.span.join().source_text()?;
        if text.starts_with('[') {
            out.push('#');
        }
        out.push_str(&text);
        out.push('\n');
    }
    let ItemMod {
        vis, unsafety, ident, ..
    } = module;
    out.push_str(&quote::quote!(#vis #unsafety mod #ident).to_string());
    out.push(' ');
    let body = module.content.as_ref()?.0.span.join().source_text()?;
    // Lines after the first are indented as far as the module itself is,
    // which the closing brace's line shows.
    let last = body.rsplit('\n').next().unwrap_or_default();
    let indent = last
        .strip_suffix('}')
        .filter(|i| i.trim().is_empty())
        .unwrap_or_default();
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(if i > 0 {
            line.strip_prefix(indent).unwrap_or(line)
        } else {
            line
        });
    }
    Some(out)
}

/// Declares a struct or enum that entities can store in their fields, by
/// deriving serde for it and giving it an `orangedb::Fingerprint`. Inside
/// a schema module, write it as `#[embed]`.
///
/// ```ignore
/// #[orangedb::embed]
/// pub struct Address {
///     pub street: String,
///     pub city: String,
/// }
/// ```
///
/// The fingerprint covers the field and variant names, their order and
/// shapes, the fields' own fingerprints and the serde attributes that change
/// the stored bytes. Generic types are fine; recursive ones are not.
#[proc_macro_attribute]
pub fn embed(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(proc_macro2::Span::call_site(), "#[orangedb::embed] takes no arguments")
            .into_compile_error()
            .into();
    }
    let mut item = parse_macro_input!(item as Item);
    match embed::expand(&mut item, true) {
        Ok(impls) => quote::quote!(#item #impls).into(),
        Err(e) => e.into_compile_error().into(),
    }
}

/// Declares the attributes that [`schema`] reads as derive helper attributes,
/// so that editors can resolve them. It generates nothing.
///
/// - `#[entity]` marks a struct as a table.
/// - `#[id]` picks the id field and what its generated newtype wraps.
/// - `#[references]` makes a field a foreign key.
/// - `#[unique]` indexes a field and rejects duplicates.
/// - `#[index]` indexes a field, or computes keys for the entity.
/// - `#[fingerprint]` labels a field whose stored format the macro can't see.
/// - `#[embed]` stores a struct or enum in fields.
///
/// See [`schema`] for the details. `schema` adds this derive to the items that
/// use them, which is why it isn't part of the public API.
#[doc(hidden)]
#[proc_macro_derive(
    SchemaAttributes,
    attributes(entity, id, references, unique, index, fingerprint, embed)
)]
pub fn schema_attributes(_: TokenStream) -> TokenStream {
    TokenStream::new()
}
