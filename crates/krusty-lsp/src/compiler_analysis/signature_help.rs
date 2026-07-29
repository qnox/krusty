//! Source call sites and callable signatures reduced for compact LSP snapshots.

use std::{borrow::Cow, collections::HashMap};

use krusty::ast::{
    ClassDecl, ClassKind, Decl, Expr, ExprId, File, FunBody, FunDecl, Param, SecondaryCtor, Stmt,
    StmtId, TypeRef,
};
use krusty::diag::Span;
use krusty::types::{type_name, Ty, TypeName};

use super::{
    rendering::{render_ty, render_type},
    source_scan::{
        matching_delimiter, skip_block_comment, skip_line_comment, skip_quoted, utf8_char_len,
    },
    FileAnalysis, FrontendSymbols,
};

const MAX_SIGNATURE_CATALOG_ENTRIES: usize = 32 * 1024;
const MAX_SIGNATURE_CATALOG_TEXT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct SignatureParameter {
    pub name: String,
    pub label_start: u32,
    pub label_end: u32,
    label_byte_start: u32,
    label_byte_end: u32,
    declared_type: String,
}

#[derive(Clone)]
pub(crate) struct SignatureCandidate {
    pub label: String,
    pub parameters: Vec<SignatureParameter>,
    semantic_parameters: Vec<Ty>,
    semantic_return: Option<Ty>,
    declared_parameters: Vec<TypeRef>,
    declared_return: Option<TypeRef>,
    name: String,
    return_type: Option<String>,
    type_parameters: Vec<String>,
    primary_constructor: bool,
}

impl SignatureCandidate {
    pub(crate) fn is_vararg(&self) -> bool {
        self.parameters.last().is_some_and(|parameter| {
            self.label[parameter.label_byte_start as usize..parameter.label_byte_end as usize]
                .starts_with("vararg ")
        })
    }
}

#[derive(Default)]
struct SignatureGroup {
    candidates: Vec<SignatureCandidate>,
}

pub(crate) struct SignatureArgument {
    pub end: u32,
    pub name: Option<String>,
}

impl SignatureArgument {
    pub(crate) fn wire_bytes(&self) -> usize {
        16usize.saturating_add(self.name.as_deref().map_or(0, str::len).saturating_mul(6))
    }
}

pub(crate) struct SignatureHelpCall {
    call: ExprId,
    pub span: Span,
    pub group: usize,
    pub selected: usize,
    pub arguments: Vec<SignatureArgument>,
    local_function: Option<StmtId>,
    generic_resolution: Option<(Vec<Ty>, Ty)>,
}

#[derive(Clone, Copy)]
pub(crate) struct SignatureHelpCallSite {
    call: ExprId,
    span: Span,
}

struct ResolvedSignatureCall {
    group: usize,
    selected: usize,
    local_function: Option<StmtId>,
    generic_resolution: Option<(Vec<Ty>, Ty)>,
}

/// Bounded source-set declaration catalog shared only while compact per-file indexes are built.
pub(crate) struct SignatureHelpSymbols {
    groups: Vec<SignatureGroup>,
    top_by_source: HashMap<(u32, u32), (usize, usize)>,
    top_by_qualified: HashMap<(String, String, String), usize>,
    top_by_simple: HashMap<String, Vec<usize>>,
    members: HashMap<(TypeName, String), usize>,
    constructors_by_qualified: HashMap<(String, String), usize>,
    constructors_by_simple: HashMap<String, Vec<usize>>,
}

#[derive(Default)]
struct CatalogBudget {
    entries: usize,
    text_bytes: usize,
}

impl CatalogBudget {
    fn reserve(&mut self, candidate: &SignatureCandidate) -> bool {
        let text_bytes = candidate.parameters.iter().fold(
            candidate
                .label
                .len()
                .saturating_add(candidate.name.len())
                .saturating_add(candidate.return_type.as_deref().map_or(0, str::len))
                .saturating_add(
                    candidate
                        .type_parameters
                        .iter()
                        .map(String::len)
                        .sum::<usize>(),
                ),
            |bytes, parameter| {
                bytes
                    .saturating_add(parameter.name.len())
                    .saturating_add(parameter.declared_type.len())
            },
        );
        if self.entries >= MAX_SIGNATURE_CATALOG_ENTRIES
            || text_bytes > MAX_SIGNATURE_CATALOG_TEXT_BYTES.saturating_sub(self.text_bytes)
        {
            return false;
        }
        self.entries += 1;
        self.text_bytes += text_bytes;
        true
    }
}

impl SignatureHelpSymbols {
    pub(crate) fn from_source_set(
        sources: &[&str],
        files: &[FileAnalysis],
        symbols: &FrontendSymbols,
    ) -> Self {
        let mut result = Self {
            groups: Vec::new(),
            top_by_source: HashMap::new(),
            top_by_qualified: HashMap::new(),
            top_by_simple: HashMap::new(),
            members: HashMap::new(),
            constructors_by_qualified: HashMap::new(),
            constructors_by_simple: HashMap::new(),
        };
        let mut budget = CatalogBudget::default();
        for (file_index, (source, analysis)) in sources.iter().zip(files).enumerate() {
            let package = analysis.file.package.clone().unwrap_or_default();
            for &declaration in &analysis.file.decls {
                if analysis.file.is_local_declaration(declaration) {
                    continue;
                }
                match analysis.file.decl(declaration) {
                    Decl::Fun(function) => {
                        let signature = symbols.source_function_signature(
                            &function.name,
                            file_index as u32,
                            declaration,
                        );
                        let candidate = render_function_signature(
                            source,
                            analysis,
                            function,
                            signature
                                .map(|signature| signature.params.clone())
                                .unwrap_or_default(),
                            signature.map(|signature| signature.ret),
                        );
                        if !budget.reserve(&candidate) {
                            return result;
                        }
                        let receiver = function
                            .receiver
                            .as_ref()
                            .map(render_type)
                            .unwrap_or_default();
                        let key = (package.clone(), function.name.clone(), receiver);
                        let group = if let Some(&group) = result.top_by_qualified.get(&key) {
                            group
                        } else {
                            let group = result.groups.len();
                            result.groups.push(SignatureGroup::default());
                            result.top_by_qualified.insert(key.clone(), group);
                            if key.2.is_empty() {
                                result
                                    .top_by_simple
                                    .entry(key.1.clone())
                                    .or_default()
                                    .push(group);
                            }
                            group
                        };
                        let candidate_index = result.groups[group].candidates.len();
                        result.groups[group].candidates.push(candidate);
                        result
                            .top_by_source
                            .insert((file_index as u32, declaration.0), (group, candidate_index));
                    }
                    Decl::Class(class)
                        if !result.add_class(
                            source,
                            analysis,
                            symbols,
                            &package,
                            class,
                            &mut budget,
                        ) =>
                    {
                        return result;
                    }
                    _ => {}
                }
            }
        }
        result
    }

