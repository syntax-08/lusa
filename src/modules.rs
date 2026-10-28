//! Namespaced modules, path imports, and visibility (Module 23).
//!
//! The resolver runs after the legacy textual importer and before the lexer
//! used by either front end.  It loads file-backed modules, validates `pub`
//! privacy, resolves `use` aliases/re-exports, and rewrites every module-level
//! declaration to a collision-free internal name.  The resulting single
//! translation unit therefore runs unchanged through the interpreter, the
//! bytecode compiler, and the self-hosted C11 backend.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::error::{LResult, LusaError, Span, E_MODULE, E_PARSE};
use crate::lexer;
use crate::tokens::{Token, TokenKind as TK};

/// Resolves namespaced modules in `src`.
///
/// `base` is the directory used for top-level `mod name;` lookups.  When the
/// source came from a file, `root_file` lets the resolver reject recursive
/// inclusion of that file.
pub fn expand_modules(
    src: &str,
    base: Option<&Path>,
    root_file: Option<&Path>,
) -> LResult<String> {
    let mut resolver = Resolver::new();
    let base_dir = match base {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let source = Rc::new(SourceText::new(src));
    let tokens = lexer::lex(src)?;
    let root = resolver.collect_module(
        Vec::new(),
        None,
        source,
        tokens,
        (0, src.chars().count()),
        base_dir.clone(),
        base_dir,
    )?;
    if let Some(path) = root_file {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            resolver.loaded_files.insert(canonical, root);
        }
    }
    resolver.build_symbols();
    resolver.resolve_uses()?;
    let structs = resolver.render_hoisted_structs()?;
    let body = resolver.render_module(root)?;
    let mut out = structs;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&body);
    Ok(out)
}

struct SourceText {
    chars: Vec<char>,
}

impl SourceText {
    fn new(src: &str) -> Self {
        SourceText {
            chars: src.chars().collect(),
        }
    }

    fn slice(&self, span: Span) -> String {
        let start = span.start.min(self.chars.len());
        let end = span.end.min(self.chars.len()).max(start);
        self.chars[start..end].iter().collect()
    }

    fn gap(&self, start: usize, end: usize) -> String {
        let start = start.min(self.chars.len());
        let end = end.min(self.chars.len()).max(start);
        self.chars[start..end].iter().collect()
    }
}

struct ModuleData {
    path: Vec<String>,
    parent: Option<usize>,
    source: Rc<SourceText>,
    tokens: Vec<Token>,
    /// Char range rendered for this module.  Inline modules use the region
    /// between their braces; file modules use the whole file.
    range: (usize, usize),
    /// Directory used to resolve this module's child files.
    child_base: PathBuf,
    items: HashMap<String, ItemDecl>,
    uses: Vec<UseDecl>,
    mods: Vec<ModDecl>,
    symbols: HashMap<String, Symbol>,
    /// Top-level declaration token index -> internal (mangled) name.
    rename: HashMap<usize, String>,
    /// Top-level `pub` tokens, stripped from the rendered source.
    pub_tokens: HashSet<usize>,
    /// Top-level struct declaration token ranges (attributes included).
    /// These are hoisted to the top of the translation unit because the
    /// parser resolves `Name{...}` literals against declarations seen so far.
    struct_ranges: Vec<(usize, usize)>,
}

impl ModuleData {
    fn label(&self) -> String {
        if self.path.is_empty() {
            "crate root".to_string()
        } else {
            self.path.join("::")
        }
    }
}

#[derive(Clone)]
struct ItemDecl {
    name: String,
    public: bool,
    kind: ItemKind,
    mangled: String,
    span: Span,
}

#[derive(Clone)]
enum ItemKind {
    Item,
    Module(usize),
}

#[derive(Clone)]
struct Symbol {
    name: String,
    public: bool,
    /// Module that declared this symbol (used for descendant visibility).
    defined_in: usize,
    kind: SymbolKind,
    span: Span,
}

#[derive(Clone)]
enum SymbolKind {
    Item { mangled: String },
    Module { child: usize },
}

#[derive(Clone)]
struct UseDecl {
    path: Vec<String>,
    alias: String,
    glob: bool,
    public: bool,
    token_start: usize,
    token_end: usize,
    span: Span,
}

#[derive(Clone)]
struct ModDecl {
    child: usize,
    token_start: usize,
    header_end: usize,
}

enum ModStyle {
    File,
    Inline {
        body_start: usize,
        body_end: usize,
        char_start: usize,
        char_end: usize,
    },
}

struct RawMod {
    name: String,
    public: bool,
    style: ModStyle,
    token_start: usize,
    header_end: usize,
    span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameClose {
    End,
    Until,
}

struct Frame {
    close: FrameClose,
    await_do: bool,
    do_done: bool,
}

impl Frame {
    fn end() -> Frame {
        Frame {
            close: FrameClose::End,
            await_do: false,
            do_done: false,
        }
    }

    fn awaiting_do() -> Frame {
        Frame {
            close: FrameClose::End,
            await_do: true,
            do_done: false,
        }
    }

    fn until() -> Frame {
        Frame {
            close: FrameClose::Until,
            await_do: false,
            do_done: false,
        }
    }
}

#[derive(Default)]
struct RefPlan {
    /// Token index -> replacement text.
    single: HashMap<usize, String>,
    /// Path start token index -> (exclusive end token, replacement text).
    paths: HashMap<usize, (usize, String)>,
}

struct Resolver {
    modules: Vec<ModuleData>,
    by_path: HashMap<String, usize>,
    loaded_files: HashMap<PathBuf, usize>,
    loading_files: HashSet<PathBuf>,
}

impl Resolver {
    fn new() -> Self {
        Resolver {
            modules: Vec::new(),
            by_path: HashMap::new(),
            loaded_files: HashMap::new(),
            loading_files: HashSet::new(),
        }
    }

    fn collect_module(
        &mut self,
        path: Vec<String>,
        parent: Option<usize>,
        source: Rc<SourceText>,
        tokens: Vec<Token>,
        range: (usize, usize),
        child_base: PathBuf,
        _module_base: PathBuf,
    ) -> LResult<usize> {
        let key = path.join("::");
        if self.by_path.contains_key(&key) {
            return Err(LusaError::new(
                E_PARSE,
                None,
                format!("module `{}` is declared more than once", path.join("::")),
            )
            .help("each inline or file-backed module may be declared once per program"));
        }
        let id = self.modules.len();
        self.by_path.insert(key, id);
        self.modules.push(ModuleData {
            path: path.clone(),
            parent,
            source,
            tokens,
            range,
            child_base,
            items: HashMap::new(),
            uses: Vec::new(),
            mods: Vec::new(),
            symbols: HashMap::new(),
            rename: HashMap::new(),
            pub_tokens: HashSet::new(),
            struct_ranges: Vec::new(),
        });
        self.scan_module(id)?;
        Ok(id)
    }

