use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use proc_macro2::{Spacing, TokenStream, TokenTree};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{Expr, ExprMacro, Lit};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IncludeString {
    Relative(String),
    ManifestDir(Vec<ManifestPart>),
}

/// One discovered include site. Determinate targets resolve against the
/// repository; opaque sites (`env!("OUT_DIR")`, bare idents, unsupported
/// macros, macro matchers shaped like an invocation) degrade to a
/// conservative package watch plus an analysis limitation naming file and
/// line, never a scan failure. An opaque shape is not proven-bad — only
/// proven-bad literals (escape, missing target) still fail closed at
/// resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IncludeDiscovery {
    Resolved(IncludeString),
    Opaque {
        macro_name: &'static str,
        line: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ManifestPart {
    Literal(String),
    ManifestDir,
}

impl IncludeString {
    fn from_static(expression: StaticString) -> Self {
        if expression
            .parts
            .iter()
            .any(|part| matches!(part, ManifestPart::ManifestDir))
        {
            Self::ManifestDir(expression.parts)
        } else {
            Self::Relative(
                expression
                    .parts
                    .into_iter()
                    .filter_map(|part| match part {
                        ManifestPart::Literal(value) => Some(value),
                        ManifestPart::ManifestDir => None,
                    })
                    .collect(),
            )
        }
    }

    pub(crate) fn display(&self) -> String {
        match self {
            Self::Relative(value) => value.clone(),
            Self::ManifestDir(parts) => parts
                .iter()
                .map(|part| match part {
                    ManifestPart::Literal(value) => value.as_str(),
                    ManifestPart::ManifestDir => "<CARGO_MANIFEST_DIR>",
                })
                .collect(),
        }
    }
}

#[derive(Default)]
struct StaticString {
    parts: Vec<ManifestPart>,
}

impl StaticString {
    fn literal(value: String) -> Self {
        Self {
            parts: vec![ManifestPart::Literal(value)],
        }
    }

    fn manifest_dir() -> Self {
        Self {
            parts: vec![ManifestPart::ManifestDir],
        }
    }

    fn append(&mut self, mut other: Self) {
        self.parts.append(&mut other.parts);
    }
}

/// Parse static path expressions accepted by Rust include macros. Rust token
/// parsing is deliberately used here instead of scanning bytes: comments,
/// raw/cooked literals, macro trivia, and every macro delimiter are handled by
/// the Rust lexer, while only the static expression subset is evaluated.
/// Non-static arguments degrade to [`IncludeDiscovery::Opaque`] with the
/// invocation line; only source that does not lex at all still fails.
pub(crate) fn parse_include_paths(source: &str) -> Result<Vec<IncludeDiscovery>, String> {
    let tokens = TokenStream::from_str(source)
        .map_err(|error| format!("invalid Rust source while scanning includes: {error}"))?;
    let mut scanner = IncludeScanner::default();
    scanner.scan_stream(tokens);
    Ok(scanner.includes)
}

#[cfg(test)]
pub(crate) fn parse_include_str_literals(source: &str) -> Result<Vec<String>, String> {
    parse_include_paths(source)?
        .into_iter()
        .map(|discovery| match discovery {
            IncludeDiscovery::Resolved(IncludeString::Relative(value)) => Ok(value),
            IncludeDiscovery::Resolved(IncludeString::ManifestDir(_)) => Err(
                "include_str! CARGO_MANIFEST_DIR expression requires repository context".to_owned(),
            ),
            IncludeDiscovery::Opaque { macro_name, line } => Err(format!(
                "{macro_name} at line {line} is not a static string expression"
            )),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncludeAlias {
    Builtin,
    Shadowed,
}

#[derive(Clone, Debug)]
struct MacroWrapper {
    parameter: String,
    body: TokenStream,
}

#[derive(Default)]
struct IncludeScanner {
    includes: Vec<IncludeDiscovery>,
}

impl IncludeScanner {
    fn scan_stream(&mut self, stream: TokenStream) {
        let tokens = stream.into_iter().collect::<Vec<_>>();
        let mut aliases = BTreeMap::new();
        let mut wrappers = BTreeMap::new();
        self.scan_tokens(&tokens, &mut aliases, &mut wrappers);
    }

    fn scan_tokens(
        &mut self,
        tokens: &[TokenTree],
        aliases: &mut BTreeMap<String, IncludeAlias>,
        wrappers: &mut BTreeMap<String, MacroWrapper>,
    ) {
        // Rust imports and macro names are visible throughout their lexical
        // scope. Pre-collect declarations so a later use item also applies to
        // an earlier invocation; nested module scopes get independent maps.
        collect_scope_aliases(tokens, aliases);
        let mut index = 0;
        while index < tokens.len() {
            if let Some((name, body, next)) = macro_rules_definition(tokens, index) {
                aliases.insert(name.clone(), IncludeAlias::Shadowed);
                wrappers.remove(&name);
                if let Some(wrapper) = parse_macro_wrapper(body) {
                    wrappers.insert(name, wrapper);
                } else {
                    // An unsupported macro shape may still emit an include;
                    // retain the conservative opaque watch used by the old
                    // token scanner without treating a supported wrapper's
                    // metavariable as a real include site.
                    let nested = body.stream().into_iter().collect::<Vec<_>>();
                    self.scan_tokens(&nested, aliases, wrappers);
                }
                // Do not scan macro metavariables as ordinary include calls.
                index = next;
                continue;
            }
            if matches!(&tokens[index], TokenTree::Ident(identifier) if identifier == "use") {
                let mut end = index + 1;
                while end < tokens.len() && !is_punct(&tokens[end], ';') {
                    end += 1;
                }
                collect_use_tree(&tokens[index + 1..end], &[], aliases);
                index = end.saturating_add(1);
                continue;
            }
            if let Some((name, group)) = macro_invocation(tokens, index)
                && let Some(wrapper) = wrappers.get(&name).cloned()
            {
                let arguments = group.stream().into_iter().collect::<Vec<_>>();
                if arguments.len() == 1 && matches!(arguments.first(), Some(TokenTree::Literal(_)))
                {
                    let expanded =
                        substitute_macro_tokens(&wrapper.body, &wrapper.parameter, &arguments);
                    let expanded = expanded.into_iter().collect::<Vec<_>>();
                    self.scan_tokens(&expanded, aliases, wrappers);
                }
                index += 3;
                continue;
            }
            if let Some(macro_name) = include_macro_name(tokens, index, aliases) {
                let line = token_line(&tokens[index]);
                if let Some((_, group)) = include_invocation(tokens, index, aliases) {
                    match parse_static_string_expression(group.stream(), macro_name) {
                        Ok(expression) => self.includes.push(IncludeDiscovery::Resolved(
                            IncludeString::from_static(expression),
                        )),
                        Err(_) => self
                            .includes
                            .push(IncludeDiscovery::Opaque { macro_name, line }),
                    }
                    // An include argument may itself contain a macro group;
                    // recurse to discover includes in arbitrary macro bodies.
                    let nested = group.stream().into_iter().collect::<Vec<_>>();
                    self.scan_tokens(&nested, aliases, wrappers);
                    index += 3;
                } else {
                    // Shaped like an invocation but undelimited (for example
                    // a macro_rules matcher fragment): remain conservative.
                    self.includes
                        .push(IncludeDiscovery::Opaque { macro_name, line });
                    index += 2;
                }
                continue;
            }
            if let TokenTree::Group(group) = &tokens[index] {
                // Blocks inherit outer imports; module bodies introduce a new
                // lexical import scope. This prevents child aliases leaking
                // into siblings or the parent module.
                let module_scope = is_module_body(tokens, index);
                let mut nested_aliases = if module_scope {
                    BTreeMap::new()
                } else {
                    aliases.clone()
                };
                let mut nested_wrappers = if module_scope {
                    BTreeMap::new()
                } else {
                    wrappers.clone()
                };
                let nested = group.stream().into_iter().collect::<Vec<_>>();
                self.scan_tokens(&nested, &mut nested_aliases, &mut nested_wrappers);
            }
            index += 1;
        }
    }
}

fn collect_scope_aliases(tokens: &[TokenTree], aliases: &mut BTreeMap<String, IncludeAlias>) {
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], TokenTree::Ident(identifier) if identifier == "use") {
            let mut end = index + 1;
            while end < tokens.len() && !is_punct(&tokens[end], ';') {
                end += 1;
            }
            collect_use_tree(&tokens[index + 1..end], &[], aliases);
            index = end.saturating_add(1);
            continue;
        }
        if let Some((_, _, next)) = macro_rules_definition(tokens, index) {
            index = next;
            continue;
        }
        index += 1;
    }
}

fn include_invocation<'a>(
    tokens: &'a [TokenTree],
    index: usize,
    aliases: &BTreeMap<String, IncludeAlias>,
) -> Option<(&'static str, &'a proc_macro2::Group)> {
    let macro_name = include_macro_name(tokens, index, aliases)?;
    let Some(TokenTree::Group(group)) = tokens.get(index + 2) else {
        return None;
    };
    Some((macro_name, group))
}

