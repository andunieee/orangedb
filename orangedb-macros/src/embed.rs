//! `#[orangedb::embed]`: a struct or enum that entities can store in their
//! fields.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{GenericParam, Item, parse_quote};

use crate::print::{self, Errors, Print};

/// Adds the serde derives to `item` and returns its `Fingerprint` impl.
///
/// `strip` removes the `#[fingerprint]` labels the fingerprint consumed: it
/// is set for a standalone `#[orangedb::embed]`, but not inside a schema
/// module, where a derive keeps them resolvable for editors.
pub fn expand(item: &mut Item, strip: bool) -> syn::Result<TokenStream> {
    let (attrs, generics, ident, print) = match item {
        Item::Struct(s) => {
            let mut print = Print::new("struct");
            print.str(&print::serde_attrs(&s.attrs, false)?.text);
            print::fields(&mut print, &mut s.fields, strip)?;
            (&mut s.attrs, &mut s.generics, &s.ident, print)
        }
        Item::Enum(e) => {
            let mut print = Print::new("enum");
            print.str(&print::serde_attrs(&e.attrs, false)?.text);
            let mut errors = Errors::default();
            for v in e.variants.iter_mut() {
                print.str("variant");
                print.str(&v.ident.to_string());
                let result = print::serde_attrs(&v.attrs, false).and_then(|wire| {
                    print.str(&wire.text);
                    print::fields(&mut print, &mut v.fields, strip)
                });
                if let Err(err) = result {
                    errors.push(err);
                }
            }
            errors.finish()?;
            (&mut e.attrs, &mut e.generics, &e.ident, print)
        }
        other => {
            return Err(syn::Error::new(
                other.span(),
                "#[orangedb::embed] goes on a struct or an enum",
            ));
        }
    };

    for param in &generics.params {
        match param {
            GenericParam::Type(_) => {}
            GenericParam::Lifetime(l) => {
                return Err(syn::Error::new(
                    l.span(),
                    "embedded types can't borrow: rows are decoded into owned values",
                ));
            }
            GenericParam::Const(c) => {
                return Err(syn::Error::new(c.span(), "embedded types can't have const parameters"));
            }
        }
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let mut bounded = where_clause.cloned().unwrap_or_else(|| parse_quote!(where));
    for param in generics.type_params() {
        let t = &param.ident;
        bounded.predicates.push(parse_quote!(#t: ::orangedb::Fingerprint));
    }
    let print = print.finish();
    let impls = quote! {
        impl #impl_generics ::orangedb::__private::Sealed for #ident #ty_generics #where_clause {}

        impl #impl_generics ::orangedb::Fingerprint for #ident #ty_generics #bounded {
            const FINGERPRINT: u64 = #print;
        }
    };

    // Ahead of the item's own attributes, which may use `serde`.
    attrs.insert(
        0,
        parse_quote! {
            #[derive(::orangedb::__private::serde::Serialize, ::orangedb::__private::serde::Deserialize)]
        },
    );
    attrs.insert(1, parse_quote!(#[serde(crate = "::orangedb::__private::serde")]));
    Ok(impls)
}
