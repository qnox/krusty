//! The name universe a source set's declarations bind against.
//!
//! Before any signature exists, every file's package, imports and type aliases decide which simple
//! names it can see and what each one binds to. Kotlin has no global simple-name index: a bare name
//! binds only through a default-import, an explicit import, or the file's own package, so this is
//! built per file and never widened afterwards. The module's own declared identities are collected
//! in the same pass, because a source declaration shadows a classpath type of the same simple name.

use super::*;

/// Everything the declaration walk needs in order to bind a written name, decided once for the
/// whole source set.
pub(in crate::resolve) struct SourceTypeUniverse {
    /// Type aliases declared per source file, in declaration order.
    pub(in crate::resolve) file_type_aliases: Vec<Vec<(String, Vec<String>, TypeRef)>>,
    /// Each file's package spelling, empty for the root package.
    pub(in crate::resolve) source_packages: Vec<String>,
    /// Each file's imports, normalized away from the two source forms.
    pub(in crate::resolve) source_imports: Vec<Vec<CompactSourceImport>>,
    /// Names visible to every file in the set.
    pub(in crate::resolve) class_names: ClassNames,
    /// Per-file view of [`Self::class_names`], extended with that file's own imports and alias
    /// expansions. Two files may import different aliases under the same spelling.
    pub(in crate::resolve) file_class_names: Vec<ClassNames>,
    /// Classifier identities this module declares. A source declaration shadows a classpath type
    /// of the same simple name, so only a duplicate among these is a conflict.
    pub(in crate::resolve) user_defined: std::collections::HashSet<TypeName>,
    /// Identities this module declares as a base class, for classifying a parenless supertype
    /// before `ModuleSymbols` can exist.
    pub(in crate::resolve) user_base_classes: std::collections::HashSet<TypeName>,
}