    fn add_class(
        &mut self,
        source: &str,
        analysis: &FileAnalysis,
        symbols: &FrontendSymbols,
        package: &str,
        class: &ClassDecl,
        budget: &mut CatalogBudget,
    ) -> bool {
        let owner = class_owner(package, &class.name);
        let class_signature = symbols.class_by_type_name(owner);
        if !matches!(class.kind, ClassKind::Interface | ClassKind::Object) {
            let mut group = None;
            let class_name = class.name.rsplit('.').next().unwrap_or(&class.name);
            if class.has_primary_ctor {
                let primary = render_primary_constructor_signature(
                    source,
                    &analysis.file,
                    class_name,
                    class,
                    class_signature
                        .map(|signature| signature.ctor_params.clone())
                        .unwrap_or_default(),
                );
                if !budget.reserve(&primary) {
                    return false;
                }
                let constructor_group = *group.get_or_insert_with(|| {
                    let group = self.groups.len();
                    self.groups.push(SignatureGroup::default());
                    group
                });
                self.groups[constructor_group].candidates.push(primary);
            }
            for (constructor_index, constructor) in class.secondary_ctors.iter().enumerate() {
                let candidate = render_secondary_constructor_signature(
                    source,
                    class_name,
                    constructor,
                    &analysis.file,
                    class_signature
                        .and_then(|signature| signature.secondary_ctors.get(constructor_index))
                        .cloned()
                        .unwrap_or_default(),
                );
                if !budget.reserve(&candidate) {
                    return false;
                }
                let constructor_group = *group.get_or_insert_with(|| {
                    let group = self.groups.len();
                    self.groups.push(SignatureGroup::default());
                    group
                });
                self.groups[constructor_group].candidates.push(candidate);
            }
            if let Some(group) = group {
                self.constructors_by_qualified
                    .insert((package.to_string(), class.name.clone()), group);
                self.constructors_by_simple
                    .entry(class_name.to_string())
                    .or_default()
                    .push(group);
            }
        }

        let mut method_offsets = HashMap::<&str, usize>::new();
        for method in &class.methods {
            let offset = method_offsets.entry(&method.name).or_default();
            let semantic_signature = class_signature.and_then(|class_signature| {
                class_signature.methods_named(&method.name).get(*offset)
            });
            *offset += 1;
            let candidate = render_function_signature(
                source,
                analysis,
                method,
                semantic_signature
                    .map(|signature| signature.params.clone())
                    .unwrap_or_default(),
                semantic_signature.map(|signature| signature.ret),
            );
            if !budget.reserve(&candidate) {
                return false;
            }
            let key = (owner, method.name.clone());
            let group = if let Some(&group) = self.members.get(&key) {
                group
            } else {
                let group = self.groups.len();
                self.groups.push(SignatureGroup::default());
                self.members.insert(key, group);
                group
            };
            self.groups[group].candidates.push(candidate);
        }
        true
    }

    pub(crate) fn group(&self, group: usize) -> &[SignatureCandidate] {
        self.groups
            .get(group)
            .map_or(&[], |group| group.candidates.as_slice())
    }

    pub(crate) fn call_sites(
        &self,
        source: &str,
        analysis: &FileAnalysis,
        max_calls: usize,
    ) -> Vec<SignatureHelpCallSite> {
        if max_calls == 0 {
            return Vec::new();
        }
        let mut calls = Vec::with_capacity(analysis.file.expr_arena.len().min(max_calls));
        for (index, expression) in analysis.file.expr_arena.iter().enumerate() {
            if calls.len() >= max_calls {
                break;
            }
            let call = ExprId(index as u32);
            let callee = match expression {
                Expr::Call { callee, .. } => Some(*callee),
                Expr::SafeCall {
                    receiver: _,
                    name: _,
                    args: Some(_),
                } => None,
                _ => continue,
            };
            let Some(span) = call_source_span(source, &analysis.file, call, callee) else {
                continue;
            };
            calls.push(SignatureHelpCallSite { call, span });
        }
        calls.sort_by_key(|site| (site.span.lo, std::cmp::Reverse(site.span.hi)));
        calls
    }

    pub(crate) fn call(
        &self,
        source: &str,
        analysis: &FileAnalysis,
        symbols: &FrontendSymbols,
        site: SignatureHelpCallSite,
        max_argument_wire_bytes: usize,
    ) -> Result<Option<SignatureHelpCall>, ()> {
        let (callee, arguments) = match analysis.file.expr(site.call) {
            Expr::Call { callee, args } => (Some(*callee), args.as_slice()),
            Expr::SafeCall {
                receiver: _,
                name: _,
                args: Some(args),
            } => (None, args.as_slice()),
            _ => return Ok(None),
        };
        let Some(resolved) = self.group_for_call(analysis, symbols, site.call, callee) else {
            return Ok(None);
        };
        let ResolvedSignatureCall {
            group,
            selected,
            local_function,
            generic_resolution,
        } = resolved;
        if local_function.is_none() && self.group(group).is_empty() {
            return Ok(None);
        }
        let arguments = call_source_arguments(
            source,
            &analysis.file,
            site.call,
            site.span,
            arguments,
            max_argument_wire_bytes,
        )?;
        let candidate_count = if local_function.is_some() {
            1
        } else {
            self.group(group).len()
        };
        Ok(Some(SignatureHelpCall {
            call: site.call,
            span: site.span,
            group,
            selected: selected.min(candidate_count.saturating_sub(1)),
            arguments,
            local_function,
            generic_resolution,
        }))
    }

    fn group_for_call(
        &self,
        analysis: &FileAnalysis,
        symbols: &FrontendSymbols,
        call: ExprId,
        callee: Option<ExprId>,
    ) -> Option<ResolvedSignatureCall> {
        let types = analysis.types.as_ref();
        if let Some(resolved) = types.and_then(|types| types.resolved_local_function(call)) {
            let Stmt::LocalFun(_) = analysis.file.stmt(resolved.stmt_id) else {
                return None;
            };
            return Some(ResolvedSignatureCall {
                group: usize::MAX,
                selected: 0,
                local_function: Some(resolved.stmt_id),
                generic_resolution: None,
            });
        }
        if let Some(source) = types.and_then(|types| types.resolved_source_call(call)) {
            if let Some(&target) = self.top_by_source.get(&source) {
                let generic_resolution = if let Some(resolved) =
                    types.and_then(|types| types.resolved_module_top_level(call))
                {
                    let base = &self.groups[target.0].candidates[target.1];
                    if !base.type_parameters.is_empty() {
                        Some((resolved.params.clone(), resolved.ret))
                    } else {
                        None
                    }
                } else {
                    None
                };
                return Some(ResolvedSignatureCall {
                    group: target.0,
                    selected: target.1,
                    local_function: None,
                    generic_resolution,
                });
            }
        }
        if let Some((owner, name, parameters)) =
            types.and_then(|types| types.resolved_module_member_signature(call))
        {
            let group = *self.members.get(&(owner, name.to_string()))?;
            let selected = self.groups[group]
                .candidates
                .iter()
                .position(|candidate| candidate.semantic_parameters == parameters)
                .unwrap_or(0);
            return Some(ResolvedSignatureCall {
                group,
                selected,
                local_function: None,
                generic_resolution: None,
            });
        }

        let callee = callee?;
        match analysis.file.expr(callee) {
            Expr::Name(name) => {
                if let Some(group) = self.constructor_group(&analysis.file, name) {
                    let selected =
                        self.select_constructor_candidate(analysis, symbols, call, group);
                    return Some(ResolvedSignatureCall {
                        group,
                        selected,
                        local_function: None,
                        generic_resolution: None,
                    });
                }
                self.top_group(&analysis.file, name)
                    .map(|group| ResolvedSignatureCall {
                        group,
                        selected: 0,
                        local_function: None,
                        generic_resolution: None,
                    })
            }
            Expr::Member { name, .. } => types
                .and_then(|types| types.resolved_module_member_signature(call))
                .and_then(|(owner, _, parameters)| {
                    let group = *self.members.get(&(owner, name.clone()))?;
                    let selected = self.groups[group]
                        .candidates
                        .iter()
                        .position(|candidate| candidate.semantic_parameters == parameters)
                        .unwrap_or(0);
                    Some(ResolvedSignatureCall {
                        group,
                        selected,
                        local_function: None,
                        generic_resolution: None,
                    })
                }),
            _ => None,
        }
    }