    fn scan_module(&mut self, id: usize) -> LResult<()> {
        let path = self.modules[id].path.clone();
        let tokens = self.modules[id].tokens.clone();
        let child_base = self.modules[id].child_base.clone();

        let mut items: HashMap<String, ItemDecl> = HashMap::new();
        let mut uses: Vec<UseDecl> = Vec::new();
        let mut raw_mods: Vec<RawMod> = Vec::new();
        let mut rename: HashMap<usize, String> = HashMap::new();
        let mut pub_tokens: HashSet<usize> = HashSet::new();
        let mut struct_ranges: Vec<(usize, usize)> = Vec::new();
        let mut pending_attr: Option<usize> = None;
        let mut stack: Vec<Frame> = Vec::new();
        let mut pending_pub = false;
        let mut i = 0usize;

        while i < tokens.len() {
            if matches!(tokens[i].kind, TK::Eof) {
                break;
            }

            if stack.is_empty() {
                if is_attr_start(&tokens, i) {
                    if pending_attr.is_none() {
                        pending_attr = Some(i);
                    }
                    i = skip_attribute(&tokens, i)?;
                    continue;
                }
                if is_name(&tokens[i], "pub") {
                    if pending_pub {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(tokens[i].span),
                            "duplicate `pub` modifier",
                        ));
                    }
                    pending_pub = true;
                    pub_tokens.insert(i);
                    i += 1;
                    continue;
                }
                if matches!(tokens[i].kind, TK::Mod) {
                    let (raw, next) = parse_mod(&tokens, i, pending_pub)?;
                    pending_pub = false;
                    pending_attr = None;
                    raw_mods.push(raw);
                    i = next;
                    continue;
                }
                if is_name(&tokens[i], "use") {
                    let (use_decl, next) = parse_use(&tokens, i, pending_pub)?;
                    pending_pub = false;
                    pending_attr = None;
                    uses.push(use_decl);
                    i = next;
                    continue;
                }
                if matches!(tokens[i].kind, TK::Function) {
                    pending_attr = None;
                    if let Some(name_idx) = plain_decl_name(&tokens, i) {
                        if !is_function_target_sugar(&tokens, name_idx) {
                            let name = name_at(&tokens, name_idx);
                            let mangled = mangle(&path, &name);
                            insert_item(
                                &mut items,
                                ItemDecl {
                                    name: name.clone(),
                                    public: pending_pub,
                                    kind: ItemKind::Item,
                                    mangled: mangled.clone(),
                                    span: tokens[name_idx].span,
                                },
                            )?;
                            rename.insert(name_idx, mangled);
                        }
                        pending_pub = false;
                        stack.push(Frame::end());
                        i = name_idx + 1;
                        continue;
                    }
                    pending_pub = false;
                    stack.push(Frame::end());
                    i += 1;
                    continue;
                }
                if matches!(tokens[i].kind, TK::Async) {
                    pending_attr = None;
                    if let Some(name_idx) = async_decl_name(&tokens, i) {
                        if !is_function_target_sugar(&tokens, name_idx) {
                            let name = name_at(&tokens, name_idx);
                            let mangled = mangle(&path, &name);
                            insert_item(
                                &mut items,
                                ItemDecl {
                                    name,
                                    public: pending_pub,
                                    kind: ItemKind::Item,
                                    mangled: mangled.clone(),
                                    span: tokens[name_idx].span,
                                },
                            )?;
                            rename.insert(name_idx, mangled);
                        }
                        pending_pub = false;
                        stack.push(Frame::end());
                        i = name_idx + 1;
                        continue;
                    }
                    pending_pub = false;
                    if is_async_function_header(&tokens, i) {
                        stack.push(Frame::end());
                    }
                    i += 1;
                    continue;
                }
                if matches!(tokens[i].kind, TK::Struct) {
                    if let Some(name_idx) = plain_decl_name(&tokens, i) {
                        let name = name_at(&tokens, name_idx);
                        let mangled = mangle(&path, &name);
                        insert_item(
                            &mut items,
                            ItemDecl {
                                name: name.clone(),
                                public: pending_pub,
                                kind: ItemKind::Item,
                                mangled: mangled.clone(),
                                span: tokens[name_idx].span,
                            },
                        )?;
                        rename.insert(name_idx, mangled);
                        pending_pub = false;
                        let range_start = pending_attr.take().unwrap_or(i);
                        let range_end = if matches!(
                            tokens.get(name_idx + 1).map(|t| &t.kind),
                            Some(TK::LBrace)
                        ) {
                            let close = find_matching(&tokens, name_idx + 1, TK::LBrace, TK::RBrace)
                                .ok_or_else(|| {
                                    LusaError::new(
                                        E_PARSE,
                                        Some(tokens[i].span),
                                        "unterminated `struct` body",
                                    )
                                })?;
                            close + 1
                        } else {
                            let mut j = name_idx + 1;
                            while j < tokens.len()
                                && !matches!(tokens[j].kind, TK::End | TK::Eof)
                            {
                                j += 1;
                            }
                            if !matches!(tokens.get(j).map(|t| &t.kind), Some(TK::End)) {
                                return Err(LusaError::new(
                                    E_PARSE,
                                    Some(tokens[i].span),
                                    "unterminated `struct` (expected `end`)",
                                ));
                            }
                            j + 1
                        };
                        struct_ranges.push((range_start, range_end));
                        i = range_end;
                        continue;
                    }
                    pending_pub = false;
                    pending_attr = None;
                    stack.push(Frame::end());
                    i += 1;
                    continue;
                }
                if matches!(tokens[i].kind, TK::Local) {
                    pending_attr = None;
                    // `local function name(...) ... end` declares one local
                    // binding whose value is a function literal.  Consume the
                    // header here so the `function` token is not seen again as
                    // a top-level declaration.
                    if matches!(tokens.get(i + 1).map(|t| &t.kind), Some(TK::Function)) {
                        if let Some(name_idx) = plain_decl_name(&tokens, i + 1) {
                            let name = name_at(&tokens, name_idx);
                            let mangled = mangle(&path, &name);
                            insert_item(
                                &mut items,
                                ItemDecl {
                                    name: name.clone(),
                                    public: pending_pub,
                                    kind: ItemKind::Item,
                                    mangled: mangled.clone(),
                                    span: tokens[name_idx].span,
                                },
                            )?;
                            rename.insert(name_idx, mangled);
                        }
                        pending_pub = false;
                        stack.push(Frame::end());
                        i += 2;
                        continue;
                    }
                    let targets = local_targets(&tokens, i);
                    for name_idx in targets {
                        let name = name_at(&tokens, name_idx);
                        let mangled = mangle(&path, &name);
                        insert_or_reuse_item(
                            &mut items,
                            ItemDecl {
                                name: name.clone(),
                                public: pending_pub,
                                kind: ItemKind::Item,
                                mangled: mangled.clone(),
                                span: tokens[name_idx].span,
                            },
                        );
                        rename.insert(name_idx, mangled);
                    }
                    pending_pub = false;
                    i += 1;
                    continue;
                }
                pending_attr = None;
                if let Some(targets) = declaration_assignment_targets(&tokens, i) {
                    for name_idx in targets {
                        let name = name_at(&tokens, name_idx);
                        let mangled = mangle(&path, &name);
                        insert_or_reuse_item(
                            &mut items,
                            ItemDecl {
                                name: name.clone(),
                                public: pending_pub,
                                kind: ItemKind::Item,
                                mangled: mangled.clone(),
                                span: tokens[name_idx].span,
                            },
                        );
                        rename.insert(name_idx, mangled);
                    }
                    pending_pub = false;
                } else if pending_pub {
                    return Err(LusaError::new(
                        E_MODULE,
                        Some(tokens[i].span),
                        "`pub` must be followed by a module item",
                    )
                    .help(
                        "visibility is supported on `mod`, `use`, `function`, `async function`, `struct`, and top-level `local` declarations",
                    ));
                }
            }

