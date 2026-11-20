//! JSON-lines adapter for the pinned syn AST and proc-macro2 source spans.
mod cfg;
mod template;

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use syn::{
    spanned::Spanned,
    visit::{self, Visit},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    source: String,
    grammar: String,
}

#[derive(Serialize)]
struct InlineModule {
    name: String,
    path: Option<String>,
}

#[derive(Default)]
struct Inventory {
    template_grammar: bool,
    regions: Vec<Value>,
    modules: Vec<Value>,
    includes: Vec<Value>,
    functions: Vec<Value>,
    macros: Vec<Value>,
    semantic_attributes: Vec<Value>,
    scope: Vec<String>,
    errors: Vec<String>,
    inline: Vec<InlineModule>,
    source: String,
}

impl Inventory {
    fn region(&mut self, attrs: &[syn::Attribute], span: Span) {
        if attrs.is_empty() {
            return;
        }
        let range = span.byte_range();
        let owner = owner_key(self.source[range.clone()].parse().unwrap_or_default());
        self.attributes(attrs, &owner);
        // syn excludes the separator from field/variant spans; rustc removes it
        // with a cfg-disabled owner. Include an immediately following comma.
        let mut end = range.end;
        let suffix = &self.source[end..];
        let whitespace = suffix.len() - suffix.trim_start_matches([' ', '\t', '\r', '\n']).len();
        if suffix.as_bytes().get(whitespace) == Some(&b',') {
            end += whitespace + 1;
        }
        match cfg::attributes(attrs) {
            Ok(condition) => self.regions.push(json!({
                "start": range.start, "end": end,
                "condition": condition,
            })),
            Err(e) => self.errors.push(e.to_string()),
        }
    }
    // Tokio select handler/future brace groups are ordinary Rust blocks. Parse
    // them with syn; reject conditional syntax outside such blocks. No expansion
    // or custom general macro parser is claimed.
    fn select_blocks(&mut self, tokens: TokenStream) {
        let mut outside = TokenStream::new();
        for token in tokens {
            match token {
                TokenTree::Group(g) if g.delimiter() == proc_macro2::Delimiter::Brace => {
                    let stream = TokenStream::from(TokenTree::Group(g));
                    match syn::parse2::<syn::Block>(stream) {
                        Ok(block) => self.visit_block(&block),
                        Err(e) => self.errors.push(format!("unsupported select block: {e}")),
                    }
                }
                TokenTree::Group(g) => outside.extend([TokenTree::Group(g)]),
                token => outside.extend([token]),
            }
        }
        if opaque_cfg(outside) {
            self.errors
                .push("unsupported conditional select branch".into());
        }
    }
    fn function(&mut self, name: &syn::Ident, span: Span) {
        self.functions.push(json!({"name": name.to_string(),
            "start": span.start().line, "end": span.end().line}));
    }
    fn attributes(&mut self, attrs: &[syn::Attribute], owner: &str) {
        for attr in attrs {
            let mut entries = Vec::new();
            if let Err(error) = semantic_meta(&attr.meta, json!(true), &mut entries) {
                self.errors.push(error.to_string());
                continue;
            }
            for mut entry in entries {
                let mut scope = self.scope.clone();
                scope.push(owner.to_owned());
                entry["owner"] = json!(scope);
                entry["start"] = json!(attr.span().byte_range().start);
                entry["end"] = json!(attr.span().byte_range().end);
                self.semantic_attributes.push(entry);
            }
        }
    }
}

fn named(path: &syn::Path, name: &str) -> bool {
    path.segments.len() == 1
        && path.segments[0].ident.to_string().trim_start_matches("r#") == name
}

// Token-normalized owner headers keep existing attributes stable under whitespace
// and body edits. Enclosing item/method headers distinguish parent owners.
fn owner_key(tokens: TokenStream) -> String {
    let mut tokens = tokens.into_iter().peekable();
    while matches!(tokens.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '#') {
        tokens.next();
        if matches!(tokens.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '!') {
            tokens.next();
        }
        tokens.next(); // The AST has already validated the attribute bracket.
    }
    tokens.take_while(|t| !matches!(t, TokenTree::Group(g)
        if g.delimiter() == proc_macro2::Delimiter::Brace))
        .collect::<TokenStream>().to_string()
}

