//! Fingerprint expressions. The macro can't see other types, so it emits a
//! const expression that combines the parts it can see (names, shapes, serde
//! attributes) with the `Fingerprint` consts of the field types, which the
//! compiler then evaluates.

use proc_macro2::TokenStream;
use quote::{ToTokens, quote, quote_spanned};
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Field, Fields, Lit, Meta, Token, Type};

/// Bumped whenever the way rows are encoded changes, which changes every
/// fingerprint with it.
pub const FORMAT: &str = "orangedb/postcard-1";

/// A fingerprint under construction: a chain of `Fp` calls.
pub struct Print(TokenStream);

impl Print {
    pub fn new(tag: &str) -> Self {
        Print(quote!(::orangedb::__private::Fp::new(#tag)))
    }

    pub fn str(&mut self, s: &str) {
        self.0.extend(quote!(.str(#s)));
    }

    pub fn u64(&mut self, value: TokenStream) {
        self.0.extend(quote!(.u64(#value)));
    }

    pub fn finish(self) -> TokenStream {
        let chain = self.0;
        quote!(#chain.finish())
    }
}

/// How a field takes part in its container's fingerprint.
pub enum FieldKind {
    /// Through its type's `Fingerprint`.
    Typed,
    /// Through a label from `#[fingerprint = "..."]`.
    Labelled,
    /// `#[serde(skip)]`: not stored at all.
    Skipped,
}

/// Adds a field to `print` and strips its `#[fingerprint]` attribute when
/// `strip` is set.
pub fn field(print: &mut Print, f: &mut Field, name: &str, strip: bool) -> syn::Result<FieldKind> {
    let label = take_label(&mut f.attrs, strip)?;
    let wire = serde_attrs(&f.attrs, label.is_some())?;
    print.str("field");
    print.str(name);
    print.str(&wire.text);
    Ok(if wire.skip {
        FieldKind::Skipped
    } else if let Some(label) = label {
        print.str("label");
        print.str(&label);
        FieldKind::Labelled
    } else {
        print.u64(type_print(&f.ty));
        FieldKind::Typed
    })
}

/// `<T as Fingerprint>::FINGERPRINT`, so that a type without one fails to
/// compile with the error pointing at it.
pub fn type_print(ty: &Type) -> TokenStream {
    quote_spanned!(ty.span()=> <#ty as ::orangedb::Fingerprint>::FINGERPRINT)
}

/// Adds every field of a struct or enum variant.
pub fn fields(print: &mut Print, fields: &mut Fields, strip: bool) -> syn::Result<()> {
    let mut errors = Errors::default();
    match fields {
        Fields::Named(named) => {
            print.str("named");
            for f in named.named.iter_mut() {
                let name = f.ident.as_ref().expect("named field").to_string();
                if let Err(e) = field(print, f, &name, strip) {
                    errors.push(e);
                }
            }
        }
        Fields::Unnamed(unnamed) => {
            print.str("tuple");
            for (i, f) in unnamed.unnamed.iter_mut().enumerate() {
                if let Err(e) = field(print, f, &i.to_string(), strip) {
                    errors.push(e);
                }
            }
        }
        Fields::Unit => print.str("unit"),
    }
    errors.finish()
}

/// `#[fingerprint = "..."]`, removed from the attributes when `strip` is set.
fn take_label(attrs: &mut Vec<Attribute>, strip: bool) -> syn::Result<Option<String>> {
    let Some(pos) = attrs.iter().position(|a| a.path().is_ident("fingerprint")) else {
        return Ok(None);
    };
    let attr = if strip { attrs.remove(pos) } else { attrs[pos].clone() };
    let usage = "expected #[fingerprint = \"...\"], a label that you change whenever the field's stored format changes";
    match &attr.meta {
        Meta::NameValue(nv) => match &nv.value {
            Expr::Lit(lit) => match &lit.lit {
                Lit::Str(s) => Ok(Some(s.value())),
                _ => Err(syn::Error::new(lit.span(), usage)),
            },
            other => Err(syn::Error::new(other.span(), usage)),
        },
        _ => Err(syn::Error::new(attr.span(), usage)),
    }
}

/// The serde attributes of one item, reduced to what changes the stored
/// bytes.
pub struct Wire {
    /// The attributes that matter, in a canonical form.
    pub text: String,
    pub skip: bool,
}

/// Rows are stored with postcard, which writes fields by position and enum
/// variants by index and has no field names or type tags. So renames,
/// aliases and defaults don't change the stored bytes; `skip` and
/// `transparent` do; and anything that needs a self-describing format, or
/// that hides the format behind custom code, is rejected. Attributes this
/// list doesn't know about are kept, to be safe.
///
/// `labelled` is whether the field has a `#[fingerprint]` label, which
/// vouches for custom (de)serializers.
pub fn serde_attrs(attrs: &[Attribute], labelled: bool) -> syn::Result<Wire> {
    let mut wire = Wire {
        text: String::new(),
        skip: false,
    };
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            let name = tokens(&meta.path);
            let value = value(&meta)?;
            let unsupported = |why: &str| Err(meta.error(format!("`serde({name})` is not supported: {why}")));
            match name.as_str() {
                "rename"
                | "rename_all"
                | "rename_all_fields"
                | "alias"
                | "default"
                | "bound"
                | "crate"
                | "deny_unknown_fields"
                | "expecting"
                | "borrow" => {}
                "skip" => {
                    wire.skip = true;
                    wire.text.push_str("skip;");
                }
                "transparent" => wire.text.push_str("transparent;"),
                "with" | "serialize_with" | "deserialize_with" if labelled => {
                    wire.text.push_str(&format!("{name}={value};"));
                }
                "with" | "serialize_with" | "deserialize_with" => {
                    return Err(meta.error(format!(
                        "orangedb can't see the stored format behind `serde({name})`; \
                         describe it with #[fingerprint = \"...\"] on the field"
                    )));
                }
                "flatten" | "untagged" | "tag" | "content" => unsupported(
                    "it needs a self-describing format, and orangedb stores rows with postcard, \
                     which isn't one",
                )?,
                "skip_serializing" | "skip_deserializing" | "skip_serializing_if" => unsupported(
                    "it would read rows back differently from how they were written; \
                     use `serde(skip)` to leave a field out entirely",
                )?,
                "from" | "into" | "try_from" | "remote" | "getter" => unsupported(
                    "it hides the stored format from the fingerprint; \
                     use a field with #[fingerprint = \"...\"] instead",
                )?,
                "other" | "variant_identifier" | "field_identifier" => {
                    unsupported("orangedb stores enums by variant index")?
                }
                _ => wire.text.push_str(&format!("{name}={value};")),
            }
            Ok(())
        })?;
    }
    Ok(wire)
}

/// Consumes the value of a nested meta, if any, and returns it as text.
fn value(meta: &ParseNestedMeta) -> syn::Result<String> {
    if meta.input.peek(Token![=]) {
        let value: Expr = meta.value()?.parse()?;
        Ok(tokens(&value))
    } else if meta.input.peek(syn::token::Paren) {
        let group: proc_macro2::Group = meta.input.parse()?;
        Ok(tokens(&group))
    } else {
        Ok(String::new())
    }
}

pub fn tokens(t: &impl ToTokens) -> String {
    t.to_token_stream().to_string().split_whitespace().collect()
}

#[derive(Default)]
pub struct Errors(Option<syn::Error>);

impl Errors {
    pub fn push(&mut self, e: syn::Error) {
        match &mut self.0 {
            Some(all) => all.combine(e),
            None => self.0 = Some(e),
        }
    }

    pub fn finish(self) -> syn::Result<()> {
        self.0.map_or(Ok(()), Err)
    }
}