            update_scan_stack(&tokens, i, &mut stack);
            i += 1;
        }

        // Load children after the module's own declarations are known, so
        // duplicate names are diagnosed deterministically.
        for raw in raw_mods {
            if items.contains_key(&raw.name) {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(raw.span),
                    format!("`{}` is defined more than once in this module", raw.name),
                ));
            }
            let child_path = {
                let mut p = path.clone();
                p.push(raw.name.clone());
                p
            };
            let child = match raw.style {
                ModStyle::Inline {
                    body_start,
                    body_end,
                    char_start,
                    char_end,
                } => {
                    let source = Rc::clone(&self.modules[id].source);
                    let body_tokens = self.modules[id].tokens[body_start..body_end].to_vec();
                    self.collect_module(
                        child_path,
                        Some(id),
                        source,
                        body_tokens,
                        (char_start, char_end),
                        child_base.clone(),
                        child_base.clone(),
                    )?
                }
                ModStyle::File => {
                    let file = resolve_module_file(&raw.name, &child_base).ok_or_else(|| {
                        LusaError::new(
                            E_PARSE,
                            Some(raw.span),
                            format!("cannot find module `{}`", raw.name),
                        )
                        .help(format!(
                            "looked for `{}.lusa` and `{}/mod.lusa` relative to {}",
                            raw.name,
                            raw.name,
                            child_base.display()
                        ))
                    })?;
                    let canonical = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
                    if let Some(existing) = self.loaded_files.get(&canonical) {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(raw.span),
                            format!(
                                "module file `{}` is already loaded as `{}`",
                                file.display(),
                                self.modules[*existing].label()
                            ),
                        ));
                    }
                    if self.loading_files.contains(&canonical) {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(raw.span),
                            format!("module import cycle involving `{}`", file.display()),
                        ));
                    }
                    self.loading_files.insert(canonical.clone());
                    let text = std::fs::read_to_string(&file).map_err(|e| {
                        LusaError::new(
                            E_PARSE,
                            Some(raw.span),
                            format!("cannot read module `{}`: {e}", file.display()),
                        )
                    })?;
                    let expanded = crate::imports::expand_legacy_file(&text, &file)?;
                    let source = Rc::new(SourceText::new(&expanded));
                    let tokens = lexer::lex(&expanded)?;
                    let module_base = file
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| child_base.clone());
                    let mut nested_base = module_base.clone();
                    let stem_is_mod = file
                        .file_stem()
                        .map(|s| s.to_string_lossy().eq_ignore_ascii_case("mod"))
                        .unwrap_or(false);
                    if !stem_is_mod {
                        if let Some(stem) = file.file_stem() {
                            let candidate = module_base.join(stem);
                            if candidate.is_dir() {
                                nested_base = candidate;
                            }
                        }
                    }
                    let range = (0usize, expanded.chars().count());
                    let child = self.collect_module(
                        child_path,
                        Some(id),
                        source,
                        tokens,
                        range,
                        nested_base,
                        module_base,
                    )?;
                    self.loading_files.remove(&canonical);
                    self.loaded_files.insert(canonical, child);
                    child
                }
            };
            items.insert(
                raw.name.clone(),
                ItemDecl {
                    name: raw.name.clone(),
                    public: raw.public,
                    kind: ItemKind::Module(child),
                    mangled: String::new(),
                    span: raw.span,
                },
            );
            self.modules[id].mods.push(ModDecl {
                child,
                token_start: raw.token_start,
                header_end: raw.header_end,
            });
        }

        self.modules[id].items = items;
        self.modules[id].uses = uses;
        self.modules[id].rename = rename;
        self.modules[id].pub_tokens = pub_tokens;
        self.modules[id].struct_ranges = struct_ranges;
        Ok(())
    }

    fn build_symbols(&mut self) {
        for id in 0..self.modules.len() {
            let items = self.modules[id].items.clone();
            let mut symbols = HashMap::new();
            for (name, decl) in items {
                let kind = match decl.kind {
                    ItemKind::Item => SymbolKind::Item {
                        mangled: decl.mangled,
                    },
                    ItemKind::Module(child) => SymbolKind::Module { child },
                };
                symbols.insert(
                    name.clone(),
                    Symbol {
                        name,
                        public: decl.public,
                        defined_in: id,
                        kind,
                        span: decl.span,
                    },
                );
            }
            self.modules[id].symbols = symbols;
        }
    }

    fn resolve_uses(&mut self) -> LResult<()> {
        let mut pending: Vec<(usize, UseDecl)> = Vec::new();
        for (id, module) in self.modules.iter().enumerate() {
            for use_decl in &module.uses {
                pending.push((id, use_decl.clone()));
            }
        }

        while !pending.is_empty() {
            let mut retry: Vec<(usize, UseDecl, LusaError)> = Vec::new();
            let mut progress = false;
            for (id, use_decl) in pending {
                match self.resolve_path(id, &use_decl.path, true, use_decl.span) {
                    Ok(symbol) => {
                        self.install_use(id, &use_decl, symbol)?;
                        progress = true;
                    }
                    Err(err) if err.code == E_MODULE => return Err(err),
                    Err(err) => retry.push((id, use_decl, err)),
                }
            }
            if retry.is_empty() {
                return Ok(());
            }
            if !progress {
                let mut err = retry.remove(0).2;
                err.help.push(
                    "`use` paths resolve relative to the current module; use `crate::`, `self::`, or `super::` for explicit roots"
                        .to_string(),
                );
                return Err(err);
            }
            pending = retry
                .into_iter()
                .map(|(id, use_decl, _)| (id, use_decl))
                .collect();
        }
        Ok(())
    }

    fn install_use(&mut self, id: usize, use_decl: &UseDecl, symbol: Symbol) -> LResult<()> {
        if use_decl.public && !symbol.public {
            return Err(LusaError::new(
                E_MODULE,
                Some(use_decl.span),
                format!(
                    "private item `{}` cannot be re-exported with `pub use`",
                    symbol.name
                ),
            )
            .help(format!(
                "mark `{}` as `pub` in `{}`, or make this a private `use`",
                symbol.name,
                self.modules[symbol.defined_in].label()
            )));
        }
        if use_decl.glob {
            let target = match symbol.kind {
                SymbolKind::Module { child } => child,
                SymbolKind::Item { .. } => {
                    return Err(LusaError::new(
                        E_PARSE,
                        Some(use_decl.span),
                        "`use ...::*` requires a module path",
                    ));
                }
            };
            let names: Vec<(String, Symbol)> = self.modules[target]
                .symbols
                .iter()
                .filter(|(_, sym)| sym.public)
                .map(|(name, sym)| (name.clone(), sym.clone()))
                .collect();
            for (name, mut alias) in names {
                alias.name = name.clone();
                alias.public = use_decl.public;
                alias.defined_in = id;
                alias.span = use_decl.span;
                self.insert_alias(id, name, alias, use_decl.span)?;
            }
            return Ok(());
        }

        let name = use_decl.alias.clone();
        let mut alias = symbol;
        alias.name = name.clone();
        alias.public = use_decl.public;
        alias.defined_in = id;
        alias.span = use_decl.span;
        self.insert_alias(id, name, alias, use_decl.span)
    }

    fn insert_alias(
        &mut self,
        id: usize,
        name: String,
        symbol: Symbol,
        span: Span,
    ) -> LResult<()> {
        if let Some(existing) = self.modules[id].symbols.get(&name) {
            return Err(LusaError::new(
                E_PARSE,
                Some(span),
                format!(
                    "`{}` is already defined in module `{}`",
                    name,
                    self.modules[id].label()
                ),
            )
            .note(
                format!("the previous definition is also named `{name}`"),
                Some(existing.span),
            ));
        }
        self.modules[id].symbols.insert(name, symbol);
        Ok(())
    }

    fn resolve_path(
        &self,
        from: usize,
        segments: &[String],
        check_visibility: bool,
        span: Span,
    ) -> LResult<Symbol> {
        if segments.is_empty() {
            return Err(LusaError::new(
                E_PARSE,
                Some(span),
                "empty module path",
            ));
        }

        let mut start = from;
        let mut index = 0usize;
        match segments[0].as_str() {
            "crate" => {
                start = 0;
                index = 1;
            }
            "self" => {
                start = from;
                index = 1;
            }
            "super" => {
                let mut current = from;
                while segments.get(index).map(String::as_str) == Some("super") {
                    current = self.modules[current].parent.ok_or_else(|| {
                        LusaError::new(
                            E_MODULE,
                            Some(span),
                            "`super` has no parent module at the crate root",
                        )
                    })?;
                    index += 1;
                }
                start = current;
            }
            _ => {}
        }
        if index >= segments.len() {
            return Err(LusaError::new(
                E_PARSE,
                Some(span),
                format!("module path `{}` does not name an item", segments.join("::")),
            ));
        }

        let mut module = start;
        for (offset, segment) in segments[index..].iter().enumerate() {
            let symbol = self.modules[module].symbols.get(segment).ok_or_else(|| {
                LusaError::new(
                    E_PARSE,
                    Some(span),
                    format!(
                        "cannot resolve `{}` in module `{}`",
                        segments.join("::"),
                        self.modules[module].label()
                    ),
                )
                .help("declare the item or add a matching `use` import")
            })?;
            if check_visibility && !self.visible(from, symbol) {
                let what = match symbol.kind {
                    SymbolKind::Module { .. } => "module",
                    SymbolKind::Item { .. } => "item",
                };
                return Err(LusaError::new(
                    E_MODULE,
                    Some(span),
                    format!(
                        "{} `{}` is private to module `{}`",
                        what,
                        symbol.name,
                        self.modules[symbol.defined_in].label()
                    ),
                )
                .help(format!(
                    "mark `{}` as `pub` in `{}`, or add a `pub use` re-export",
                    symbol.name,
                    self.modules[symbol.defined_in].label()
                )));
            }
            let last = offset + index == segments.len() - 1;
            if last {
                return Ok(symbol.clone());
            }
            match symbol.kind {
                SymbolKind::Module { child } => module = child,
                SymbolKind::Item { .. } => {
                    return Err(LusaError::new(
                        E_PARSE,
                        Some(span),
                        format!("`{}` is not a module", segment),
                    ));
                }
            }
        }
        unreachable!("module path loop always returns")
    }

    /// True when `segments` starts at a module and must therefore resolve as a
    /// namespaced path.  A non-module head (for example `table::field`, the
    /// historical spelling of `.field`) is left untouched so existing programs
    /// keep working.
    fn path_starts_known(&self, from: usize, segments: &[String]) -> bool {
        match segments.first().map(String::as_str) {
            Some("crate") | Some("self") | Some("super") => true,
            Some(first) => match self.modules[from].symbols.get(first) {
                Some(symbol) => matches!(symbol.kind, SymbolKind::Module { .. }),
                None => false,
            },
            None => false,
        }
    }

    fn visible(&self, from: usize, symbol: &Symbol) -> bool {
        symbol.public || self.is_descendant_or_same(from, symbol.defined_in)
    }

    fn is_descendant_or_same(&self, mut current: usize, ancestor: usize) -> bool {
        loop {
            if current == ancestor {
                return true;
            }
            match self.modules[current].parent {
                Some(parent) => current = parent,
                None => return false,
            }
        }
    }

    fn plan_references(&self, id: usize) -> LResult<RefPlan> {
        let module = &self.modules[id];
        let tokens = &module.tokens;
        let mut plan = RefPlan::default();
        let mut scopes: Vec<HashSet<String>> = Vec::new();
        let mut frames: Vec<Frame> = Vec::new();
        let mut skip_tokens: HashSet<usize> = HashSet::new();
        let mut skip_names: HashSet<usize> = module.rename.keys().copied().collect();
        let mut skip_ranges: HashMap<usize, usize> = HashMap::new();
        let mut braces = 0usize;

        for decl in &module.mods {
            skip_ranges.insert(decl.token_start, decl.header_end);
        }
        for use_decl in &module.uses {
            skip_ranges.insert(use_decl.token_start, use_decl.token_end);
        }
        for &pub_token in &module.pub_tokens {
            skip_names.insert(pub_token);
        }

        let mut i = 0usize;
        while i < tokens.len() {
            if matches!(tokens[i].kind, TK::Eof) {
                break;
            }
            if let Some(end) = skip_ranges.get(&i) {
                i = *end;
                continue;
            }

            if is_attr_start(tokens, i) {
                i = skip_attribute(tokens, i)?;
                continue;
            }
            if skip_tokens.contains(&i) {
                i += 1;
                continue;
            }
            match tokens[i].kind {
                TK::Function => {
                    if let Some(name_idx) = plain_decl_name(tokens, i) {
                        if !is_function_target_sugar(tokens, name_idx) {
                            skip_names.insert(name_idx);
                        }
                    }
                    scopes.push(HashSet::new());
                    for idx in function_params(tokens, i) {
                        skip_names.insert(idx);
                        scopes.last_mut().unwrap().insert(name_at(tokens, idx));
                    }
                    frames.push(Frame::end());
                }
                TK::Async => {
                    if is_async_function_header(tokens, i) {
                        let fn_idx = i + 1;
                        skip_tokens.insert(fn_idx);
                        if let Some(name_idx) = async_decl_name(tokens, i) {
                            if !is_function_target_sugar(tokens, name_idx) {
                                skip_names.insert(name_idx);
                            }
                        }
                        scopes.push(HashSet::new());
                        for idx in function_params(tokens, fn_idx) {
                            skip_names.insert(idx);
                            scopes.last_mut().unwrap().insert(name_at(tokens, idx));
                        }
                        frames.push(Frame::end());
                    }
                }
                TK::Local => {
                    scopes.push(HashSet::new());
                    for idx in local_targets(tokens, i) {
                        skip_names.insert(idx);
                        if !frames.is_empty() {
                            scopes.last_mut().unwrap().insert(name_at(tokens, idx));
                        }
                    }
                    // The empty scope above is not a block; remove it again
                    // unless a target needed a function-local binding.
                    if frames.is_empty() {
                        scopes.pop();
                    } else {
                        // Local declarations live in the surrounding block,
                        // not in a new one.  Move the names one scope outward.
                        let declared = scopes.pop().unwrap();
                        if let Some(outer) = scopes.last_mut() {
                            outer.extend(declared);
                        } else {
                            scopes.push(declared);
                        }
                    }
                }
                TK::For => {
                    let vars = for_targets(tokens, i);
                    frames.push(Frame::awaiting_do());
                    scopes.push(HashSet::new());
                    for idx in vars {
                        skip_names.insert(idx);
                        scopes.last_mut().unwrap().insert(name_at(tokens, idx));
                    }
                }
                TK::While => {
                    frames.push(Frame::awaiting_do());
                    scopes.push(HashSet::new());
                }
                TK::Unsafe => {
                    if matches!(tokens.get(i + 1).map(|t| &t.kind), Some(TK::Do)) {
                        frames.push(Frame::awaiting_do());
                        scopes.push(HashSet::new());
                    } else {
                        frames.push(Frame::end());
                        scopes.push(HashSet::new());
                    }
                }
                TK::Do => {
                    if let Some(frame) = frames.last_mut() {
                        if frame.await_do && !frame.do_done {
                            frame.do_done = true;
                            i += 1;
                            continue;
                        }
                    }
                    frames.push(Frame::end());
                    scopes.push(HashSet::new());
                }
                TK::If | TK::Match => {
                    frames.push(Frame::end());
                    scopes.push(HashSet::new());
                }
                TK::Repeat => {
                    frames.push(Frame::until());
                    scopes.push(HashSet::new());
                }
                TK::Struct => {
                    if let Some(name_idx) = plain_decl_name(tokens, i) {
                        skip_names.insert(name_idx);
                        if !matches!(
                            tokens.get(name_idx + 1).map(|t| &t.kind),
                            Some(TK::LBrace)
                        ) {
                            frames.push(Frame::end());
                            scopes.push(HashSet::new());
                        }
                    } else {
                        frames.push(Frame::end());
                        scopes.push(HashSet::new());
                    }
                }
                TK::LBrace => braces += 1,
                TK::RBrace => braces = braces.saturating_sub(1),
                TK::End => {
                    if frames.pop().is_some() {
                        scopes.pop();
                    }
                }
                TK::Until => {
                    if frames.last().map(|f| f.close) == Some(FrameClose::Until) {
                        frames.pop();
                        scopes.pop();
                    }
                }
                TK::Name(_) => {
                    if skip_names.contains(&i) {
                        i += 1;
                        continue;
                    }
                    let name = name_at(tokens, i);
                    let prev = if i > 0 {
                        Some(&tokens[i - 1].kind)
                    } else {
                        None
                    };
                    if matches!(prev, Some(TK::Dot) | Some(TK::ColonColon)) {
                        i += 1;
                        continue;
                    }
                    if matches!(prev, Some(TK::Colon)) {
                        // `name: value` in a struct literal keeps `value`
                        // resolvable; `object:method(` stays untouched.
                        if matches!(tokens.get(i + 1).map(|t| &t.kind), Some(TK::LParen))
                            && !colon_is_field_separator(tokens, i, braces)
                        {
                            i += 1;
                            continue;
                        }
                    }
                    if colon_is_field_name(tokens, i, braces) {
                        i += 1;
                        continue;
                    }

                    if matches!(tokens.get(i + 1).map(|t| &t.kind), Some(TK::ColonColon)) {
                        let (segments, end) = path_segments(tokens, i);
                        let in_local = scopes.iter().rev().any(|s| s.contains(&name));
                        let special = matches!(name.as_str(), "crate" | "self" | "super");
                        if in_local && !special {
                            i = end;
                            continue;
                        }
                        match self.resolve_path(id, &segments, true, tokens[i].span) {
                            Ok(symbol) => {
                                if let SymbolKind::Item { mangled } = symbol.kind {
                                    plan.paths.insert(i, (end, mangled));
                                }
                            }
                            Err(err) => {
                                if self.path_starts_known(id, &segments) {
                                    return Err(err);
                                }
                            }
                        }
                        i = end;
                        continue;
                    }

                    if scopes.iter().rev().any(|s| s.contains(&name)) {
                        i += 1;
                        continue;
                    }
                    if let Some(symbol) = self.modules[id].symbols.get(&name) {
                        if let SymbolKind::Item { mangled } = &symbol.kind {
                            plan.single.insert(i, mangled.clone());
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }

        Ok(plan)
    }

    fn render_hoisted_structs(&self) -> LResult<String> {
        let mut out = String::new();
        for id in 0..self.modules.len() {
            let module = &self.modules[id];
            if module.struct_ranges.is_empty() {
                continue;
            }
            let plan = self.plan_references(id)?;
            for (start, end) in &module.struct_ranges {
                let text = self.render_token_range(id, *start, *end, &plan);
                let text = text.trim_end_matches(['\r', '\n']);
                if text.trim().is_empty() {
                    continue;
                }
                out.push_str(text);
                out.push_str("\n\n");
            }
        }
        Ok(out)
    }

    /// Renders a token range with the same declaration/reference rewriting as
    /// `render_module`.  Used for hoisted struct declarations.
    fn render_token_range(&self, id: usize, start: usize, end: usize, plan: &RefPlan) -> String {
        let module = &self.modules[id];
        let source = &module.source;
        let tokens = &module.tokens;
        let mut out = String::new();
        if start >= end || start >= tokens.len() {
            return out;
        }
        let mut last = tokens[start].span.start;
        let mut i = start;
        while i < end && i < tokens.len() {
            if module.pub_tokens.contains(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                last = tokens[i].span.end;
                i += 1;
                continue;
            }
            if let Some(replacement) = module.rename.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                out.push_str(replacement);
                last = tokens[i].span.end;
                i += 1;
                continue;
            }
            if let Some((path_end, replacement)) = plan.paths.get(&i) {
                if *path_end <= end {
                    out.push_str(&source.gap(last, tokens[i].span.start));
                    out.push_str(replacement);
                    last = tokens[*path_end - 1].span.end;
                    i = *path_end;
                    continue;
                }
            }
            if let Some(replacement) = plan.single.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                out.push_str(replacement);
                last = tokens[i].span.end;
                i += 1;
                continue;
            }
            out.push_str(&source.gap(last, tokens[i].span.start));
            out.push_str(&source.slice(tokens[i].span));
            last = tokens[i].span.end;
            i += 1;
        }
        out.push_str(&source.gap(last, tokens[end - 1].span.end));
        out
    }

    fn render_module(&self, id: usize) -> LResult<String> {
        let module = &self.modules[id];
        let plan = self.plan_references(id)?;
        let source = Rc::clone(&module.source);
        let tokens = &module.tokens;
        let (range_start, range_end) = module.range;
        let mut mods_at: HashMap<usize, &ModDecl> = HashMap::new();
        for decl in &module.mods {
            mods_at.insert(decl.token_start, decl);
        }
        let mut uses_at: HashMap<usize, &UseDecl> = HashMap::new();
        for use_decl in &module.uses {
            uses_at.insert(use_decl.token_start, use_decl);
        }
        let mut hoisted_at: HashMap<usize, usize> = HashMap::new();
        for (start, end) in &module.struct_ranges {
            hoisted_at.insert(*start, *end);
        }

        let mut out = String::new();
        let mut last = range_start;
        let mut i = 0usize;
        while i < tokens.len() {
            if matches!(tokens[i].kind, TK::Eof) || tokens[i].span.start >= range_end {
                break;
            }
            if let Some(end) = hoisted_at.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                last = tokens[*end - 1].span.end;
                i = *end;
                continue;
            }
            if let Some(decl) = mods_at.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                let child = self.render_module(decl.child)?;
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(child.trim_end_matches(['\r', '\n']));
                out.push('\n');
                let skipped_end = tokens[decl.header_end - 1].span.end;
                last = skipped_end;
                i = decl.header_end;
                continue;
            }
            if let Some(use_decl) = uses_at.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                last = tokens[use_decl.token_end - 1].span.end;
                i = use_decl.token_end;
                continue;
            }
            if module.pub_tokens.contains(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                last = tokens[i].span.end;
                i += 1;
                continue;
            }
            if let Some(replacement) = module.rename.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                out.push_str(replacement);
                last = tokens[i].span.end;
                i += 1;
                continue;
            }
            if let Some((end, replacement)) = plan.paths.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                out.push_str(replacement);
                last = tokens[*end - 1].span.end;
                i = *end;
                continue;
            }
            if let Some(replacement) = plan.single.get(&i) {
                out.push_str(&source.gap(last, tokens[i].span.start));
                out.push_str(replacement);
                last = tokens[i].span.end;
                i += 1;
                continue;
            }
            out.push_str(&source.gap(last, tokens[i].span.start));
            out.push_str(&source.slice(tokens[i].span));
            last = tokens[i].span.end;
            i += 1;
        }
        out.push_str(&source.gap(last, range_end));
        Ok(out)
    }
}