fn semantic_meta(meta: &syn::Meta, enabled: Value, out: &mut Vec<Value>) -> syn::Result<()> {
    use syn::{parse::Parser, punctuated::Punctuated, Meta, Token};
    let name = meta.path();
    if named(name, "cfg_attr") || named(name, "allow") {
        let Meta::List(list) = meta else {
            return Err(syn::Error::new_spanned(meta, "malformed semantic attribute"));
        };
        let args: Vec<Meta> = Punctuated::<Meta, Token![,]>::parse_terminated
            .parse2(list.tokens.clone())?.into_iter().collect();
        if named(name, "cfg_attr") {
            if args.len() < 2 {
                return Err(syn::Error::new_spanned(meta, "empty cfg_attr"));
            }
            let condition = json!({"all": [enabled, cfg::predicate(&args[0])?]});
            for nested in &args[1..] {
                semantic_meta(nested, condition.clone(), out)?;
            }
        } else {
            for lint in &args {
                if named(lint.path(), "dead_code") {
                    if !matches!(lint, Meta::Path(_)) {
                        return Err(syn::Error::new_spanned(lint, "malformed dead_code lint"));
                    }
                    out.push(json!({"kind": "allow_dead_code", "value": "dead_code",
                                    "condition": enabled}));
                }
            }
        }
    } else if named(name, "path") {
        if let Meta::NameValue(value) = meta {
            if let syn::Expr::Lit(value) = &value.value {
                if let syn::Lit::Str(value) = &value.lit {
                    out.push(json!({"kind": "path", "value": value.value(), "condition": enabled}));
                    return Ok(());
                }
            }
        }
        return Err(syn::Error::new_spanned(meta, "non-literal semantic path"));
    }
    Ok(())
}

// Each attribute-bearing AST owner supplies its entire span, not an attribute
// substring. This table follows syn 2.0.118's Visit API (including fields/patterns).
macro_rules! owners {
    ($($method:ident : $ty:ident),* $(,)?) => {$(
        fn $method(&mut self, node: &'ast syn::$ty) {
            self.region(&node.attrs, node.span());
            visit::$method(self, node);
        }
    )*};
}