fn include_macro_name(
    tokens: &[TokenTree],
    index: usize,
    aliases: &BTreeMap<String, IncludeAlias>,
) -> Option<&'static str> {
    let TokenTree::Ident(identifier) = &tokens[index] else {
        return None;
    };
    let macro_name = match identifier.to_string().as_str() {
        "include_str" => "include_str!",
        "include_bytes" => "include_bytes!",
        _ if aliases.get(&identifier.to_string()) == Some(&IncludeAlias::Builtin) => {
            "aliased include macro!"
        }
        _ => return None,
    };
    // A path-qualified invocation is normally a user macro. `std::` and
    // `core::` are the standard-library paths that re-export include macros.
    if is_double_colon_before(tokens, index) && !is_standard_library_qualified(tokens, index) {
        return None;
    }
    if !tokens
        .get(index + 1)
        .is_some_and(|token| is_punct(token, '!'))
    {
        return None;
    }
    Some(macro_name)
}

fn collect_use_tree(
    tokens: &[TokenTree],
    inherited_prefix: &[String],
    aliases: &mut BTreeMap<String, IncludeAlias>,
) {
    let mut branch_start = 0;
    for index in 0..=tokens.len() {
        if index == tokens.len() || is_punct(&tokens[index], ',') {
            collect_use_branch(&tokens[branch_start..index], inherited_prefix, aliases);
            branch_start = index.saturating_add(1);
        }
    }
}