fn name_at(tokens: &[Token], index: usize) -> String {
    match tokens.get(index).map(|t| &t.kind) {
        Some(TK::Name(name)) => name.clone(),
        _ => String::new(),
    }
}

fn is_name(token: &Token, name: &str) -> bool {
    matches!(&token.kind, TK::Name(n) if n == name)
}

fn mangle(path: &[String], name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("__lusa_m_{}__{}", path.join("_"), name)
    }
}

fn insert_item(items: &mut HashMap<String, ItemDecl>, decl: ItemDecl) -> LResult<()> {
    if items.contains_key(&decl.name) {
        return Err(LusaError::new(
            E_PARSE,
            Some(decl.span),
            format!("`{}` is defined more than once in this module", decl.name),
        ));
    }
    items.insert(decl.name.clone(), decl);
    Ok(())
}

fn insert_or_reuse_item(items: &mut HashMap<String, ItemDecl>, decl: ItemDecl) {
    // A later top-level assignment refers to the same global; it does not
    // redeclare it.  Keep the first visibility and span.
    if !items.contains_key(&decl.name) {
        items.insert(decl.name.clone(), decl);
    }
}

fn is_attr_start(tokens: &[Token], index: usize) -> bool {
    matches!(tokens.get(index).map(|t| &t.kind), Some(TK::Len))
        && matches!(tokens.get(index + 1).map(|t| &t.kind), Some(TK::LBracket))
}