    pub(crate) fn candidates_for_call<'a>(
        &'a self,
        source: &str,
        analysis: &FileAnalysis,
        symbols: &FrontendSymbols,
        call: &SignatureHelpCall,
    ) -> Cow<'a, [SignatureCandidate]> {
        let has_named_arguments = call
            .arguments
            .iter()
            .any(|argument| argument.name.is_some());
        if call.local_function.is_none()
            && call.generic_resolution.is_none()
            && !has_named_arguments
        {
            return Cow::Borrowed(self.group(call.group));
        }

        let mut candidates = if let Some(statement) = call.local_function {
            match analysis.file.stmt(statement) {
                Stmt::LocalFun(function) => {
                    let signature = analysis
                        .types
                        .as_ref()
                        .and_then(|types| types.resolved_local_function(call.call))
                        .map(|resolved| &resolved.sig);
                    vec![render_function_signature(
                        source,
                        analysis,
                        function,
                        signature
                            .map(|signature| signature.params.clone())
                            .unwrap_or_default(),
                        signature.map(|signature| signature.ret),
                    )]
                }
                _ => Vec::new(),
            }
        } else {
            self.group(call.group).to_vec()
        };

        if let Some((resolved_parameters, resolved_return)) = &call.generic_resolution {
            if let Some(candidate) = candidates.get_mut(call.selected) {
                *candidate = specialize_candidate(
                    analysis,
                    symbols,
                    call.call,
                    candidate,
                    resolved_parameters,
                    *resolved_return,
                );
            }
        }
        if has_named_arguments {
            candidates = candidates
                .into_iter()
                .map(|candidate| named_argument_candidate(candidate, &call.arguments))
                .collect();
        }
        Cow::Owned(candidates)
    }

    pub(crate) fn call_shares_group(call: &SignatureHelpCall) -> bool {
        call.local_function.is_none()
            && call.generic_resolution.is_none()
            && !call
                .arguments
                .iter()
                .any(|argument| argument.name.is_some())
    }

    fn select_constructor_candidate(
        &self,
        analysis: &FileAnalysis,
        symbols: &FrontendSymbols,
        call: ExprId,
        group: usize,
    ) -> usize {
        let Expr::Call { args, .. } = analysis.file.expr(call) else {
            return 0;
        };
        let Some(types) = analysis.types.as_ref() else {
            return 0;
        };
        let names = analysis.file.call_arg_names.get(&call.0);
        let matcher = symbols.source_constructor_matcher();
        self.group(group)
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                constructor_candidate_score(&matcher, candidate, args, names, &types.expr_types)
                    .map(|score| (score, std::cmp::Reverse(index), index))
            })
            .max()
            .map_or(0, |(_, _, index)| index)
    }

    fn top_group(&self, file: &File, name: &str) -> Option<usize> {
        let package = file.package.as_deref().unwrap_or_default();
        if let Some(&group) =
            self.top_by_qualified
                .get(&(package.to_string(), name.to_string(), String::new()))
        {
            return Some(group);
        }
        for import in &file.imports {
            if import.rsplit('.').next() == Some(name) {
                let imported_package = import.rsplit_once('.').map_or("", |(package, _)| package);
                if let Some(&group) = self.top_by_qualified.get(&(
                    imported_package.to_string(),
                    name.to_string(),
                    String::new(),
                )) {
                    return Some(group);
                }
            }
        }
        self.top_by_simple
            .get(name)
            .and_then(|groups| (groups.len() == 1).then_some(groups[0]))
    }

    fn constructor_group(&self, file: &File, name: &str) -> Option<usize> {
        let package = file.package.as_deref().unwrap_or_default();
        if let Some(&group) = self
            .constructors_by_qualified
            .get(&(package.to_string(), name.to_string()))
        {
            return Some(group);
        }
        for import in &file.imports {
            if import.rsplit('.').next() == Some(name) {
                let imported_package = import.rsplit_once('.').map_or("", |(package, _)| package);
                if let Some(&group) = self
                    .constructors_by_qualified
                    .get(&(imported_package.to_string(), name.to_string()))
                {
                    return Some(group);
                }
            }
        }
        self.constructors_by_simple
            .get(name)
            .and_then(|groups| (groups.len() == 1).then_some(groups[0]))
    }
}

fn render_function_signature(
    source: &str,
    analysis: &FileAnalysis,
    function: &FunDecl,
    semantic_parameters: Vec<Ty>,
    semantic_return: Option<Ty>,
) -> SignatureCandidate {
    let context_count = function.context_count.min(function.params.len());
    let parameters = &function.params[context_count..];
    let semantic_parameters = semantic_parameters
        .into_iter()
        .skip(context_count)
        .collect();
    let rendered_parameters = parameters
        .iter()
        .map(|parameter| render_parameter(source, &analysis.file, parameter))
        .collect::<Vec<_>>();
    let return_type = function
        .ret
        .as_ref()
        .map(render_type)
        .or_else(|| inferred_function_return(analysis, function).map(render_ty))
        .unwrap_or_else(|| "Unit".to_string());
    let mut candidate = render_signature(
        &function.name,
        rendered_parameters,
        Some(&return_type),
        semantic_parameters,
        semantic_return,
        function.type_params.clone(),
    );
    if !function.type_params.is_empty() {
        candidate.declared_parameters = parameters
            .iter()
            .map(|parameter| parameter.ty.clone())
            .collect();
        candidate.declared_return = function.ret.clone();
    }
    candidate
}

fn render_primary_constructor_signature(
    source: &str,
    file: &File,
    name: &str,
    class: &ClassDecl,
    semantic_parameters: Vec<Ty>,
) -> SignatureCandidate {
    let parameters = class
        .props
        .iter()
        .map(|parameter| {
            render_parameter_parts(
                source,
                &parameter.name,
                &render_type(&parameter.ty),
                parameter.is_vararg,
                parameter.default,
                Some(file),
            )
        })
        .collect();
    let mut candidate = render_signature(
        name,
        parameters,
        None,
        semantic_parameters,
        None,
        Vec::new(),
    );
    candidate.primary_constructor = true;
    candidate
}

fn render_secondary_constructor_signature(
    source: &str,
    name: &str,
    constructor: &SecondaryCtor,
    file: &File,
    semantic_parameters: Vec<Ty>,
) -> SignatureCandidate {
    let parameters = constructor
        .params
        .iter()
        .map(|parameter| render_parameter(source, file, parameter))
        .collect();
    render_signature(
        name,
        parameters,
        None,
        semantic_parameters,
        None,
        Vec::new(),
    )
}

fn render_parameter(source: &str, file: &File, parameter: &Param) -> (String, String) {
    render_parameter_parts(
        source,
        &parameter.name,
        &render_type(&parameter.ty),
        parameter.is_vararg,
        parameter.default,
        Some(file),
    )
}

fn render_parameter_parts(
    source: &str,
    name: &str,
    ty: &str,
    vararg: bool,
    default: Option<ExprId>,
    file: Option<&File>,
) -> (String, String) {
    let mut label = String::new();
    if vararg {
        label.push_str("vararg ");
    }
    label.push_str(name);
    label.push_str(": ");
    label.push_str(ty);
    if let Some(default) = default {
        label.push_str(" = ");
        let value = file
            .and_then(|file| file.expr_span(default))
            .and_then(|span| source.get(span.lo as usize..span.hi as usize))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("...");
        label.push_str(value);
    }
    (name.to_string(), label)
}

