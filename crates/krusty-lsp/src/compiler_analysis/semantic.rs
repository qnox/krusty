//! Semantic symbol classification over checked frontend data.

use std::{cmp::Reverse, collections::HashMap};

use krusty::ast::{
    ClassDecl, ClassKind, Decl, Expr, ExprId, File, FunBody, FunDecl, Param, PropDecl, PropParam,
    Stmt, TypeRef,
};
use krusty::diag::{DiagSink, Span};
use krusty::frontend::{
    lex_name_tokens, FrontendNameToken, FrontendNameTokenKind, FrontendSymbols, FrontendTypeInfo,
};
use krusty::types::Ty;

use super::{
    navigation::{declaration_name_span, definition_name_span, MemberKind},
    DefinitionOccurrence, DefinitionSymbols, DefinitionTarget, FileAnalysis,
};

/// Editor-neutral semantic categories. Discriminants intentionally follow the LSP 3.17 predefined
/// legend, so an LSP adapter can serialize the compact value without a lookup table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum HighlightKind {
    Namespace = 0,
    Class = 1,
    Enum = 2,
    Interface = 3,
    Struct = 4,
    TypeParameter = 5,
    Type = 6,
    Parameter = 7,
    Variable = 8,
    Property = 9,
    EnumMember = 10,
    Function = 12,
    Method = 13,
    Operator = 21,
    Decorator = 22,
}

/// Editor-neutral semantic modifiers. Bits intentionally follow the LSP 3.17 predefined legend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HighlightModifiers(u16);

impl HighlightModifiers {
    pub const DECLARATION: u16 = 1 << 0;
    pub const READONLY: u16 = 1 << 2;
    pub const STATIC: u16 = 1 << 3;
    pub const DEPRECATED: u16 = 1 << 4;
    pub const ABSTRACT: u16 = 1 << 5;
    pub const ASYNC: u16 = 1 << 6;
    pub const MODIFICATION: u16 = 1 << 7;
    pub const DEFAULT_LIBRARY: u16 = 1 << 9;

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }
}

/// One classified source name. The compiler AST and type tables can be dropped after these are built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightOccurrence {
    pub span: Span,
    pub kind: HighlightKind,
    pub modifiers: HighlightModifiers,
}

pub struct SemanticOccurrences {
    pub highlights: Vec<HighlightOccurrence>,
    pub definitions: Vec<DefinitionOccurrence>,
}

struct SemanticClassifier<'a> {
    source: &'a str,
    file: &'a File,
    file_index: u32,
    symbols: &'a FrontendSymbols,
    type_info: Option<&'a FrontendTypeInfo>,
    tokens: Vec<FrontendNameToken>,
    classified: Vec<Option<HighlightOccurrence>>,
    definitions: Vec<DefinitionOccurrence>,
    definition_limit: usize,
    token_by_span: HashMap<(u32, u32), usize>,
    statement_scopes: HashMap<(u32, u32), Span>,
    callees: HashMap<ExprId, ExprId>,
    highlight_symbols: &'a HighlightSymbols,
    definition_symbols: &'a DefinitionSymbols,
    bindings: Vec<Binding>,
    properties: HashMap<String, u16>,
    functions: HashMap<String, u16>,
}

struct SemanticContext<'a> {
    file_index: u32,
    symbols: &'a FrontendSymbols,
    type_info: Option<&'a FrontendTypeInfo>,
    highlight_symbols: &'a HighlightSymbols,
    definition_symbols: &'a DefinitionSymbols,
    definition_limit: usize,
}

struct Binding {
    name: String,
    scope: Span,
    declared_at: u32,
    kind: HighlightKind,
    modifiers: u16,
    definition: Option<Span>,
    definition_owner: Option<String>,
}

#[derive(Clone, Copy)]
struct MemberHighlight {
    kind: HighlightKind,
    modifiers: u16,
}

/// Source-set semantic metadata that compiler signatures intentionally do not retain (for example,
/// `data`, `operator`, and source deprecation). One shared table keeps cross-file editor
/// classification exact without adding editor concerns to the compiler's public symbol ABI.
pub struct HighlightSymbols {
    class_kinds: HashMap<String, HighlightKind>,
    class_modifiers: HashMap<String, u16>,
    members: HashMap<(String, String), MemberHighlight>,
}

impl HighlightSymbols {
    pub fn from_source_set(
        sources: &[&str],
        files: &[FileAnalysis],
        symbols: &FrontendSymbols,
    ) -> Self {
        let mut metadata = Self {
            class_kinds: symbols
                .classes
                .iter()
                .map(|(name, class)| {
                    let kind = if symbols.enums.contains_key(name) {
                        HighlightKind::Enum
                    } else if class.is_annotation {
                        HighlightKind::Decorator
                    } else if class.is_interface {
                        HighlightKind::Interface
                    } else if class.is_object {
                        HighlightKind::Type
                    } else {
                        HighlightKind::Class
                    };
                    (name.clone(), kind)
                })
                .collect(),
            class_modifiers: HashMap::new(),
            members: HashMap::new(),
        };
        for (source, file) in sources.iter().copied().zip(files) {
            for &declaration in &file.file.decls {
                if let Decl::Class(class) = file.file.decl(declaration) {
                    metadata.collect_class(source, class);
                }
            }
        }
        metadata
    }

    fn collect_class(&mut self, source: &str, class: &ClassDecl) {
        self.class_kinds.insert(
            class.name.clone(),
            match class.kind {
                ClassKind::Enum => HighlightKind::Enum,
                ClassKind::Interface => HighlightKind::Interface,
                ClassKind::Annotation => HighlightKind::Decorator,
                ClassKind::Object => HighlightKind::Type,
                ClassKind::Class if class.is_data => HighlightKind::Struct,
                ClassKind::Class => HighlightKind::Class,
            },
        );
        if is_deprecated(&class.annotations) {
            self.class_modifiers
                .insert(class.name.clone(), HighlightModifiers::DEPRECATED);
        }
        for property in &class.props {
            self.members.insert(
                (class.name.clone(), property.name.clone()),
                MemberHighlight {
                    kind: HighlightKind::Property,
                    modifiers: variable_modifier(property.is_var)
                        | if is_deprecated(&property.annotations) {
                            HighlightModifiers::DEPRECATED
                        } else {
                            0
                        },
                },
            );
        }
        for property in class.body_props.iter().chain(&class.companion_props) {
            self.members.insert(
                (class.name.clone(), property.name.clone()),
                MemberHighlight {
                    kind: HighlightKind::Property,
                    modifiers: variable_modifier(property.is_var),
                },
            );
        }
        for function in class.methods.iter().chain(&class.companion_methods) {
            self.members.insert(
                (class.name.clone(), function.name.clone()),
                MemberHighlight {
                    kind: if source_has_modifier_before(source, function.span.lo, "operator") {
                        HighlightKind::Operator
                    } else {
                        HighlightKind::Method
                    },
                    modifiers: function_modifiers(function),
                },
            );
        }
        for entry in &class.enum_entries {
            self.members.insert(
                (class.name.clone(), entry.name.clone()),
                MemberHighlight {
                    kind: HighlightKind::EnumMember,
                    modifiers: HighlightModifiers::READONLY
                        | if is_deprecated(&entry.annotations) {
                            HighlightModifiers::DEPRECATED
                        } else {
                            0
                        },
                },
            );
        }
    }
}