fn skip_attribute(tokens: &[Token], index: usize) -> LResult<usize> {
    let close = find_matching(tokens, index + 1, TK::LBracket, TK::RBracket).ok_or_else(|| {
        LusaError::new(
            E_PARSE,
            Some(tokens[index].span),
            "unterminated attribute (expected `]`)",
        )
    })?;
    Ok(close + 1)
}

fn find_matching(tokens: &[Token], open: usize, open_kind: TK, close_kind: TK) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < tokens.len() {
        if tokens[i].kind == open_kind {
            depth += 1;
        } else if tokens[i].kind == close_kind {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn parse_mod(tokens: &[Token], start: usize, public: bool) -> LResult<(RawMod, usize)> {
    let name_idx = start + 1;
    let name = match tokens.get(name_idx).map(|t| &t.kind) {
        Some(TK::Name(n)) => n.clone(),
        _ => {
            return Err(LusaError::new(
                E_PARSE,
                Some(tokens[start].span),
                "expected a module name after `mod`",
            ))
        }
    };
    let after = name_idx + 1;
    match tokens.get(after).map(|t| &t.kind) {
        Some(TK::Semi) => Ok((
            RawMod {
                name,
                public,
                style: ModStyle::File,
                token_start: start,
                header_end: after + 1,
                span: Span::new(tokens[start].span.start, tokens[after].span.end),
            },
            after + 1,
        )),
        Some(TK::LBrace) => {
            let close = find_matching(tokens, after, TK::LBrace, TK::RBrace).ok_or_else(|| {
                LusaError::new(
                    E_PARSE,
                    Some(tokens[after].span),
                    "unterminated inline module (expected `}`)",
                )
            })?;
            Ok((
                RawMod {
                    name,
                    public,
                    style: ModStyle::Inline {
                        body_start: after + 1,
                        body_end: close,
                        char_start: tokens[after].span.end,
                        char_end: tokens[close].span.start,
                    },
                    token_start: start,
                    header_end: close + 1,
                    span: Span::new(tokens[start].span.start, tokens[close].span.end),
                },
                close + 1,
            ))
        }
        _ => Err(LusaError::new(
            E_PARSE,
            Some(tokens[start].span),
            format!(
                "module `{name}` must end with `;` or a `{{ ... }}` body"
            ),
        )
        .help("write `mod name;` for a file module or `mod name { ... }` for an inline module; the older un-terminated `mod name` textual include is still accepted")),
    }
}

fn parse_use(tokens: &[Token], start: usize, public: bool) -> LResult<(UseDecl, usize)> {
    let mut j = start + 1;
    let mut path: Vec<String> = Vec::new();
    loop {
        match tokens.get(j).map(|t| &t.kind) {
            Some(TK::Name(name)) => {
                path.push(name.clone());
                j += 1;
            }
            _ => break,
        }
        if matches!(tokens.get(j).map(|t| &t.kind), Some(TK::ColonColon)) {
            j += 1;
            continue;
        }
        break;
    }
    if path.is_empty() {
        return Err(LusaError::new(
            E_PARSE,
            Some(tokens[start].span),
            "expected a path after `use`",
        ));
    }
    if matches!(tokens.get(j).map(|t| &t.kind), Some(TK::LBrace)) {
        return Err(LusaError::new(
            E_PARSE,
            Some(tokens[start].span),
            "grouped `use` imports are not supported yet",
        )
        .help("import each item with its own `use` declaration"));
    }

    let mut glob = false;
    if matches!(tokens.get(j).map(|t| &t.kind), Some(TK::Star)) {
        glob = true;
        j += 1;
    }
    let mut alias = if glob {
        String::new()
    } else {
        path.last().cloned().unwrap_or_default()
    };
    if is_name(
        tokens.get(j).unwrap_or(&tokens[tokens.len() - 1]),
        "as",
    ) {
        j += 1;
        match tokens.get(j).map(|t| &t.kind) {
            Some(TK::Name(name)) => {
                alias = name.clone();
                j += 1;
            }
            _ => {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(tokens[start].span),
                    "expected an alias name after `as`",
                ))
            }
        }
    }
    if glob {
        if alias.is_empty() {
            alias = "*".to_string();
        }
    }
    if alias.is_empty() {
        return Err(LusaError::new(
            E_PARSE,
            Some(tokens[start].span),
            "`use` path must name an item or provide an alias",
        ));
    }
    if matches!(tokens.get(j).map(|t| &t.kind), Some(TK::Semi)) {
        j += 1;
    }
    let end_span = tokens[j - 1].span;
    Ok((
        UseDecl {
            path,
            alias,
            glob,
            public,
            token_start: start,
            token_end: j,
            span: Span::new(tokens[start].span.start, end_span.end),
        },
        j,
    ))
}