fn render_signature(
    name: &str,
    parameters: Vec<(String, String)>,
    return_type: Option<&str>,
    semantic_parameters: Vec<Ty>,
    semantic_return: Option<Ty>,
    type_parameters: Vec<String>,
) -> SignatureCandidate {
    let mut label = String::new();
    label.push_str(name);
    label.push('(');
    let mut utf16_offset = utf16_len(name).saturating_add(1);
    let mut rendered_parameters = Vec::with_capacity(parameters.len());
    for (index, (name, parameter)) in parameters.into_iter().enumerate() {
        if index > 0 {
            label.push_str(", ");
            utf16_offset = utf16_offset.saturating_add(2);
        }
        let byte_start = label.len() as u32;
        let start = utf16_offset;
        label.push_str(&parameter);
        utf16_offset = utf16_offset.saturating_add(utf16_len(&parameter));
        let byte_end = label.len() as u32;
        let end = utf16_offset;
        let declared_type = parameter_type(&parameter).unwrap_or_default().to_string();
        rendered_parameters.push(SignatureParameter {
            name,
            label_start: start,
            label_end: end,
            label_byte_start: byte_start,
            label_byte_end: byte_end,
            declared_type,
        });
    }
    label.push(')');
    if let Some(return_type) = return_type {
        label.push_str(": ");
        label.push_str(return_type);
    }
    SignatureCandidate {
        label,
        parameters: rendered_parameters,
        semantic_parameters,
        semantic_return,
        declared_parameters: Vec::new(),
        declared_return: None,
        name: name.to_string(),
        return_type: return_type.map(str::to_string),
        type_parameters,
        primary_constructor: false,
    }
}

fn parameter_type(parameter: &str) -> Option<&str> {
    let (_, rest) = parameter.split_once(": ")?;
    Some(rest.split_once(" = ").map_or(rest, |(ty, _)| ty))
}

fn specialize_candidate(
    analysis: &FileAnalysis,
    symbols: &FrontendSymbols,
    call: ExprId,
    candidate: &SignatureCandidate,
    resolved_parameters: &[Ty],
    resolved_return: Ty,
) -> SignatureCandidate {
    let bindings = inferred_generic_bindings(
        analysis,
        symbols,
        call,
        candidate,
        resolved_parameters,
        resolved_return,
    );
    let parameters = candidate
        .parameters
        .iter()
        .map(|parameter| {
            let display = &candidate.label
                [parameter.label_byte_start as usize..parameter.label_byte_end as usize];
            let specialized_type = specialize_type_text(&parameter.declared_type, &bindings, false);
            let display =
                replace_parameter_type(display, &parameter.declared_type, &specialized_type);
            (parameter.name.clone(), display)
        })
        .collect();
    let return_type = candidate
        .return_type
        .as_deref()
        .map(|return_type| specialize_type_text(return_type, &bindings, true));
    render_signature(
        &candidate.name,
        parameters,
        return_type.as_deref(),
        resolved_parameters.to_vec(),
        Some(resolved_return),
        Vec::new(),
    )
}

fn inferred_generic_bindings(
    analysis: &FileAnalysis,
    symbols: &FrontendSymbols,
    call: ExprId,
    candidate: &SignatureCandidate,
    resolved_parameters: &[Ty],
    resolved_return: Ty,
) -> HashMap<String, GenericBinding> {
    let matcher = symbols.source_constructor_matcher();
    let mut semantic_bindings = HashMap::new();
    for ((declared, semantic), resolved) in candidate
        .declared_parameters
        .iter()
        .zip(&candidate.semantic_parameters)
        .zip(resolved_parameters)
    {
        if *resolved != Ty::Error && !resolved.is_erased_top() {
            bind_declared_type_ref(
                declared,
                *semantic,
                *resolved,
                &candidate.type_parameters,
                &mut semantic_bindings,
                &matcher,
            );
        }
    }
    if let (Some(return_type), Some(semantic_return)) =
        (&candidate.declared_return, candidate.semantic_return)
    {
        if resolved_return != Ty::Error && !resolved_return.is_erased_top() {
            bind_declared_type_ref(
                return_type,
                semantic_return,
                resolved_return,
                &candidate.type_parameters,
                &mut semantic_bindings,
                &matcher,
            );
        }
    }
    let Expr::Call { args, .. } = analysis.file.expr(call) else {
        return semantic_bindings;
    };
    let Some(types) = analysis.types.as_ref() else {
        return semantic_bindings;
    };
    let mut argument_bindings = HashMap::new();
    let argument_names = analysis.file.call_arg_names.get(&call.0);
    for (argument_index, argument) in args.iter().enumerate() {
        let parameter_index = argument_names
            .and_then(|names| names.get(argument_index))
            .and_then(|name| name.as_deref())
            .and_then(|name| {
                candidate
                    .parameters
                    .iter()
                    .position(|parameter| parameter.name == name)
            })
            .unwrap_or(argument_index);
        let Some((declared, semantic)) = candidate
            .declared_parameters
            .get(parameter_index)
            .zip(candidate.semantic_parameters.get(parameter_index))
        else {
            continue;
        };
        let Some(actual) = types.expr_types.get(argument.0 as usize).copied() else {
            continue;
        };
        if actual != Ty::Error {
            bind_declared_type_ref(
                declared,
                *semantic,
                actual,
                &candidate.type_parameters,
                &mut argument_bindings,
                &matcher,
            );
        }
    }
    for (parameter, binding) in semantic_bindings {
        argument_bindings.entry(parameter).or_insert(binding);
    }
    argument_bindings
}

fn bind_declared_type_ref(
    declared: &TypeRef,
    semantic: Ty,
    actual: Ty,
    type_parameters: &[String],
    bindings: &mut HashMap<String, GenericBinding>,
    matcher: &krusty::frontend::SourceConstructorMatcher<'_>,
) {
    let actual = if declared.nullable() {
        match actual {
            Ty::Nullable(inner) => *inner,
            Ty::Null => return,
            actual => actual,
        }
    } else {
        actual
    };
    if actual == Ty::Null {
        return;
    }
    if declared.targs.is_empty()
        && declared.arg.is_none()
        && declared.fun_params.is_empty()
        && type_parameters
            .iter()
            .any(|parameter| parameter == &declared.name)
    {
        merge_generic_binding(bindings, &declared.name, actual, Some(matcher));
        return;
    }
    if declared.name == "<fun>" {
        let (Ty::Fun(semantic), Ty::Fun(actual)) = (semantic.non_null(), actual) else {
            return;
        };
        if declared.fun_params.len() != actual.params.len()
            || semantic.params.len() != actual.params.len()
        {
            return;
        }
        for ((declared, &semantic), &actual) in declared
            .fun_params
            .iter()
            .zip(&semantic.params)
            .zip(&actual.params)
        {
            bind_declared_type_ref(
                declared,
                semantic,
                actual,
                type_parameters,
                bindings,
                matcher,
            );
        }
        if let Some(declared) = declared.arg.as_deref() {
            bind_declared_type_ref(
                declared,
                semantic.ret,
                actual.ret,
                type_parameters,
                bindings,
                matcher,
            );
        }
        return;
    }
    let (Ty::Obj(semantic_name, semantic_arguments), Ty::Obj(actual_name, actual_arguments)) =
        (semantic.non_null(), actual)
    else {
        return;
    };
    if !matcher.type_names_match(semantic_name, actual_name) {
        return;
    }
    let declared_arguments = if declared.targs.is_empty() {
        declared.arg.iter().map(Box::as_ref).collect::<Vec<_>>()
    } else {
        declared.targs.iter().collect::<Vec<_>>()
    };
    if declared_arguments.len() != semantic_arguments.len()
        || declared_arguments.len() != actual_arguments.len()
    {
        return;
    }
    for ((declared, &semantic), &actual) in declared_arguments
        .into_iter()
        .zip(semantic_arguments)
        .zip(actual_arguments)
    {
        bind_declared_type_ref(
            declared,
            semantic,
            actual,
            type_parameters,
            bindings,
            matcher,
        );
    }
}

