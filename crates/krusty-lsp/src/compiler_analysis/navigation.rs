//! Source declaration locations used while reducing navigation to compact file/span pairs.

use std::collections::{HashMap, HashSet};

use krusty::ast::{ClassDecl, ClassKind, Decl, File, FunDecl, Modality, PropDecl, TypeRef};
use krusty::diag::{DiagSink, Span};
use krusty::frontend::{
    lex_name_tokens, FrontendNameToken, FrontendNameTokenKind, FrontendSymbols,
};
use krusty::types::{Ty, Visibility};

use super::{
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
    hover_values: HashMap<(u32, u32, u32), String>,
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
                        let class_symbols = symbols.class_by_internal(&owner);
                        for parameter in &class.props {
                            if parameter.is_property {
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
                                        target,
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
                                        property_type(
                                            property,
                                            analysis,
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
                                        target,
                                    });
                            }
                        }
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
                                        target,
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
                                        target,
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
                                        property_type(
                                            property,
                                            analysis,
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
                                        target,
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
                                    target,
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
                                    property_type(
                                        property,
                                        analysis,
                                        if property.receiver.is_some() {
                                            symbols.ext_props.values().find_map(|signature| {
                                                (signature.source
                                                    == (file_index as u32, declaration.0))
                                                    .then_some(signature.ty)
                                            })
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
    let modality = if function.is_override {
        "override "
    } else if function.is_abstract {
        "abstract "
    } else if function.is_open {
        "open "
    } else {
        ""
    };
    let tailrec = if function.is_tailrec { "tailrec " } else { "" };
    let inline = if function.is_inline { "inline " } else { "" };
    let operator = if function.is_operator {
        "operator "
    } else {
        ""
    };
    let suspend = if function.is_suspend { "suspend " } else { "" };
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

fn property_type(
    property: &PropDecl,
    analysis: &FileAnalysis,
    resolved_type: Option<Ty>,
) -> Option<Ty> {
    resolved_type.or_else(|| {
        property
            .init
            .or_else(|| {
                property.getter.as_ref().and_then(|getter| match getter {
                    krusty::ast::FunBody::Expr(body) | krusty::ast::FunBody::Block(body) => {
                        Some(*body)
                    }
                    krusty::ast::FunBody::None => None,
                })
            })
            .or(property.delegate)
            .and_then(|init| {
                analysis
                    .types
                    .as_ref()?
                    .delegate_getvalue(init)
                    .map(|target| target.ret())
                    .or_else(|| {
                        analysis
                            .types
                            .as_ref()?
                            .expr_types
                            .get(init.0 as usize)
                            .copied()
                    })
            })
    })
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
