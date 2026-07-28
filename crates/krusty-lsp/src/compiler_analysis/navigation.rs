//! Source declaration locations used while reducing navigation to compact file/span pairs.

use std::collections::{HashMap, HashSet};

use krusty::ast::{
    ClassDecl, ClassKind, Decl, DeclId, File, FunDecl, Modality, Param, PropDecl, TypeRef,
};
use krusty::diag::{DiagSink, Span};
use krusty::frontend::{
    lex_name_tokens, FrontendNameToken, FrontendNameTokenKind, FrontendSymbols,
};
use krusty::types::{Ty, TypeName, Visibility};

use super::{
    checked_property_type,
    rendering::{render_ty, render_type},
    FileAnalysis,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    source_method: Option<SourceMethod>,
    target: DefinitionTarget,
    is_override: bool,
    is_inheritable: bool,
}

#[derive(Clone, Copy)]
struct SourceMethod {
    file: u32,
    declaration: u32,
    method: u32,
}

struct ParentDefinition {
    owner: String,
    source: SourceParent,
}

enum SourceParent {
    Interface { class: SourceClass, interface: u32 },
    Base { class: SourceClass },
}

#[derive(Clone, Copy)]
struct SourceClass {
    file: u32,
    declaration: u32,
}

#[derive(Clone, PartialEq, Eq)]
struct ImplementationType {
    constructor: ImplementationTypeConstructor,
    nullable: bool,
    argument: Option<Box<ImplementationType>>,
    arguments: Vec<ImplementationType>,
    function_parameters: Vec<ImplementationType>,
    function_has_receiver: bool,
    function_is_suspend: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImplementationTypeConstructor {
    RootParameter(u32),
    MethodParameter(u32),
    Named(Ty),
    Function,
}

struct ExtensionDefinition {
    package: String,
    target: DefinitionTarget,
}

#[derive(Default)]
pub struct DefinitionSymbols {
    classes: HashMap<String, DefinitionTarget>,
    class_types: HashMap<TypeName, DefinitionTarget>,
    declarations: HashMap<(u32, u32), DefinitionTarget>,
    source_classes: HashMap<String, SourceClass>,
    members: HashMap<(String, String), Vec<MemberDefinition>>,
    member_parents: HashMap<String, Vec<ParentDefinition>>,
    object_owners: HashSet<String>,
    extensions: HashMap<(Ty, String), Vec<ExtensionDefinition>>,
    top_levels: HashMap<(String, String, MemberKind), Vec<DefinitionTarget>>,
    hover_values: HashMap<(u32, u32, u32), String>,
    self_targets: Vec<Vec<DefinitionTarget>>,
    implementations: HashMap<DefinitionTarget, Vec<DefinitionTarget>>,
}

impl DefinitionSymbols {
    pub fn from_source_set(
        sources: &[&str],
        files: &[FileAnalysis],
        symbols: &FrontendSymbols,
        implementation_limit: usize,
    ) -> Self {
        let mut definitions = Self::default();
        for (file_index, (source, analysis)) in sources.iter().copied().zip(files).enumerate() {
            let mut diagnostics = DiagSink::new();
            let tokens = lex_name_tokens(source, &mut diagnostics);
            let package = package_key(&analysis.file);
            for &declaration in &analysis.file.decls {
                match analysis.file.decl(declaration) {
                    Decl::Class(class) => {
                        let declaration_name = class.name.rsplit('.').next().unwrap_or(&class.name);
                        let Some(span) = declaration_name_span(
                            &tokens,
                            source,
                            class.span,
                            declaration_name,
                            false,
                        ) else {
                            continue;
                        };
                        let target = DefinitionTarget {
                            file: file_index as u32,
                            span,
                        };
                        definitions.insert_hover(
                            target,
                            render_class_hover(
                                class,
                                source_name(source, target.span, declaration_name),
                                source,
                            ),
                        );
                        definitions
                            .declarations
                            .insert((file_index as u32, declaration.0), target);
                        let internal_name = class.name.replace('.', "$");
                        let owner = qualified_name(&package, &internal_name);
                        definitions.classes.insert(owner.clone(), target);
                        let source_class = SourceClass {
                            file: file_index as u32,
                            declaration: declaration.0,
                        };
                        definitions
                            .source_classes
                            .insert(owner.clone(), source_class);
                        let class_symbols = symbols.class_by_internal(&owner);
                        for parameter in &class.props {
                            if parameter.is_property && parameter.span.lo < parameter.span.hi {
                                let target = DefinitionTarget {
                                    file: file_index as u32,
                                    span: definition_name_span(source, parameter.span),
                                };
                                definitions.insert_hover(
                                    target,
                                    format!(
                                        "{}: {}",
                                        source_name(source, target.span, &parameter.name),
                                        render_type(&parameter.ty)
                                    ),
                                );
                                definitions
                                    .members
                                    .entry((owner.clone(), parameter.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::InstanceValue,
                                        params: Some(Vec::new()),
                                        source_method: None,
                                        target,
                                        is_override: parameter.is_override,
                                        is_inheritable: parameter.visibility != Visibility::Private
                                            && parameter.is_open,
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
                                let target = DefinitionTarget {
                                    file: file_index as u32,
                                    span,
                                };
                                definitions.insert_hover(
                                    target,
                                    render_property_hover(
                                        property,
                                        checked_property_type(
                                            property,
                                            analysis.types.as_ref(),
                                            class_symbols
                                                .and_then(|symbols| symbols.prop(&property.name))
                                                .map(|(ty, _)| ty),
                                        ),
                                        source_name(source, target.span, &property.name),
                                    ),
                                );
                                definitions
                                    .members
                                    .entry((owner.clone(), property.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::InstanceValue,
                                        params: Some(Vec::new()),
                                        source_method: None,
                                        target,
                                        is_override: property.is_override,
                                        is_inheritable: property.visibility != Visibility::Private
                                            && (class.kind == ClassKind::Interface
                                                || property.is_open
                                                || property.is_abstract),
                                    });
                            }
                        }
                        if let Some(class_symbols) = class_symbols {
                            definitions
                                .class_types
                                .insert(class_symbols.internal_name(), target);
                            if class_symbols.is_object() {
                                definitions.object_owners.insert(owner.clone());
                            }
                            let mut parents = class_symbols
                                .interfaces
                                .iter_ids()
                                .enumerate()
                                .map(|(interface, parent)| ParentDefinition {
                                    owner: parent.render(),
                                    source: SourceParent::Interface {
                                        class: source_class,
                                        interface: interface as u32,
                                    },
                                })
                                .collect::<Vec<_>>();
                            if let Some(parent) = class_symbols.super_internal {
                                parents.push(ParentDefinition {
                                    owner: parent.render(),
                                    source: SourceParent::Base {
                                        class: source_class,
                                    },
                                });
                            }
                            definitions.member_parents.insert(owner.clone(), parents);
                        }
                        let mut method_ordinals = HashMap::<String, usize>::new();
                        for (method_index, function) in class.methods.iter().enumerate() {
                            if let Some(span) = declaration_name_span(
                                &tokens,
                                source,
                                function.span,
                                &function.name,
                                false,
                            ) {
                                let signature = class_symbols.and_then(|class_symbols| {
                                    let ordinal =
                                        method_ordinals.entry(function.name.clone()).or_default();
                                    let signature =
                                        class_symbols.methods_named(&function.name).get(*ordinal);
                                    *ordinal += 1;
                                    signature
                                });
                                let target = DefinitionTarget {
                                    file: file_index as u32,
                                    span,
                                };
                                definitions.insert_hover(
                                    target,
                                    render_function_hover(
                                        function,
                                        signature.map(|signature| signature.ret),
                                        source_name(source, target.span, &function.name),
                                        &tokens,
                                        source,
                                    ),
                                );
                                definitions
                                    .members
                                    .entry((owner.clone(), function.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::InstanceFunction,
                                        params: signature.map(|signature| signature.params.clone()),
                                        source_method: Some(SourceMethod {
                                            file: file_index as u32,
                                            declaration: declaration.0,
                                            method: method_index as u32,
                                        }),
                                        target,
                                        is_override: function.is_override(),
                                        is_inheritable: function.visibility != Visibility::Private
                                            && (class.kind == ClassKind::Interface
                                                || function.is_open()
                                                || function.is_abstract()),
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
                                let signature = class_symbols.and_then(|class_symbols| {
                                    class_symbols.static_methods.get(&function.name)
                                });
                                let target = DefinitionTarget {
                                    file: file_index as u32,
                                    span,
                                };
                                definitions.insert_hover(
                                    target,
                                    render_function_hover(
                                        function,
                                        signature.map(|signature| signature.ret),
                                        source_name(source, target.span, &function.name),
                                        &tokens,
                                        source,
                                    ),
                                );
                                definitions
                                    .members
                                    .entry((owner.clone(), function.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::StaticFunction,
                                        params: signature.map(|signature| signature.params.clone()),
                                        source_method: None,
                                        target,
                                        is_override: false,
                                        is_inheritable: false,
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
                                let target = DefinitionTarget {
                                    file: file_index as u32,
                                    span,
                                };
                                definitions.insert_hover(
                                    target,
                                    render_property_hover(
                                        property,
                                        checked_property_type(
                                            property,
                                            analysis.types.as_ref(),
                                            class_symbols.and_then(|symbols| {
                                                symbols.static_props.get(&property.name).copied()
                                            }),
                                        ),
                                        source_name(source, target.span, &property.name),
                                    ),
                                );
                                definitions
                                    .members
                                    .entry((owner.clone(), property.name.clone()))
                                    .or_default()
                                    .push(MemberDefinition {
                                        kind: MemberKind::StaticValue,
                                        params: Some(Vec::new()),
                                        source_method: None,
                                        target,
                                        is_override: false,
                                        is_inheritable: false,
                                    });
                            }
                        }
                        for entry in &class.enum_entries {
                            let target = DefinitionTarget {
                                file: file_index as u32,
                                span: definition_name_span(source, entry.span),
                            };
                            definitions.insert_hover(
                                target,
                                source_name(source, target.span, &entry.name).to_string(),
                            );
                            definitions
                                .members
                                .entry((owner.clone(), entry.name.clone()))
                                .or_default()
                                .push(MemberDefinition {
                                    kind: MemberKind::StaticValue,
                                    params: None,
                                    source_method: None,
                                    target,
                                    is_override: false,
                                    is_inheritable: false,
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
                            let signature =
                                symbols.funs.get(&function.name).and_then(|signatures| {
                                    signatures.iter().find(|signature| {
                                        signature.source_file == Some(file_index as u32)
                                            && signature.source_decl == Some(declaration)
                                    })
                                });
                            definitions.insert_hover(
                                target,
                                render_function_hover(
                                    function,
                                    signature.map(|signature| signature.ret),
                                    source_name(source, target.span, &function.name),
                                    &tokens,
                                    source,
                                ),
                            );
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
                            definitions.insert_hover(
                                target,
                                render_property_hover(
                                    property,
                                    checked_property_type(
                                        property,
                                        analysis.types.as_ref(),
                                        if property.receiver.is_some() {
                                            symbols
                                                .source_extension_property((
                                                    file_index as u32,
                                                    declaration.0,
                                                ))
                                                .map(|signature| signature.ty)
                                        } else {
                                            symbols.props.get(&property.name).map(|(ty, _, _)| *ty)
                                        },
                                    ),
                                    source_name(source, target.span, &property.name),
                                ),
                            );
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
        for ((receiver, name), signatures) in &symbols.ext_props {
            for signature in signatures {
                let Some(target) =
                    definitions.declaration_target(signature.source.0, signature.source.1)
                else {
                    continue;
                };
                definitions
                    .extensions
                    .entry((*receiver, name.clone()))
                    .or_default()
                    .push(ExtensionDefinition {
                        package: signature.package.clone(),
                        target,
                    });
            }
        }
        definitions.build_implementation_targets(files, symbols, implementation_limit);
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

    fn build_implementation_targets(
        &mut self,
        files: &[FileAnalysis],
        symbols: &FrontendSymbols,
        limit: usize,
    ) {
        const WORK_PER_RELATION: usize = 16;

        if limit == 0 {
            return;
        }
        let mut owners = self.classes.keys().map(String::as_str).collect::<Vec<_>>();
        owners.sort_unstable();
        let mut exact_member_lookup =
            HashMap::<(&str, &str, MemberKind, Option<&[Ty]>), Vec<&MemberDefinition>>::new();
        let mut generic_member_lookup =
            HashMap::<(&str, &str, MemberKind, Option<usize>), Vec<&MemberDefinition>>::new();
        let mut override_members_by_owner = HashMap::<&str, Vec<(&str, &MemberDefinition)>>::new();
        for ((owner, name), definitions) in &self.members {
            override_members_by_owner.entry(owner).or_default().extend(
                definitions
                    .iter()
                    .filter(|definition| definition.is_override)
                    .map(|definition| (name.as_str(), definition)),
            );
            for definition in definitions
                .iter()
                .filter(|definition| definition.is_inheritable)
            {
                let has_type_parameters = definition
                    .source_method
                    .is_some_and(|source| source_method_has_type_parameters(files, source));
                if has_type_parameters {
                    generic_member_lookup
                        .entry((
                            owner.as_str(),
                            name.as_str(),
                            definition.kind,
                            definition.params.as_ref().map(Vec::len),
                        ))
                        .or_default()
                        .push(definition);
                } else {
                    exact_member_lookup
                        .entry((
                            owner.as_str(),
                            name.as_str(),
                            definition.kind,
                            definition.params.as_deref(),
                        ))
                        .or_default()
                        .push(definition);
                }
            }
        }
        for definitions in override_members_by_owner.values_mut() {
            definitions.sort_unstable_by(|(left_name, left), (right_name, right)| {
                (
                    left_name,
                    left.target.file,
                    left.target.span.lo,
                    left.target.span.hi,
                )
                    .cmp(&(
                        right_name,
                        right.target.file,
                        right.target.span.lo,
                        right.target.span.hi,
                    ))
            });
        }
        let mut relations = Vec::<(DefinitionTarget, DefinitionTarget)>::new();
        let mut direct_member_relations = Vec::<(DefinitionTarget, DefinitionTarget)>::new();
        let mut work_remaining = limit.saturating_mul(WORK_PER_RELATION);
        'owners: for child_owner in owners {
            if relations.len() + direct_member_relations.len() >= limit || work_remaining == 0 {
                break;
            }
            let Some(&child_target) = self.classes.get(child_owner) else {
                continue;
            };
            let ancestors = self.ancestor_owners(child_owner, &mut work_remaining);
            for ancestor in &ancestors {
                if let Some(&ancestor_target) = self.classes.get(*ancestor) {
                    if relations.len() >= limit {
                        break;
                    }
                    relations.push((ancestor_target, child_target));
                }
            }
            if relations.len() >= limit {
                break;
            }

            let child_members = override_members_by_owner
                .get(child_owner)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for &(name, child) in child_members {
                if relations.len() + direct_member_relations.len() >= limit || work_remaining == 0 {
                    break 'owners;
                }
                let direct_parents = self
                    .member_parents
                    .get(child_owner)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let child_bindings = match child
                    .source_method
                    .and_then(|source| source_method(files, source))
                {
                    Some((class, _)) => {
                        let Some(bindings) = root_type_bindings(class, &mut work_remaining) else {
                            break 'owners;
                        };
                        bindings
                    }
                    None => Vec::new(),
                };
                let mut pending = Vec::new();
                for parent in direct_parents {
                    if work_remaining == 0 {
                        break 'owners;
                    }
                    work_remaining -= 1;
                    let Some(bindings) = self.parent_bindings(
                        parent,
                        &child_bindings,
                        files,
                        symbols,
                        &mut work_remaining,
                    ) else {
                        break 'owners;
                    };
                    pending.push((parent.owner.as_str(), bindings));
                }
                pending.reverse();
                let mut seen = HashSet::<&str>::new();
                while let Some((parent_owner, bindings)) = pending.pop() {
                    if relations.len() + direct_member_relations.len() >= limit
                        || work_remaining == 0
                    {
                        break 'owners;
                    }
                    work_remaining -= 1;
                    if !seen.insert(parent_owner) {
                        continue;
                    }
                    let exact_key = (parent_owner, name, child.kind, child.params.as_deref());
                    let compatible_key = (
                        parent_owner,
                        name,
                        child.kind,
                        child.params.as_ref().map(Vec::len),
                    );
                    let mut compatible = None;
                    let mut ambiguous = false;
                    let exact = exact_member_lookup
                        .get(&exact_key)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    let generic = generic_member_lookup
                        .get(&compatible_key)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    for parent in exact.iter().chain(generic) {
                        if work_remaining == 0 {
                            break 'owners;
                        }
                        work_remaining -= 1;
                        if !member_signatures_compatible(
                            child,
                            parent,
                            files,
                            symbols,
                            &child_bindings,
                            &bindings,
                            &mut work_remaining,
                        ) {
                            continue;
                        }
                        if compatible.is_some() {
                            ambiguous = true;
                            break;
                        }
                        compatible = Some(*parent);
                    }
                    if let (Some(parent), false) = (compatible, ambiguous) {
                        direct_member_relations.push((parent.target, child.target));
                        continue;
                    }
                    if ambiguous {
                        continue;
                    }
                    if let Some(grandparents) = self.member_parents.get(parent_owner) {
                        let mut next = Vec::new();
                        for grandparent in grandparents {
                            if work_remaining == 0 {
                                break 'owners;
                            }
                            work_remaining -= 1;
                            let Some(grandparent_bindings) = self.parent_bindings(
                                grandparent,
                                &bindings,
                                files,
                                symbols,
                                &mut work_remaining,
                            ) else {
                                break 'owners;
                            };
                            next.push((grandparent.owner.as_str(), grandparent_bindings));
                        }
                        pending.extend(next.into_iter().rev());
                    }
                }
            }
        }
        direct_member_relations.sort_unstable_by_key(|(parent, child)| {
            (
                parent.file,
                parent.span.lo,
                parent.span.hi,
                child.file,
                child.span.lo,
                child.span.hi,
            )
        });
        direct_member_relations.dedup();
        relations.extend(
            direct_member_relations
                .iter()
                .copied()
                .take(limit - relations.len()),
        );
        append_transitive_member_relations(
            &direct_member_relations,
            &mut relations,
            &mut work_remaining,
            limit,
        );
        drop(override_members_by_owner);
        drop(generic_member_lookup);
        drop(exact_member_lookup);
        for (declaration, implementation) in relations {
            self.implementations
                .entry(declaration)
                .or_default()
                .push(implementation);
        }
        for targets in self.implementations.values_mut() {
            targets.sort_unstable_by_key(|target| (target.file, target.span.lo, target.span.hi));
            targets.dedup();
        }
    }

    fn ancestor_owners<'a>(&'a self, owner: &str, work_remaining: &mut usize) -> Vec<&'a str> {
        let mut pending = Vec::new();
        if let Some(parents) = self.member_parents.get(owner) {
            let mut next = Vec::new();
            for parent in parents.iter().rev() {
                if *work_remaining == 0 {
                    break;
                }
                *work_remaining -= 1;
                next.push(parent.owner.as_str());
            }
            pending.extend(next.into_iter().rev());
        }
        let mut seen = HashSet::<&str>::new();
        while let Some(parent) = pending.pop() {
            if !seen.insert(parent) {
                continue;
            }
            if let Some(grandparents) = self.member_parents.get(parent) {
                let mut next = Vec::new();
                for grandparent in grandparents.iter().rev() {
                    if *work_remaining == 0 {
                        break;
                    }
                    *work_remaining -= 1;
                    next.push(grandparent.owner.as_str());
                }
                pending.extend(next.into_iter().rev());
            }
        }
        let mut ancestors = seen.into_iter().collect::<Vec<_>>();
        ancestors.sort_unstable();
        ancestors
    }

    fn parent_bindings(
        &self,
        parent: &ParentDefinition,
        current_bindings: &[ImplementationType],
        files: &[FileAnalysis],
        symbols: &FrontendSymbols,
        work_remaining: &mut usize,
    ) -> Option<Vec<ImplementationType>> {
        let Some(&parent_source) = self.source_classes.get(&parent.owner) else {
            return Some(Vec::new());
        };
        let Some(parent_class) = source_class(files, parent_source) else {
            return Some(Vec::new());
        };
        let binding_count = parent_class.type_params.len();
        if binding_count > *work_remaining {
            *work_remaining = 0;
            return None;
        }
        *work_remaining -= binding_count;
        let current_source = match &parent.source {
            SourceParent::Interface { class, .. } | SourceParent::Base { class } => *class,
        };
        let Some(current_class) = source_class(files, current_source) else {
            return Some(Vec::new());
        };
        let arguments = match &parent.source {
            SourceParent::Interface { class, interface } => {
                let Some(arguments) = source_class(files, *class)
                    .and_then(|class| class.supertypes.get(*interface as usize))
                    .map(|parent| parent.targs.as_slice())
                else {
                    return Some(Vec::new());
                };
                arguments
            }
            SourceParent::Base { class } => {
                let Some(arguments) =
                    source_class(files, *class).map(|class| class.base_type_args.as_slice())
                else {
                    return Some(Vec::new());
                };
                arguments
            }
        };
        arguments
            .iter()
            .take(binding_count)
            .map(|argument| {
                implementation_type(
                    argument,
                    current_class,
                    current_bindings,
                    &[],
                    symbols,
                    work_remaining,
                )
            })
            .collect()
    }

    fn insert_hover(&mut self, target: DefinitionTarget, value: String) {
        self.hover_values
            .insert((target.file, target.span.lo, target.span.hi), value);
    }

    fn dotted_class(&self, name: &str) -> Option<(String, DefinitionTarget)> {
        let parts = name.split('.').collect::<Vec<_>>();
        let mut found = None;
        for class_start in 0..parts.len() {
            let package = parts[..class_start].join("/");
            let class = parts[class_start..].join("$");
            let owner = qualified_name(&package, &class);
            let Some(&target) = self.classes.get(&owner) else {
                continue;
            };
            match found {
                None => found = Some((owner, target)),
                Some((_, existing)) if existing == target => {}
                Some(_) => return None,
            }
        }
        found
    }

    pub(crate) fn class_target(&self, file: &File, name: &str) -> Option<DefinitionTarget> {
        self.class_owner(file, name)
            .and_then(|owner| self.class_target_for_owner(&owner))
    }

    pub(crate) fn class_target_for_owner(&self, owner: &str) -> Option<DefinitionTarget> {
        self.classes.get(owner).copied()
    }

    pub(crate) fn class_target_for_type(&self, owner: TypeName) -> Option<DefinitionTarget> {
        self.class_types.get(&owner).copied()
    }

    pub(crate) fn declaration_target(
        &self,
        file: u32,
        declaration: u32,
    ) -> Option<DefinitionTarget> {
        self.declarations.get(&(file, declaration)).copied()
    }

    pub(crate) fn hover_value(&self, target: DefinitionTarget) -> Option<&str> {
        self.hover_values
            .get(&(target.file, target.span.lo, target.span.hi))
            .map(String::as_str)
    }

    pub(crate) fn file_targets(&self, file: u32) -> &[DefinitionTarget] {
        self.self_targets
            .get(file as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn implementation_targets(&self, target: DefinitionTarget) -> &[DefinitionTarget] {
        self.implementations
            .get(&target)
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
            let package = package_key(file);
            let local_nested = qualified_name(&package, &name.replace('.', "$"));
            if self.classes.contains_key(&local_nested) {
                return Some(local_nested);
            }
            if let Some((owner, _)) = self.dotted_class(name) {
                return Some(owner);
            }
            let (outer, nested) = name.split_once('.')?;
            let nested = nested.replace('.', "$");
            let outer = self.class_owner(file, outer)?;
            let owner = format!("{outer}${nested}");
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
            .filter_map(|import| self.dotted_class(import).map(|(owner, _)| owner))
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
            .filter_map(|owner| {
                self.dotted_class(&format!("{owner}.{name}"))
                    .map(|(owner, _)| owner)
            })
            .collect::<Vec<_>>();
        wildcard_owners.sort_unstable();
        wildcard_owners.dedup();
        match wildcard_owners.as_slice() {
            [owner] => Some(owner.clone()),
            _ => None,
        }
    }

    pub(crate) fn nested_class_target(&self, owner: &str, name: &str) -> Option<DefinitionTarget> {
        self.classes.get(&format!("{owner}${name}")).copied()
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
                .any(|parent| self.collect_member_targets(&parent.owner, name, kind, seen, targets))
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

fn append_transitive_member_relations(
    direct: &[(DefinitionTarget, DefinitionTarget)],
    relations: &mut Vec<(DefinitionTarget, DefinitionTarget)>,
    work_remaining: &mut usize,
    limit: usize,
) {
    if relations.len() >= limit || *work_remaining == 0 {
        return;
    }
    let mut adjacency = HashMap::<DefinitionTarget, Vec<DefinitionTarget>>::new();
    for &(parent, child) in direct {
        adjacency.entry(parent).or_default().push(child);
    }
    for children in adjacency.values_mut() {
        children.sort_unstable_by_key(|target| (target.file, target.span.lo, target.span.hi));
        children.dedup();
    }
    let mut roots = adjacency.keys().copied().collect::<Vec<_>>();
    roots.sort_unstable_by_key(|target| (target.file, target.span.lo, target.span.hi));
    for root in roots {
        if relations.len() >= limit || *work_remaining == 0 {
            break;
        }
        let Some(children) = adjacency.get(&root) else {
            continue;
        };
        let mut seen = HashSet::from([root]);
        seen.extend(children.iter().copied());
        let mut pending = children.iter().rev().copied().collect::<Vec<_>>();
        while let Some(child) = pending.pop() {
            let Some(grandchildren) = adjacency.get(&child) else {
                continue;
            };
            for &grandchild in grandchildren {
                if relations.len() >= limit || *work_remaining == 0 {
                    return;
                }
                *work_remaining -= 1;
                if !seen.insert(grandchild) {
                    continue;
                }
                relations.push((root, grandchild));
                pending.push(grandchild);
            }
        }
    }
}

fn member_signatures_compatible(
    child: &MemberDefinition,
    parent: &MemberDefinition,
    files: &[FileAnalysis],
    symbols: &FrontendSymbols,
    child_bindings: &[ImplementationType],
    parent_bindings: &[ImplementationType],
    work_remaining: &mut usize,
) -> bool {
    if child.kind != parent.kind {
        return false;
    }
    let (Some(child_params), Some(parent_params)) = (&child.params, &parent.params) else {
        return false;
    };
    let child_source = child
        .source_method
        .and_then(|source| source_method(files, source));
    let parent_source = parent
        .source_method
        .and_then(|source| source_method(files, source));
    let (Some((child_class, child_method)), Some((parent_class, parent_method))) =
        (child_source, parent_source)
    else {
        return child_source.is_none() && parent_source.is_none() && child_params == parent_params;
    };
    if child_params.len() != parent_params.len()
        || child_params.len() != child_method.params.len()
        || parent_params.len() != parent_method.params.len()
        || child_method.type_params.len() != parent_method.type_params.len()
    {
        return false;
    }
    match (&child_method.receiver, &parent_method.receiver) {
        (Some(child), Some(parent)) => {
            let Some(child) = implementation_type(
                child,
                child_class,
                child_bindings,
                &child_method.type_params,
                symbols,
                work_remaining,
            ) else {
                return false;
            };
            let Some(parent) = implementation_type(
                parent,
                parent_class,
                parent_bindings,
                &parent_method.type_params,
                symbols,
                work_remaining,
            ) else {
                return false;
            };
            if child != parent {
                return false;
            }
        }
        (None, None) => {}
        _ => return false,
    }
    child_method
        .params
        .iter()
        .zip(&parent_method.params)
        .all(|(child, parent)| {
            let Some(child) = implementation_parameter_type(
                child,
                child_class,
                child_bindings,
                child_method,
                symbols,
                work_remaining,
            ) else {
                return false;
            };
            let Some(parent) = implementation_parameter_type(
                parent,
                parent_class,
                parent_bindings,
                parent_method,
                symbols,
                work_remaining,
            ) else {
                return false;
            };
            child == parent
        })
}

fn source_method_has_type_parameters(files: &[FileAnalysis], source: SourceMethod) -> bool {
    source_method(files, source).is_some_and(|(class, method)| {
        !class.type_params.is_empty() || !method.type_params.is_empty()
    })
}

fn source_method(files: &[FileAnalysis], source: SourceMethod) -> Option<(&ClassDecl, &FunDecl)> {
    let declaration = files
        .get(source.file as usize)?
        .file
        .decl(DeclId(source.declaration));
    let Decl::Class(class) = declaration else {
        return None;
    };
    Some((class, class.methods.get(source.method as usize)?))
}

fn source_class(files: &[FileAnalysis], source: SourceClass) -> Option<&ClassDecl> {
    let declaration = files
        .get(source.file as usize)?
        .file
        .decl(DeclId(source.declaration));
    let Decl::Class(class) = declaration else {
        return None;
    };
    Some(class)
}

fn root_type_bindings(
    class: &ClassDecl,
    work_remaining: &mut usize,
) -> Option<Vec<ImplementationType>> {
    if class.type_params.len() > *work_remaining {
        *work_remaining = 0;
        return None;
    }
    *work_remaining -= class.type_params.len();
    Some(
        class
            .type_params
            .iter()
            .enumerate()
            .map(|(index, _)| ImplementationType {
                constructor: ImplementationTypeConstructor::RootParameter(index as u32),
                nullable: false,
                argument: None,
                arguments: Vec::new(),
                function_parameters: Vec::new(),
                function_has_receiver: false,
                function_is_suspend: false,
            })
            .collect(),
    )
}

fn implementation_parameter_type(
    parameter: &Param,
    current_class: &ClassDecl,
    current_bindings: &[ImplementationType],
    current_method: &FunDecl,
    symbols: &FrontendSymbols,
    work_remaining: &mut usize,
) -> Option<ImplementationType> {
    let element = implementation_type(
        &parameter.ty,
        current_class,
        current_bindings,
        &current_method.type_params,
        symbols,
        work_remaining,
    )?;
    if !parameter.is_vararg {
        return Some(element);
    }
    if let ImplementationType {
        constructor: ImplementationTypeConstructor::Named(ty),
        nullable: false,
        argument: None,
        arguments,
        function_parameters,
        ..
    } = &element
    {
        if arguments.is_empty() && function_parameters.is_empty() {
            let array = Ty::array(*ty);
            if !array
                .obj_internal()
                .is_some_and(|name| name.matches("kotlin/Array"))
            {
                return Some(ImplementationType {
                    constructor: ImplementationTypeConstructor::Named(array),
                    nullable: false,
                    argument: None,
                    arguments: Vec::new(),
                    function_parameters: Vec::new(),
                    function_has_receiver: false,
                    function_is_suspend: false,
                });
            }
        }
    }
    Some(ImplementationType {
        constructor: ImplementationTypeConstructor::Named(Ty::obj("kotlin/Array")),
        nullable: false,
        argument: Some(Box::new(element)),
        arguments: Vec::new(),
        function_parameters: Vec::new(),
        function_has_receiver: false,
        function_is_suspend: false,
    })
}

fn implementation_type(
    reference: &TypeRef,
    current_class: &ClassDecl,
    current_bindings: &[ImplementationType],
    method_parameters: &[String],
    symbols: &FrontendSymbols,
    work_remaining: &mut usize,
) -> Option<ImplementationType> {
    if *work_remaining == 0 {
        return None;
    }
    *work_remaining -= 1;
    if let Some(parameter) = method_parameters
        .iter()
        .position(|parameter| parameter == &reference.name)
    {
        return Some(ImplementationType {
            constructor: ImplementationTypeConstructor::MethodParameter(parameter as u32),
            nullable: reference.nullable(),
            argument: None,
            arguments: Vec::new(),
            function_parameters: Vec::new(),
            function_has_receiver: false,
            function_is_suspend: false,
        });
    }
    if let Some(parameter) = current_class
        .type_params
        .iter()
        .position(|parameter| parameter == &reference.name)
    {
        let mut binding = current_bindings.get(parameter)?.clone();
        binding.nullable |= reference.nullable();
        return Some(binding);
    }
    if !reference.fun_params.is_empty() || reference.name == "<fun>" {
        let function_parameters = reference
            .fun_params
            .iter()
            .map(|parameter| {
                implementation_type(
                    parameter,
                    current_class,
                    current_bindings,
                    method_parameters,
                    symbols,
                    work_remaining,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        let argument = match reference.arg.as_deref() {
            Some(result) => Some(Box::new(implementation_type(
                result,
                current_class,
                current_bindings,
                method_parameters,
                symbols,
                work_remaining,
            )?)),
            None => None,
        };
        return Some(ImplementationType {
            constructor: ImplementationTypeConstructor::Function,
            nullable: reference.nullable(),
            argument,
            arguments: Vec::new(),
            function_parameters,
            function_has_receiver: reference.fun_has_receiver(),
            function_is_suspend: reference.fun_suspend(),
        });
    }
    let named = if let Some(builtin) = Ty::from_name(&reference.name) {
        builtin
    } else if let Some(element) = Ty::primitive_array_element(&reference.name) {
        Ty::array(element)
    } else {
        let internal = symbols.class_names.get(&reference.name)?;
        if let Some(builtin) = internal
            .strip_prefix("__ty/")
            .and_then(|name| Ty::from_name(&name))
        {
            builtin
        } else {
            Ty::obj_name(internal)
        }
    };
    let argument = match reference.arg.as_deref() {
        Some(argument) => Some(Box::new(implementation_type(
            argument,
            current_class,
            current_bindings,
            method_parameters,
            symbols,
            work_remaining,
        )?)),
        None => None,
    };
    let arguments = reference
        .targs
        .iter()
        .map(|argument| {
            implementation_type(
                argument,
                current_class,
                current_bindings,
                method_parameters,
                symbols,
                work_remaining,
            )
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ImplementationType {
        constructor: ImplementationTypeConstructor::Named(named),
        nullable: reference.nullable(),
        argument,
        arguments,
        function_parameters: Vec::new(),
        function_has_receiver: false,
        function_is_suspend: false,
    })
}

fn render_class_hover(class: &ClassDecl, name: &str, source: &str) -> String {
    let visibility = render_visibility(class.visibility);
    let modality = match class.modality {
        Modality::Open => "open ",
        Modality::Abstract => "abstract ",
        Modality::Sealed => "sealed ",
        Modality::Final => "",
    };
    let kind = match class.kind {
        ClassKind::Interface if class.is_fun_interface => "fun interface",
        ClassKind::Interface => "interface",
        ClassKind::Enum => "enum class",
        ClassKind::Object => "object",
        ClassKind::Annotation => "annotation class",
        ClassKind::Class if class.is_value => "@JvmInline\ninline class",
        ClassKind::Class if class.inner_of.is_some() => "inner class",
        ClassKind::Class if class.is_data => "data class",
        ClassKind::Class => "class",
    };
    let type_parameters = render_type_parameters(
        &class.type_params,
        &class.type_param_bounds,
        &HashSet::new(),
        &HashSet::new(),
        "",
    );
    let constructor = if class.props.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            class
                .props
                .iter()
                .map(|parameter| {
                    let vararg = if parameter.is_vararg { "vararg " } else { "" };
                    let span = definition_name_span(source, parameter.span);
                    format!(
                        "{vararg}{}: {}",
                        source_name(source, span, &parameter.name),
                        render_type(&parameter.ty)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "{visibility}{modality}{kind} {}{type_parameters}{constructor}",
        name
    )
}

pub(crate) fn render_function_hover(
    function: &FunDecl,
    inferred_return: Option<Ty>,
    name: &str,
    tokens: &[FrontendNameToken],
    source: &str,
) -> String {
    let visibility = render_visibility(function.visibility);
    let modality = if function.is_override() {
        "override "
    } else if function.is_abstract() {
        "abstract "
    } else if function.is_open() {
        "open "
    } else {
        ""
    };
    let tailrec = if function.is_tailrec() {
        "tailrec "
    } else {
        ""
    };
    let inline = if function.is_inline() { "inline " } else { "" };
    let operator = if function.is_operator() {
        "operator "
    } else {
        ""
    };
    let suspend = if function.is_suspend() {
        "suspend "
    } else {
        ""
    };
    let type_parameters = render_type_parameters(
        &function.type_params,
        &function.type_param_bounds,
        &function.non_null_type_params,
        &function.reified_type_params,
        " ",
    );
    let receiver = function
        .receiver
        .as_ref()
        .map(|receiver| format!("{}.", render_type(receiver)))
        .unwrap_or_default();
    let parameters = function
        .params
        .iter()
        .map(|parameter| {
            let vararg = if parameter.is_vararg { "vararg " } else { "" };
            let owner = Span::new(function.span.lo, parameter.ty.span.lo);
            let name = declaration_name_span(tokens, source, owner, &parameter.name, true)
                .map(|span| source_name(source, span, &parameter.name))
                .unwrap_or(&parameter.name);
            format!("{vararg}{name}: {}", render_type(&parameter.ty))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let result = function
        .ret
        .as_ref()
        .map(render_type)
        .or_else(|| inferred_return.map(render_ty))
        .unwrap_or_else(|| "<error>".to_string());
    format!(
        "{visibility}{modality}{tailrec}{inline}{operator}{suspend}fun {type_parameters}{receiver}{}({parameters}): {result}",
        name
    )
}

fn render_property_hover(property: &PropDecl, inferred_type: Option<Ty>, name: &str) -> String {
    let ty = property
        .ty
        .as_ref()
        .map(render_type)
        .or_else(|| inferred_type.map(render_ty))
        .unwrap_or_else(|| "<error>".to_string());
    let visibility = render_visibility(property.visibility);
    let modality = if property.is_override {
        "override "
    } else if property.is_abstract {
        "abstract "
    } else if property.is_open {
        "open "
    } else {
        ""
    };
    let type_parameters = if property.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}> ", property.type_params.join(", "))
    };
    let receiver = property
        .receiver
        .as_ref()
        .map(|receiver| format!("{}.", render_type(receiver)))
        .unwrap_or_default();
    let const_ = if property.is_const { "const " } else { "" };
    let lateinit = if property.is_lateinit {
        "lateinit "
    } else {
        ""
    };
    let mut rendered = format!(
        "{visibility}{modality}{const_}{lateinit}{} {type_parameters}{receiver}{}: {ty}",
        if property.is_var { "var" } else { "val" },
        name
    );
    if property.getter.is_some() || property.delegate.is_some() {
        rendered.push_str("\n  get()");
    }
    if let Some(setter) = &property.setter {
        rendered.push_str("\n  set(");
        rendered.push_str(setter.param.as_deref().unwrap_or("value"));
        rendered.push(')');
    }
    rendered
}

fn render_visibility(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Public => "",
        Visibility::Internal => "internal ",
        Visibility::Protected => "protected ",
        Visibility::Private => "private ",
    }
}

fn render_type_parameters(
    parameters: &[String],
    bounds: &[(String, TypeRef)],
    non_null: &HashSet<String>,
    reified: &HashSet<String>,
    suffix: &str,
) -> String {
    if parameters.is_empty() {
        return String::new();
    }
    let parameters = parameters
        .iter()
        .map(|name| {
            let bound = bounds
                .iter()
                .find_map(|(parameter, bound)| {
                    (parameter == name).then(|| format!("{name} : {}", render_type(bound)))
                })
                .or_else(|| non_null.contains(name).then(|| format!("{name} : Any")))
                .unwrap_or_else(|| name.clone());
            if reified.contains(name) {
                format!("reified {bound}")
            } else {
                bound
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{parameters}>{suffix}")
}

pub(crate) fn source_name<'a>(source: &'a str, span: Span, fallback: &'a str) -> &'a str {
    source
        .get(span.lo as usize..span.hi as usize)
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_walk_is_iterative_cycle_safe_and_work_bounded() {
        let mut symbols = DefinitionSymbols::default();
        for index in 1..10_000 {
            symbols.member_parents.insert(
                format!("C{index:05}"),
                vec![ParentDefinition {
                    owner: format!("C{:05}", index - 1),
                    source: SourceParent::Base {
                        class: SourceClass {
                            file: 0,
                            declaration: 0,
                        },
                    },
                }],
            );
        }
        symbols.member_parents.insert(
            "C00000".to_string(),
            vec![ParentDefinition {
                owner: "C09999".to_string(),
                source: SourceParent::Base {
                    class: SourceClass {
                        file: 0,
                        declaration: 0,
                    },
                },
            }],
        );

        let mut work_remaining = 1;
        let ancestors = symbols.ancestor_owners("C09999", &mut work_remaining);
        assert_eq!(ancestors, ["C09998"]);
        assert_eq!(work_remaining, 0);
    }

    #[test]
    fn ancestor_walk_does_not_collect_a_wide_parent_set_past_its_work_budget() {
        let mut symbols = DefinitionSymbols::default();
        symbols.member_parents.insert(
            "Child".to_string(),
            (0..10_000)
                .map(|index| ParentDefinition {
                    owner: format!("P{index:05}"),
                    source: SourceParent::Base {
                        class: SourceClass {
                            file: 0,
                            declaration: 0,
                        },
                    },
                })
                .collect(),
        );

        let mut work_remaining = 2;
        let ancestors = symbols.ancestor_owners("Child", &mut work_remaining);
        assert_eq!(ancestors, ["P09998", "P09999"]);
        assert_eq!(work_remaining, 0);
    }
}