fn collect_use_branch(
    branch: &[TokenTree],
    inherited_prefix: &[String],
    aliases: &mut BTreeMap<String, IncludeAlias>,
) {
    let Some((group_index, group)) = branch.iter().enumerate().find_map(|(index, token)| {
        if let TokenTree::Group(group) = token {
            Some((index, group))
        } else {
            None
        }
    }) else {
        let Some(as_index) = branch
            .iter()
            .position(|token| matches!(token, TokenTree::Ident(identifier) if identifier == "as"))
        else {
            return;
        };
        let Some(TokenTree::Ident(alias)) = branch.get(as_index + 1) else {
            return;
        };
        let mut path = inherited_prefix.to_vec();
        let Some(mut branch_path) = use_path_segments(&branch[..as_index]) else {
            return;
        };
        path.append(&mut branch_path);
        let alias_kind = if matches!(
            path.as_slice(),
            [qualifier, name]
                if matches!(qualifier.as_str(), "std" | "core")
                    && matches!(name.as_str(), "include_str" | "include_bytes")
        ) {
            IncludeAlias::Builtin
        } else {
            IncludeAlias::Shadowed
        };
        aliases.insert(alias.to_string(), alias_kind);
        return;
    };

    let mut prefix = inherited_prefix.to_vec();
    if let Some(mut branch_prefix) = use_path_segments(&branch[..group_index]) {
        prefix.append(&mut branch_prefix);
    }
    let nested = group.stream().into_iter().collect::<Vec<_>>();
    collect_use_tree(&nested, &prefix, aliases);
}

