//! Turns the annotated module into a schema model, validating everything that
//! can be checked before code generation.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use syn::parse::ParseStream;
use syn::spanned::Spanned;
use syn::{
    Attribute, Fields, FnArg, GenericArgument, Ident, Item, ItemFn, ItemMod, ItemStruct, PathArguments, Token, Type,
};

use crate::print::{self, Errors, FieldKind, Print};

pub struct Schema {
    pub entities: Vec<Entity>,
    pub migration: Option<Migration>,
}

/// A `#[migrate]` function: `fn(old: &old_schema::ReadTx, new: &WriteTx)`.
pub struct Migration {
    pub function: Ident,
    /// The old schema's `ReadTx`.
    pub old: Type,
}

pub struct Entity {
    pub ident: Ident,
    pub vis: syn::Visibility,
    pub accessor: Ident,
    pub id: IdField,
    pub fields: Vec<Field>,
    /// A const expression for `Entity::FINGERPRINT`.
    pub fingerprint: TokenStream,
}

pub struct IdField {
    pub ident: Ident,
    /// The newtype the macro generates, named by the field's declared type.
    pub ty: Ident,
    pub kind: IdKind,
}

pub enum IdKind {
    Auto,
    Uuid,
    Natural(Box<Type>),
}

impl IdKind {
    pub fn inner(&self) -> Type {
        match self {
            IdKind::Auto => syn::parse_quote!(u64),
            IdKind::Uuid => syn::parse_quote!(::orangedb::__private::uuid::Uuid),
            IdKind::Natural(ty) => (**ty).clone(),
        }
    }
}

pub struct Field {
    pub ident: Ident,
    pub shape: Shape,
    /// `T` in `T`, `Option<T>` or `Vec<T>`.
    pub inner: Type,
    pub references: Option<Reference>,
    pub unique: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    One,
    Option,
    Vec,
}

pub struct Reference {
    pub target: Ident,
    pub on_delete: OnDelete,
}

#[derive(Clone, Copy, PartialEq)]
pub enum OnDelete {
    Restrict,
    Cascade,
    SetNull,
}

/// Parses the module, strips orangedb attributes from it in place and
/// returns the schema they described.
pub fn schema(module: &mut ItemMod) -> syn::Result<Schema> {
    let Some((_, items)) = &mut module.content else {
        return Err(syn::Error::new(
            module.span(),
            "#[orangedb::schema] needs an inline module: `mod name { ... }`",
        ));
    };

    let mut entities = Vec::new();
    let mut migration: Option<Migration> = None;
    let mut errors = Errors::default();
    for item in items.iter_mut() {
        if let Item::Struct(s) = item
            && let Some(attr) = take_attr(&mut s.attrs, "entity")
        {
            match entity(attr, s) {
                Ok(e) => entities.push(e),
                Err(e) => errors.push(e),
            }
        }
        if let Item::Fn(f) = item
            && let Some(attr) = take_attr(&mut f.attrs, "migrate")
        {
            match migrate(attr, f) {
                Ok(_) if migration.is_some() => errors.push(syn::Error::new(
                    f.sig.ident.span(),
                    "a schema has one #[migrate] function, from the schema before it; \
                     older schemas migrate with their own",
                )),
                Ok(m) => migration = Some(m),
                Err(e) => errors.push(e),
            }
        }
    }

    let ids: HashMap<String, &Entity> = entities.iter().map(|e| (e.ident.to_string(), e)).collect();
    for e in &entities {
        if let Some(other) = entities.iter().find(|o| o.ident != e.ident && o.accessor == e.accessor) {
            errors.push(syn::Error::new(
                e.accessor.span(),
                format!(
                    "{} and {} both use the accessor `{}`; set #[entity(accessor = ...)] on one of them",
                    e.ident, other.ident, e.accessor
                ),
            ));
        }
        for f in &e.fields {
            let Some(r) = &f.references else { continue };
            if !ids.contains_key(&r.target.to_string()) {
                errors.push(syn::Error::new(
                    r.target.span(),
                    format!("`{}` is not an #[entity] in this schema", r.target),
                ));
            }
        }
    }
    if entities.is_empty() {
        errors.push(syn::Error::new(
            module.ident.span(),
            "the schema has no #[entity] structs",
        ));
    }

    errors.finish()?;
    Ok(Schema { entities, migration })
}