#[derive(Debug)]
struct GenericBinding {
    parameter: String,
    result: String,
    types: Vec<Ty>,
}

fn merge_generic_binding(
    bindings: &mut HashMap<String, GenericBinding>,
    parameter: &str,
    inferred: Ty,
    matcher: Option<&krusty::frontend::SourceConstructorMatcher<'_>>,
) {
    use std::collections::hash_map::Entry;

    match bindings.entry(parameter.to_string()) {
        Entry::Vacant(entry) => {
            let rendered = render_ty(inferred);
            entry.insert(GenericBinding {
                parameter: rendered.clone(),
                result: rendered,
                types: vec![inferred],
            });
        }
        Entry::Occupied(mut entry) => {
            entry.get_mut().types.push(inferred);
            let types = &entry.get().types;
            let nullable = types.iter().any(|ty| ty.is_nullable());
            let rendered = types
                .iter()
                .map(|ty| render_ty(ty.non_null()))
                .collect::<Vec<_>>();
            if rendered.iter().all(|ty| ty == &rendered[0]) {
                let resolved = format!("{}{}", rendered[0], if nullable { "?" } else { "" });
                let binding = entry.get_mut();
                binding.parameter = resolved.clone();
                binding.result = resolved;
                return;
            }
            let constraints = matcher
                .map(|matcher| {
                    matcher.common_supertypes(
                        &types.iter().map(|ty| ty.non_null()).collect::<Vec<_>>(),
                    )
                })
                .unwrap_or_default();
            let common = constraints
                .iter()
                .map(|constraint| {
                    let name = matcher
                        .map(|matcher| matcher.source_name(constraint.name))
                        .unwrap_or_else(|| constraint.name.segment().replace('$', "."));
                    if constraint.type_parameters == 0 {
                        name
                    } else {
                        format!(
                            "{}<{}>",
                            name,
                            vec!["*"; constraint.type_parameters].join(", ")
                        )
                    }
                })
                .collect::<Vec<_>>();
            let fallback = if nullable { "Any?" } else { "Any" };
            let parameter = if !nullable && !common.is_empty() {
                common.join(" & ")
            } else if nullable && common.len() == 1 && common[0] != "Any" {
                format!("{}?", common[0])
            } else {
                fallback.to_string()
            };
            let result = if common.len() == 1 && common[0] != "Any" {
                format!("{}{}", common[0], if nullable { "?" } else { "" })
            } else {
                fallback.to_string()
            };
            let binding = entry.get_mut();
            binding.parameter = parameter;
            binding.result = result;
        }
    }
}

fn specialize_type_text(
    ty: &str,
    bindings: &HashMap<String, GenericBinding>,
    result_position: bool,
) -> String {
    if bindings.is_empty() {
        return ty.to_string();
    }
    let mut result = String::with_capacity(ty.len());
    let mut offset = 0;
    while offset < ty.len() {
        let character = ty[offset..]
            .chars()
            .next()
            .expect("offset is a char boundary");
        if character == '_' || character.is_alphabetic() {
            let start = offset;
            offset += character.len_utf8();
            while offset < ty.len() {
                let continuation = ty[offset..]
                    .chars()
                    .next()
                    .expect("offset is a char boundary");
                if continuation == '_' || continuation.is_alphanumeric() {
                    offset += continuation.len_utf8();
                } else {
                    break;
                }
            }
            let identifier = &ty[start..offset];
            result.push_str(bindings.get(identifier).map_or(identifier, |binding| {
                if result_position {
                    binding.result.as_str()
                } else {
                    binding.parameter.as_str()
                }
            }));
        } else {
            result.push(character);
            offset += character.len_utf8();
        }
    }
    result
}

fn replace_parameter_type(display: &str, old: &str, new: &str) -> String {
    let Some(type_start) = display.find(": ").map(|start| start + 2) else {
        return display.to_string();
    };
    let type_end = display[type_start..]
        .find(" = ")
        .map_or(display.len(), |end| type_start + end);
    if &display[type_start..type_end] != old {
        return display.to_string();
    }
    let mut result = String::with_capacity(display.len() + new.len().saturating_sub(old.len()));
    result.push_str(&display[..type_start]);
    result.push_str(new);
    result.push_str(&display[type_end..]);
    result
}

fn named_argument_candidate(
    candidate: SignatureCandidate,
    arguments: &[SignatureArgument],
) -> SignatureCandidate {
    let named_arguments = arguments
        .iter()
        .filter_map(|argument| argument.name.as_deref())
        .collect::<Vec<_>>();
    let all_names_match = named_arguments.iter().all(|name| {
        candidate
            .parameters
            .iter()
            .any(|parameter| parameter.name == *name)
    });
    let mut order = Vec::with_capacity(candidate.parameters.len());
    let mut seen = vec![false; candidate.parameters.len()];
    if all_names_match {
        for name in &named_arguments {
            if let Some(index) = candidate
                .parameters
                .iter()
                .position(|parameter| parameter.name == *name)
            {
                if !seen[index] {
                    seen[index] = true;
                    order.push(index);
                }
            }
        }
    }
    for (parameter, was_seen) in seen.iter().enumerate() {
        if !was_seen {
            order.push(parameter);
        }
    }
    let parameters = order
        .into_iter()
        .map(|index| {
            let parameter = &candidate.parameters[index];
            let display = &candidate.label
                [parameter.label_byte_start as usize..parameter.label_byte_end as usize];
            (parameter.name.clone(), format!("[{display}]"))
        })
        .collect();
    render_signature(
        &candidate.name,
        parameters,
        candidate.return_type.as_deref(),
        candidate.semantic_parameters,
        candidate.semantic_return,
        candidate.type_parameters,
    )
}

fn constructor_candidate_score(
    matcher: &krusty::frontend::SourceConstructorMatcher<'_>,
    candidate: &SignatureCandidate,
    arguments: &[ExprId],
    argument_names: Option<&Vec<Option<String>>>,
    expression_types: &[Ty],
) -> Option<usize> {
    if arguments.len() > candidate.parameters.len() {
        return None;
    }
    let mut score = usize::from(arguments.len() == candidate.parameters.len()) * 10_000
        + usize::from(candidate.primary_constructor) * 1_000;
    for (argument_index, argument) in arguments.iter().enumerate() {
        let parameter_index = argument_names
            .and_then(|names| names.get(argument_index))
            .and_then(|name| name.as_deref())
            .map(|name| {
                candidate
                    .parameters
                    .iter()
                    .position(|parameter| parameter.name == name)
            })
            .unwrap_or(Some(argument_index))?;
        let actual = *expression_types.get(argument.0 as usize)?;
        let Some(expected) = candidate.semantic_parameters.get(parameter_index).copied() else {
            score = score.saturating_add(1);
            continue;
        };
        let compatibility = if expected == actual {
            4
        } else if matcher.argument_matches(expected, actual) {
            2
        } else {
            return None;
        };
        score = score.saturating_add(compatibility);
    }
    Some(score.saturating_sub(candidate.parameters.len().saturating_sub(arguments.len())))
}