fn use_path_segments(tokens: &[TokenTree]) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut expect_segment = true;
    for token in tokens {
        match token {
            TokenTree::Ident(identifier) if expect_segment => {
                segments.push(identifier.to_string());
                expect_segment = false;
            }
            TokenTree::Punct(punct) if punct.as_char() == ':' && !expect_segment => {
                expect_segment = true;
            }
            TokenTree::Punct(punct) if punct.as_char() == ':' && expect_segment => {}
            _ => return None,
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

fn macro_rules_definition(
    tokens: &[TokenTree],
    index: usize,
) -> Option<(String, &proc_macro2::Group, usize)> {
    if !matches!(
        tokens.get(index),
        Some(TokenTree::Ident(identifier)) if identifier == "macro_rules"
    ) || !tokens
        .get(index + 1)
        .is_some_and(|token| is_punct(token, '!'))
    {
        return None;
    }
    let TokenTree::Ident(name) = tokens.get(index + 2)? else {
        return None;
    };
    let TokenTree::Group(body) = tokens.get(index + 3)? else {
        return None;
    };
    Some((name.to_string(), body, index + 4))
}

fn parse_macro_wrapper(body: &proc_macro2::Group) -> Option<MacroWrapper> {
    let tokens = body.stream().into_iter().collect::<Vec<_>>();
    let (pattern, expansion) = tokens.iter().enumerate().find_map(|(index, token)| {
        let TokenTree::Group(pattern) = token else {
            return None;
        };
        let equals = index + 1;
        if !is_punct(tokens.get(equals)?, '=') || !is_punct(tokens.get(equals + 1)?, '>') {
            return None;
        }
        let TokenTree::Group(expansion) = tokens.get(equals + 2)? else {
            return None;
        };
        Some((pattern, expansion))
    })?;
    let parameter = macro_literal_parameter(pattern)?;
    Some(MacroWrapper {
        parameter,
        body: expansion.stream(),
    })
}

fn macro_literal_parameter(pattern: &proc_macro2::Group) -> Option<String> {
    let tokens = pattern.stream().into_iter().collect::<Vec<_>>();
    for index in 0..tokens.len().saturating_sub(3) {
        if !is_punct(&tokens[index], '$') {
            continue;
        }
        let TokenTree::Ident(name) = &tokens[index + 1] else {
            continue;
        };
        if !is_punct(&tokens[index + 2], ':')
            || !matches!(&tokens[index + 3], TokenTree::Ident(kind) if kind == "literal")
        {
            continue;
        }
        return Some(name.to_string());
    }
    None
}

fn macro_invocation(tokens: &[TokenTree], index: usize) -> Option<(String, &proc_macro2::Group)> {
    let TokenTree::Ident(identifier) = tokens.get(index)? else {
        return None;
    };
    if !tokens
        .get(index + 1)
        .is_some_and(|token| is_punct(token, '!'))
    {
        return None;
    }
    let Some(TokenTree::Group(group)) = tokens.get(index + 2) else {
        return None;
    };
    Some((identifier.to_string(), group))
}

fn substitute_macro_tokens(
    tokens: &TokenStream,
    parameter: &str,
    arguments: &[TokenTree],
) -> TokenStream {
    let tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut output = TokenStream::new();
    let mut index = 0;
    while index < tokens.len() {
        if is_punct(&tokens[index], '$')
            && matches!(tokens.get(index + 1), Some(TokenTree::Ident(name)) if name == parameter)
        {
            output.extend(arguments.iter().cloned());
            index += 2;
            continue;
        }
        let token = match &tokens[index] {
            TokenTree::Group(group) => {
                let nested = substitute_macro_tokens(&group.stream(), parameter, arguments);
                let mut replacement = proc_macro2::Group::new(group.delimiter(), nested);
                replacement.set_span(group.span());
                TokenTree::Group(replacement)
            }
            token => token.clone(),
        };
        output.extend([token]);
        index += 1;
    }
    output
}

fn is_module_body(tokens: &[TokenTree], index: usize) -> bool {
    matches!(
        tokens.get(index.wrapping_sub(2)),
        Some(TokenTree::Ident(identifier)) if identifier == "mod"
    ) || matches!(
        tokens.get(index.wrapping_sub(3)),
        Some(TokenTree::Ident(keyword)) if keyword == "mod"
    )
}

/// 1-based source line of an include invocation, from the lexer's spans.
fn token_line(token: &TokenTree) -> usize {
    match token {
        TokenTree::Ident(identifier) => identifier.span().start().line,
        TokenTree::Group(group) => group.span().start().line,
        TokenTree::Punct(punct) => punct.span().start().line,
        TokenTree::Literal(literal) => literal.span().start().line,
    }
}

fn parse_static_string_expression(
    tokens: TokenStream,
    macro_name: &str,
) -> Result<StaticString, String> {
    let expression = syn::parse2::<Expr>(tokens)
        .map_err(|error| format!("{macro_name} must use a static string expression: {error}"))?;
    evaluate_static_expression(&expression, macro_name)
}

fn evaluate_static_expression(expression: &Expr, macro_name: &str) -> Result<StaticString, String> {
    match expression {
        Expr::Lit(literal) => match &literal.lit {
            Lit::Str(value) => Ok(StaticString::literal(value.value())),
            _ => Err(format!("{macro_name} must use a static string expression")),
        },
        Expr::Macro(expression) => evaluate_static_macro(expression, macro_name),
        _ => Err(format!("{macro_name} must use a static string expression")),
    }
}

fn evaluate_static_macro(
    expression: &ExprMacro,
    include_macro: &str,
) -> Result<StaticString, String> {
    let path = &expression.mac.path;
    let name = path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();
    if path.segments.len() != 1 {
        return Err(format!(
            "{include_macro} macro {name}! is not a supported static expression"
        ));
    }
    let arguments = Punctuated::<Expr, Comma>::parse_terminated
        .parse2(expression.mac.tokens.clone())
        .map_err(|error| format!("{include_macro} {name}! has invalid arguments: {error}"))?;
    match name.as_str() {
        "concat" => {
            let mut combined = StaticString::default();
            for argument in arguments {
                combined.append(evaluate_static_expression(&argument, include_macro)?);
            }
            Ok(combined)
        }
        "env" => {
            if arguments.len() != 1 {
                return Err(format!(
                    "{include_macro} env! must use exactly one string literal argument"
                ));
            }
            match arguments.first() {
                Some(Expr::Lit(literal)) => match &literal.lit {
                    Lit::Str(value) if value.value() == "CARGO_MANIFEST_DIR" => {
                        Ok(StaticString::manifest_dir())
                    }
                    _ => Err(format!(
                        "{include_macro} only resolves env!(\"CARGO_MANIFEST_DIR\")"
                    )),
                },
                _ => Err(format!(
                    "{include_macro} only resolves env!(\"CARGO_MANIFEST_DIR\")"
                )),
            }
        }
        _ => Err(format!(
            "{include_macro} macro {name}! is not a supported static expression"
        )),
    }
}

fn is_punct(token: &TokenTree, expected: char) -> bool {
    matches!(token, TokenTree::Punct(punct) if punct.as_char() == expected)
}

fn is_double_colon_before(tokens: &[TokenTree], index: usize) -> bool {
    let Some(first_index) = index.checked_sub(2) else {
        return false;
    };
    let Some(second_index) = index.checked_sub(1) else {
        return false;
    };
    let (Some(TokenTree::Punct(first)), Some(TokenTree::Punct(second))) =
        (tokens.get(first_index), tokens.get(second_index))
    else {
        return false;
    };
    first.as_char() == ':'
        && first.spacing() == Spacing::Joint
        && second.as_char() == ':'
        && second.spacing() == Spacing::Alone
}

fn is_standard_library_qualified(tokens: &[TokenTree], index: usize) -> bool {
    let Some(qualifier_index) = index.checked_sub(3) else {
        return false;
    };
    let Some(TokenTree::Ident(qualifier)) = tokens.get(qualifier_index) else {
        return false;
    };
    if !matches!(qualifier.to_string().as_str(), "std" | "core") {
        return false;
    }
    // Reject `other::std::include_str!`; permit direct `std::` / `core::`
    // and absolute `::std::` / `::core::` spellings.
    !is_double_colon_before(tokens, qualifier_index) || qualifier_index == 2
}

/// Path resolution errors stay structured so both schema scanners can render
/// the same diagnostics without carrying a second copy of boundary logic.
#[derive(Debug)]
pub(crate) enum IncludePathError {
    Escapes,
    Missing,
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

/// Resolve one include with exact `concat!` semantics, then canonicalize the
/// complete path. Canonicalizing the parent chain before accepting `.github`
/// inputs closes the external-symlink bypass while still allowing tracked
/// symlinked include directories whose final canonical file is in the repo.
pub(crate) fn resolve_include_path(
    root: &Path,
    source_parent: &str,
    package_root: &str,
    included: &IncludeString,
    file_set: &BTreeSet<String>,
) -> Result<String, IncludePathError> {
    let canonical_root = fs::canonicalize(root).map_err(|source| IncludePathError::Io {
        operation: "canonicalize repository root",
        path: root.to_owned(),
        source,
    })?;
    let candidate = match included {
        IncludeString::Relative(value) => {
            let logical =
                resolve_repo_path(source_parent, value).ok_or(IncludePathError::Escapes)?;
            canonical_root.join(logical)
        }
        IncludeString::ManifestDir(parts) => {
            let manifest_dir =
                fs::canonicalize(canonical_root.join(package_root)).map_err(|source| {
                    IncludePathError::Io {
                        operation: "resolve Cargo manifest directory",
                        path: canonical_root.join(package_root),
                        source,
                    }
                })?;
            let mut exact = OsString::new();
            for part in parts {
                match part {
                    ManifestPart::Literal(value) => exact.push(value),
                    ManifestPart::ManifestDir => exact.push(manifest_dir.as_os_str()),
                }
            }
            let exact = PathBuf::from(exact);
            if exact.is_absolute() {
                exact
            } else {
                canonical_root.join(source_parent).join(exact)
            }
        }
    };
    if !lexically_within(&canonical_root, &candidate) {
        return Err(IncludePathError::Escapes);
    }
    let canonical_target = match fs::canonicalize(&candidate) {
        Ok(path) => path,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let parent = canonicalize_existing_parent(&candidate)?;
            if !parent.starts_with(&canonical_root) {
                return Err(IncludePathError::Escapes);
            }
            return Err(IncludePathError::Missing);
        }
        Err(source) => {
            return Err(IncludePathError::Io {
                operation: "resolve include target",
                path: candidate,
                source,
            });
        }
    };
    let Ok(relative) = canonical_target.strip_prefix(&canonical_root) else {
        return Err(IncludePathError::Escapes);
    };
    let relative = relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if file_set.contains(&relative) {
        return Ok(relative);
    }
    if is_static_github_input(&relative) {
        let metadata = fs::symlink_metadata(&candidate).map_err(|source| IncludePathError::Io {
            operation: "inspect include target",
            path: candidate.clone(),
            source,
        })?;
        if metadata.is_file() && !metadata.file_type().is_symlink() {
            return Ok(relative);
        }
    }
    Err(IncludePathError::Missing)
}

fn lexically_within(root: &Path, candidate: &Path) -> bool {
    let Some(root) = normalize_lexical_path(root) else {
        return false;
    };
    let Some(candidate) = normalize_lexical_path(candidate) else {
        return false;
    };
    candidate.starts_with(root)
}

fn normalize_lexical_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
        }
    }
    Some(normalized)
}

