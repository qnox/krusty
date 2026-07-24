//! Source declaration locations used while reducing navigation to compact file/span pairs.

use std::collections::{HashMap, HashSet};

use krusty::ast::{Decl, File};
use krusty::diag::{DiagSink, Span};
use krusty::frontend::{
    lex_name_tokens, FrontendNameToken, FrontendNameTokenKind, FrontendSymbols,
};
use krusty::types::Ty;

use super::FileAnalysis;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefinitionTarget {
    pub file: u32,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefinitionOccurrence {
    pub span: Span,
    pub target: DefinitionTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MemberKind {
    InstanceValue,
    InstanceFunction,
    StaticValue,
    StaticFunction,
}

struct MemberDefinition {
    kind: MemberKind,
    params: Option<Vec<Ty>>,
    target: DefinitionTarget,
}

struct ExtensionDefinition {
    package: String,
    target: DefinitionTarget,
}

#[derive(Default)]
pub struct DefinitionSymbols {
    classes: HashMap<String, DefinitionTarget>,
    declarations: HashMap<(u32, u32), DefinitionTarget>,
    members: HashMap<(String, String), Vec<MemberDefinition>>,
    member_parents: HashMap<String, Vec<String>>,
    object_owners: HashSet<String>,
    extensions: HashMap<(Ty, String), Vec<ExtensionDefinition>>,
    top_levels: HashMap<(String, String, MemberKind), Vec<DefinitionTarget>>,
    self_targets: Vec<Vec<DefinitionTarget>>,
}

impl DefinitionSymbols {
    pub fn from_source_set(
        sources: &[&str],
        files: &[FileAnalysis],
        symbols: &FrontendSymbols,
    ) -> Self {
        let mut definitions = Self::default();
        for (file_index, (source, analysis)) in sources.iter().copied().zip(files).enumerate() {
            let mut diagnostics = DiagSink::new();
            let tokens = lex_name_tokens(source, &mut diagnostics);
            let package = package_key(&analysis.file);
            for &declaration in &analysis.file.decls {
                match analysis.file.decl(declaration) {
                    Decl::Class(class) => {
                        let Some(span) =
                            declaration_name_span(&tokens, source, class.span, &class.name, false)
                        else {
                            continue;
                        };
                        let target = DefinitionTarget {
                            file: file_index as u32,
                            span,
                        };
                        definitions
                            .declarations
                            .insert((file_index as u32, declaration.0), target);
                        let owner = qualified_name(&package, &class.name);
                        definitions.classes.insert(owner.clone(), target);
                        for parameter in &class.props {
                            if parameter.is_property {
                                definitions
                                    .members
                                    .entry((owner.clone(), parameter.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::InstanceValue,
                                        params: Some(Vec::new()),
                                        target: DefinitionTarget {
                                            file: file_index as u32,
                                            span: definition_name_span(source, parameter.span),
                                        },
                                    });
                            }
                        }
                        for property in &class.body_props {
                            if let Some(span) = declaration_name_span(
                                &tokens,
                                source,
                                property.span,
                                &property.name,
                                false,
                            ) {
                                definitions
                                    .members
                                    .entry((owner.clone(), property.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::InstanceValue,
                                        params: Some(Vec::new()),
                                        target: DefinitionTarget {
                                            file: file_index as u32,
                                            span,
                                        },
                                    });
                            }
                        }
                        let class_symbols = symbols.class_by_internal(&owner);
                        if let Some(class_symbols) = class_symbols {
                            if class_symbols.is_object {
                                definitions.object_owners.insert(owner.clone());
                            }
                            let mut parents = class_symbols
                                .interfaces
                                .iter_ids()
                                .map(|parent| parent.render())
                                .collect::<Vec<_>>();
                            if let Some(parent) = class_symbols.super_internal {
                                parents.push(parent.render());
                            }
                            definitions.member_parents.insert(owner.clone(), parents);
                        }
                        let mut method_ordinals = HashMap::<String, usize>::new();
                        for function in &class.methods {
                            if let Some(span) = declaration_name_span(
                                &tokens,
                                source,
                                function.span,
                                &function.name,
                                false,
                            ) {
                                definitions
                                    .members
                                    .entry((owner.clone(), function.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::InstanceFunction,
                                        params: class_symbols.and_then(|class_symbols| {
                                            let ordinal = method_ordinals
                                                .entry(function.name.clone())
                                                .or_default();
                                            let params = class_symbols
                                                .methods_named(&function.name)
                                                .get(*ordinal)
                                                .map(|signature| signature.params.clone());
                                            *ordinal += 1;
                                            params
                                        }),
                                        target: DefinitionTarget {
                                            file: file_index as u32,
                                            span,
                                        },
                                    });
                            }
                        }
                        for function in &class.companion_methods {
                            if let Some(span) = declaration_name_span(
                                &tokens,
                                source,
                                function.span,
                                &function.name,
                                false,
                            ) {
                                definitions
                                    .members
                                    .entry((owner.clone(), function.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::StaticFunction,
                                        params: class_symbols
                                            .and_then(|class_symbols| {
                                                class_symbols.static_methods.get(&function.name)
                                            })
                                            .map(|signature| signature.params.clone()),
                                        target: DefinitionTarget {
                                            file: file_index as u32,
                                            span,
                                        },
                                    });
                            }
                        }
                        for property in &class.companion_props {
                            if let Some(span) = declaration_name_span(
                                &tokens,
                                source,
                                property.span,
                                &property.name,
                                false,
                            ) {
                                definitions
                                    .members
                                    .entry((owner.clone(), property.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::StaticValue,
                                        params: Some(Vec::new()),
                                        target: DefinitionTarget {
                                            file: file_index as u32,
                                            span,
                                        },
                                    });
                            }
                        }
                        for entry in &class.enum_entries {
                            definitions
                                .members
                                .entry((owner.clone(), entry.name.clone()))
                                .or_default()
                                .push(MemberDefinition {
                                    kind: MemberKind::StaticValue,
                                    params: None,
                                    target: DefinitionTarget {
                                        file: file_index as u32,
                                        span: definition_name_span(source, entry.span),
                                    },
                                });
                        }
                    }
                    Decl::Fun(function) => {
                        if let Some(span) = declaration_name_span(
                            &tokens,
                            source,
                            function.span,
                            &function.name,
                            false,
                        ) {
                            let target = DefinitionTarget {
                                file: file_index as u32,
                                span,
                            };
                            if function.receiver.is_none() {
                                definitions
                                    .top_levels
                                    .entry((
                                        package.clone(),
                                        function.name.clone(),
                                        MemberKind::StaticFunction,
                                    ))
                                    .or_default()
                                    .push(target);
                            }
                            definitions
                                .declarations
                                .insert((file_index as u32, declaration.0), target);
                        }
                    }
                    Decl::Property(property) => {
                        if let Some(span) = declaration_name_span(
                            &tokens,
                            source,
                            property.span,
                            &property.name,
                            false,
                        ) {
                            let target = DefinitionTarget {
                                file: file_index as u32,
                                span,
                            };
                            if property.receiver.is_none() {
                                definitions
                                    .top_levels
                                    .entry((
                                        package.clone(),
                                        property.name.clone(),
                                        MemberKind::StaticValue,
                                    ))
                                    .or_default()
                                    .push(target);
                            }
                            definitions
                                .declarations
                                .insert((file_index as u32, declaration.0), target);
                        }
                    }
                }
            }
        }
        for ((receiver, name), signature) in &symbols.ext_props {
            let Some(target) =
                definitions.declaration_target(signature.source.0, signature.source.1)
            else {
                continue;
            };
            let package = files
                .get(signature.source.0 as usize)
                .map(|file| package_key(&file.file))
                .unwrap_or_default();
            definitions
                .extensions
                .entry((*receiver, name.clone()))
                .or_default()
                .push(ExtensionDefinition { package, target });
        }
        let mut self_targets = vec![Vec::new(); files.len()];
        for target in definitions.declarations.values().copied().chain(
            definitions
                .members
                .values()
                .flatten()
                .map(|definition| definition.target),
        ) {
            if let Some(targets) = self_targets.get_mut(target.file as usize) {
                targets.push(target);
            }
        }
        for targets in &mut self_targets {
            targets.sort_unstable_by_key(|target| (target.span.lo, target.span.hi));
            targets.dedup();
        }
        definitions.self_targets = self_targets;
        definitions
    }

    pub(crate) fn class_target(&self, file: &File, name: &str) -> Option<DefinitionTarget> {
        self.class_owner(file, name)
            .and_then(|owner| self.classes.get(&owner))
            .copied()
    }

    pub(crate) fn declaration_target(
        &self,
        file: u32,
        declaration: u32,
    ) -> Option<DefinitionTarget> {
        self.declarations.get(&(file, declaration)).copied()
    }

    pub(crate) fn file_targets(&self, file: u32) -> &[DefinitionTarget] {
        self.self_targets
            .get(file as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn is_file_target(&self, target: DefinitionTarget) -> bool {
        self.file_targets(target.file)
            .binary_search_by_key(&(target.span.lo, target.span.hi), |candidate| {
                (candidate.span.lo, candidate.span.hi)
            })
            .is_ok()
    }

    pub(crate) fn class_owner(&self, file: &File, name: &str) -> Option<String> {
        if name.contains('.') {
            let owner = name.replace('.', "/");
            return self.classes.contains_key(&owner).then_some(owner);
        }
        let package = package_key(file);
        let local = qualified_name(&package, name);
        if self.classes.contains_key(&local) {
            return Some(local);
        }
        let mut explicit_owners = file
            .imports
            .iter()
            .filter(|import| !import.ends_with(".*") && import.rsplit('.').next() == Some(name))
            .map(|import| import.replace('.', "/"))
            .collect::<Vec<_>>();
        explicit_owners.sort_unstable();
        explicit_owners.dedup();
        if !explicit_owners.is_empty() {
            return match explicit_owners.as_slice() {
                [owner] if self.classes.contains_key(owner) => Some(owner.clone()),
                _ => None,
            };
        }
        let mut wildcard_owners = file
            .imports
            .iter()
            .filter_map(|import| import.strip_suffix(".*"))
            .map(|package| qualified_name(&package.replace('.', "/"), name))
            .filter(|owner| self.classes.contains_key(owner))
            .collect::<Vec<_>>();
        wildcard_owners.sort_unstable();
        wildcard_owners.dedup();
        match wildcard_owners.as_slice() {
            [owner] => Some(owner.clone()),
            _ => None,
        }
    }

    pub(crate) fn is_object_owner(&self, owner: &str) -> bool {
        self.object_owners.contains(owner)
    }

    pub(crate) fn member_target(
        &self,
        owner: &str,
        name: &str,
        kind: MemberKind,
        params: &[Ty],
    ) -> Option<DefinitionTarget> {
        self.members
            .get(&(owner.to_owned(), name.to_owned()))
            .and_then(|definitions| {
                definitions.iter().find(|definition| {
                    definition.kind == kind && definition.params.as_deref() == Some(params)
                })
            })
            .map(|definition| definition.target)
    }

    pub(crate) fn extension_value_target(
        &self,
        receiver: Ty,
        name: &str,
        file: &File,
    ) -> Option<DefinitionTarget> {
        for candidate in receiver.erased_recv_candidates() {
            let Some(definitions) = self.extensions.get(&(candidate, name.to_owned())) else {
                continue;
            };
            let mut matches = definitions
                .iter()
                .filter(|definition| extension_is_in_scope(file, name, &definition.package));
            if let Some(target) = matches.next().map(|definition| definition.target) {
                return matches.next().is_none().then_some(target);
            }
        }
        None
    }

    pub(crate) fn member_targets(
        &self,
        owner: &str,
        name: &str,
        kind: MemberKind,
    ) -> Vec<DefinitionTarget> {
        let mut targets = Vec::new();
        self.collect_member_targets(owner, name, kind, &mut HashSet::new(), &mut targets);
        targets
    }

    fn collect_member_targets(
        &self,
        owner: &str,
        name: &str,
        kind: MemberKind,
        seen: &mut HashSet<String>,
        targets: &mut Vec<DefinitionTarget>,
    ) -> bool {
        if !seen.insert(owner.to_owned()) {
            return false;
        }
        if let Some(definitions) = self.members.get(&(owner.to_owned(), name.to_owned())) {
            targets.extend(
                definitions
                    .iter()
                    .filter(|definition| definition.kind == kind)
                    .map(|definition| definition.target),
            );
            if !targets.is_empty() {
                return true;
            }
        }
        if matches!(kind, MemberKind::StaticValue | MemberKind::StaticFunction) {
            return false;
        }
        self.member_parents.get(owner).is_some_and(|parents| {
            parents
                .iter()
                .any(|parent| self.collect_member_targets(parent, name, kind, seen, targets))
        })
    }

    pub(crate) fn top_level_targets(
        &self,
        file: &File,
        name: &str,
        kind: MemberKind,
    ) -> Vec<DefinitionTarget> {
        let package = package_key(file);
        if let Some(targets) = self.top_levels.get(&(package, name.to_owned(), kind)) {
            return targets.clone();
        }
        let mut explicit_packages = Vec::new();
        for import in &file.imports {
            if !import.ends_with(".*") && import.rsplit('.').next() == Some(name) {
                let mut components = import.rsplitn(2, '.');
                let _ = components.next();
                explicit_packages.push(components.next().unwrap_or_default().replace('.', "/"));
            }
        }
        explicit_packages.sort_unstable();
        explicit_packages.dedup();
        if !explicit_packages.is_empty() {
            return match explicit_packages.as_slice() {
                [package] => self
                    .top_levels
                    .get(&(package.clone(), name.to_owned(), kind))
                    .cloned()
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
        }

        let mut wildcard_packages = file
            .imports
            .iter()
            .filter_map(|import| import.strip_suffix(".*"))
            .map(|package| package.replace('.', "/"))
            .filter(|package| {
                self.top_levels
                    .contains_key(&(package.clone(), name.to_owned(), kind))
            })
            .collect::<Vec<_>>();
        wildcard_packages.sort_unstable();
        wildcard_packages.dedup();
        match wildcard_packages.as_slice() {
            [package] => self
                .top_levels
                .get(&(package.clone(), name.to_owned(), kind))
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

fn package_key(file: &File) -> String {
    file.package
        .as_deref()
        .unwrap_or_default()
        .replace('.', "/")
}

fn extension_is_in_scope(file: &File, name: &str, package: &str) -> bool {
    if package_key(file) == package {
        return true;
    }
    file.imports.iter().any(|import| {
        import
            .strip_suffix(".*")
            .is_some_and(|import_package| import_package.replace('.', "/") == package)
            || import
                .rsplit_once('.')
                .is_some_and(|(import_package, item)| {
                    item == name && import_package.replace('.', "/") == package
                })
    })
}

fn qualified_name(package: &str, name: &str) -> String {
    if package.is_empty() {
        name.to_owned()
    } else {
        format!("{package}/{name}")
    }
}

pub(crate) fn declaration_name_span(
    tokens: &[FrontendNameToken],
    source: &str,
    owner: Span,
    name: &str,
    last: bool,
) -> Option<Span> {
    let mut matches = tokens.iter().filter(|token| {
        token.kind == FrontendNameTokenKind::Ident
            && owner.lo <= token.span.lo
            && token.span.hi <= owner.hi
            && token.text(source) == name
    });
    let span = if last {
        matches.next_back().map(|token| token.span)
    } else {
        matches.next().map(|token| token.span)
    }?;
    Some(definition_name_span(source, span))
}

pub(crate) fn definition_name_span(source: &str, span: Span) -> Span {
    let bytes = source.as_bytes();
    let lo = span.lo as usize;
    let hi = span.hi as usize;
    if lo > 0
        && hi < bytes.len()
        && bytes.get(lo - 1) == Some(&b'`')
        && bytes.get(hi) == Some(&b'`')
    {
        Span::new(span.lo - 1, span.hi + 1)
    } else {
        span
    }
}