impl FileAnalysis {
    /// Classify declarations and references using the checked frontend and a reduced name-token pass.
    pub fn highlight_occurrences(
        &self,
        source: &str,
        symbols: &FrontendSymbols,
        highlight_symbols: &HighlightSymbols,
    ) -> Vec<HighlightOccurrence> {
        let mut diagnostics = DiagSink::new();
        let tokens = lex_name_tokens(source, &mut diagnostics);
        let definition_symbols = DefinitionSymbols::default();
        let mut classifier = SemanticClassifier::new(
            source,
            &self.file,
            tokens,
            SemanticContext {
                file_index: 0,
                symbols,
                type_info: self.types.as_ref(),
                highlight_symbols,
                definition_symbols: &definition_symbols,
                definition_limit: 0,
            },
        );
        classifier.classify();
        classifier.finish().highlights
    }

    pub fn semantic_occurrences(
        &self,
        source: &str,
        file_index: u32,
        symbols: &FrontendSymbols,
        highlight_symbols: &HighlightSymbols,
        definition_symbols: &DefinitionSymbols,
        definition_limit: usize,
    ) -> SemanticOccurrences {
        let mut diagnostics = DiagSink::new();
        let tokens = lex_name_tokens(source, &mut diagnostics);
        let mut classifier = SemanticClassifier::new(
            source,
            &self.file,
            tokens,
            SemanticContext {
                file_index,
                symbols,
                type_info: self.types.as_ref(),
                highlight_symbols,
                definition_symbols,
                definition_limit,
            },
        );
        classifier.classify();
        classifier.finish()
    }
}