fn migrate(attr: Attribute, f: &ItemFn) -> syn::Result<Migration> {
    let usage = "expected `fn name(old: &old_schema::ReadTx, new: &WriteTx) -> orangedb::Result<()>`";
    if !matches!(attr.meta, syn::Meta::Path(_)) {
        return Err(syn::Error::new(attr.span(), "#[migrate] takes no arguments"));
    }
    let args: Vec<&FnArg> = f.sig.inputs.iter().collect();
    let [FnArg::Typed(old), FnArg::Typed(_)] = args.as_slice() else {
        return Err(syn::Error::new(f.sig.inputs.span(), usage));
    };
    let Type::Reference(old) = &*old.ty else {
        return Err(syn::Error::new(old.ty.span(), usage));
    };
    if old.mutability.is_some() || !f.sig.generics.params.is_empty() {
        return Err(syn::Error::new(f.sig.span(), usage));
    }
    Ok(Migration {
        function: f.sig.ident.clone(),
        old: (*old.elem).clone(),
    })
}

fn entity(attr: Attribute, s: &mut ItemStruct) -> syn::Result<Entity> {
    let mut accessor = None;
    if !matches!(attr.meta, syn::Meta::Path(_)) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("accessor") {
                accessor = Some(meta.value()?.parse::<Ident>()?);
                Ok(())
            } else if meta.path.is_ident("version") {
                Err(meta.error(
                    "entities have no version: the fingerprint is the version, and a changed \
                     entity is migrated with a #[migrate] function",
                ))
            } else {
                Err(meta.error("expected `accessor = name`"))
            }
        })?;
    }

    if !s.generics.params.is_empty() {
        return Err(syn::Error::new(s.generics.span(), "entities cannot be generic"));
    }
    let Fields::Named(named) = &mut s.fields else {
        return Err(syn::Error::new(s.span(), "entities must have named fields"));
    };

    let mut errors = Errors::default();
    let mut id = None;
    let mut fields = Vec::new();
    let mut fingerprint = Print::new("entity");
    fingerprint.str(print::FORMAT);
    match print::serde_attrs(&s.attrs, false) {
        Ok(wire) => fingerprint.str(&wire.text),
        Err(e) => errors.push(e),
    }
    for f in named.named.iter_mut() {
        let ident = f.ident.clone().expect("named field");
        let id_attr = take_attr(&mut f.attrs, "id");
        let ref_attr = take_attr(&mut f.attrs, "references");
        let unique_attr = take_attr(&mut f.attrs, "unique");
        match print::field(&mut fingerprint, f, &ident.to_string()) {
            Ok(FieldKind::Skipped) if id_attr.is_some() || ref_attr.is_some() || unique_attr.is_some() => {
                errors.push(syn::Error::new(
                    ident.span(),
                    "#[id], #[references] and #[unique] fields can't be `serde(skip)`",
                ));
            }
            Ok(FieldKind::Labelled) if id_attr.is_some() => {
                errors.push(syn::Error::new(
                    ident.span(),
                    "the id's stored format is its key type's, so it can't have a #[fingerprint] label",
                ));
            }
            Ok(_) => {}
            Err(e) => errors.push(e),
        }

        if let Some(attr) = id_attr {
            if ref_attr.is_some() || unique_attr.is_some() {
                errors.push(syn::Error::new(
                    attr.span(),
                    "the id field cannot also be #[references] or #[unique]",
                ));
            }
            if id.is_some() {
                errors.push(syn::Error::new(attr.span(), "an entity can only have one #[id] field"));
                continue;
            }
            match id_field(&attr, ident, &f.ty) {
                Ok(field) => {
                    // The key type itself is covered by the id newtype's
                    // fingerprint, which is its inner type's.
                    fingerprint.str(match field.kind {
                        IdKind::Auto => "id:auto",
                        IdKind::Uuid => "id:uuid",
                        IdKind::Natural(_) => "id:key",
                    });
                    id = Some(field);
                }
                Err(e) => errors.push(e),
            }
            continue;
        }

        let (shape, inner) = shape(&f.ty);
        let references = match ref_attr {
            None => None,
            Some(attr) => match reference(&attr) {
                Ok(r) => {
                    if r.on_delete == OnDelete::SetNull && shape == Shape::One {
                        errors.push(syn::Error::new(
                            attr.span(),
                            "on_delete = set_null needs an Option<_> or Vec<_> field",
                        ));
                    }
                    fingerprint.str("references");
                    fingerprint.str(&r.target.to_string());
                    Some(r)
                }
                Err(e) => {
                    errors.push(e);
                    None
                }
            },
        };
        if let Some(attr) = &unique_attr {
            if !matches!(attr.meta, syn::Meta::Path(_)) {
                errors.push(syn::Error::new(attr.span(), "#[unique] takes no arguments"));
            }
            if shape == Shape::Vec {
                errors.push(syn::Error::new(attr.span(), "#[unique] is not supported on Vec fields"));
            }
            fingerprint.str("unique");
        }
        fields.push(Field {
            ident,
            shape,
            inner,
            references,
            unique: unique_attr.is_some(),
        });
    }

    let id = match id {
        Some(id) => Some(id),
        None => {
            errors.push(syn::Error::new(
                s.ident.span(),
                "missing #[id] field: mark one with #[id(auto)], #[id(uuid)] or #[id(KeyType)]",
            ));
            None
        }
    };
    errors.finish()?;

    let accessor = accessor.unwrap_or_else(|| Ident::new(&plural(&snake_case(&s.ident.to_string())), s.ident.span()));
    Ok(Entity {
        ident: s.ident.clone(),
        vis: s.vis.clone(),
        accessor,
        id: id.expect("checked above"),
        fields,
        fingerprint: fingerprint.finish(),
    })
}