fn inferred_function_return(analysis: &FileAnalysis, function: &FunDecl) -> Option<Ty> {
    let body = match function.body {
        FunBody::Expr(body) | FunBody::Block(body) => body,
        FunBody::None => return None,
    };
    analysis
        .types
        .as_ref()
        .and_then(|types| types.expr_types.get(body.0 as usize))
        .copied()
        .filter(|ty| *ty != Ty::Error)
}

fn class_owner(package: &str, name: &str) -> TypeName {
    let class = name.replace('.', "$");
    type_name(&if package.is_empty() {
        class
    } else {
        format!("{}/{class}", package.replace('.', "/"))
    })
}

fn call_source_span(
    source: &str,
    file: &File,
    call: ExprId,
    callee: Option<ExprId>,
) -> Option<Span> {
    let call_span = *file.expr_spans.get(call.0 as usize)?;
    let scan_start = callee
        .and_then(|callee| file.expr_spans.get(callee.0 as usize))
        .map_or(call_span.lo, |span| span.hi) as usize;
    let bytes = source.as_bytes();
    let end = (call_span.hi as usize).min(bytes.len());
    let open = find_open_parenthesis(bytes, scan_start, end)?;
    let close = matching_parenthesis(bytes, open, end).unwrap_or(end);
    Some(Span::new((open + 1) as u32, close as u32))
}

fn call_source_arguments(
    source: &str,
    file: &File,
    call: ExprId,
    span: Span,
    arguments: &[ExprId],
    max_wire_bytes: usize,
) -> Result<Vec<SignatureArgument>, ()> {
    if max_wire_bytes < 16 {
        return Err(());
    }
    let bytes = source.as_bytes();
    let close = span.hi as usize;
    let mut in_parentheses = arguments
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, argument)| {
            file.expr_spans
                .get(argument.0 as usize)
                .is_some_and(|span| span.lo as usize <= close)
        })
        .peekable();
    let argument_names = file.call_arg_names.get(&call.0);
    let capacity = arguments.len().min(max_wire_bytes / 16).max(1);
    let mut result = Vec::with_capacity(capacity);
    let mut used_wire_bytes = 0usize;
    let Some((mut original_index, mut argument)) = in_parentheses.next() else {
        push_signature_argument(
            &mut result,
            &mut used_wire_bytes,
            max_wire_bytes,
            close as u32,
            None,
        )?;
        return Ok(result);
    };
    loop {
        let end = if let Some((_, next)) = in_parentheses.peek() {
            let current = file.expr_spans[argument.0 as usize];
            let next = file.expr_spans[next.0 as usize];
            find_separator_comma(bytes, current.hi as usize, next.lo as usize)
                .unwrap_or(next.lo as usize)
        } else {
            close
        };
        let name = argument_names
            .and_then(|names| names.get(original_index))
            .and_then(Option::as_deref);
        push_signature_argument(
            &mut result,
            &mut used_wire_bytes,
            max_wire_bytes,
            end as u32,
            name,
        )?;
        let Some((next_index, next_argument)) = in_parentheses.next() else {
            let last = file.expr_spans[argument.0 as usize];
            if let Some(comma) = find_separator_comma(bytes, last.hi as usize, close) {
                if let Some(argument) = result.last_mut() {
                    argument.end = comma as u32;
                }
                push_signature_argument(
                    &mut result,
                    &mut used_wire_bytes,
                    max_wire_bytes,
                    close as u32,
                    None,
                )?;
            }
            break;
        };
        original_index = next_index;
        argument = next_argument;
    }
    Ok(result)
}

fn push_signature_argument(
    result: &mut Vec<SignatureArgument>,
    used_wire_bytes: &mut usize,
    max_wire_bytes: usize,
    end: u32,
    name: Option<&str>,
) -> Result<(), ()> {
    let wire_bytes = 16usize.saturating_add(name.map_or(0, str::len).saturating_mul(6));
    if wire_bytes > max_wire_bytes.saturating_sub(*used_wire_bytes) {
        return Err(());
    }
    *used_wire_bytes += wire_bytes;
    result.push(SignatureArgument {
        end,
        name: name.map(str::to_string),
    });
    Ok(())
}

fn find_separator_comma(bytes: &[u8], mut index: usize, end: usize) -> Option<usize> {
    while index < end {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index, end);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index, end);
            }
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, end),
            b',' => return Some(index),
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    None
}

fn find_open_parenthesis(bytes: &[u8], mut index: usize, end: usize) -> Option<usize> {
    while index < end {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = skip_line_comment(bytes, index, end);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index, end);
            }
            b'"' | b'\'' | b'`' => index = skip_quoted(bytes, index, end),
            b'(' => return Some(index),
            _ => index += utf8_char_len(bytes[index]),
        }
    }
    None
}

fn matching_parenthesis(bytes: &[u8], open: usize, end: usize) -> Option<usize> {
    matching_delimiter(bytes, open, end, b'(', b')')
}

