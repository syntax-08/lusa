//! Compile-time imports.
//!
//! Lusa modules are included textually before parsing:
//!
//! ```lusa
//! import "point.lusa"   -- exact relative/absolute path
//! mod point             -- resolves point.lusa, point/mod.lusa or point/main.lusa
//! ```
//!
//! Expansion is recursive, resolves paths relative to the importing file, and
//! includes every canonical path at most once.  The merged source then goes
//! through the normal lexer/parser/type-checker pipeline, so imported structs,
//! functions, and globals share one symbol table.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{LResult, LusaError, Span, E_PARSE};
use crate::lexer;
use crate::tokens::{StrSeg, TokenKind};

/// Expands imports using `base` (or the current directory) as the root.
pub fn expand_imports(src: &str, base: Option<&Path>) -> LResult<String> {
    let mut visited = HashSet::new();
    expand_inner(src, base, &mut visited)
}

/// Expands imports from a file, treating that file as already visited.
pub fn expand_imports_file(src: &str, path: &Path) -> LResult<String> {
    let mut visited = HashSet::new();
    if let Ok(canonical) = std::fs::canonicalize(path) {
        visited.insert(canonical);
    }
    expand_inner(src, path.parent(), &mut visited)
}

fn fallback_dir(base: Option<&Path>) -> PathBuf {
    match base {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

fn import_error(span: Span, message: String) -> LusaError {
    LusaError::new(E_PARSE, Some(span), message)
        .help("`import \"file.lusa\"` uses a path relative to the importing file; `mod name` looks for name.lusa")
}

fn resolve_import(name: &str, base: &Path) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let direct = Path::new(name);
    let candidate = if direct.is_absolute() {
        direct.to_path_buf()
    } else {
        base.join(direct)
    };
    if candidate.is_file() {
        return Some(candidate);
    }
    if candidate.extension().is_none() {
        let with_ext = candidate.with_extension("lusa");
        if with_ext.is_file() {
            return Some(with_ext);
        }
    }
    None
}

fn resolve_module(name: &str, base: &Path) -> Option<PathBuf> {
    let rel = name.replace('.', "/");
    let direct = base.join(format!("{rel}.lusa"));
    if direct.is_file() {
        return Some(direct);
    }
    for entry in ["mod.lusa", "main.lusa"] {
        let nested = base.join(&rel).join(entry);
        if nested.is_file() {
            return Some(nested);
        }
    }
    None
}

fn load_expanded(path: &Path, span: Span, visited: &mut HashSet<PathBuf>) -> LResult<String> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical.clone()) {
        // Already included by another module; a module contributes its
        // definitions exactly once.
        return Ok(String::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| import_error(span, format!("cannot read imported file `{}`: {e}", path.display())))?;
    expand_inner(&text, canonical.parent(), visited)
}

fn string_literal(segs: &[StrSeg], span: Span) -> LResult<String> {
    let mut out = String::new();
    for seg in segs {
        match seg {
            StrSeg::Lit(text) => out.push_str(text),
            StrSeg::Expr(_) => {
                return Err(import_error(
                    span,
                    "import paths must be plain string literals".to_string(),
                ));
            }
        }
    }
    Ok(out)
}

fn expand_inner(src: &str, base: Option<&Path>, visited: &mut HashSet<PathBuf>) -> LResult<String> {
    let toks = lexer::lex(src)?;
    let chars: Vec<char> = src.chars().collect();
    let base = fallback_dir(base);
    let mut out = String::with_capacity(src.len());
    let mut last = 0usize;
    let mut i = 0usize;

    while i < toks.len() {
        let token = &toks[i];
        let replacement = match &token.kind {
            TokenKind::Import => {
                let next = toks.get(i + 1);
                let (name, span) = match next {
                    Some(t) => match &t.kind {
                        TokenKind::Str(segs) => (string_literal(segs, t.span)?, t.span),
                        _ => {
                            return Err(import_error(
                                t.span,
                                format!("expected a string literal after `import`, found {}", t.desc()),
                            ));
                        }
                    },
                    None => {
                        return Err(import_error(
                            token.span,
                            "expected a string literal after `import`".to_string(),
                        ));
                    }
                };
                let path = resolve_import(&name, &base).ok_or_else(|| {
                    import_error(span, format!("cannot find imported file `{name}`"))
                })?;
                i += 1; // skip the string token as well
                let text = load_expanded(&path, span, visited)?;
                Some((span, text))
            }
            TokenKind::Mod => {
                let mut j = i + 1;
                let mut parts: Vec<String> = Vec::new();
                loop {
                    match toks.get(j).map(|t| &t.kind) {
                        Some(TokenKind::Name(name)) => {
                            parts.push(name.clone());
                            j += 1;
                        }
                        _ => break,
                    }
                    if matches!(toks.get(j).map(|t| &t.kind), Some(TokenKind::Dot)) {
                        j += 1;
                    } else {
                        break;
                    }
                }
                if parts.is_empty() {
                    let span = toks.get(i + 1).map(|t| t.span).unwrap_or(token.span);
                    return Err(import_error(
                        span,
                        "expected a module name after `mod`".to_string(),
                    ));
                }
                let end_token = &toks[j - 1];
                let span = Span::new(token.span.start, end_token.span.end);
                let dotted = parts.join(".");
                let path = resolve_module(&dotted, &base)
                    .ok_or_else(|| import_error(span, format!("cannot find module `{dotted}`")))?;
                i = j - 1;
                let text = load_expanded(&path, span, visited)?;
                Some((span, text))
            }
            _ => None,
        };

        if let Some((span, text)) = replacement {
            let start = token.span.start;
            if last < start && start <= chars.len() {
                out.extend(chars[last..start].iter());
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&text);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            last = span.end;
        }
        i += 1;
    }

    if last < chars.len() {
        out.extend(chars[last..].iter());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_files() {
        let dir = std::env::temp_dir().join(format!("lusa-imports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("part.lusa"), "local value = 7\n").expect("write part");
        let source = "import \"part.lusa\"\nprint(value)\n";
        let expanded = expand_imports(source, Some(&dir)).expect("expand");
        assert!(expanded.contains("local value = 7"));
        assert!(!expanded.contains("import"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expands_a_module_once() {
        let dir = std::env::temp_dir().join(format!("lusa-mod-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("m.lusa"), "local value = 7\n").expect("write");
        let source = "mod m\nimport \"m.lusa\"\n";
        let expanded = expand_imports(source, Some(&dir)).expect("expand");
        assert_eq!(expanded.matches("local value = 7").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