pub(in crate::resolve) fn source_type_universe(
    files: &[File],
    libraries: &dyn SemanticPlatform,
    diags: &mut DiagSink,
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
) -> SourceTypeUniverse {
    let file_type_aliases = compact_headers.map_or_else(
        || {
            files
                .iter()
                .map(|file| file.type_alias_fun.clone())
                .collect::<Vec<_>>()
        },
        |headers| {
            (0..headers.sources.len())
                .map(|file_index| {
                    let source = crate::fir::SourceFileId::from_raw(file_index as u32);
                    if headers.has_headers(source) {
                        streamed_pass_one_file_type_aliases(headers, source)
                            .expect("production file type aliases must have compact headers")
                    } else {
                        Vec::new()
                    }
                })
                .collect()
        },
    );
    let source_packages = compact_headers.map_or_else(
        || {
            files
                .iter()
                .map(|file| file.package.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        },
        |headers| {
            (0..headers.sources.len())
                .map(|source| {
                    compact_source_package_spelling(
                        headers,
                        crate::fir::SourceFileId::from_raw(source as u32),
                    )
                    .unwrap_or_default()
                })
                .collect()
        },
    );
    let source_imports = compact_headers.map_or_else(
        || {
            files
                .iter()
                .map(|file| {
                    file.import_paths
                        .iter()
                        .map(|import| CompactSourceImport {
                            visible_name: import.imported_name().unwrap_or_default().to_owned(),
                            path: import.path(),
                            wildcard: import.wildcard,
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        },
        |headers| {
            (0..headers.sources.len())
                .map(|source| {
                    compact_source_imports(
                        headers,
                        crate::fir::SourceFileId::from_raw(source as u32),
                    )
                    .unwrap_or_default()
                })
                .collect()
        },
    );
    let platform_default_imports = libraries.platform_default_import_packages();
    // Pass 1: every class simple-name -> internal name (no bodies, just the type universe). Nothing is
    // pre-seeded: every referenced type resolves through the file's imports and default packages below
    // (the import machinery), so a bare name binds ONLY to a default-import / imported / same-package class
    // — kotlinc semantics — never to an arbitrary classpath class. A classpath `typealias`
    // (`ArrayList` → `java/util/ArrayList`) resolves through the same probe: `resolve_type` returns the
    // alias's target.
    let mut class_names = ClassNames::new(std::rc::Rc::new(HashMap::new()));
    // A user-declared top-level class *shadows* any classpath/JDK type of the same simple name
    // (legal Kotlin — the JDK one would need an explicit import). Only a duplicate among the
    // user's own declarations is a conflict, so track which names the user has defined. The dedup
    // key is the package-qualified *internal* name, not the simple name: two classes sharing a
    // simple name in different packages (e.g. a test's root-package `EmptyContinuation` and the
    // injected `helpers.EmptyContinuation`) are distinct declarations, not a conflict.
    let mut user_defined: std::collections::HashSet<TypeName> = std::collections::HashSet::new();
    // Bootstrap class/interface identity for the same all-files pass. `ModuleSymbols` cannot exist
    // until this table is complete, but a parenless supertype still needs semantic classification:
    // another-file source base and a classpath base must produce the same `super_internal` shape.
    let mut user_base_classes: std::collections::HashSet<TypeName> =
        std::collections::HashSet::new();
    let mut user_aliases: std::collections::HashSet<TypeName> = std::collections::HashSet::new();
    if let Some(headers) = compact_headers {
        for stub in headers
            .stubs
            .iter()
            .filter(|stub| stub.kind == crate::fir::DeclarationKind::Classifier)
        {
            diags.set_file(stub.source.raw());
            let (source_name, internal) = compact_classifier_identity(headers, stub)
                .expect("a compact classifier must retain its lookup name and source package");
            if !user_defined.insert(internal) {
                diags.error(
                    stub.range,
                    format!("conflicting declarations: {source_name}"),
                );
            }
            if !stub.flags.has(crate::fir::DeclarationFlags::INTERFACE)
                && !stub.flags.has(crate::fir::DeclarationFlags::SINGLETON)
            {
                user_base_classes.insert(internal);
            }
            class_names.insert_name(source_name, internal);
        }
        for (source, aliases) in file_type_aliases.iter().enumerate() {
            let package = headers
                .sources
                .get(crate::fir::SourceFileId::from_raw(source as u32))
                .map(|file| file.package)
                .unwrap_or(TypeName::ROOT);
            for (alias, _, target) in aliases {
                if target.name != "<fun>" && !target.name.is_empty() {
                    user_aliases.insert(crate::types::type_name_child(package, alias));
                }
            }
        }
    } else {
        for (i, file) in files.iter().enumerate() {
            diags.set_file(i as u32);
            for &d in &file.decls {
                if let Decl::Class(c) = file.decl(d) {
                    let internal = class_internal(file, &c.name);
                    if !user_defined.insert(type_name(&internal)) {
                        diags.error(c.span, format!("conflicting declarations: {}", c.name));
                    }
                    if !c.is_interface() && !c.is_singleton() {
                        user_base_classes.insert(type_name(&internal));
                    }
                    class_names.insert(c.name.clone(), internal.clone());
                }
            }
            let package = file.package.as_deref().unwrap_or("").replace('.', "/");
            for (alias, _) in &file.type_aliases {
                let qualified = if package.is_empty() {
                    alias.clone()
                } else {
                    format!("{package}/{alias}")
                };
                user_aliases.insert(type_name(&qualified));
            }
        }
    }
    // (The library set's `seed` already merged the intrinsic Kotlin built-in → target class mapping,
    // e.g. the ported `JavaToKotlinClassMap`, beneath any classpath/user declarations.)

    // Resolve every referenced simple name through the file's imports — an explicit import, a
    // wildcard/default-import package that actually provides the type, or a dotted FQ. This is Kotlin's
    // import-driven name resolution (there is no global simple-name index): a bare name binds ONLY to a
    // default-import / imported / same-package class, verified to exist via `resolve_type`. Runs BEFORE
    // alias expansion so a user `typealias A = Foo` (Foo a classpath type) finds `Foo` already resolved.
    let imports_by_file = {
        let mut consensus: HashMap<String, Option<TypeName>> = HashMap::new();
        // A classpath `typealias` resolves to its TARGET classifier above. When the target takes a
        // different argument list than the alias declares (`Lens<S, A>` = `PLens<S, S, A, A>`), a use
        // site must substitute into the alias's expansion instead of pasting its arguments onto the
        // target — so record the template under the spelling that named the alias.
        let mut expansion_consensus: HashMap<String, Option<crate::libraries::AliasExpansion>> =
            HashMap::new();
        let source_count = compact_headers.map_or(files.len(), |headers| headers.sources.len());
        let mut imports_by_file = Vec::with_capacity(source_count);
        for file_index in 0..source_count {
            let imap = source_imports[file_index]
                .iter()
                .filter(|import| !import.wildcard)
                .map(|import| (import.visible_name.clone(), import.path.replace('.', "/")))
                .collect::<HashMap<_, _>>();
            let source = BootstrapSymbolSource {
                declarations: &user_defined,
                aliases: &user_aliases,
                libraries,
            };
            let own = type_name(&source_packages[file_index].replace('.', "/"));
            let explicit_star = source_imports[file_index]
                .iter()
                .filter(|import| import.wildcard)
                .filter_map(
                    |import| match qualifier_path(&import.path, &source, None).ok()? {
                        ResolvedQualifier::Package(package) => Some(package),
                        ResolvedQualifier::Classifier(classifier) => Some(classifier),
                        ResolvedQualifier::Value => None,
                    },
                )
                .collect::<Vec<_>>();
            let kotlin_defaults = KOTLIN_DEFAULT_IMPORT_PACKAGES
                .iter()
                .map(|package| type_name(&package.replace('.', "/")))
                .collect();
            let platform_defaults = platform_default_imports
                .iter()
                .map(|package| type_name(&package.replace('.', "/")))
                .collect();
            let levels = [vec![own], explicit_star, kotlin_defaults, platform_defaults];
            let mut file_imports = HashMap::new();
            let mut file_expansions: HashMap<String, crate::libraries::AliasExpansion> =
                HashMap::new();
            let mut file_unresolved = HashMap::new();
            // Candidate simple names: every type referenced in the file (so a WILDCARD import can supply
            // it) plus the explicit-import names themselves.
            let mut names = std::collections::HashSet::new();
            if let Some(headers) = compact_headers {
                collect_streamed_header_type_names(
                    headers,
                    crate::fir::SourceFileId::from_raw(file_index as u32),
                    &mut names,
                );
            } else {
                collect_file_type_names(&files[file_index], &mut names);
            }
            names.extend(imap.keys().cloned());
            for name in names {
                // `collect_file_type_names` deliberately combines ordinary `TypeRef` spellings with
                // supertype/delegation fields that the parser stores in JVM-like slash form. Normalize
                // that representation ONCE before selecting any provider: explicit imports,
                // same-package source declarations, module classifiers, and library classifiers must
                // all see the same Kotlin source spelling. Keep `name` unchanged below as the binding
                // key because later consumers query `class_names` with the original AST spelling.
                let source_name = name.replace('/', ".");
                let (full, unresolved) = if let Some(path) = imap.get(&source_name) {
                    // An explicit import is the selected root. A missing segment is final: do not
                    // reopen same-package, star-import, or absolute-package interpretations.
                    match classifier_path(path, &source, None) {
                        Ok(classifier) => (Some(classifier), None),
                        Err(_) => (
                            None,
                            Some(
                                source_name
                                    .split('.')
                                    .next()
                                    .unwrap_or(&source_name)
                                    .to_string(),
                            ),
                        ),
                    }
                } else if source_name.contains('.') {
                    let root = source_name.split('.').next().unwrap_or_default();
                    match classifier_from_imports(root, &imap, &levels, &source) {
                        InheritedNestedClassifier::Ambiguous => (None, Some(root.to_string())),
                        scoped_root => {
                            match classifier_path(&source_name, &source, scoped_root.found()) {
                                Ok(classifier) => (Some(classifier), None),
                                Err(QualifierError::UnresolvedSegment { name, .. })
                                | Err(QualifierError::AmbiguousRoot { name, .. }) => {
                                    (None, Some(name))
                                }
                                Err(QualifierError::NotANameChain { .. }) => {
                                    (None, Some(root.to_string()))
                                }
                            }
                        }
                    }
                } else {
                    let classifier =
                        classifier_from_imports(&source_name, &imap, &levels, &source).found();
                    let unresolved = classifier.is_none().then(|| source_name.clone());
                    (classifier, unresolved)
                };
                let full = full.map(|full| libraries.canonical_source_type_name(full));
                if let Some(segment) = unresolved {
                    file_unresolved.insert(name.clone(), segment);
                }
                match consensus.get_mut(&name) {
                    Some(previous) if *previous != full => *previous = None,
                    Some(_) => {}
                    None => {
                        consensus.insert(name.clone(), full);
                    }
                }
                // Probe the alias's OWN fully-qualified name the way the classifier itself was
                // resolved: an explicit import is FINAL (a miss does not reopen star imports), a
                // fully-qualified spelling names the alias directly, and star/default levels are
                // consulted in precedence order rather than flattened. Whether the template APPLIES
                // is decided at the use site, against the classifier that actually resolved.
                let expansion = if let Some(path) = imap.get(&source_name) {
                    Some(crate::types::type_name(&path.replace('.', "/")))
                        .and_then(|identity| libraries.type_alias_expansion(identity))
                } else if source_name.contains('.') {
                    Some(crate::types::type_name(&source_name.replace('.', "/")))
                        .and_then(|identity| libraries.type_alias_expansion(identity))
                } else {
                    // Alias metadata follows the SAME winning classifier level. A higher-precedence
                    // class/source alias is final even when it has no classpath expansion; never skip
                    // it and borrow a same-named alias template from a lower default import.
                    levels
                        .iter()
                        .find_map(|level| {
                            let candidates = level
                                .iter()
                                .filter_map(|&package| {
                                    classifier_identity(
                                        &source,
                                        crate::symbol_source::SymbolNamespace::Package(package),
                                        &source_name,
                                    )
                                    .map(|target| (package, target))
                                })
                                .collect::<Vec<_>>();
                            if candidates.is_empty() {
                                return None;
                            }
                            let mut selected: Option<crate::libraries::AliasExpansion> = None;
                            for (package, target) in candidates {
                                if full != Some(libraries.canonical_source_type_name(target)) {
                                    return Some(None);
                                }
                                let expansion = {
                                    let identity =
                                        crate::types::type_name_child(package, &source_name);
                                    libraries.type_alias_expansion(identity)
                                };
                                match (&selected, expansion) {
                                    (None, Some(expansion)) => selected = Some(expansion),
                                    (Some(previous), Some(expansion)) if *previous == expansion => {
                                    }
                                    // A real classifier or a distinct alias declaration in the winning
                                    // level means there is no single expansion identity to record.
                                    _ => return Some(None),
                                }
                            }
                            Some(selected)
                        })
                        .flatten()
                }
                .filter(|expansion| full == Some(expansion.target));
                if let Some(expansion) = expansion {
                    // Module-wide agreement mirrors the classifier `consensus` beside it: a
                    // spelling two files bind to DIFFERENT aliases has no module-level answer, and
                    // each file's own map below still expands it correctly.
                    match expansion_consensus.get_mut(&name) {
                        Some(previous) if previous.as_ref() != Some(&expansion) => *previous = None,
                        Some(_) => {}
                        None => {
                            expansion_consensus.insert(name.clone(), Some(expansion.clone()));
                        }
                    }
                    file_expansions.insert(name.clone(), expansion);
                }
                if let Some(full) = full {
                    file_imports.insert(name, full);
                }
            }
            imports_by_file.push((file_imports, file_expansions, file_unresolved));
        }
        for (simple, full) in consensus {
            if let Some(full) = full {
                if let Some(expansion) = expansion_consensus.remove(&simple).flatten() {
                    class_names.insert_alias_expansion(simple.clone(), expansion);
                }
                class_names.insert_name(simple, full);
            }
        }
        imports_by_file
    };

    // Expand the input files' USER type aliases into class_names (classpath aliases already resolved
    // through the import pass, via `resolve_type`'s alias redirect).
    // `typealias A = B` where B is a user/classpath/import-resolved class → A resolves to the same internal.
    // `typealias A = Primitive` → A maps to `"__ty/<PrimName>"` (decoded in ty_of_ref).
    // `typealias A = java.lang.Foo` → A resolves to the JVM internal name `java/lang/Foo`.
    // Multiple passes handle chains: A = B, B = C.
    let mut alias_map: HashMap<String, String> = HashMap::new();
    // The declaring package is known HERE and nowhere downstream — `alias_map` is spelling-keyed —
    // so the alias's qualified identity is captured alongside, for `Type.abbreviated_type`.
    let mut source_alias_identities: Vec<(String, TypeName)> = Vec::new();
    for file_index in 0..source_packages.len() {
        let alias_package = &source_packages[file_index];
        for (alias, _, target) in &file_type_aliases[file_index] {
            if target.name == "<fun>" || target.name.is_empty() {
                continue;
            }
            alias_map.insert(alias.clone(), target.name.clone());
            // A FULLY QUALIFIED spelling (`app.Cargo`) reaches name resolution intact — the parse
            // seam expands only simple spellings — and qualified resolution answers it with the
            // alias's OWN declaration, because an alias declaration is a name a package contains.
            // That is right for resolving the name and wrong for the type it denotes: an alias is a
            // resolution edge, not a classifier, so the dotted spelling has to expand to the target
            // exactly as the bare one does. Without this the emitted descriptor named `app/Cargo`,
            // a class nothing declares or emits.
            if !alias_package.is_empty() {
                alias_map.insert(format!("{alias_package}.{alias}"), target.name.clone());
            }
        }
        // Identities come from `type_alias_fun`, not `type_aliases`: the latter records only aliases
        // with a CLASSIFIER target, so a function-type alias (`typealias Handler<T> = (T) -> String`,
        // whose target has no class name) is absent from it — and it abbreviates like any other.
        let package = &source_packages[file_index];
        let internal = package.replace('.', "/");
        for (alias, _, _) in &file_type_aliases[file_index] {
            let qualified = if internal.is_empty() {
                alias.clone()
            } else {
                format!("{internal}/{alias}")
            };
            let identity = crate::types::type_name(&qualified);
            source_alias_identities.push((alias.clone(), identity));
            // A use site may spell the alias fully qualified (`app.Cargo`); kotlinc abbreviates it
            // identically, so the dotted spelling resolves to the same declaration.
            if !package.is_empty() {
                source_alias_identities.push((format!("{package}.{alias}"), identity));
            }
        }
    }
    for (spelling, identity) in &source_alias_identities {
        class_names.insert_source_alias_identity(spelling.clone(), *identity);
    }
    expand_type_aliases(&mut class_names, &alias_map);
    let class_names = class_names.into_shared();
    let file_class_names: Vec<ClassNames> = imports_by_file
        .into_iter()
        .map(|(imports, expansions, unresolved)| {
            let mut names = class_names.clone();
            for (simple, full) in imports {
                names.insert_name(simple, full);
            }
            // Alias templates are per FILE: two files may import different aliases under the same
            // spelling, and each must expand through its own.
            for (simple, expansion) in expansions {
                names.insert_alias_expansion(simple, expansion);
            }
            for (spelling, segment) in unresolved {
                names.insert_unresolved_segment(spelling, segment);
            }
            expand_type_aliases(&mut names, &alias_map);
            names
        })
        .collect();
    SourceTypeUniverse {
        file_type_aliases,
        source_packages,
        source_imports,
        class_names,
        file_class_names,
        user_defined,
        user_base_classes,
    }
}