impl<'ast> Visit<'ast> for Inventory {
    fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
        // syn represents an empty statement as an empty Verbatim plus ';'.
        // It has no conditional owner; retain it in the enclosing region.
        if let syn::Stmt::Expr(syn::Expr::Verbatim(tokens), Some(_)) = node {
            if tokens.is_empty() {
                return;
            }
        }
        let first = self.regions.len();
        visit::visit_stmt(self, node);
        if let syn::Stmt::Expr(expr, Some(semi)) = node {
            let range = expr.span().byte_range();
            // syn's Expr omits its statement separator. Only extend the exact
            // owner, including intervening trivia, to the AST-owned semicolon.
            // Items, locals, macros and later empty statements own their own ';'.
            for region in &mut self.regions[first..] {
                if region["start"] == range.start && region["end"] == range.end {
                    region["end"] = json!(semi.span.byte_range().end);
                }
            }
        }
    }
    owners! {
        visit_arm: Arm,
        visit_bare_fn_arg: BareFnArg,
        visit_bare_variadic: BareVariadic,
        visit_const_param: ConstParam,
        visit_derive_input: DeriveInput,
        visit_expr_array: ExprArray,
        visit_expr_assign: ExprAssign,
        visit_expr_async: ExprAsync,
        visit_expr_await: ExprAwait,
        visit_expr_binary: ExprBinary,
        visit_expr_block: ExprBlock,
        visit_expr_break: ExprBreak,
        visit_expr_call: ExprCall,
        visit_expr_cast: ExprCast,
        visit_expr_closure: ExprClosure,
        visit_expr_const: ExprConst,
        visit_expr_continue: ExprContinue,
        visit_expr_field: ExprField,
        visit_expr_for_loop: ExprForLoop,
        visit_expr_group: ExprGroup,
        visit_expr_if: ExprIf,
        visit_expr_index: ExprIndex,
        visit_expr_infer: ExprInfer,
        visit_expr_let: ExprLet,
        visit_expr_lit: ExprLit,
        visit_expr_loop: ExprLoop,
        visit_expr_macro: ExprMacro,
        visit_expr_match: ExprMatch,
        visit_expr_method_call: ExprMethodCall,
        visit_expr_paren: ExprParen,
        visit_expr_path: ExprPath,
        visit_expr_range: ExprRange,
        visit_expr_raw_addr: ExprRawAddr,
        visit_expr_reference: ExprReference,
        visit_expr_repeat: ExprRepeat,
        visit_expr_return: ExprReturn,
        visit_expr_struct: ExprStruct,
        visit_expr_try: ExprTry,
        visit_expr_try_block: ExprTryBlock,
        visit_expr_tuple: ExprTuple,
        visit_expr_unary: ExprUnary,
        visit_expr_unsafe: ExprUnsafe,
        visit_expr_while: ExprWhile,
        visit_expr_yield: ExprYield,
        visit_field: Field,
        visit_field_pat: FieldPat,
        visit_field_value: FieldValue,
        visit_foreign_item_fn: ForeignItemFn,
        visit_foreign_item_macro: ForeignItemMacro,
        visit_foreign_item_static: ForeignItemStatic,
        visit_foreign_item_type: ForeignItemType,
        visit_impl_item_const: ImplItemConst,
        visit_impl_item_macro: ImplItemMacro,
        visit_impl_item_type: ImplItemType,
        visit_item_const: ItemConst,
        visit_item_enum: ItemEnum,
        visit_item_extern_crate: ItemExternCrate,
        visit_item_foreign_mod: ItemForeignMod,
        visit_item_impl: ItemImpl,
        visit_item_macro: ItemMacro,
        visit_item_static: ItemStatic,
        visit_item_struct: ItemStruct,
        visit_item_trait: ItemTrait,
        visit_item_trait_alias: ItemTraitAlias,
        visit_item_type: ItemType,
        visit_item_union: ItemUnion,
        visit_item_use: ItemUse,
        visit_lifetime_param: LifetimeParam,
        visit_local: Local,
        visit_pat_ident: PatIdent,
        visit_pat_or: PatOr,
        visit_pat_paren: PatParen,
        visit_pat_reference: PatReference,
        visit_pat_rest: PatRest,
        visit_pat_slice: PatSlice,
        visit_pat_struct: PatStruct,
        visit_pat_tuple: PatTuple,
        visit_pat_tuple_struct: PatTupleStruct,
        visit_pat_type: PatType,
        visit_pat_wild: PatWild,
        visit_receiver: Receiver,
        visit_stmt_macro: StmtMacro,
        visit_trait_item_const: TraitItemConst,
        visit_trait_item_macro: TraitItemMacro,
        visit_trait_item_type: TraitItemType,
        visit_type_param: TypeParam,
        visit_variadic: Variadic,
    }
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.region(&node.attrs, node.span());
        self.function(&node.sig.ident, node.span());
        visit::visit_item_fn(self, node);
    }
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.region(&node.attrs, node.span());
        self.function(&node.sig.ident, node.span());
        self.scope.push(owner_key(node.sig.to_token_stream()));
        visit::visit_impl_item_fn(self, node);
        self.scope.pop();
    }
    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.region(&node.attrs, node.span());
        self.function(&node.sig.ident, node.span());
        self.scope.push(owner_key(node.sig.to_token_stream()));
        visit::visit_trait_item_fn(self, node);
        self.scope.pop();
    }
    fn visit_variant(&mut self, node: &'ast syn::Variant) {
        self.region(&node.attrs, node.span());
        // A variant's name is unique in its enum; exclude payload/discriminant
        // tokens so unrelated field edits cannot rename descendant owners.
        self.scope.push(format!("variant {}", node.ident));
        visit::visit_variant(self, node);
        self.scope.pop();
    }
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.region(&node.attrs, node.span());
        let path = node.attrs.iter().find(|a| a.path().is_ident("path"));
        let path = path
            .map(|a| match &a.meta {
                syn::Meta::NameValue(n) => match &n.value {
                    syn::Expr::Lit(l) => match &l.lit {
                        syn::Lit::Str(s) => Ok(s.value()),
                        _ => Err("non-string module path"),
                    },
                    _ => Err("non-literal module path"),
                },
                _ => Err("malformed module path"),
            })
            .transpose();
        let path = match path {
            Ok(p) => p,
            Err(e) => {
                self.errors.push(e.into());
                None
            }
        };
        if node.content.is_none() {
            self.modules
                .push(json!({"name": node.ident.to_string(), "path": path,
                "inline": self.inline, "offset": node.span().byte_range().end - 1}));
        } else {
            self.inline.push(InlineModule {
                name: node.ident.to_string(),
                path,
            });
        }
        visit::visit_item_mod(self, node);
        if node.content.is_some() {
            self.inline.pop();
        }
    }
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let name = node.path.to_token_stream().to_string();
        let tokens = node.tokens.to_string();
        let range = node.span().byte_range();
        if node.path.is_ident("include") {
            self.includes
                .push(json!({"tokens": tokens, "offset": range.start}));
        }
        self.macros
            .push(json!({"name": name, "start": range.start, "end": range.end,
            "tokens": token_count(node.tokens.clone())}));
        // Macro DSLs are opaque maintained source. A DSL that carries actual cfg
        // attributes or module declarations needs a supported expansion adapter.
        if self.template_grammar && node.path.is_ident("macro_rules") && template::eligible(&node.tokens) {
            match template::owners(node.tokens.clone()) {
                Ok(owners) => for owner in owners { self.region(&owner.attrs, owner.span()); },
                Err(e) => self.errors.push(e),
            }
        } else if matches!(name.as_str(), "tokio :: select" | "tokio::select") {
            self.select_blocks(node.tokens.clone());
        } else if opaque_cfg(node.tokens.clone()) {
            self.errors.push(format!(
                "conditional syntax inside opaque macro {name} at {}",
                range.start
            ));
        }
    }
    fn visit_item(&mut self, node: &'ast syn::Item) {
        if let syn::Item::Verbatim(_) = node {
            self.errors.push("verbatim item".into());
        }
        self.scope.push(owner_key(node.to_token_stream()));
        visit::visit_item(self, node);
        self.scope.pop();
    }
    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        if let syn::Expr::Verbatim(_) = node {
            self.errors.push("verbatim expression".into());
        }
        visit::visit_expr(self, node);
    }
}

