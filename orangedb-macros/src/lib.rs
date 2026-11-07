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
/// fingerprint for an entity runs the schema's `#[migrate]` function, or
/// fails with `Error::SchemaMismatch` if it has none:
///
/// ```ignore
/// #[orangedb::schema]
/// pub mod v2 {
///     use super::v1;
///
///     #[migrate]
///     fn from_v1(old: &v1::ReadTx, new: &WriteTx) -> orangedb::Result<()> {
///         for user in old.users().all()? {
///             new.users().insert(&User { id: UserId(user.id.0), email: user.email, verified: false })?;
///         }
///         Ok(())
///     }
///
///     #[entity]
///     pub struct User { /* ... */ }
/// }
/// ```
///
/// The function reads the database as the old schema saw it and writes it
/// at the new one, in a single transaction. Entities whose fingerprint
/// didn't change are already there; the others start out empty, and their
/// old tables are dropped afterwards, as are entities the new schema no
/// longer declares. References are checked once the function returns, so
/// rows can be written in any order. If the database is older still, the old
/// schema's own `#[migrate]` function runs first.
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
        let attrs = match item {
            Item::Struct(s) => &mut s.attrs,
            Item::Enum(e) => &mut e.attrs,
            _ => continue,
        };
        if let Some(pos) = attrs.iter().position(|a| a.path().is_ident("embed")) {
            attrs.remove(pos);
            match embed::expand(item) {
                Ok(tokens) => embedded.push(tokens),
                Err(e) => return e.into_compile_error().into(),
            }
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
                    #[derive(::orangedb::__private::serde::Serialize, ::orangedb::__private::serde::Deserialize)]
                },
            );
            s.attrs
                .insert(1, syn::parse_quote!(#[serde(crate = "::orangedb::__private::serde")]));
        }
    }
    items.push(Item::Verbatim(expand::expand(&schema, &source)));
    quote::quote!(#module).into()
}

/// The module as it was written, before `schema` strips attributes from it.
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
    match embed::expand(&mut item) {
        Ok(impls) => quote::quote!(#item #impls).into(),
        Err(e) => e.into_compile_error().into(),
    }
}