fn id_field(attr: &Attribute, ident: Ident, ty: &Type) -> syn::Result<IdField> {
    let usage = "expected #[id(auto)], #[id(uuid)] or #[id(KeyType)], e.g. #[id(String)]";
    let arg: Type = attr.parse_args().map_err(|e| syn::Error::new(e.span(), usage))?;
    let kind = match &arg {
        Type::Path(p) if p.path.is_ident("auto") => IdKind::Auto,
        Type::Path(p) if p.path.is_ident("uuid") => IdKind::Uuid,
        _ => IdKind::Natural(Box::new(arg)),
    };
    let Type::Path(p) = ty else {
        return Err(syn::Error::new(
            ty.span(),
            "the id field's type names the id type to generate, e.g. `UserId`",
        ));
    };
    let Some(ty) = p.path.get_ident() else {
        return Err(syn::Error::new(
            ty.span(),
            "the id field's type names the id type to generate, e.g. `UserId`",
        ));
    };
    Ok(IdField {
        ident,
        ty: ty.clone(),
        kind,
    })
}

fn reference(attr: &Attribute) -> syn::Result<Reference> {
    attr.parse_args_with(|input: ParseStream| {
        let target: Ident = input.parse()?;
        let mut on_delete = OnDelete::Restrict;
        if input.parse::<Option<Token![,]>>()?.is_some() && !input.is_empty() {
            let key: Ident = input.parse()?;
            if key != "on_delete" {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `on_delete = restrict | cascade | set_null`",
                ));
            }
            input.parse::<Token![=]>()?;
            let action: Ident = input.parse()?;
            on_delete = match action.to_string().as_str() {
                "restrict" => OnDelete::Restrict,
                "cascade" => OnDelete::Cascade,
                "set_null" => OnDelete::SetNull,
                _ => {
                    return Err(syn::Error::new(
                        action.span(),
                        "expected `restrict`, `cascade` or `set_null`",
                    ));
                }
            };
            input.parse::<Option<Token![,]>>()?;
        }
        Ok(Reference { target, on_delete })
    })
}

fn take_attr(attrs: &mut Vec<Attribute>, name: &str) -> Option<Attribute> {
    let pos = attrs.iter().position(|a| a.path().is_ident(name))?;
    Some(attrs.remove(pos))
}

fn shape(ty: &Type) -> (Shape, Type) {
    if let Type::Path(p) = ty
        && p.qself.is_none()
        && let Some(last) = p.path.segments.last()
        && let PathArguments::AngleBracketed(args) = &last.arguments
        && args.args.len() == 1
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        if last.ident == "Option" {
            return (Shape::Option, inner.clone());
        }
        if last.ident == "Vec" {
            return (Shape::Vec, inner.clone());
        }
    }
    (Shape::One, ty.clone())
}

pub fn snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn plural(s: &str) -> String {
    let consonant_y = s.ends_with('y') && !s[..s.len() - 1].ends_with(['a', 'e', 'i', 'o', 'u']);
    if consonant_y {
        format!("{}ies", &s[..s.len() - 1])
    } else if s.ends_with(['s', 'x', 'z']) || s.ends_with("ch") || s.ends_with("sh") {
        format!("{s}es")
    } else {
        format!("{s}s")
    }
}