fn token_count(tokens: TokenStream) -> usize {
    tokens
        .into_iter()
        .map(|t| match t {
            TokenTree::Group(g) => 2 + token_count(g.stream()),
            _ => 1,
        })
        .sum()
}

fn opaque_cfg(tokens: TokenStream) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    for (index, token) in tokens.iter().enumerate() {
        let TokenTree::Punct(p) = token else { continue };
        if p.as_char() != '#' {
            continue;
        }
        let next = index
            + 1
            + usize::from(matches!(tokens.get(index + 1),
            Some(TokenTree::Punct(p)) if p.as_char() == '!'));
        if let Some(TokenTree::Group(g)) = tokens.get(next) {
            if g.delimiter() == proc_macro2::Delimiter::Bracket {
                if let Ok(m) = syn::parse2::<syn::Meta>(g.stream()) {
                    if m.path().is_ident("cfg") || m.path().is_ident("cfg_attr") {
                        return true;
                    }
                    let mut attributes = Vec::new();
                    if semantic_meta(&m, json!(true), &mut attributes).is_err() || !attributes.is_empty() {
                        return true;
                    }
                }
            }
        }
    }
    tokens.into_iter().any(|t| match t {
        TokenTree::Group(g) => opaque_cfg(g.stream()),
        _ => false,
    })
}

fn scan(request: Request) -> syn::Result<Value> {
    let mut inventory = Inventory {
        source: request.source.clone(),
        template_grammar: request.grammar == "file_dispatch_templates_v1",
        ..Inventory::default()
    };
    match request.grammar.as_str() {
        "file" | "file_dispatch_templates_v1" => {
            let ast = syn::parse_file(&request.source)?;
            inventory.attributes(&ast.attrs, "file");
            let condition = cfg::attributes(&ast.attrs)?;
            inventory
                .regions
                .push(json!({"start": 0, "end": request.source.len(),
                "condition": condition}));
            inventory.visit_file(&ast);
        }
        "expression" => inventory.visit_expr(&syn::parse_str(&request.source)?),
        _ => return Err(syn::Error::new(Span::call_site(), "unsupported grammar")),
    }
    Ok(
        json!({"regions": inventory.regions, "modules": inventory.modules,
        "includes": inventory.includes, "functions": inventory.functions,
        "macros": inventory.macros, "semantic_attributes": inventory.semantic_attributes,
        "errors": inventory.errors}),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--identity") {
        println!(
            "{}",
            json!({
                "src/main.rs": include_str!("main.rs"),
                "src/cfg.rs": include_str!("cfg.rs"),
                "src/template.rs": include_str!("template.rs"),
                "Cargo.toml": include_str!("../Cargo.toml"),
                "Cargo.lock": include_str!("../Cargo.lock"),
            })
        );
        return Ok(());
    }
    let mut output = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let result = match serde_json::from_str::<Request>(&line?) {
            Ok(request) => match scan(request) {
                Ok(result) => result,
                Err(e) => json!({"errors": [e.to_string()]}),
            },
            Err(e) => json!({"errors": [e.to_string()]}),
        };
        serde_json::to_writer(&mut output, &result)?;
        writeln!(output)?;
        output.flush()?;
    }
    Ok(())
}