fn update_scan_stack(tokens: &[Token], index: usize, stack: &mut Vec<Frame>) {
    match tokens[index].kind {
        TK::End => {
            stack.pop();
        }
        TK::Until => {
            if stack.last().map(|f| f.close) == Some(FrameClose::Until) {
                stack.pop();
            }
        }
        TK::Function => {
            let previous_is_async = index > 0 && matches!(tokens[index - 1].kind, TK::Async);
            if !previous_is_async {
                stack.push(Frame::end());
            }
        }
        TK::Async => {
            if is_async_function_header(tokens, index) {
                stack.push(Frame::end());
            }
        }
        TK::If | TK::Match => stack.push(Frame::end()),
        TK::For | TK::While => stack.push(Frame::awaiting_do()),
        TK::Do => {
            if let Some(frame) = stack.last_mut() {
                if frame.await_do && !frame.do_done {
                    frame.do_done = true;
                    return;
                }
            }
            stack.push(Frame::end());
        }
        TK::Unsafe => {
            if matches!(tokens.get(index + 1).map(|t| &t.kind), Some(TK::Do)) {
                stack.push(Frame::awaiting_do());
            } else {
                stack.push(Frame::end());
            }
        }
        TK::Repeat => stack.push(Frame::until()),
        TK::Struct => {
            let mut j = index + 1;
            if matches!(tokens.get(j).map(|t| &t.kind), Some(TK::Name(_))) {
                j += 1;
            }
            if !matches!(tokens.get(j).map(|t| &t.kind), Some(TK::LBrace)) {
                stack.push(Frame::end());
            }
        }
        _ => {}
    }
}