impl<'a> SemanticClassifier<'a> {
    fn new(
        source: &'a str,
        file: &'a File,
        tokens: Vec<FrontendNameToken>,
        context: SemanticContext<'a>,
    ) -> Self {
        let SemanticContext {
            file_index,
            symbols,
            type_info,
            highlight_symbols,
            definition_symbols,
            definition_limit,
        } = context;
        let token_by_span = tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| token.kind == FrontendNameTokenKind::Ident)
            .map(|(index, token)| ((token.span.lo, token.span.hi), index))
            .collect();
        let classified = vec![None; tokens.len()];
        let block_scopes: Vec<_> = file
            .expr_arena
            .iter()
            .enumerate()
            .filter(|(_, expression)| matches!(expression, Expr::Block { .. }))
            .map(|(index, _)| file.expr_spans[index])
            .collect();
        let file_span = Span::new(0, source.len() as u32);
        let statement_scopes = file
            .stmt_spans
            .iter()
            .copied()
            .map(|statement| {
                let scope = block_scopes
                    .iter()
                    .copied()
                    .filter(|scope| scope.lo <= statement.lo && scope.hi >= statement.hi)
                    .min_by_key(|scope| scope.hi.saturating_sub(scope.lo))
                    .unwrap_or(file_span);
                ((statement.lo, statement.hi), scope)
            })
            .collect();
        let callees = file
            .expr_arena
            .iter()
            .enumerate()
            .filter_map(|(index, expression)| match expression {
                Expr::Call { callee, .. } => Some((*callee, ExprId(index as u32))),
                _ => None,
            })
            .collect();
        Self {
            source,
            file,
            file_index,
            symbols,
            type_info,
            tokens,
            classified,
            definitions: Vec::new(),
            definition_limit,
            token_by_span,
            statement_scopes,
            callees,
            highlight_symbols,
            definition_symbols,
            bindings: Vec::new(),
            properties: HashMap::new(),
            functions: HashMap::new(),
        }
    }

    fn classify(&mut self) {
        for target in self
            .definition_symbols
            .file_targets(self.file_index)
            .to_vec()
        {
            self.push_definition(target.span, target);
        }
        self.mark_namespaces_and_annotations();
        for &declaration in &self.file.decls {
            match self.file.decl(declaration) {
                Decl::Fun(function) => self.mark_function(function, false, true),
                Decl::Class(class) => self.mark_class(class),
                Decl::Property(property) => {
                    let definition = self.mark_property(property, true);
                    self.add_binding(
                        &property.name,
                        self.file_span(),
                        0,
                        HighlightKind::Property,
                        variable_modifier(property.is_var) | HighlightModifiers::STATIC,
                        definition,
                    );
                    if let Some(ty) = &property.ty {
                        self.set_last_binding_owner(ty);
                    }
                }
            }
        }
        for (index, statement) in self.file.stmt_arena.iter().enumerate() {
            self.mark_statement(statement, self.file.stmt_spans[index]);
        }
        for (index, expression) in self.file.expr_arena.iter().enumerate() {
            self.mark_expression(ExprId(index as u32), expression);
        }
        for arguments in self.file.call_type_args.values() {
            for argument in arguments {
                self.mark_type(argument);
            }
        }
    }

    fn finish(self) -> SemanticOccurrences {
        SemanticOccurrences {
            highlights: self.classified.into_iter().flatten().collect(),
            definitions: self.definitions,
        }
    }

    fn mark_namespaces_and_annotations(&mut self) {
        let mut namespace_line = false;
        let mut import_line = false;
        let mut import_names = Vec::new();
        for index in 0..self.tokens.len() {
            match self.tokens[index].kind {
                FrontendNameTokenKind::Package => {
                    namespace_line = true;
                }
                FrontendNameTokenKind::Import => {
                    import_line = true;
                    import_names.clear();
                }
                FrontendNameTokenKind::Newline => {
                    if import_line {
                        self.mark_import_names(&import_names);
                    }
                    namespace_line = false;
                    import_line = false;
                }
                FrontendNameTokenKind::Ident if import_line => import_names.push(index),
                FrontendNameTokenKind::Ident if namespace_line => {
                    self.mark_index(index, HighlightKind::Namespace, 0);
                }
                FrontendNameTokenKind::Ident
                    if self.tokens[index].text(self.source) == "typealias" =>
                {
                    if let Some(alias) = self
                        .tokens
                        .get(index + 1)
                        .filter(|alias| alias.kind == FrontendNameTokenKind::Ident)
                    {
                        let alias_name = alias.text(self.source);
                        let highlight = self
                            .file
                            .type_aliases
                            .iter()
                            .find(|(name, _)| name == alias_name)
                            .map(|(_, target)| {
                                let leaf = target.rsplit('.').next().unwrap_or(target);
                                self.type_token(leaf, alias.span.lo)
                            })
                            .or_else(|| {
                                self.file
                                    .type_alias_fun
                                    .iter()
                                    .any(|(name, _, _)| name == alias_name)
                                    .then_some((
                                        HighlightKind::Interface,
                                        HighlightModifiers::DEFAULT_LIBRARY,
                                    ))
                            });
                        if let Some((kind, modifiers)) = highlight {
                            self.mark_index(
                                index + 1,
                                kind,
                                modifiers | HighlightModifiers::DECLARATION,
                            );
                        }
                    }
                }
                FrontendNameTokenKind::Ident
                    if index > 0 && self.tokens[index - 1].kind == FrontendNameTokenKind::At =>
                {
                    self.mark_index(index, HighlightKind::Decorator, 0);
                }
                _ => {}
            }
        }
        if import_line {
            self.mark_import_names(&import_names);
        }
    }

    fn mark_import_names(&mut self, names: &[usize]) {
        let alias_marker = names
            .iter()
            .position(|&index| self.tokens[index].text(self.source) == "as");
        let path = alias_marker.map_or(names, |marker| &names[..marker]);
        let Some((&terminal, namespaces)) = path.split_last() else {
            return;
        };
        for &index in namespaces {
            self.mark_index(index, HighlightKind::Namespace, 0);
        }
        let name = self.tokens[terminal].text(self.source);
        let (kind, modifiers) = if let Some(&kind) = self.highlight_symbols.class_kinds.get(name) {
            (kind, self.default_library_modifier(name))
        } else if is_kotlin_builtin_type(name) {
            (HighlightKind::Class, HighlightModifiers::DEFAULT_LIBRARY)
        } else if self.symbols.props.contains_key(name) {
            (
                HighlightKind::Property,
                variable_modifier(self.symbols.props[name].1) | HighlightModifiers::STATIC,
            )
        } else if self.symbols.funs.contains_key(name) {
            (
                HighlightKind::Function,
                HighlightModifiers::STATIC | self.default_library_modifier(name),
            )
        } else {
            (HighlightKind::Namespace, 0)
        };
        self.mark_index(terminal, kind, modifiers);
        let qualified = path
            .iter()
            .map(|&index| self.tokens[index].text(self.source))
            .collect::<Vec<_>>()
            .join(".");
        if let Some(target) = self.definition_symbols.class_target(self.file, &qualified) {
            self.push_definition(self.tokens[terminal].span, target);
        }
        if let Some(alias) = alias_marker
            .and_then(|marker| names.get(marker + 1))
            .copied()
        {
            self.mark_index(alias, kind, modifiers | HighlightModifiers::DECLARATION);
        }
    }

    fn mark_class(&mut self, class: &ClassDecl) {
        let kind = self
            .highlight_symbols
            .class_kinds
            .get(&class.name)
            .copied()
            .unwrap_or(HighlightKind::Class);
        let mut modifiers = HighlightModifiers::DECLARATION;
        if class.modality.is_abstract() {
            modifiers |= HighlightModifiers::ABSTRACT;
        }
        if is_deprecated(&class.annotations) {
            modifiers |= HighlightModifiers::DEPRECATED;
        }
        self.mark_named_in(class.span, &class.name, kind, modifiers, false);
        self.mark_type_parameters(class.span, class.span, &class.type_params);
        for (_, bound) in &class.type_param_bounds {
            self.mark_type(bound);
        }
        for parameter in &class.props {
            self.mark_constructor_parameter(class.span, parameter);
        }
        for supertype in &class.supertypes {
            self.mark_type(supertype);
        }
        for method in &class.methods {
            self.mark_function(method, true, false);
            self.add_member_function_binding(class.span, method, false);
        }
        let companion_scope = class
            .companion_methods
            .iter()
            .map(|method| method.span)
            .chain(class.companion_props.iter().map(|property| property.span))
            .reduce(|left, right| Span::new(left.lo.min(right.lo), left.hi.max(right.hi)));
        for method in &class.companion_methods {
            self.mark_function(method, true, true);
            self.add_member_function_binding(companion_scope.unwrap_or(class.span), method, true);
        }
        for property in &class.body_props {
            let definition = self.mark_property(property, false);
            self.add_binding(
                &property.name,
                class.span,
                class.span.lo,
                HighlightKind::Property,
                variable_modifier(property.is_var),
                definition,
            );
            if let Some(ty) = &property.ty {
                self.set_last_binding_owner(ty);
            }
        }
        for property in &class.companion_props {
            let definition = self.mark_property(property, true);
            self.add_binding(
                &property.name,
                companion_scope.unwrap_or(class.span),
                companion_scope.unwrap_or(class.span).lo,
                HighlightKind::Property,
                variable_modifier(property.is_var) | HighlightModifiers::STATIC,
                definition,
            );
            if let Some(ty) = &property.ty {
                self.set_last_binding_owner(ty);
            }
        }
        for entry in &class.enum_entries {
            self.mark_exact(
                entry.span,
                HighlightKind::EnumMember,
                HighlightModifiers::DECLARATION
                    | HighlightModifiers::READONLY
                    | if is_deprecated(&entry.annotations) {
                        HighlightModifiers::DEPRECATED
                    } else {
                        0
                    },
            );
            for method in &entry.methods {
                self.mark_function(method, true, false);
            }
            for property in &entry.props {
                self.mark_property(property, false);
            }
        }
    }

    fn mark_function(&mut self, function: &FunDecl, member: bool, static_member: bool) {
        let kind = if self.has_modifier_before_name(function.span, &function.name, "operator") {
            HighlightKind::Operator
        } else if member {
            HighlightKind::Method
        } else {
            HighlightKind::Function
        };
        let mut modifiers = HighlightModifiers::DECLARATION;
        if static_member {
            modifiers |= HighlightModifiers::STATIC;
        }
        if function.is_abstract {
            modifiers |= HighlightModifiers::ABSTRACT;
        }
        if function.is_suspend {
            modifiers |= HighlightModifiers::ASYNC;
        }
        if is_deprecated(&function.annotations) {
            modifiers |= HighlightModifiers::DEPRECATED;
        }
        self.mark_named_in(function.span, &function.name, kind, modifiers, false);
        self.functions
            .entry(function.name.clone())
            .or_insert(modifiers & !HighlightModifiers::DECLARATION);
        let scope = self.function_scope(function);
        self.mark_type_parameters(function.span, function.span, &function.type_params);
        for (_, bound) in &function.type_param_bounds {
            self.mark_type(bound);
        }
        if let Some(receiver) = &function.receiver {
            self.mark_type(receiver);
        }
        for parameter in &function.params {
            self.mark_parameter(function.span, scope, parameter);
        }
        if let Some(ret) = &function.ret {
            self.mark_type(ret);
        }
    }

    fn mark_parameter(&mut self, owner: Span, scope: Span, parameter: &Param) {
        let definition = self.mark_named_before_span(
            owner,
            &parameter.name,
            parameter.ty.span.lo,
            HighlightKind::Parameter,
            HighlightModifiers::DECLARATION | HighlightModifiers::READONLY,
        );
        self.add_binding(
            &parameter.name,
            scope,
            scope.lo,
            HighlightKind::Parameter,
            HighlightModifiers::READONLY,
            definition,
        );
        self.set_last_binding_owner(&parameter.ty);
        self.mark_type(&parameter.ty);
    }

    fn mark_constructor_parameter(&mut self, scope: Span, parameter: &PropParam) {
        let (reference_kind, value_modifiers) = if parameter.is_property {
            (HighlightKind::Property, variable_modifier(parameter.is_var))
        } else {
            (HighlightKind::Parameter, HighlightModifiers::READONLY)
        };
        let deprecated = if is_deprecated(&parameter.annotations) {
            HighlightModifiers::DEPRECATED
        } else {
            0
        };
        self.mark_exact(
            parameter.span,
            // The official Kotlin LSP highlights every primary-constructor declaration as a
            // readonly parameter, including a mutable `var` property parameter. References still
            // resolve as properties below, preserving member highlighting (`user.name`) and
            // property mutability.
            HighlightKind::Parameter,
            HighlightModifiers::DECLARATION | HighlightModifiers::READONLY | deprecated,
        );
        if parameter.is_property {
            self.properties
                .insert(parameter.name.clone(), value_modifiers);
        }
        self.add_binding(
            &parameter.name,
            scope,
            scope.lo,
            reference_kind,
            value_modifiers,
            Some(definition_name_span(self.source, parameter.span)),
        );
        self.set_last_binding_owner(&parameter.ty);
        self.mark_type(&parameter.ty);
    }

    fn mark_property(&mut self, property: &PropDecl, static_property: bool) -> Option<Span> {
        let value_modifiers = variable_modifier(property.is_var);
        let modifiers = HighlightModifiers::DECLARATION
            | value_modifiers
            | if static_property {
                HighlightModifiers::STATIC
            } else {
                0
            };
        let definition = self.mark_named_in_span(
            property.span,
            &property.name,
            HighlightKind::Property,
            modifiers,
            false,
        );
        self.properties
            .entry(property.name.clone())
            .or_insert(value_modifiers);
        self.mark_type_parameters(property.span, property.span, &property.type_params);
        for (_, bound) in &property.type_param_bounds {
            self.mark_type(bound);
        }
        if let Some(receiver) = &property.receiver {
            self.mark_type(receiver);
        }
        if let Some(ty) = &property.ty {
            self.mark_type(ty);
        }
        definition
    }

    fn mark_type_parameters(&mut self, owner: Span, scope: Span, names: &[String]) {
        for name in names {
            let definition = self.mark_named_in_span(
                owner,
                name,
                HighlightKind::TypeParameter,
                HighlightModifiers::DECLARATION,
                false,
            );
            self.add_binding(
                name,
                scope,
                scope.lo,
                HighlightKind::TypeParameter,
                0,
                definition,
            );
        }
    }

    fn mark_statement(&mut self, statement: &Stmt, span: Span) {
        match statement {
            Stmt::Local {
                is_var, name, ty, ..
            }
            | Stmt::LocalDelegate {
                is_var, name, ty, ..
            } => {
                let value_modifiers = variable_modifier(*is_var);
                let definition = self.mark_named_in_span(
                    span,
                    name,
                    HighlightKind::Variable,
                    HighlightModifiers::DECLARATION | value_modifiers,
                    false,
                );
                let scope = self.enclosing_block_scope(span);
                self.add_binding(
                    name,
                    scope,
                    span.hi,
                    HighlightKind::Variable,
                    value_modifiers,
                    definition,
                );
                if let Some(ty) = ty {
                    self.set_last_binding_owner(ty);
                    self.mark_type(ty);
                }
            }
            Stmt::LocalLateinit { name, ty } => {
                let definition = self.mark_named_in_span(
                    span,
                    name,
                    HighlightKind::Variable,
                    HighlightModifiers::DECLARATION | HighlightModifiers::MODIFICATION,
                    false,
                );
                self.add_binding(
                    name,
                    self.enclosing_block_scope(span),
                    span.hi,
                    HighlightKind::Variable,
                    HighlightModifiers::MODIFICATION,
                    definition,
                );
                self.set_last_binding_owner(ty);
                self.mark_type(ty);
            }
            Stmt::Destructure { entries, .. } => {
                let mut after = span.lo;
                for (name, is_var) in entries {
                    let value_modifiers = variable_modifier(*is_var);
                    let mut definition = None;
                    if let Some(index) = self.find_named(span, name, Some(after), None, false) {
                        after = self.tokens[index].span.hi;
                        definition =
                            Some(definition_name_span(self.source, self.tokens[index].span));
                        self.mark_index(
                            index,
                            HighlightKind::Variable,
                            HighlightModifiers::DECLARATION | value_modifiers,
                        );
                    }
                    self.add_binding(
                        name,
                        self.enclosing_block_scope(span),
                        span.hi,
                        HighlightKind::Variable,
                        value_modifiers,
                        definition,
                    );
                }
            }
            Stmt::Assign { name, .. } | Stmt::IncDec { name, .. } => {
                let modifiers =
                    self.value_modifiers(name, span.lo) | HighlightModifiers::MODIFICATION;
                self.mark_named_in(span, name, HighlightKind::Variable, modifiers, false);
            }
            Stmt::AssignMember { name, .. } => {
                self.mark_named_in(
                    span,
                    name,
                    HighlightKind::Property,
                    HighlightModifiers::MODIFICATION,
                    true,
                );
            }
            Stmt::For { name, .. } | Stmt::ForEach { name, .. } => {
                let definition = self.mark_named_in_span(
                    span,
                    name,
                    HighlightKind::Variable,
                    HighlightModifiers::DECLARATION | HighlightModifiers::READONLY,
                    false,
                );
                let scope = match statement {
                    Stmt::For { body, .. } | Stmt::ForEach { body, .. } => {
                        self.file.expr_spans[body.0 as usize]
                    }
                    _ => unreachable!(),
                };
                self.add_binding(
                    name,
                    scope,
                    scope.lo,
                    HighlightKind::Variable,
                    HighlightModifiers::READONLY,
                    definition,
                );
            }
            Stmt::LocalFun(function) => {
                self.mark_function(function, false, false);
                let definition = self
                    .find_named(function.span, &function.name, None, None, false)
                    .map(|index| definition_name_span(self.source, self.tokens[index].span));
                let kind =
                    if self.has_modifier_before_name(function.span, &function.name, "operator") {
                        HighlightKind::Operator
                    } else {
                        HighlightKind::Function
                    };
                self.add_binding(
                    &function.name,
                    self.enclosing_block_scope(span),
                    span.lo,
                    kind,
                    function_modifiers(function),
                    definition,
                );
            }
            Stmt::LocalClass(class) => self.mark_class(class),
            _ => {}
        }
    }

    fn mark_expression(&mut self, id: ExprId, expression: &Expr) {
        let span = self.file.expr_spans[id.0 as usize];
        match expression {
            Expr::Name(name) => {
                let (kind, modifiers) = if let Some(&call) = self.callees.get(&id) {
                    if self.is_constructor_call(call, name) {
                        self.type_token(name, span.lo)
                    } else {
                        let scoped = self.binding_at_kind(name, span.lo, true);
                        (
                            if let Some(binding) = scoped {
                                binding.kind
                            } else if self
                                .type_info
                                .is_some_and(|types| types.resolved_call_is_member(call))
                            {
                                HighlightKind::Method
                            } else {
                                HighlightKind::Function
                            },
                            self.function_reference_modifiers(call, name),
                        )
                    }
                } else if let Some(&kind) = self.highlight_symbols.class_kinds.get(name) {
                    (
                        kind,
                        self.default_library_modifier(name)
                            | self
                                .highlight_symbols
                                .class_modifiers
                                .get(name)
                                .copied()
                                .unwrap_or(0),
                    )
                } else if let Some(binding) = self.binding_at_kind(name, span.lo, false) {
                    (binding.kind, binding.modifiers)
                } else if self.symbols.props.contains_key(name) {
                    let is_var = self.symbols.props[name].1;
                    (
                        HighlightKind::Property,
                        variable_modifier(is_var) | HighlightModifiers::STATIC,
                    )
                } else {
                    (HighlightKind::Variable, 0)
                };
                self.mark_exact(span, kind, modifiers);
                if let Some(target) = self.name_definition(name, span.lo, id) {
                    self.push_definition(span, target);
                } else {
                    let kind = if self.callees.contains_key(&id) {
                        MemberKind::StaticFunction
                    } else {
                        MemberKind::StaticValue
                    };
                    let targets = self
                        .definition_symbols
                        .top_level_targets(self.file, name, kind);
                    for target in targets {
                        self.push_definition(span, target);
                    }
                }
            }
            Expr::Member { receiver, name } => {
                let call = self.callees.get(&id).copied();
                let highlight = self.member_highlight(*receiver, name, call);
                if let Some(source_span) =
                    self.mark_named_in_span(span, name, highlight.kind, highlight.modifiers, true)
                {
                    self.record_member_definitions(
                        source_span,
                        *receiver,
                        name,
                        Some(call.unwrap_or(id)),
                        self.member_kind(*receiver, call.is_some()),
                    );
                }
            }
            Expr::SafeCall {
                receiver,
                name,
                args,
            } => {
                let call = args.as_ref().map(|_| id);
                let highlight = self.member_highlight(*receiver, name, call);
                if let Some(source_span) =
                    self.mark_named_in_span(span, name, highlight.kind, highlight.modifiers, true)
                {
                    self.record_member_definitions(
                        source_span,
                        *receiver,
                        name,
                        Some(id),
                        self.member_kind(*receiver, args.is_some()),
                    );
                }
            }
            Expr::CallableRef { receiver, name } if name != "class" => {
                let highlight = if let Some(receiver) = receiver {
                    self.member_highlight(*receiver, name, None)
                } else {
                    let property = self
                        .type_info
                        .is_some_and(|types| types.bound_property_refs.contains_key(&id));
                    MemberHighlight {
                        kind: if property {
                            HighlightKind::Property
                        } else {
                            HighlightKind::Function
                        },
                        modifiers: if property {
                            self.properties.get(name).copied().unwrap_or(0)
                        } else {
                            self.function_reference_modifiers(id, name)
                        },
                    }
                };
                if let Some(source_span) =
                    self.mark_named_in_span(span, name, highlight.kind, highlight.modifiers, true)
                {
                    if let Some(receiver) = receiver {
                        self.record_member_definitions(
                            source_span,
                            *receiver,
                            name,
                            None,
                            self.member_kind(
                                *receiver,
                                matches!(
                                    highlight.kind,
                                    HighlightKind::Method | HighlightKind::Operator
                                ),
                            ),
                        );
                    } else {
                        let kind = if matches!(
                            highlight.kind,
                            HighlightKind::Method
                                | HighlightKind::Function
                                | HighlightKind::Operator
                        ) {
                            MemberKind::StaticFunction
                        } else {
                            MemberKind::StaticValue
                        };
                        let targets = self
                            .definition_symbols
                            .top_level_targets(self.file, name, kind);
                        for target in targets {
                            self.push_definition(source_span, target);
                        }
                    }
                }
            }
            Expr::Is { ty, .. } | Expr::As { ty, .. } => self.mark_type(ty),
            Expr::Lambda { params, .. } => {
                for name in params {
                    let definition = self.mark_named_in_span(
                        span,
                        name,
                        HighlightKind::Parameter,
                        HighlightModifiers::DECLARATION | HighlightModifiers::READONLY,
                        false,
                    );
                    let scope = match expression {
                        Expr::Lambda { body, .. } => self.file.expr_spans[body.0 as usize],
                        _ => unreachable!(),
                    };
                    self.add_binding(
                        name,
                        scope,
                        scope.lo,
                        HighlightKind::Parameter,
                        HighlightModifiers::READONLY,
                        definition,
                    );
                }
                if let Some(types) = self.file.lambda_param_types.get(&id.0) {
                    for ty in types.iter().flatten() {
                        self.mark_type(ty);
                    }
                }
            }
            Expr::Try { catches, .. } => {
                for catch in catches {
                    let definition = self.mark_named_before_span(
                        span,
                        &catch.name,
                        catch.ty.span.lo,
                        HighlightKind::Variable,
                        HighlightModifiers::DECLARATION | HighlightModifiers::READONLY,
                    );
                    let scope = self.file.expr_spans[catch.body.0 as usize];
                    self.add_binding(
                        &catch.name,
                        scope,
                        scope.lo,
                        HighlightKind::Variable,
                        HighlightModifiers::READONLY,
                        definition,
                    );
                    self.mark_type(&catch.ty);
                }
            }
            _ => {}
        }
    }

    fn is_constructor_call(&self, call: ExprId, name: &str) -> bool {
        self.type_info
            .is_some_and(|types| types.resolved_constructors.contains_key(&call))
            || self.symbols.class_names.contains_key(name) && !self.symbols.funs.contains_key(name)
    }

    fn name_definition(&self, name: &str, at: u32, expression: ExprId) -> Option<DefinitionTarget> {
        let resolved_expression = self.callees.get(&expression).copied().unwrap_or(expression);
        let member_kind = if self.callees.contains_key(&expression) {
            MemberKind::InstanceFunction
        } else {
            MemberKind::InstanceValue
        };
        if let Some(target) = self.checked_companion_target(
            resolved_expression,
            name,
            if self.callees.contains_key(&expression) {
                MemberKind::StaticFunction
            } else {
                MemberKind::StaticValue
            },
        ) {
            return Some(target);
        }
        if let Some(target) = self.checked_member_target(resolved_expression, name, member_kind) {
            return Some(target);
        }
        if let Some(&call) = self.callees.get(&expression) {
            if let Some((file, declaration)) = self
                .type_info
                .and_then(|types| types.resolved_source_call(call))
            {
                if let Some(target) = self
                    .definition_symbols
                    .declaration_target(file, declaration)
                {
                    return Some(target);
                }
            }
            if let Some(resolved) = self
                .type_info
                .and_then(|types| types.resolved_local_function(call))
            {
                if let Stmt::LocalFun(function) = self.file.stmt(resolved.stmt_id) {
                    if let Some(span) = declaration_name_span(
                        &self.tokens,
                        self.source,
                        function.span,
                        &function.name,
                        false,
                    ) {
                        return Some(DefinitionTarget {
                            file: self.file_index,
                            span,
                        });
                    }
                }
            }
            if self.is_constructor_call(call, name) {
                return self.definition_symbols.class_target(self.file, name);
            }
        }
        if self.highlight_symbols.class_kinds.contains_key(name) {
            return self.definition_symbols.class_target(self.file, name);
        }
        self.binding_at_kind(name, at, self.callees.contains_key(&expression))
            .and_then(|binding| binding.definition)
            .map(|span| DefinitionTarget {
                file: self.file_index,
                span,
            })
    }

    fn push_definition(&mut self, span: Span, target: DefinitionTarget) {
        if self.definitions.len() < self.definition_limit {
            self.definitions.push(DefinitionOccurrence {
                span: definition_name_span(self.source, span),
                target,
            });
        }
    }

    fn checked_member_target(
        &self,
        expression: ExprId,
        name: &str,
        kind: MemberKind,
    ) -> Option<DefinitionTarget> {
        let (owner, resolved_name, params) = self
            .type_info?
            .resolved_module_member_signature(expression)?;
        (resolved_name == name)
            .then(|| {
                self.definition_symbols
                    .member_target(&owner.render(), name, kind, params)
            })
            .flatten()
    }

    fn checked_companion_target(
        &self,
        expression: ExprId,
        name: &str,
        kind: MemberKind,
    ) -> Option<DefinitionTarget> {
        let member = self.type_info?.resolved_companion(expression)?;
        if member.name != name {
            return None;
        }
        let owner = member.owner?.render();
        let owner = owner.strip_suffix("$Companion").unwrap_or(&owner);
        self.definition_symbols
            .member_target(owner, name, kind, &member.params)
    }

    fn record_member_definitions(
        &mut self,
        source_span: Span,
        receiver: ExprId,
        name: &str,
        resolved_expression: Option<ExprId>,
        kind: MemberKind,
    ) {
        if let Some(expression) = resolved_expression {
            if let Some(target) = self
                .type_info
                .and_then(|types| types.resolved_super_call(expression))
                .and_then(|resolved| {
                    self.definition_symbols.member_target(
                        &resolved.owner.render(),
                        name,
                        MemberKind::InstanceFunction,
                        &resolved.params,
                    )
                })
            {
                self.push_definition(source_span, target);
                return;
            }
            if let Some(target) = self.checked_companion_target(expression, name, kind) {
                self.push_definition(source_span, target);
                return;
            }
            if let Some((file, declaration)) = self
                .type_info
                .and_then(|types| types.resolved_source_call(expression))
            {
                if let Some(target) = self
                    .definition_symbols
                    .declaration_target(file, declaration)
                {
                    self.push_definition(source_span, target);
                }
                return;
            }
            if let Some((owner, resolved_name, params)) = self
                .type_info
                .and_then(|types| types.resolved_module_member_signature(expression))
            {
                if resolved_name == name {
                    if let Some(target) =
                        self.definition_symbols
                            .member_target(&owner.render(), name, kind, params)
                    {
                        self.push_definition(source_span, target);
                    }
                    return;
                }
            }
        }
        let Some(owner) = self.receiver_definition_owner(receiver) else {
            return;
        };
        let targets = self.definition_symbols.member_targets(&owner, name, kind);
        if !targets.is_empty() {
            for target in targets {
                self.push_definition(source_span, target);
            }
            return;
        }
        if kind == MemberKind::InstanceValue {
            if let Some(receiver_ty) = self
                .type_info
                .and_then(|types| types.expr_types.get(receiver.0 as usize))
            {
                if let Some(target) =
                    self.definition_symbols
                        .extension_value_target(*receiver_ty, name, self.file)
                {
                    self.push_definition(source_span, target);
                }
            }
        }
    }

    fn member_kind(&self, receiver: ExprId, function: bool) -> MemberKind {
        let static_receiver = match self.file.expr(receiver) {
            Expr::Name(name) => self
                .definition_symbols
                .class_owner(self.file, name)
                .is_some_and(|owner| !self.definition_symbols.is_object_owner(&owner)),
            _ => false,
        };
        match (static_receiver, function) {
            (false, false) => MemberKind::InstanceValue,
            (false, true) => MemberKind::InstanceFunction,
            (true, false) => MemberKind::StaticValue,
            (true, true) => MemberKind::StaticFunction,
        }
    }

    fn receiver_definition_owner(&self, receiver: ExprId) -> Option<String> {
        if let Expr::Name(name) = self.file.expr(receiver) {
            if let Some(owner) = self
                .binding_at(name, self.file.expr_spans[receiver.0 as usize].lo)
                .and_then(|binding| binding.definition_owner.clone())
            {
                return Some(owner);
            }
            if let Some(owner) = self.definition_symbols.class_owner(self.file, name) {
                return Some(owner);
            }
        }
        let ty = self
            .type_info?
            .expr_types
            .get(receiver.0 as usize)?
            .non_null();
        let Ty::Obj(owner, _) = ty else {
            return None;
        };
        Some(owner.render())
    }

    fn member_highlight(
        &self,
        receiver: ExprId,
        name: &str,
        call: Option<ExprId>,
    ) -> MemberHighlight {
        if let Some(call) = call {
            if self
                .type_info
                .is_some_and(|types| types.resolved_extension(call).is_some())
            {
                return MemberHighlight {
                    kind: HighlightKind::Function,
                    modifiers: self.function_reference_modifiers(call, name),
                };
            }
        }
        if let Some(owner) = self.receiver_owner(receiver) {
            if let Some(&highlight) = self
                .highlight_symbols
                .members
                .get(&(owner.clone(), name.to_owned()))
            {
                return highlight;
            }
            if self
                .symbols
                .enums
                .get(&owner)
                .is_some_and(|entries| entries.iter().any(|entry| entry == name))
            {
                return MemberHighlight {
                    kind: HighlightKind::EnumMember,
                    modifiers: HighlightModifiers::READONLY | HighlightModifiers::STATIC,
                };
            }
            if let Some(class) = self.symbols.classes.get(&owner) {
                if let Some((_, is_var)) = class.prop(name) {
                    return MemberHighlight {
                        kind: HighlightKind::Property,
                        modifiers: variable_modifier(is_var),
                    };
                }
                if class.has_method(name) || class.static_methods.contains_key(name) {
                    return MemberHighlight {
                        kind: HighlightKind::Method,
                        modifiers: if class.static_methods.contains_key(name) {
                            HighlightModifiers::STATIC
                        } else {
                            0
                        },
                    };
                }
            }
        }
        if let Some(call) = call {
            MemberHighlight {
                kind: HighlightKind::Method,
                modifiers: self.function_reference_modifiers(call, name)
                    & !HighlightModifiers::STATIC,
            }
        } else {
            MemberHighlight {
                kind: HighlightKind::Property,
                modifiers: 0,
            }
        }
    }

    fn receiver_owner(&self, receiver: ExprId) -> Option<String> {
        if let Expr::Name(name) = self.file.expr(receiver) {
            if self.highlight_symbols.class_kinds.contains_key(name) {
                return Some(name.clone());
            }
        }
        let ty = self
            .type_info?
            .expr_types
            .get(receiver.0 as usize)?
            .non_null();
        let Ty::Obj(owner, _) = ty else {
            return None;
        };
        Some(
            owner
                .render()
                .rsplit(['/', '$'])
                .next()
                .unwrap_or_default()
                .to_owned(),
        )
    }

    fn function_reference_modifiers(&self, call: ExprId, name: &str) -> u16 {
        let mut modifiers = if self
            .type_info
            .is_some_and(|types| types.resolved_calls.contains_key(&call))
        {
            0
        } else {
            self.functions.get(name).copied().unwrap_or_else(|| {
                if self.symbols.funs.contains_key(name) {
                    HighlightModifiers::STATIC
                } else {
                    0
                }
            })
        };
        let Some(types) = self.type_info else {
            return modifiers;
        };
        if let Some(callable) = types.resolved_top_level(call) {
            modifiers |= HighlightModifiers::STATIC;
            if callable.suspend {
                modifiers |= HighlightModifiers::ASYNC;
            }
            if callable.owner_starts_with("kotlin/") {
                modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
            }
        } else if let Some(callable) = types.resolved_extension(call) {
            modifiers |= HighlightModifiers::STATIC;
            if callable.suspend {
                modifiers |= HighlightModifiers::ASYNC;
            }
            if callable.owner_starts_with("kotlin/") {
                modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
            }
        } else if let Some(member) = types.resolved_member(call) {
            if member.suspend {
                modifiers |= HighlightModifiers::ASYNC;
            }
            if member
                .member
                .owner
                .is_some_and(|owner| owner.starts_with("kotlin/"))
            {
                modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
            }
        } else if let Some(member) = types.resolved_companion(call) {
            modifiers |= HighlightModifiers::STATIC;
            if member
                .owner
                .is_some_and(|owner| owner.starts_with("kotlin/"))
            {
                modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
            }
        } else if let Some(callable) = types.resolved_module_top_level(call) {
            modifiers |= HighlightModifiers::STATIC;
            if callable.suspend {
                modifiers |= HighlightModifiers::ASYNC;
            }
        }
        modifiers
    }

    fn mark_type(&mut self, ty: &TypeRef) {
        if ty.name == "<fun>" {
            for parameter in &ty.fun_params {
                self.mark_type(parameter);
            }
            if let Some(ret) = &ty.arg {
                self.mark_type(ret);
            }
            return;
        }
        let leaf = ty.name.rsplit('.').next().unwrap_or(&ty.name);
        let (kind, modifiers) = self.type_token(leaf, ty.span.lo);
        if let Some(mut index) = self.token_by_span.get(&(ty.span.lo, ty.span.hi)).copied() {
            let components = ty.name.split('.').count();
            for _ in 1..components {
                self.mark_index(index, HighlightKind::Namespace, 0);
                let Some(next) = self.tokens.get(index + 2) else {
                    break;
                };
                if self.tokens.get(index + 1).map(|token| token.kind)
                    != Some(FrontendNameTokenKind::Dot)
                    || next.kind != FrontendNameTokenKind::Ident
                {
                    break;
                }
                index += 2;
            }
            self.mark_index(index, kind, modifiers);
            let source_span = self.tokens[index].span;
            if let Some(target) = self
                .binding_at_matching(leaf, source_span.lo, |binding| {
                    binding.kind == HighlightKind::TypeParameter
                })
                .and_then(|binding| binding.definition)
                .map(|span| DefinitionTarget {
                    file: self.file_index,
                    span,
                })
                .or_else(|| self.definition_symbols.class_target(self.file, &ty.name))
            {
                self.push_definition(source_span, target);
            }
        }
        if let Some(argument) = &ty.arg {
            self.mark_type(argument);
        }
        for argument in &ty.targs {
            self.mark_type(argument);
        }
        for parameter in &ty.fun_params {
            self.mark_type(parameter);
        }
    }

    fn type_token(&self, name: &str, at: u32) -> (HighlightKind, u16) {
        if self
            .binding_at_matching(name, at, |binding| {
                binding.kind == HighlightKind::TypeParameter
            })
            .is_some()
        {
            return (HighlightKind::TypeParameter, 0);
        }
        (
            self.highlight_symbols
                .class_kinds
                .get(name)
                .copied()
                .unwrap_or(HighlightKind::Class),
            self.default_library_modifier(name)
                | self
                    .highlight_symbols
                    .class_modifiers
                    .get(name)
                    .copied()
                    .unwrap_or(0),
        )
    }

    fn default_library_modifier(&self, name: &str) -> u16 {
        if is_kotlin_builtin_type(name)
            || self
                .symbols
                .class_names
                .get(name)
                .is_some_and(|internal| internal.render().starts_with("kotlin/"))
        {
            HighlightModifiers::DEFAULT_LIBRARY
        } else {
            0
        }
    }

    fn value_modifiers(&self, name: &str, at: u32) -> u16 {
        self.binding_at(name, at)
            .map(|binding| binding.modifiers)
            .or_else(|| self.properties.get(name).copied())
            .unwrap_or(0)
    }

    fn file_span(&self) -> Span {
        Span::new(0, self.source.len() as u32)
    }

    fn function_scope(&self, function: &FunDecl) -> Span {
        match function.body {
            FunBody::Expr(body) | FunBody::Block(body) => self.file.expr_spans[body.0 as usize],
            FunBody::None => function.span,
        }
    }

    fn enclosing_block_scope(&self, span: Span) -> Span {
        self.statement_scopes
            .get(&(span.lo, span.hi))
            .copied()
            .unwrap_or_else(|| self.file_span())
    }

    fn add_binding(
        &mut self,
        name: &str,
        scope: Span,
        declared_at: u32,
        kind: HighlightKind,
        modifiers: u16,
        definition: Option<Span>,
    ) {
        if let Some(span) = definition {
            let target = DefinitionTarget {
                file: self.file_index,
                span,
            };
            if !self.definition_symbols.is_file_target(target) {
                self.push_definition(span, target);
            }
        }
        self.bindings.push(Binding {
            name: name.to_owned(),
            scope,
            declared_at,
            kind,
            modifiers,
            definition,
            definition_owner: None,
        });
    }

    fn set_last_binding_owner(&mut self, ty: &TypeRef) {
        let owner = self.definition_symbols.class_owner(self.file, &ty.name);
        if let Some(binding) = self.bindings.last_mut() {
            binding.definition_owner = owner;
        }
    }

    fn add_member_function_binding(
        &mut self,
        scope: Span,
        function: &FunDecl,
        static_member: bool,
    ) {
        let kind = if self.has_modifier_before_name(function.span, &function.name, "operator") {
            HighlightKind::Operator
        } else {
            HighlightKind::Method
        };
        let mut modifiers = if static_member {
            HighlightModifiers::STATIC
        } else {
            0
        };
        if function.is_abstract {
            modifiers |= HighlightModifiers::ABSTRACT;
        }
        if function.is_suspend {
            modifiers |= HighlightModifiers::ASYNC;
        }
        if is_deprecated(&function.annotations) {
            modifiers |= HighlightModifiers::DEPRECATED;
        }
        let definition = self
            .find_named(function.span, &function.name, None, None, false)
            .map(|index| definition_name_span(self.source, self.tokens[index].span));
        self.add_binding(&function.name, scope, scope.lo, kind, modifiers, definition);
    }

    fn binding_at(&self, name: &str, at: u32) -> Option<&Binding> {
        self.binding_at_matching(name, at, |_| true)
    }

    fn binding_at_kind(&self, name: &str, at: u32, function: bool) -> Option<&Binding> {
        self.binding_at_matching(name, at, |binding| {
            let is_function = matches!(
                binding.kind,
                HighlightKind::Function | HighlightKind::Method | HighlightKind::Operator
            );
            is_function == function
        })
    }

    fn binding_at_matching(
        &self,
        name: &str,
        at: u32,
        predicate: impl Fn(&Binding) -> bool,
    ) -> Option<&Binding> {
        self.bindings
            .iter()
            .filter(|binding| {
                binding.name == name
                    && binding.scope.lo <= at
                    && at <= binding.scope.hi
                    && binding.declared_at <= at
                    && predicate(binding)
            })
            .min_by_key(|binding| {
                (
                    binding.scope.hi.saturating_sub(binding.scope.lo),
                    Reverse(binding.declared_at),
                )
            })
    }

    fn mark_exact(&mut self, span: Span, kind: HighlightKind, modifiers: u16) {
        if let Some(index) = self.token_by_span.get(&(span.lo, span.hi)).copied() {
            self.mark_index(index, kind, modifiers);
        }
    }

    fn mark_named_before_span(
        &mut self,
        owner: Span,
        name: &str,
        before: u32,
        kind: HighlightKind,
        modifiers: u16,
    ) -> Option<Span> {
        let index = self.find_named(owner, name, None, Some(before), true)?;
        let span = self.tokens[index].span;
        self.mark_index(index, kind, modifiers);
        Some(definition_name_span(self.source, span))
    }

    fn mark_named_in(
        &mut self,
        owner: Span,
        name: &str,
        kind: HighlightKind,
        modifiers: u16,
        last: bool,
    ) {
        if let Some(index) = self.find_named(owner, name, None, None, last) {
            self.mark_index(index, kind, modifiers);
        }
    }

    fn mark_named_in_span(
        &mut self,
        owner: Span,
        name: &str,
        kind: HighlightKind,
        modifiers: u16,
        last: bool,
    ) -> Option<Span> {
        let index = self.find_named(owner, name, None, None, last)?;
        let span = self.tokens[index].span;
        self.mark_index(index, kind, modifiers);
        Some(definition_name_span(self.source, span))
    }

    fn find_named(
        &self,
        owner: Span,
        name: &str,
        after: Option<u32>,
        before: Option<u32>,
        last: bool,
    ) -> Option<usize> {
        let matches = self.tokens.iter().enumerate().filter(|(_, token)| {
            token.kind == FrontendNameTokenKind::Ident
                && token.span.lo >= owner.lo
                && token.span.hi <= owner.hi
                && after.is_none_or(|after| token.span.lo >= after)
                && before.is_none_or(|before| token.span.hi <= before)
                && token.text(self.source) == name
        });
        if last {
            matches.map(|(index, _)| index).next_back()
        } else {
            matches.map(|(index, _)| index).next()
        }
    }

    fn has_modifier_before_name(&self, owner: Span, name: &str, modifier: &str) -> bool {
        let Some(name_index) = self.find_named(owner, name, None, None, false) else {
            return false;
        };
        self.tokens[..name_index]
            .iter()
            .rev()
            .take_while(|token| token.kind != FrontendNameTokenKind::Newline)
            .any(|token| {
                token.kind == FrontendNameTokenKind::Ident && token.text(self.source) == modifier
            })
    }

    fn mark_index(&mut self, index: usize, kind: HighlightKind, modifiers: u16) {
        self.classified[index] = Some(HighlightOccurrence {
            span: self.tokens[index].span,
            kind,
            modifiers: HighlightModifiers::from_bits(modifiers),
        });
    }
}