fn canonicalize_existing_parent(path: &Path) -> Result<PathBuf, IncludePathError> {
    let mut probe = path.to_owned();
    loop {
        match fs::canonicalize(&probe) {
            Ok(path) => return Ok(path),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let Some(parent) = probe.parent().map(Path::to_owned) else {
                    return Err(IncludePathError::Missing);
                };
                if parent == probe {
                    return Err(IncludePathError::Missing);
                }
                probe = parent;
            }
            Err(source) => {
                return Err(IncludePathError::Io {
                    operation: "resolve include parent",
                    path: probe,
                    source,
                });
            }
        }
    }
}

fn is_static_github_input(target: &str) -> bool {
    target.starts_with(".github/")
        && target != ".github/UNIFIED-ACTIONS.md"
        && !target.starts_with(".github/ci/")
        && !target.starts_with(".github/workflows/")
}

fn resolve_repo_path(root: &str, relative: &str) -> Option<String> {
    let mut parts = if root == "." {
        Vec::new()
    } else {
        root.split('/').map(str::to_owned).collect::<Vec<_>>()
    };
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(component) => parts.push(component.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_include_paths, IncludeDiscovery, IncludeString, ManifestPart};

    #[test]
    fn parses_manifest_dir_concat_with_suffix() {
        assert_eq!(
            parse_include_paths(
                "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/lib.rs\"));"
            )
            .ok(),
            Some(vec![IncludeDiscovery::Resolved(
                IncludeString::ManifestDir(vec![
                    ManifestPart::ManifestDir,
                    ManifestPart::Literal("/src/lib.rs".to_owned()),
                ])
            )])
        );
    }

    #[test]
    fn parses_include_bytes_and_rust_escapes() {
        assert_eq!(
            parse_include_paths("include_bytes!(concat!(\"assets\\\\\", \"font\\x2Ettf\"));").ok(),
            Some(vec![IncludeDiscovery::Resolved(IncludeString::Relative(
                "assets\\font.ttf".to_owned()
            ))])
        );
    }

    #[test]
    fn accepts_comments_and_all_macro_delimiters() {
        let sources = [
            "include_str /* trivia */ ! (concat /* trivia */ ! [ r#\"a\"#, \"/b\" ])",
            "include_str /* trivia */ ! [concat /* trivia */ ! { r#\"a\"#, \"/b\" }]",
            "include_bytes /* trivia */ ! {concat /* trivia */ ! (r#\"a\"#, \"/b\")}",
        ];
        for source in sources {
            assert_eq!(
                parse_include_paths(source).ok(),
                Some(vec![IncludeDiscovery::Resolved(IncludeString::Relative(
                    "a/b".to_owned()
                ))]),
                "failed source: {source}"
            );
        }
    }

    #[test]
    fn accepts_include_macros_in_struct_field_values() {
        let source = r##"
struct Holder {
    text: &'static str,
    raw: &'static str,
    bytes: &'static [u8],
}

const _: Holder = Holder {
    text: include_str!("data.txt"),
    raw: include_str!(r#"raw.txt"#),
    bytes: include_bytes!(concat!("assets/", r#"bytes.bin"#)),
};

const _: &str = crate::include_str!("user-macro.txt");
"##;
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![
                IncludeDiscovery::Resolved(IncludeString::Relative("data.txt".to_owned())),
                IncludeDiscovery::Resolved(IncludeString::Relative("raw.txt".to_owned())),
                IncludeDiscovery::Resolved(IncludeString::Relative("assets/bytes.bin".to_owned())),
            ])
        );
    }

    #[test]
    fn accepts_standard_library_qualified_includes_but_ignores_user_macros() {
        let source = r#"
const _: &str = std::include_str!("std.txt");
const _: &[u8] = core::include_bytes!("core.bin");
const _: &str = crate::include_str!("user-macro.txt");
const _: &str = other::std::include_str!("nested-user-macro.txt");
"#;
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![
                IncludeDiscovery::Resolved(IncludeString::Relative("std.txt".to_owned())),
                IncludeDiscovery::Resolved(IncludeString::Relative("core.bin".to_owned())),
            ])
        );
    }

    #[test]
    fn accepts_aliases_of_standard_library_include_macros() {
        let source = r#"
use std::include_str as asset;
use core::{include_bytes as bytes};
use other::include_str as ignored;
const _: &str = asset!("aliased.txt");
const _: &[u8] = bytes!("aliased.bin");
const _: &str = ignored!("user-macro.txt");
"#;
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![
                IncludeDiscovery::Resolved(IncludeString::Relative("aliased.txt".to_owned())),
                IncludeDiscovery::Resolved(IncludeString::Relative("aliased.bin".to_owned())),
            ])
        );
    }

    #[test]
    fn expands_literal_include_wrapper_macros_without_scanning_metavariables() {
        let source = r#"
macro_rules! embed {
    ($p:literal) => { include_str!($p) };
}
const _: &str = embed!("wrapped.txt");
"#;
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![IncludeDiscovery::Resolved(IncludeString::Relative(
                "wrapped.txt".to_owned()
            ))])
        );
    }

    #[test]
    fn aliases_and_macro_names_follow_lexical_module_scope() {
        let source = r#"
const _: &str = asset!("root.txt");
use std::include_str as asset;

mod child {
    const PATH: &str = "not-an-include.txt";
    const _: &str = asset!(PATH);
    macro_rules! asset {
        ($p:literal) => { $p };
    }
    const _: &str = asset!(PATH);
}

mod sibling {
    use core::include_bytes as asset;
    const _: &[u8] = asset!("sibling.bin");
}
"#;
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![
                IncludeDiscovery::Resolved(IncludeString::Relative("root.txt".to_owned())),
                IncludeDiscovery::Resolved(IncludeString::Relative("sibling.bin".to_owned())),
            ])
        );
    }

    #[test]
    fn dynamic_include_in_struct_field_value_degrades_to_opaque() {
        let source = r#"
const PATH: &str = "data.txt";

struct Holder {
    text: &'static str,
}

const _: Holder = Holder {
    text: include_str!(PATH),
};
"#;
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![IncludeDiscovery::Opaque {
                macro_name: "include_str!",
                line: 9,
            }])
        );
    }

    #[test]
    fn preserves_manifest_dir_concat_boundaries() {
        assert_eq!(
            parse_include_paths(
                "include_str!(concat!(\"prefix\", env!(\"CARGO_MANIFEST_DIR\"), \"/suffix\"));"
            )
            .ok(),
            Some(vec![IncludeDiscovery::Resolved(
                IncludeString::ManifestDir(vec![
                    ManifestPart::Literal("prefix".to_owned()),
                    ManifestPart::ManifestDir,
                    ManifestPart::Literal("/suffix".to_owned()),
                ])
            )])
        );
    }

    #[test]
    fn indeterminate_shapes_degrade_to_opaque_with_invocation_lines() {
        let source = concat!(
            "const OUT: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/x\"));\n",
            "const OPT: &str = include_str!(option_env!(\"MAYBE_PATH\").unwrap_or(\"d\"));\n",
            "const IDENT: &str = include_str!(PATH);\n",
            "const MANIFEST: &str = include_str!(concat!(\n",
            "    env!(\"CARGO_MANIFEST_DIR\"),\n",
            "    \"/src/lib.rs\",\n",
            "));\n",
        );
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![
                IncludeDiscovery::Opaque {
                    macro_name: "include_str!",
                    line: 1,
                },
                IncludeDiscovery::Opaque {
                    macro_name: "include_str!",
                    line: 2,
                },
                IncludeDiscovery::Opaque {
                    macro_name: "include_str!",
                    line: 3,
                },
                IncludeDiscovery::Resolved(IncludeString::ManifestDir(vec![
                    ManifestPart::ManifestDir,
                    ManifestPart::Literal("/src/lib.rs".to_owned()),
                ])),
            ])
        );
    }

    #[test]
    fn undelimited_invocation_shaped_fragment_degrades_to_opaque() {
        let source = "macro_rules! m { (include_str! $($t:tt)*) => { $crate::emit!($($t)*) }; }\n";
        assert_eq!(
            parse_include_paths(source).ok(),
            Some(vec![IncludeDiscovery::Opaque {
                macro_name: "include_str!",
                line: 1,
            }])
        );
    }

    #[test]
    fn rejects_unterminated_input() {
        for source in [
            "/* include_str!(\"x\")",
            "include_str!(r#\"x);",
            "include_str! { \"x\"",
        ] {
            let result = parse_include_paths(source);
            assert!(result.is_err(), "invalid source was accepted: {source}");
        }
    }
}