fn plain_decl_name(tokens: &[Token], index: usize) -> Option<usize> {
    let idx = index + 1;
    matches!(tokens.get(idx).map(|t| &t.kind), Some(TK::Name(_))).then_some(idx)
}

/// `function object:method(...)`, `function object.method(...)`, and
/// `function table::method(...)` attach a function to an existing expression
/// instead of declaring a new top-level binding.  The name after `function`
/// is therefore a reference that must be resolved like any other use.
fn is_function_target_sugar(tokens: &[Token], name_idx: usize) -> bool {
    matches!(
        tokens.get(name_idx + 1).map(|t| &t.kind),
        Some(TK::Colon) | Some(TK::Dot) | Some(TK::ColonColon)
    )
}

/// True when `index` points at a struct-literal/declaration field name.
fn colon_is_field_name(tokens: &[Token], index: usize, braces: usize) -> bool {
    if !matches!(tokens.get(index + 1).map(|t| &t.kind), Some(TK::Colon)) {
        return false;
    }
    if index == 0 {
        return false;
    }
    let prev = &tokens[index - 1].kind;
    if braces > 0 && matches!(prev, TK::LBrace | TK::Comma | TK::Semi) {
        return true;
    }
    // Bare struct form: `struct Point x: i32, y: i32 end`.
    matches!(prev, TK::Struct) || (index >= 2 && matches!(tokens[index - 2].kind, TK::Struct))
}

/// True when the colon immediately before `index` separates a struct-literal
/// field from its value (`Point{x: decode(raw)}`) instead of introducing a
/// method name (`object:method(args)`).
fn colon_is_field_separator(tokens: &[Token], index: usize, braces: usize) -> bool {
    if braces == 0 || index < 2 {
        return false;
    }
    if !matches!(tokens[index - 2].kind, TK::Name(_)) {
        return false;
    }
    matches!(
        tokens.get(index - 3).map(|t| &t.kind),
        Some(TK::LBrace) | Some(TK::Comma) | Some(TK::Semi)
    )
}

fn async_decl_name(tokens: &[Token], index: usize) -> Option<usize> {
    match tokens.get(index + 1).map(|t| &t.kind) {
        Some(TK::Function) => plain_decl_name(tokens, index + 1),
        Some(TK::Name(name)) if name == "fn" => plain_decl_name(tokens, index + 1),
        _ => None,
    }
}

fn is_async_function_header(tokens: &[Token], index: usize) -> bool {
    match tokens.get(index + 1).map(|t| &t.kind) {
        Some(TK::Function) => true,
        Some(TK::Name(name)) if name == "fn" => true,
        _ => false,
    }
}

fn local_targets(tokens: &[Token], index: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut j = index + 1;
    let mut want_target = true;
    let mut in_annotation = false;
    let mut angle_depth = 0usize;
    let mut bracket_depth = 0usize;
    while j < tokens.len() {
        match &tokens[j].kind {
            TK::Assign if angle_depth == 0 && bracket_depth == 0 => break,
            TK::Comma if angle_depth == 0 && bracket_depth == 0 => {
                if in_annotation {
                    in_annotation = false;
                }
                want_target = true;
                j += 1;
            }
            TK::Name(_) if want_target && angle_depth == 0 && bracket_depth == 0 => {
                out.push(j);
                want_target = false;
                in_annotation = false;
                j += 1;
            }
            TK::Colon if !want_target && angle_depth == 0 && bracket_depth == 0 => {
                in_annotation = true;
                j += 1;
            }
            TK::Lt if in_annotation => {
                angle_depth += 1;
                j += 1;
            }
            TK::Gt if in_annotation && angle_depth > 0 => {
                angle_depth -= 1;
                j += 1;
            }
            TK::LBracket if in_annotation => {
                bracket_depth += 1;
                j += 1;
            }
            TK::RBracket if in_annotation && bracket_depth > 0 => {
                bracket_depth -= 1;
                j += 1;
            }
            TK::Eof | TK::End | TK::Else | TK::Elseif | TK::Until => break,
            _ => {
                j += 1;
            }
        }
    }
    out
}