fn utf16_len(value: &str) -> u32 {
    value.encode_utf16().count().try_into().unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::rc::Rc;

    use krusty::jvm::classpath::Classpath;
    use krusty::jvm::jvm_libraries::JvmLibraries;
    use krusty::types::Ty;

    use super::*;

    #[test]
    fn jvm_builtin_constraints_come_from_the_platform_type_oracle() {
        let analysis = super::super::analyze_source_set(
            &[""],
            Box::new(JvmLibraries::new(Rc::new(Classpath::new(Vec::new())))),
        );
        let matcher = analysis.symbols.source_constructor_matcher();
        assert!(matcher.type_names_match(type_name("left/Box"), type_name("left/Box")));
        assert!(!matcher.type_names_match(type_name("left/Box"), type_name("right/Box")));
        let constraints = matcher.common_supertypes(&[Ty::Int, Ty::String]);
        assert_eq!(
            constraints
                .iter()
                .map(|constraint| {
                    (
                        matcher.source_name(constraint.name),
                        constraint.type_parameters,
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("Comparable".to_string(), 1),
                ("Serializable".to_string(), 0)
            ]
        );
        assert!(matcher
            .common_supertypes(&[Ty::UInt, Ty::String])
            .iter()
            .all(|constraint| matcher.source_name(constraint.name) != "Serializable"));

        let mut bindings = HashMap::new();
        merge_generic_binding(&mut bindings, "T", Ty::Int, Some(&matcher));
        merge_generic_binding(&mut bindings, "T", Ty::String, Some(&matcher));
        assert_eq!(
            bindings.get("T").map(|binding| binding.parameter.as_str()),
            Some("Comparable<*> & Serializable")
        );
        assert_eq!(
            bindings.get("T").map(|binding| binding.result.as_str()),
            Some("Any")
        );
    }

    #[test]
    fn semantic_generic_bindings_keep_qualified_type_identities() {
        let source = "fun <T> identity(value: T): T = value\n";
        let analysis = super::super::analyze_source_set(
            &[source],
            Box::new(JvmLibraries::new(Rc::new(Classpath::new(Vec::new())))),
        );
        let declaration = analysis.files[0].file.decls[0];
        let signature = analysis
            .symbols
            .source_function_signature("identity", 0, declaration)
            .expect("source signature");
        assert!(signature.params[0].is_erased_top());
        assert!(signature.ret.is_erased_top());
        let Decl::Fun(function) = analysis.files[0].file.decl(declaration) else {
            panic!("identity function");
        };

        let matcher = analysis.symbols.source_constructor_matcher();
        let mut bindings = HashMap::new();
        bind_declared_type_ref(
            &function.params[0].ty,
            signature.params[0],
            Ty::Int,
            &function.type_params,
            &mut bindings,
            &matcher,
        );
        assert_eq!(bindings["T"].result, "Int");

        bindings.clear();
        let mut nullable_parameter = function.params[0].ty.clone();
        nullable_parameter.set_nullable(true);
        bind_declared_type_ref(
            &nullable_parameter,
            signature.params[0],
            Ty::nullable(Ty::Int),
            &function.type_params,
            &mut bindings,
            &matcher,
        );
        assert_eq!(bindings["T"].result, "Int");

        bindings.clear();
        let declared = TypeRef {
            name: "left.Box".to_string(),
            flags: krusty::ast::TrFlags::default(),
            arg: None,
            targs: vec![function.params[0].ty.clone()],
            span: Span::new(0, 0),
            fun_params: Vec::new(),
            fun_context_count: 0,
        };
        let semantic = Ty::obj_args("left/Box", &[Ty::obj("kotlin/Any")]);

        bind_declared_type_ref(
            &declared,
            semantic,
            Ty::obj_args("right/Box", &[Ty::Int]),
            &function.type_params,
            &mut bindings,
            &matcher,
        );
        assert!(bindings.is_empty());

        bind_declared_type_ref(
            &declared,
            semantic,
            Ty::obj_args("left/Box", &[Ty::Int]),
            &function.type_params,
            &mut bindings,
            &matcher,
        );
        assert_eq!(bindings["T"].result, "Int");
    }

    #[test]
    fn pending_call_sites_are_twelve_byte_compact_records() {
        assert_eq!(std::mem::size_of::<SignatureHelpCallSite>(), 12);
    }

    #[test]
    fn generic_binding_merge_preserves_nullability_and_falls_back_without_an_oracle() {
        let mut bindings = HashMap::new();
        merge_generic_binding(&mut bindings, "T", Ty::nullable(Ty::Int), None);
        assert_eq!(
            bindings.get("T").map(|binding| binding.parameter.as_str()),
            Some("Int?")
        );
        merge_generic_binding(&mut bindings, "T", Ty::Int, None);
        assert_eq!(
            bindings.get("T").map(|binding| binding.parameter.as_str()),
            Some("Int?")
        );

        bindings.clear();
        merge_generic_binding(&mut bindings, "T", Ty::nullable(Ty::Int), None);
        merge_generic_binding(&mut bindings, "T", Ty::String, None);
        let binding = bindings.get("T").unwrap();
        assert_eq!(binding.parameter, "Any?");
        assert_eq!(binding.result, "Any?");

        bindings.clear();
        merge_generic_binding(&mut bindings, "T", Ty::obj("demo/Left"), None);
        merge_generic_binding(&mut bindings, "T", Ty::obj("demo/Right"), None);
        let binding = bindings.get("T").unwrap();
        assert_eq!(binding.parameter, "Any");
        assert_eq!(binding.result, "Any");

        bindings.clear();
        merge_generic_binding(&mut bindings, "T", Ty::Int, None);
        merge_generic_binding(&mut bindings, "T", Ty::nullable(Ty::Int), None);
        assert_eq!(
            bindings.get("T").map(|binding| binding.parameter.as_str()),
            Some("Int?")
        );

        bindings.clear();
        merge_generic_binding(&mut bindings, "T", Ty::Int, None);
        merge_generic_binding(&mut bindings, "T", Ty::String, None);
        assert_eq!(
            bindings.get("T").map(|binding| binding.parameter.as_str()),
            Some("Any")
        );
        assert_eq!(
            bindings.get("T").map(|binding| binding.result.as_str()),
            Some("Any")
        );
    }

    #[test]
    fn specialization_and_parameter_rendering_cover_identifier_shapes() {
        let mut bindings = HashMap::new();
        bindings.insert(
            "T".to_string(),
            GenericBinding {
                parameter: "Comparable<*> & Serializable".to_string(),
                result: "Any".to_string(),
                types: vec![Ty::Int, Ty::String],
            },
        );
        assert_eq!(
            specialize_type_text("Map<T, _Missing2>?", &bindings, false),
            "Map<Comparable<*> & Serializable, _Missing2>?"
        );
        assert_eq!(
            specialize_type_text("Map<T, π>", &bindings, true),
            "Map<Any, π>"
        );
        assert_eq!(
            specialize_type_text("Name_With<T>", &bindings, true),
            "Name_With<Any>"
        );
        assert_eq!(specialize_type_text("T", &HashMap::new(), false), "T");

        assert_eq!(
            replace_parameter_type("value: T = null", "T", "String?"),
            "value: String? = null"
        );
        assert_eq!(replace_parameter_type("value", "T", "Int"), "value");
        assert_eq!(
            replace_parameter_type("value: String", "T", "Int"),
            "value: String"
        );
        assert_eq!(parameter_type("value"), None);
        assert_eq!(parameter_type("value: String = \"x\""), Some("String"));

        assert_eq!(
            render_parameter_parts("", "items", "Int", true, Some(ExprId(0)), None),
            ("items".to_string(), "vararg items: Int = ...".to_string())
        );
        assert_eq!(
            render_parameter_parts("", "value", "String", false, None, None),
            ("value".to_string(), "value: String".to_string())
        );
    }

    #[test]
    fn named_argument_reordering_and_catalog_budget_cover_both_outcomes() {
        let candidate = render_signature(
            "pick",
            vec![
                ("first".to_string(), "first: Int".to_string()),
                ("second".to_string(), "second: String".to_string()),
            ],
            Some("Unit"),
            vec![Ty::Int, Ty::obj("kotlin/String")],
            Some(Ty::Unit),
            Vec::new(),
        );
        let reordered = named_argument_candidate(
            candidate.clone(),
            &[
                SignatureArgument {
                    end: 1,
                    name: Some("second".to_string()),
                },
                SignatureArgument {
                    end: 2,
                    name: Some("second".to_string()),
                },
                SignatureArgument { end: 3, name: None },
            ],
        );
        assert_eq!(reordered.parameters[0].name, "second");
        assert_eq!(reordered.parameters[1].name, "first");

        let unchanged = named_argument_candidate(
            candidate.clone(),
            &[SignatureArgument {
                end: 1,
                name: Some("missing".to_string()),
            }],
        );
        assert_eq!(unchanged.parameters[0].name, "first");
        assert_eq!(unchanged.parameters[1].name, "second");

        let mut budget = CatalogBudget::default();
        assert!(budget.reserve(&candidate));
        budget.entries = MAX_SIGNATURE_CATALOG_ENTRIES;
        assert!(!budget.reserve(&candidate));
        budget.entries = 0;
        budget.text_bytes = MAX_SIGNATURE_CATALOG_TEXT_BYTES;
        assert!(!budget.reserve(&candidate));
    }

    #[test]
    fn source_delimiter_scanners_ignore_comments_quotes_and_all_utf8_widths() {
        let separators =
            "// hidden,\n/* hidden, /* nested, */ end */ \"hidden,\" 'h,' `h,` é中😀 , tail";
        assert_eq!(
            find_separator_comma(separators.as_bytes(), 0, separators.len()),
            separators.rfind(',')
        );
        assert_eq!(
            find_separator_comma(b"// no separator", 0, b"// no separator".len()),
            None
        );
        assert_eq!(find_separator_comma(b"/x,", 0, 3), Some(2));

        let openings = "// hidden(\n/* hidden( */ \"hidden(\" 'h(' `h(` é中😀 (tail";
        assert_eq!(
            find_open_parenthesis(openings.as_bytes(), 0, openings.len()),
            openings.rfind('(')
        );
        assert_eq!(
            find_open_parenthesis(b"/* unterminated", 0, b"/* unterminated".len()),
            None
        );
        assert_eq!(find_open_parenthesis(b"//", 0, 2), None);
        assert_eq!(find_open_parenthesis(b"/x(", 0, 3), Some(2));

        let nested = "(// hidden )\n/* hidden ) /* nested */ end */ \"()\" '()' `()` (x))";
        assert_eq!(
            matching_parenthesis(nested.as_bytes(), 0, nested.len()),
            nested.rfind(')')
        );
        assert_eq!(matching_parenthesis(b")", 0, 1), None);
        assert_eq!(matching_parenthesis(b"(x", 0, 2), None);
        assert_eq!(matching_parenthesis(b"(//", 0, 3), None);
        assert_eq!(matching_parenthesis(b"(/x)", 0, 4), Some(3));

        assert_eq!(
            skip_block_comment(b"/* outer /* nested */ end */tail", 0, 33),
            28
        );
        assert_eq!(skip_block_comment(b"/* open", 0, 7), 7);
        assert_eq!(skip_block_comment(b"*/tail", 0, 6), 2);
        assert_eq!(skip_block_comment(b"plain", 0, 5), 5);
        for quoted in [
            b"\"a\\\"b\"".as_slice(),
            b"'a'".as_slice(),
            b"`a`".as_slice(),
            b"\"\"\"a\"\"\"".as_slice(),
            b"\"open".as_slice(),
        ] {
            assert_eq!(skip_quoted(quoted, 0, quoted.len()), quoted.len());
        }

        assert_eq!(utf8_char_len(b'a'), 1);
        assert_eq!(utf8_char_len("é".as_bytes()[0]), 2);
        assert_eq!(utf8_char_len("中".as_bytes()[0]), 3);
        assert_eq!(utf8_char_len("😀".as_bytes()[0]), 4);
    }

    #[test]
    fn call_site_limits_argument_budget_and_group_sharing_cover_fast_exits() {
        let source = "fun take(value: Int): Int = value\nfun use(): Int = take(1) + take(2)\n";
        let analysis = super::super::analyze_standalone_source_set(&[source]);
        let symbols =
            SignatureHelpSymbols::from_source_set(&[source], &analysis.files, &analysis.symbols);
        assert!(symbols.call_sites(source, &analysis.files[0], 0).is_empty());
        let sites = symbols.call_sites(source, &analysis.files[0], 1);
        assert_eq!(sites.len(), 1);
        assert!(!symbols.group(0).is_empty());
        assert!(symbols.group(usize::MAX).is_empty());
        assert!(symbols
            .call(source, &analysis.files[0], &analysis.symbols, sites[0], 0)
            .is_err());

        let non_call = analysis.files[0]
            .file
            .expr_arena
            .iter()
            .position(|expression| {
                !matches!(
                    expression,
                    Expr::Call { .. } | Expr::SafeCall { args: Some(_), .. }
                )
            })
            .unwrap();
        assert!(symbols
            .call(
                source,
                &analysis.files[0],
                &analysis.symbols,
                SignatureHelpCallSite {
                    call: ExprId(non_call as u32),
                    span: Span::new(0, 0),
                },
                16,
            )
            .unwrap()
            .is_none());
        assert_eq!(
            symbols.select_constructor_candidate(
                &analysis.files[0],
                &analysis.symbols,
                ExprId(non_call as u32),
                0,
            ),
            0
        );

        let generic_candidate = render_signature(
            "identity",
            vec![("value".to_string(), "value: T".to_string())],
            Some("T"),
            vec![Ty::Error],
            None,
            vec!["T".to_string()],
        );
        assert!(inferred_generic_bindings(
            &analysis.files[0],
            &analysis.symbols,
            ExprId(non_call as u32),
            &generic_candidate,
            &[Ty::Error],
            Ty::Error,
        )
        .is_empty());
        assert!(inferred_generic_bindings(
            &analysis.files[0],
            &analysis.symbols,
            ExprId(non_call as u32),
            &generic_candidate,
            &[Ty::obj("kotlin/Any")],
            Ty::obj("kotlin/Any"),
        )
        .is_empty());
        let no_parameters = render_signature(
            "take",
            Vec::new(),
            Some("Unit"),
            Vec::new(),
            Some(Ty::Unit),
            Vec::new(),
        );
        assert!(inferred_generic_bindings(
            &analysis.files[0],
            &analysis.symbols,
            sites[0].call,
            &no_parameters,
            &[],
            Ty::Unit,
        )
        .is_empty());

        let mut untyped = super::super::analyze_standalone_source_set(&[source]);
        let mut untyped_symbols =
            SignatureHelpSymbols::from_source_set(&[source], &untyped.files, &untyped.symbols);
        let untyped_site = untyped_symbols.call_sites(source, &untyped.files[0], 1)[0];
        untyped.files[0].types = None;
        assert_eq!(
            untyped_symbols.select_constructor_candidate(
                &untyped.files[0],
                &untyped.symbols,
                untyped_site.call,
                0,
            ),
            0
        );
        for group in &mut untyped_symbols.groups {
            group.candidates.clear();
        }
        assert!(untyped_symbols
            .call(
                source,
                &untyped.files[0],
                &untyped.symbols,
                untyped_site,
                16,
            )
            .unwrap()
            .is_none());

        let unresolved_source = "fun use(): Int = missing(1)\n";
        let unresolved = super::super::analyze_standalone_source_set(&[unresolved_source]);
        let unresolved_symbols = SignatureHelpSymbols::from_source_set(
            &[unresolved_source],
            &unresolved.files,
            &unresolved.symbols,
        );
        let unresolved_site =
            unresolved_symbols.call_sites(unresolved_source, &unresolved.files[0], 1)[0];
        assert!(unresolved_symbols
            .call(
                unresolved_source,
                &unresolved.files[0],
                &unresolved.symbols,
                unresolved_site,
                16,
            )
            .unwrap()
            .is_none());

        let mut arguments = Vec::new();
        let mut used = 0;
        assert!(push_signature_argument(&mut arguments, &mut used, 15, 1, None).is_err());
        assert!(push_signature_argument(&mut arguments, &mut used, 22, 2, Some("x")).is_ok());
        assert!(push_signature_argument(&mut arguments, &mut used, 22, 3, None).is_err());

        let plain = SignatureHelpCall {
            call: ExprId(0),
            span: Span::new(0, 1),
            group: 0,
            selected: 0,
            arguments: Vec::new(),
            local_function: None,
            generic_resolution: None,
        };
        assert!(SignatureHelpSymbols::call_shares_group(&plain));

        let local = SignatureHelpCall {
            local_function: Some(StmtId(0)),
            ..plain
        };
        assert!(!SignatureHelpSymbols::call_shares_group(&local));

        let generic = SignatureHelpCall {
            local_function: None,
            generic_resolution: Some((vec![Ty::Int], Ty::Int)),
            ..local
        };
        assert!(!SignatureHelpSymbols::call_shares_group(&generic));
        let missing_candidate = SignatureHelpCall {
            call: generic.call,
            span: generic.span,
            group: 0,
            selected: usize::MAX,
            arguments: Vec::new(),
            local_function: None,
            generic_resolution: Some((vec![Ty::Int], Ty::Int)),
        };
        assert!(!symbols
            .candidates_for_call(
                source,
                &analysis.files[0],
                &analysis.symbols,
                &missing_candidate,
            )
            .is_empty());

        let named = SignatureHelpCall {
            generic_resolution: None,
            arguments: vec![SignatureArgument {
                end: 1,
                name: Some("value".to_string()),
            }],
            ..generic
        };
        assert!(!SignatureHelpSymbols::call_shares_group(&named));
    }
}