fn variable_modifier(is_var: bool) -> u16 {
    if is_var {
        HighlightModifiers::MODIFICATION
    } else {
        HighlightModifiers::READONLY
    }
}

fn is_deprecated(annotations: &[String]) -> bool {
    annotations
        .iter()
        .any(|annotation| annotation == "Deprecated")
}

fn function_modifiers(function: &FunDecl) -> u16 {
    let mut modifiers = 0;
    if function.is_abstract {
        modifiers |= HighlightModifiers::ABSTRACT;
    }
    if function.is_suspend {
        modifiers |= HighlightModifiers::ASYNC;
    }
    if is_deprecated(&function.annotations) {
        modifiers |= HighlightModifiers::DEPRECATED;
    }
    modifiers
}

fn source_has_modifier_before(source: &str, at: u32, modifier: &str) -> bool {
    let before = &source[..at as usize];
    let line = before.rsplit_once('\n').map_or(before, |(_, line)| line);
    let declaration_prefix = line.rsplit([';', '{', '}']).next().unwrap_or(line);
    declaration_prefix
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .any(|word| word == modifier)
}

fn is_kotlin_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "Any"
            | "Nothing"
            | "Unit"
            | "Boolean"
            | "Byte"
            | "Short"
            | "Int"
            | "Long"
            | "Float"
            | "Double"
            | "Char"
            | "String"
            | "Array"
            | "BooleanArray"
            | "ByteArray"
            | "ShortArray"
            | "IntArray"
            | "LongArray"
            | "FloatArray"
            | "DoubleArray"
            | "CharArray"
            | "UInt"
            | "ULong"
            | "UByte"
            | "UShort"
    )
}
