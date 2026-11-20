//! Syntax-only cfg formulae; satisfiability and attribution live in inventory.py.
use quote::ToTokens;
use serde_json::{json, Value};
use syn::{parse::Parser, punctuated::Punctuated, Meta, Token};

fn list(meta: &syn::MetaList) -> syn::Result<Vec<Meta>> {
    Ok(Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(meta.tokens.clone())?
        .into_iter()
        .collect())
}

pub fn predicate(meta: &Meta) -> syn::Result<Value> {
    match meta {
        Meta::Path(p) if p.is_ident("test") => Ok(json!("test")),
        Meta::Path(_) | Meta::NameValue(_) => Ok(json!(meta.to_token_stream().to_string())),
        Meta::List(m) => {
            let op = m.path.to_token_stream().to_string();
            let args = list(m)?
                .iter()
                .map(predicate)
                .collect::<syn::Result<Vec<_>>>()?;
            if !matches!(op.as_str(), "any" | "all" | "not") || (op == "not" && args.len() != 1) {
                return Err(syn::Error::new_spanned(m, "unsupported cfg predicate"));
            }
            Ok(json!({op: args}))
        }
    }
}

pub fn condition(meta: &Meta) -> syn::Result<Value> {
    if meta.path().is_ident("test") || meta.path().is_ident("bench") {
        return Ok(json!("test"));
    }
    if meta.path().is_ident("cfg") {
        if let Meta::List(m) = meta {
            return predicate(&syn::parse2::<Meta>(m.tokens.clone())?);
        }
        return Err(syn::Error::new_spanned(meta, "malformed cfg"));
    }
    if meta.path().is_ident("cfg_attr") {
        let Meta::List(m) = meta else {
            return Err(syn::Error::new_spanned(meta, "malformed cfg_attr"));
        };
        let args = list(m)?;
        if args.len() < 2 {
            return Err(syn::Error::new_spanned(meta, "empty cfg_attr"));
        }
        // Conditional path changes need explicit module-edge semantics, never guess.
        if args[1..].iter().any(|a| a.path().is_ident("path")) {
            return Err(syn::Error::new_spanned(
                meta,
                "cfg_attr(path) is unsupported",
            ));
        }
        let enabled = predicate(&args[0])?;
        let applied = args[1..]
            .iter()
            .map(condition)
            .collect::<syn::Result<Vec<_>>>()?;
        return Ok(json!({"any": [{"not": [enabled]}, {"all": applied}]}));
    }
    Ok(json!(true))
}

pub fn attributes(attrs: &[syn::Attribute]) -> syn::Result<Value> {
    let values = attrs
        .iter()
        .map(|a| condition(&a.meta))
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(json!({"all": values}))
}