fn for_targets(tokens: &[Token], index: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut j = index + 1;
    while j < tokens.len() {
        match &tokens[j].kind {
            TK::Name(_) => {
                out.push(j);
                j += 1;
            }
            TK::Comma => {
                j += 1;
            }
            TK::Colon => {
                // Skip a loop-variable type annotation.
                j += 1;
                let mut angle = 0usize;
                while j < tokens.len() {
                    match &tokens[j].kind {
                        TK::Lt => angle += 1,
                        TK::Gt if angle > 0 => angle -= 1,
                        TK::Comma | TK::Assign | TK::In if angle == 0 => break,
                        _ => {}
                    }
                    j += 1;
                }
            }
            TK::In | TK::Assign => break,
            _ => break,
        }
    }
    out
}

/// Detects a top-level declaration assignment (`name = value` or
/// `a, b = value`) without mistaking type annotations or table/environment
/// entries for new globals.
fn declaration_assignment_targets(tokens: &[Token], index: usize) -> Option<Vec<usize>> {
    if index > 0 {
        let allowed = matches!(
            &tokens[index - 1].kind,
            TK::Name(_)
                | TK::Num(_)
                | TK::Int(_)
                | TK::Str(_)
                | TK::True
                | TK::False
                | TK::Nil
                | TK::RParen
                | TK::RBracket
                | TK::RBrace
                | TK::End
                | TK::Until
                | TK::Semi
        );
        if !allowed {
            return None;
        }
    }
    assignment_targets(tokens, index)
}

fn assignment_targets(tokens: &[Token], index: usize) -> Option<Vec<usize>> {
    let mut targets = Vec::new();
    let mut j = index;
    loop {
        match tokens.get(j).map(|t| &t.kind) {
            Some(TK::Name(_)) => {
                targets.push(j);
                j += 1;
            }
            _ => return None,
        }
        match tokens.get(j).map(|t| &t.kind) {
            Some(TK::Comma) => {
                j += 1;
            }
            Some(TK::Assign) => return Some(targets),
            _ => return None,
        }
    }
}

fn function_params(tokens: &[Token], function_index: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut j = function_index + 1;
    while j < tokens.len() && !matches!(tokens[j].kind, TK::LParen) {
        if matches!(
            tokens[j].kind,
            TK::End | TK::Eof | TK::Assign | TK::Semi
        ) {
            return out;
        }
        j += 1;
    }
    if j >= tokens.len() {
        return out;
    }
    let mut depth = 0usize;
    let mut angle = 0usize;
    let mut bracket = 0usize;
    let mut want_name = true;
    while j < tokens.len() {
        match &tokens[j].kind {
            TK::LParen => {
                depth += 1;
            }
            TK::RParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            TK::Comma if depth == 1 && angle == 0 && bracket == 0 => want_name = true,
            TK::Colon if depth == 1 && angle == 0 && bracket == 0 => want_name = false,
            TK::Lt if depth == 1 => angle += 1,
            TK::Gt if depth == 1 && angle > 0 => angle -= 1,
            TK::LBracket if depth == 1 => bracket += 1,
            TK::RBracket if depth == 1 && bracket > 0 => bracket -= 1,
            TK::Name(_) if depth == 1 && angle == 0 && bracket == 0 && want_name => {
                out.push(j);
                want_name = false;
            }
            _ => {}
        }
        j += 1;
    }
    out
}

fn path_segments(tokens: &[Token], start: usize) -> (Vec<String>, usize) {
    let mut segments = Vec::new();
    let mut j = start;
    loop {
        match tokens.get(j).map(|t| &t.kind) {
            Some(TK::Name(name)) => segments.push(name.clone()),
            _ => break,
        }
        if matches!(tokens.get(j + 1).map(|t| &t.kind), Some(TK::ColonColon))
            && matches!(tokens.get(j + 2).map(|t| &t.kind), Some(TK::Name(_)))
        {
            j += 2;
        } else {
            j += 1;
            break;
        }
    }
    (segments, j)
}

fn resolve_module_file(name: &str, base: &Path) -> Option<PathBuf> {
    // A `name/` directory takes precedence over a sibling `name.lusa`, so a
    // module can grow into a directory without silently changing identity.
    let nested = base.join(name).join("mod.lusa");
    if nested.is_file() {
        return Some(nested);
    }
    let direct = base.join(format!("{name}.lusa"));
    if direct.is_file() {
        return Some(direct);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mangles_nested_paths_without_collisions() {
        assert_eq!(mangle(&[], "read"), "read");
        assert_eq!(mangle(&["sensor".into()], "read"), "__lusa_m_sensor__read");
        assert_eq!(
            mangle(&["firmware".into(), "gpio".into()], "read"),
            "__lusa_m_firmware_gpio__read"
        );
    }

    #[test]
    fn inline_modules_resolve_public_items() {
        let out = expand_modules(
            r#"
mod math {
    pub function double(x: i32): i32
        return x * 2
    end

    function secret(): i32
        return 1
    end
}

print(math::double(21))
"#,
            None,
            None,
        )
        .expect("modules");
        assert!(out.contains("function __lusa_m_math__double"));
        assert!(out.contains("__lusa_m_math__double(21)"));
        assert!(!out.contains("pub "));
        assert!(!out.contains("mod math"));
    }

    #[test]
    fn file_modules_hoist_structs_before_children() {
        let dir = std::env::temp_dir().join(format!("lusa-mod-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("geom").join("shapes")).unwrap();
        std::fs::write(
            dir.join("geom").join("mod.lusa"),
            r#"
pub mod shapes;

pub struct Point {
    x: i32,
    y: i32
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("geom").join("shapes").join("mod.lusa"),
            r#"
use super::Point;

pub function unit(): i32
    local p = Point{x: 1, y: 2}
    return p.x + p.y
end
"#,
        )
        .unwrap();
        let src = "mod geom;\nprint(geom::shapes::unit())\n";
        let out = expand_modules(src, Some(&dir), None).expect("modules");
        let struct_at = out.find("struct __lusa_m_geom__Point").expect("hoisted struct");
        let child_at = out.find("__lusa_m_geom_shapes__unit").expect("child item");
        assert!(struct_at < child_at, "struct must precede child module code");
        crate::parser::parse(&out).expect("expanded source parses");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pub_use_cannot_leak_private_items() {
        let err = expand_modules(
            r#"
mod core {
    function hidden(): i32
        return 1
    end

    pub use self::hidden as leaked;
}
"#,
            None,
            None,
        )
        .expect_err("re-export");
        assert_eq!(err.code, E_MODULE);
        assert!(err.msg.contains("re-exported"), "{}", err.msg);
    }

    #[test]
    fn private_items_are_rejected_from_outside() {
        let err = expand_modules(
            r#"
mod math {
    function secret(): i32
        return 1
    end
}

print(math::secret())
"#,
            None,
            None,
        )
        .expect_err("privacy");
        assert_eq!(err.code, E_MODULE);
        assert!(err.msg.contains("private"), "{}", err.msg);
    }
}
