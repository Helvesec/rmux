//! Opt-in bounded matcher grammar; all other macros retain opaque-cfg refusal.
use proc_macro2::{Delimiter, Group, Ident, TokenStream, TokenTree};
use syn::{spanned::Spanned, ItemFn};

const MATCHER: &str = "$visibility:vis fn $name:ident(&$this:ident, $argument:ident: $argument_type:ty) => $key:expr; coverage $coverage:ident; { $($pattern:pat => $value:expr),* $(,)? }";

pub fn eligible(tokens: &TokenStream) -> bool {
    match tokens.clone().into_iter().next() {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis =>
            g.stream().to_string() == MATCHER.parse::<TokenStream>().unwrap().to_string(),
        _ => false,
    }
}

fn normalize(tokens: TokenStream) -> Result<TokenStream, String> {
    let mut it = tokens.into_iter();
    let mut out = TokenStream::new();
    while let Some(t) = it.next() {
        match t {
            TokenTree::Punct(p) if p.as_char() == '$' => {
                let Some(TokenTree::Ident(id)) = it.next() else { return Err("unsupported template substitution".into()) };
                let s = id.to_string();
                let replacement = match s.as_str() {
                    "visibility" => "pub", "this" => "self",
                    "name" | "argument" | "argument_type" | "coverage" => &s,
                    _ => return Err("unknown signature metavariable".into()),
                };
                out.extend([TokenTree::Ident(Ident::new(replacement, id.span()))]);
            }
            TokenTree::Group(g) => {
                let mut mapped = Group::new(g.delimiter(), normalize(g.stream())?);
                mapped.set_span(g.span());
                out.extend([TokenTree::Group(mapped)]);
            }
            t => out.extend([t]),
        }
    }
    Ok(out)
}

pub fn owners(tokens: TokenStream) -> Result<Vec<ItemFn>, String> {
    let all: Vec<_> = tokens.into_iter().collect();
    if all.len()!=5 || !matches!(&all[1],TokenTree::Punct(p) if p.as_char()=='=')
        || !matches!(&all[2],TokenTree::Punct(p) if p.as_char()=='>')
        || !matches!(&all[4],TokenTree::Punct(p) if p.as_char()==';') {
        return Err("template must have exactly one complete rule".into());
    }
    let TokenTree::Group(body)=&all[3] else { return Err("missing template body".into()) };
    if body.delimiter()!=Delimiter::Brace { return Err("template body delimiter".into()) }
    let mut signature=TokenStream::new();
    let mut functions=Vec::new();
    for token in body.stream() {
        if let TokenTree::Group(g)=&token {
            if g.delimiter()==Delimiter::Brace {
                // Bodies remain opaque source, never expanded. Reject any nested
                // conditional instead of silently assigning its enclosing role.
                if super::opaque_cfg(g.stream()) { return Err("nested conditional template body unsupported".into()) }
                let mut empty=Group::new(Delimiter::Brace,TokenStream::new());
                empty.set_span(g.span());
                let mut normalized=normalize(signature)?;
                normalized.extend([TokenTree::Group(empty)]);
                let item=syn::parse2::<ItemFn>(normalized).map_err(|e|format!("template function signature: {e}"))?;
                functions.push(item); signature=TokenStream::new(); continue;
            }
        }
        signature.extend([token]);
    }
    if !signature.is_empty() || functions.len()!=2 { return Err("template requires exactly two complete function owners".into()) }
    let first=&functions[0];let second=&functions[1];
    if first.sig.ident!="name" || second.sig.ident!="coverage" || !first.attrs.is_empty()
        || second.attrs.len()!=1 || !second.attrs[0].path().is_ident("cfg") {
        return Err("missing or ambiguous conditional coverage owner".into());
    }
    let a=first.span().byte_range();let b=second.span().byte_range();
    if a.start>=a.end || b.start>=b.end || a.end>b.start { return Err("invalid or overlapping template owner spans".into()) }
    Ok(functions)
}
