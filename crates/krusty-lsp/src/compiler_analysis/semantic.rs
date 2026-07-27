//! Semantic symbol classification over checked frontend data.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
};

use krusty::ast::{
    BinOp, ClassDecl, ClassKind, Decl, Expr, ExprId, File, FunBody, FunDecl, Param, PropDecl,
    PropParam, Stmt, StmtId, TypeRef, UnOp,
};
use krusty::diag::{DiagSink, Span};
use krusty::frontend::{
    lex_name_tokens, CompoundAssignmentTarget, FrontendNameToken, FrontendNameTokenKind,
    FrontendSymbols, FrontendTypeInfo,
};
use krusty::types::Ty;

use super::{
    checked_property_type,
    navigation::{
        declaration_name_span, definition_name_span, render_function_hover, source_name, MemberKind,
    },
    rendering::{render_ty, render_type},
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
    pub type_definitions: Vec<DefinitionOccurrence>,
    pub implementations: Vec<DefinitionOccurrence>,
    pub hovers: Vec<HoverOccurrence>,
}

pub struct HoverOccurrence {
    pub span: Span,
    pub value: String,
}

pub(crate) fn hover_wire_cost(value: &str, new_value: bool) -> usize {
    32usize.saturating_add(if new_value {
        16usize.saturating_add(value.len().saturating_mul(6))
    } else {
        0
    })
}

pub(crate) struct SemanticLimits {
    pub definition_entries: usize,
    pub type_definition_entries: usize,
    pub implementation_entries: usize,
    pub hover_entries: usize,
    pub hover_wire_bytes: usize,
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
    type_definitions: Vec<DefinitionOccurrence>,
    hovers: Vec<HoverOccurrence>,
    definition_limit: usize,
    type_definition_limit: usize,
    implementations: Vec<DefinitionOccurrence>,
    implementation_limit: usize,
    hover_limit: usize,
    hover_bytes: usize,
    hover_byte_limit: usize,
    hover_values: HashMap<String, u32>,
    hover_entries: HashSet<[u32; 3]>,
    lambda_hover_types: HashMap<ExprId, Vec<String>>,
    token_by_span: HashMap<(u32, u32), usize>,
    statement_scopes: HashMap<(u32, u32), Span>,
    statement_inc_dec_spans: HashSet<(u32, u32)>,
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
    type_definition_limit: usize,
    implementation_limit: usize,
    hover_limit: usize,
    hover_byte_limit: usize,
}

struct Binding {
    name: String,
    scope: Span,
    declared_at: u32,
    kind: HighlightKind,
    modifiers: u16,
    definition: Option<Span>,
    definition_owner: Option<String>,
    type_definition: BindingTypeDefinition,
    hover: Option<String>,
}

#[derive(Clone, Copy)]
enum BindingTypeDefinition {
    Unknown,
    Known(Option<DefinitionTarget>),
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
    pub fn from_source_set(files: &[FileAnalysis], symbols: &FrontendSymbols) -> Self {
        let mut metadata = Self {
            class_kinds: symbols
                .classes
                .iter()
                .map(|(name, class)| {
                    let kind = if symbols.enums.contains_key(name) {
                        HighlightKind::Enum
                    } else if class.is_annotation() {
                        HighlightKind::Decorator
                    } else if class.is_interface() {
                        HighlightKind::Interface
                    } else if class.is_object() {
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
        for file in files {
            for &declaration in &file.file.decls {
                if let Decl::Class(class) = file.file.decl(declaration) {
                    metadata.collect_class(class);
                }
            }
        }
        let aliases = files
            .iter()
            .flat_map(|file| file.file.type_aliases.iter())
            .map(|(alias, target)| (alias.as_str(), target.rsplit('.').next().unwrap_or(target)))
            .collect::<HashMap<_, _>>();
        for (&alias, &target) in &aliases {
            let target = terminal_alias_target(target, &aliases);
            let kind = target
                .and_then(|target| metadata.class_kinds.get(target).copied())
                .unwrap_or(HighlightKind::Class);
            metadata.class_kinds.insert(alias.to_string(), kind);
            if let Some(modifiers) = target
                .and_then(|target| metadata.class_modifiers.get(target))
                .copied()
            {
                metadata
                    .class_modifiers
                    .insert(alias.to_string(), modifiers);
            }
        }
        metadata
    }

    fn collect_class(&mut self, class: &ClassDecl) {
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
        let mut class_modifiers = 0;
        if class.kind == ClassKind::Interface || class.modality.is_abstract() {
            class_modifiers |= HighlightModifiers::ABSTRACT;
        }
        if is_deprecated(&class.annotations) {
            class_modifiers |= HighlightModifiers::DEPRECATED;
        }
        if class_modifiers != 0 {
            self.class_modifiers
                .insert(class.name.clone(), class_modifiers);
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
                    kind: if function.is_operator() {
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
                type_definition_limit: 0,
                implementation_limit: 0,
                hover_limit: 0,
                hover_byte_limit: 0,
            },
        );
        classifier.classify();
        classifier.finish().highlights
    }

    pub(crate) fn semantic_occurrences(
        &self,
        source: &str,
        file_index: u32,
        symbols: &FrontendSymbols,
        highlight_symbols: &HighlightSymbols,
        definition_symbols: &DefinitionSymbols,
        limits: SemanticLimits,
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
                definition_limit: limits.definition_entries,
                type_definition_limit: limits.type_definition_entries,
                implementation_limit: limits.implementation_entries,
                hover_limit: limits.hover_entries,
                hover_byte_limit: limits.hover_wire_bytes,
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
            type_definition_limit,
            implementation_limit,
            hover_limit,
            hover_byte_limit,
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
        let statement_inc_dec_spans = file
            .stmt_arena
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| {
                let owns_inc_dec = match statement {
                    Stmt::IncDec { .. } => true,
                    Stmt::AssignMember { value, .. } | Stmt::AssignIndex { value, .. } => {
                        matches!(
                            file.expr(*value),
                            Expr::Call { callee, args }
                                if args.is_empty()
                                    && matches!(
                                        file.expr(*callee),
                                        Expr::Member { name, .. }
                                            if matches!(name.as_str(), "inc" | "dec")
                                    )
                        )
                    }
                    _ => false,
                };
                if owns_inc_dec {
                    let span = file.stmt_spans[index];
                    Some((span.lo, span.hi))
                } else {
                    None
                }
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
            type_definitions: Vec::new(),
            hovers: Vec::new(),
            definition_limit,
            type_definition_limit,
            implementations: Vec::new(),
            implementation_limit,
            hover_limit,
            hover_bytes: 0,
            hover_byte_limit,
            hover_values: HashMap::new(),
            hover_entries: HashSet::new(),
            lambda_hover_types: HashMap::new(),
            token_by_span,
            statement_scopes,
            statement_inc_dec_spans,
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
                Decl::Fun(function) => self.mark_function(function, false, true, false),
                Decl::Class(class) => self.mark_class(class),
                Decl::Property(property) => {
                    let (definition, type_definition) = self.mark_property(property, true);
                    self.add_binding(
                        &property.name,
                        self.file_span(),
                        0,
                        HighlightKind::Property,
                        variable_modifier(property.is_var) | HighlightModifiers::STATIC,
                        definition,
                    );
                    self.set_last_binding_type_definition(type_definition);
                    if let Some(ty) = &property.ty {
                        self.set_last_binding_owner(ty);
                    }
                }
            }
        }
        for (index, statement) in self.file.stmt_arena.iter().enumerate() {
            self.mark_statement(StmtId(index as u32), statement, self.file.stmt_spans[index]);
        }
        // Lambda bodies precede their bindings in arena order.
        for (index, expression) in self.file.expr_arena.iter().enumerate() {
            if let Expr::Lambda { params, body } = expression {
                self.mark_lambda(ExprId(index as u32), params, *body);
            }
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
            type_definitions: self.type_definitions,
            implementations: self.implementations,
            hovers: self.hovers,
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
                            let mut target = index + 2;
                            while self.tokens.get(target + 1).map(|token| token.kind)
                                == Some(FrontendNameTokenKind::Dot)
                                && self.tokens.get(target + 2).map(|token| token.kind)
                                    == Some(FrontendNameTokenKind::Ident)
                            {
                                self.mark_index(target, HighlightKind::Namespace, 0);
                                target += 2;
                            }
                            if self.tokens.get(target).map(|token| token.kind)
                                == Some(FrontendNameTokenKind::Ident)
                            {
                                self.mark_index(target, kind, modifiers);
                            }
                        }
                    }
                }
                FrontendNameTokenKind::Ident
                    if index > 0
                        && self.tokens[index - 1].kind == FrontendNameTokenKind::At
                        && self.tokens[index - 1].span.hi == self.tokens[index].span.lo =>
                {
                    self.mark_index(index, HighlightKind::Method, 0);
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
        if class.modality.is_abstract() || class.kind == ClassKind::Interface {
            modifiers |= HighlightModifiers::ABSTRACT;
        }
        if is_deprecated(&class.annotations) {
            modifiers |= HighlightModifiers::DEPRECATED;
        }
        self.mark_named_in(class.span, &class.name, kind, modifiers, false);
        if let Some(target) = self.definition_symbols.class_target(self.file, &class.name) {
            self.push_type_definition(target.span, target);
        }
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
            self.mark_function(
                method,
                true,
                false,
                class.kind == ClassKind::Interface && matches!(method.body, FunBody::None),
            );
            self.add_member_function_binding(class.span, method, false);
        }
        let companion_scope = class
            .companion_methods
            .iter()
            .map(|method| method.span)
            .chain(class.companion_props.iter().map(|property| property.span))
            .reduce(|left, right| Span::new(left.lo.min(right.lo), left.hi.max(right.hi)));
        for method in &class.companion_methods {
            self.mark_function(method, true, true, false);
            self.add_member_function_binding(companion_scope.unwrap_or(class.span), method, true);
        }
        for property in &class.body_props {
            let (definition, type_definition) = self.mark_property(property, false);
            self.add_binding(
                &property.name,
                class.span,
                class.span.lo,
                HighlightKind::Property,
                variable_modifier(property.is_var),
                definition,
            );
            self.set_last_binding_type_definition(type_definition);
            if let Some(ty) = &property.ty {
                self.set_last_binding_owner(ty);
            }
        }
        for property in &class.companion_props {
            let (definition, type_definition) = self.mark_property(property, true);
            self.add_binding(
                &property.name,
                companion_scope.unwrap_or(class.span),
                companion_scope.unwrap_or(class.span).lo,
                HighlightKind::Property,
                variable_modifier(property.is_var) | HighlightModifiers::STATIC,
                definition,
            );
            self.set_last_binding_type_definition(type_definition);
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
                self.mark_function(method, true, false, false);
            }
            for property in &entry.props {
                let _ = self.mark_property(property, false);
            }
        }
    }

    fn mark_function(
        &mut self,
        function: &FunDecl,
        member: bool,
        static_member: bool,
        abstract_owner: bool,
    ) {
        let kind = if function.is_operator() {
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
        if (function.is_abstract() && matches!(function.body, FunBody::None)) || abstract_owner {
            modifiers |= HighlightModifiers::ABSTRACT;
        }
        if function.is_suspend() {
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
        let type_definition = self.type_definition_target_for_type_ref(
            &parameter.ty,
            definition.map_or(parameter.ty.span.lo, |span| span.lo),
        );
        self.set_last_binding_type_definition(type_definition);
        if let Some(definition) = definition {
            if let Some(target) = type_definition {
                self.push_type_definition(definition, target);
            }
        }
        let name = definition
            .map(|span| source_name(self.source, span, &parameter.name))
            .unwrap_or(&parameter.name);
        self.set_last_binding_hover(format!("{}: {}", name, render_type(&parameter.ty)));
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
        let definition = definition_name_span(self.source, parameter.span);
        let type_definition =
            self.type_definition_target_for_type_ref(&parameter.ty, definition.lo);
        self.set_last_binding_type_definition(type_definition);
        if let Some(target) = type_definition {
            self.push_type_definition(definition, target);
        }
        self.set_last_binding_hover(format!(
            "{}: {}",
            source_name(self.source, definition, &parameter.name),
            render_type(&parameter.ty)
        ));
        self.set_last_binding_owner(&parameter.ty);
        self.mark_type(&parameter.ty);
    }

    fn mark_property(
        &mut self,
        property: &PropDecl,
        static_property: bool,
    ) -> (Option<Span>, Option<DefinitionTarget>) {
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
        let type_definition = if let Some(ty) = &property.ty {
            self.type_definition_target_for_type_ref(
                ty,
                definition.map_or(property.span.lo, |span| span.lo),
            )
        } else {
            checked_property_type(property, self.type_info, None)
                .and_then(|ty| self.type_definition_target_for_ty(ty))
        };
        if let (Some(definition), Some(target)) = (definition, type_definition) {
            self.push_type_definition(definition, target);
        }
        if let Some(ty) = &property.ty {
            self.mark_type(ty);
        }
        (definition, type_definition)
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
            if let Some(definition) = definition {
                let value = source_name(self.source, definition, name).to_string();
                self.set_last_binding_hover_at(value, definition);
            }
        }
    }

    fn mark_statement(&mut self, statement_id: StmtId, statement: &Stmt, span: Span) {
        match statement {
            Stmt::Return(_, Some(label)) => self.mark_parsed_label(span, label, true),
            Stmt::Break(Some(label)) | Stmt::Continue(Some(label)) => {
                self.mark_parsed_label(span, label, false);
            }
            _ => {}
        }
        match statement {
            Stmt::Local {
                is_var,
                name,
                ty,
                init,
            } => {
                if let (Some(ty), Expr::Lambda { .. }) = (ty.as_ref(), self.file.expr(*init)) {
                    self.lambda_hover_types
                        .insert(*init, ty.fun_params.iter().map(render_type).collect());
                }
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
                let type_definition = if let Some(ty) = ty {
                    self.type_definition_target_for_type_ref(
                        ty,
                        definition.map_or(span.lo, |span| span.lo),
                    )
                } else {
                    self.expression_type_definition_target(
                        *init,
                        definition.map_or(span.lo, |span| span.lo),
                    )
                };
                self.set_last_binding_type_definition(type_definition);
                if let (Some(definition), Some(target)) = (definition, type_definition) {
                    self.push_type_definition(definition, target);
                }
                let inferred = ty.as_ref().map(render_type).or_else(|| {
                    self.type_info
                        .and_then(|types| types.expr_types.get(init.0 as usize))
                        .copied()
                        .filter(|ty| *ty != Ty::Error)
                        .map(render_ty)
                });
                if let Some(ty) = inferred {
                    let name = definition
                        .map(|span| source_name(self.source, span, name))
                        .unwrap_or(name);
                    self.set_last_binding_hover(format!(
                        "{} {name}: {ty}",
                        if *is_var { "var" } else { "val" }
                    ));
                }
                if let Some(ty) = ty {
                    self.set_last_binding_owner(ty);
                    self.mark_type(ty);
                }
            }
            Stmt::LocalDelegate {
                is_var,
                name,
                ty,
                delegate,
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
                let type_definition = if let Some(ty) = ty {
                    self.type_definition_target_for_type_ref(
                        ty,
                        definition.map_or(span.lo, |span| span.lo),
                    )
                } else {
                    self.type_info
                        .and_then(|types| types.delegate_getvalue(*delegate))
                        .map(|target| target.ret())
                        .and_then(|ty| self.type_definition_target_for_ty(ty))
                };
                self.set_last_binding_type_definition(type_definition);
                if let (Some(definition), Some(target)) = (definition, type_definition) {
                    self.push_type_definition(definition, target);
                }
                let inferred = ty.as_ref().map(render_type).or_else(|| {
                    self.type_info
                        .and_then(|types| types.delegate_getvalue(*delegate))
                        .map(|target| render_ty(target.ret()))
                });
                if let Some(ty) = inferred {
                    let name = definition
                        .map(|span| source_name(self.source, span, name))
                        .unwrap_or(name);
                    self.set_last_binding_hover(format!(
                        "{} {name}: {ty}",
                        if *is_var { "var" } else { "val" }
                    ));
                }
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
                let type_definition = self.type_definition_target_for_type_ref(
                    ty,
                    definition.map_or(span.lo, |span| span.lo),
                );
                self.set_last_binding_type_definition(type_definition);
                if let (Some(definition), Some(target)) = (definition, type_definition) {
                    self.push_type_definition(definition, target);
                }
                self.set_last_binding_hover(format!("lateinit var {name}: {}", render_type(ty)));
                self.set_last_binding_owner(ty);
                self.mark_type(ty);
            }
            Stmt::Destructure { entries, .. } => {
                let mut after = span.lo;
                for (entry_index, (name, is_var)) in entries.iter().enumerate() {
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
                    let ty = self
                        .type_info
                        .and_then(|types| {
                            types.resolved_destructure_component(statement_id, entry_index)
                        })
                        .map(|target| target.ret())
                        .filter(|ty| *ty != Ty::Error);
                    let type_definition = ty.and_then(|ty| self.type_definition_target_for_ty(ty));
                    self.set_last_binding_type_definition(type_definition);
                    if let (Some(definition), Some(target)) = (definition, type_definition) {
                        self.push_type_definition(definition, target);
                    }
                    if let Some(ty) = ty {
                        let name = definition
                            .map(|span| source_name(self.source, span, name))
                            .unwrap_or(name);
                        self.set_last_binding_hover(format!(
                            "{} {name}: {}",
                            if *is_var { "var" } else { "val" },
                            render_ty(ty)
                        ));
                    }
                }
            }
            Stmt::Assign { name, value } => {
                let modifiers =
                    self.value_modifiers(name, span.lo) | HighlightModifiers::MODIFICATION;
                self.mark_named_in(span, name, HighlightKind::Variable, modifiers, false);
                self.mark_compound_assignment_operator(statement_id, *value);
            }
            Stmt::IncDec { name, .. } => {
                let modifiers =
                    self.value_modifiers(name, span.lo) | HighlightModifiers::MODIFICATION;
                self.mark_named_in(span, name, HighlightKind::Variable, modifiers, false);
                self.mark_statement_inc_dec_operator(statement_id, span, statement);
            }
            Stmt::AssignMember { name, value, .. } => {
                self.mark_named_in(
                    span,
                    name,
                    HighlightKind::Property,
                    HighlightModifiers::MODIFICATION,
                    true,
                );
                self.mark_compound_assignment_operator(statement_id, *value);
            }
            Stmt::AssignIndex { value, .. } => {
                self.mark_compound_assignment_operator(statement_id, *value);
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
                let ty = match statement {
                    Stmt::For { .. } => Some(Ty::Int),
                    Stmt::ForEach { iterable, .. } => self.type_info.and_then(|types| {
                        types
                            .iterator_protocol(*iterable)
                            .map(|protocol| protocol.elem_ty)
                            .or_else(|| {
                                types
                                    .expr_types
                                    .get(iterable.0 as usize)
                                    .copied()
                                    .and_then(Ty::array_elem)
                            })
                    }),
                    _ => None,
                };
                let type_definition = ty.and_then(|ty| self.type_definition_target_for_ty(ty));
                self.set_last_binding_type_definition(type_definition);
                if let (Some(definition), Some(target)) = (definition, type_definition) {
                    self.push_type_definition(definition, target);
                }
                if let Some(ty) = ty {
                    let name = definition
                        .map(|span| source_name(self.source, span, name))
                        .unwrap_or(name);
                    self.set_last_binding_hover(format!("val {name}: {}", render_ty(ty)));
                }
            }
            Stmt::LocalFun(function) => {
                self.mark_function(function, false, false, false);
                let definition = self
                    .find_named(function.span, &function.name, None, None, false)
                    .map(|index| definition_name_span(self.source, self.tokens[index].span));
                let kind = if function.is_operator() {
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
                let name = definition
                    .map(|span| source_name(self.source, span, &function.name))
                    .unwrap_or(&function.name);
                let inferred_return = match function.body {
                    FunBody::Expr(body) | FunBody::Block(body) => self
                        .type_info
                        .and_then(|types| types.expr_types.get(body.0 as usize))
                        .copied()
                        .filter(|ty| *ty != Ty::Error),
                    FunBody::None => None,
                };
                self.set_last_binding_hover(render_function_hover(
                    function,
                    inferred_return,
                    name,
                    &self.tokens,
                    self.source,
                ));
            }
            Stmt::LocalClass(class) => self.mark_class(class),
            _ => {}
        }
    }

    fn mark_compound_assignment_operator(&mut self, statement: StmtId, value: ExprId) {
        match self.file.expr(value) {
            Expr::Binary { op, lhs, rhs, .. } => {
                let expression_span = self.file.expr_spans[value.0 as usize];
                let lhs_span = self.file.expr_spans[lhs.0 as usize];
                if expression_span.lo == lhs_span.lo && expression_span.hi == lhs_span.hi {
                    self.mark_binary_operator(value, *op, *lhs, *rhs, Some(statement));
                }
            }
            Expr::Call { callee, args } if args.is_empty() => {
                // Kotlin LSP omits the operator token for index-storage increments.
                if !matches!(self.file.stmt(statement), Stmt::AssignMember { .. }) {
                    return;
                }
                let Expr::Member { name, .. } = self.file.expr(*callee) else {
                    return;
                };
                if !matches!(name.as_str(), "inc" | "dec") {
                    return;
                }
                let span = self.file.expr_spans[value.0 as usize];
                self.mark_operator_in(span.lo, span.hi, self.call_operator_modifiers(value));
            }
            _ => {}
        }
    }

    fn mark_binary_operator(
        &mut self,
        id: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        compound_statement: Option<StmtId>,
    ) {
        let (start, end) = if compound_statement.is_some() {
            let span = self.file.expr_spans[id.0 as usize];
            (span.lo, span.hi)
        } else {
            (
                self.file.expr_spans[lhs.0 as usize].hi,
                self.file.expr_spans[rhs.0 as usize].lo,
            )
        };
        let operator_index = if compound_statement.is_some() {
            let index = self.tokens.partition_point(|token| token.span.lo < start);
            self.tokens
                .get(index)
                .filter(|token| {
                    token.kind == FrontendNameTokenKind::Operator
                        && token.span.lo == start
                        && token.span.hi == end
                })
                .map(|_| index)
        } else {
            self.tokens.iter().position(|token| {
                token.kind == FrontendNameTokenKind::Operator
                    && token.span.lo >= start
                    && token.span.hi <= end
            })
        };
        let Some(index) = operator_index else {
            return;
        };
        let mut modifiers = HighlightModifiers::DEFAULT_LIBRARY;
        let in_place_target = compound_statement.and_then(|statement| {
            self.type_info
                .and_then(|types| types.compound_assignment_target(statement))
        });
        if let Some(target) = in_place_target {
            modifiers = match target {
                CompoundAssignmentTarget::Member { .. } => 0,
                CompoundAssignmentTarget::SourceExtension { .. } => HighlightModifiers::STATIC,
                CompoundAssignmentTarget::LibraryExtension(_) => {
                    HighlightModifiers::DEFAULT_LIBRARY
                }
            };
        } else if let Some(name) = op.arith_operator_name() {
            if self
                .type_info
                .is_some_and(|types| types.resolved_calls.contains_key(&id))
            {
                modifiers = self.function_reference_modifiers(id, name)
                    & HighlightModifiers::DEFAULT_LIBRARY;
            } else if let Some(Ty::Obj(owner, _)) = self
                .type_info
                .and_then(|types| types.expr_types.get(lhs.0 as usize))
                .copied()
                .map(Ty::non_null)
            {
                if self
                    .symbols
                    .class_by_type_name(owner)
                    .is_some_and(|class| class.has_method(name))
                {
                    modifiers = 0;
                }
            }
        }
        self.mark_index(index, HighlightKind::Operator, modifiers);
    }

    fn mark_statement_inc_dec_operator(
        &mut self,
        statement_id: StmtId,
        span: Span,
        statement: &Stmt,
    ) {
        let Stmt::IncDec { dec, .. } = statement else {
            return;
        };
        let name = if *dec { "dec" } else { "inc" };
        let modifiers = self.statement_operator_modifiers(statement_id, name);
        self.mark_operator_in(span.lo, span.hi, modifiers);
    }

    fn mark_unary_operator(&mut self, expression: ExprId, op: UnOp, operand: ExprId) {
        let name = op.operator_name();
        let span = self.file.expr_spans[expression.0 as usize];
        let operand_span = self.file.expr_spans[operand.0 as usize];
        self.mark_operator_in(
            span.lo,
            operand_span.lo,
            self.expression_operator_modifiers(expression, name),
        );
    }

    fn mark_expression_inc_dec_operator(
        &mut self,
        expression: ExprId,
        target: ExprId,
        dec: bool,
        prefix: bool,
    ) {
        let name = if dec { "dec" } else { "inc" };
        let span = self.file.expr_spans[expression.0 as usize];
        // The statement owns normalized statement-position increments.
        if self.statement_inc_dec_spans.contains(&(span.lo, span.hi)) {
            return;
        }
        let target_span = self.file.expr_spans[target.0 as usize];
        let (lo, hi) = if prefix {
            (span.lo, target_span.lo)
        } else {
            (target_span.hi, span.hi)
        };
        self.mark_operator_in(lo, hi, self.expression_operator_modifiers(expression, name));
    }

    fn mark_operator_in(&mut self, lo: u32, hi: u32, modifiers: u16) {
        if let Some(index) = self.tokens.iter().position(|token| {
            token.kind == FrontendNameTokenKind::Operator
                && token.span.lo >= lo
                && token.span.hi <= hi
        }) {
            self.mark_index(index, HighlightKind::Operator, modifiers);
        }
    }

    fn expression_operator_modifiers(&self, expression: ExprId, name: &str) -> u16 {
        let Some(types) = self.type_info else {
            return HighlightModifiers::DEFAULT_LIBRARY;
        };
        if types.resolved_operator_call(expression, name).is_none() {
            return HighlightModifiers::DEFAULT_LIBRARY;
        }
        let mut modifiers = 0;
        if types.resolved_operator_call_is_extension(expression, name) {
            modifiers |= HighlightModifiers::STATIC;
        }
        if types
            .resolved_operator_call_owner(expression, name)
            .is_some_and(|owner| default_library_member_owner(self.symbols, owner))
        {
            modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
        }
        modifiers
    }

    fn statement_operator_modifiers(&self, statement: StmtId, name: &str) -> u16 {
        let Some(types) = self.type_info else {
            return HighlightModifiers::DEFAULT_LIBRARY;
        };
        if types.resolved_stmt_operator_call(statement, name).is_none() {
            return HighlightModifiers::DEFAULT_LIBRARY;
        }
        let mut modifiers = 0;
        if types.resolved_stmt_operator_call_is_extension(statement, name) {
            modifiers |= HighlightModifiers::STATIC;
        }
        if types
            .resolved_stmt_operator_call_owner(statement, name)
            .is_some_and(|owner| default_library_member_owner(self.symbols, owner))
        {
            modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
        }
        modifiers
    }

    fn call_operator_modifiers(&self, call: ExprId) -> u16 {
        let Some(types) = self.type_info else {
            return HighlightModifiers::DEFAULT_LIBRARY;
        };
        let mut modifiers = 0;
        if types.resolved_call_is_extension(call) {
            modifiers |= HighlightModifiers::STATIC;
        }
        if types
            .resolved_call_owner(call)
            .is_some_and(|owner| default_library_member_owner(self.symbols, owner))
        {
            modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
        }
        if !types.resolved_call_is_member(call) && !types.resolved_call_is_extension(call) {
            modifiers |= HighlightModifiers::DEFAULT_LIBRARY;
        }
        modifiers
    }

    fn mark_lambda(&mut self, id: ExprId, params: &[String], body: ExprId) {
        let span = self.file.expr_spans[id.0 as usize];
        let scope = self.file.expr_spans[body.0 as usize];
        let implicit_parameter = if params.is_empty() {
            self.type_info
                .and_then(|types| types.expr_types.get(id.0 as usize))
                .copied()
                .and_then(Ty::fun_params)
                .map(|parameters| {
                    if self
                        .type_info
                        .is_some_and(|types| types.lambda_has_receiver(id))
                    {
                        parameters.get(1..).unwrap_or_default()
                    } else {
                        parameters
                    }
                })
                .filter(|parameters| parameters.len() == 1)
                .map(|parameters| parameters[0])
        } else {
            None
        };
        if let Some(ty) = implicit_parameter {
            self.add_binding(
                "it",
                scope,
                scope.lo,
                HighlightKind::Parameter,
                HighlightModifiers::READONLY,
                None,
            );
            self.set_last_binding_type_definition(self.type_definition_target_for_ty(ty));
            self.set_last_binding_hover(format!("it: {}", render_ty(ty)));
        }
        for (parameter_index, name) in params.iter().enumerate() {
            let declared_type = self
                .file
                .lambda_param_types
                .get(&id.0)
                .and_then(|types| types.get(parameter_index))
                .and_then(Option::as_ref)
                .cloned();
            let definition = self.mark_named_in_span(
                span,
                name,
                HighlightKind::Parameter,
                HighlightModifiers::DECLARATION | HighlightModifiers::READONLY,
                false,
            );
            self.add_binding(
                name,
                scope,
                scope.lo,
                HighlightKind::Parameter,
                HighlightModifiers::READONLY,
                definition,
            );
            if let Some(ty) = declared_type {
                let type_definition = self.type_definition_target_for_type_ref(
                    &ty,
                    definition.map_or(span.lo, |span| span.lo),
                );
                self.set_last_binding_type_definition(type_definition);
                if let (Some(definition), Some(target)) = (definition, type_definition) {
                    self.push_type_definition(definition, target);
                }
                let name = definition
                    .map(|span| source_name(self.source, span, name))
                    .unwrap_or(name);
                self.set_last_binding_hover(format!("{name}: {}", render_type(&ty)));
            } else if let Some(ty) = self
                .lambda_hover_types
                .get(&id)
                .and_then(|types| types.get(parameter_index))
                .cloned()
            {
                let name = definition
                    .map(|span| source_name(self.source, span, name))
                    .unwrap_or(name);
                self.set_last_binding_hover(format!("{name}: {ty}"));
            }
        }
        if let Some(types) = self.file.lambda_param_types.get(&id.0) {
            for ty in types.iter().flatten() {
                self.mark_type(ty);
            }
        }
    }

    fn mark_expression(&mut self, id: ExprId, expression: &Expr) {
        let span = self.file.expr_spans[id.0 as usize];
        match expression {
            Expr::Name(name) => {
                if let Some((receiver, label)) = name.rsplit_once('@') {
                    if receiver == "this" || receiver.starts_with("super") {
                        self.mark_receiver_label(span, receiver, label);
                        return;
                    }
                }
                if name == "this" {
                    if self.has_expression_label_before(span)
                        || !self.enclosing_classes(span.lo).is_empty()
                    {
                        self.mark_exact(span, HighlightKind::Class, 0);
                    }
                    return;
                }
                if name == "super" {
                    return;
                }
                let (kind, modifiers) = if let Some(&call) = self.callees.get(&id) {
                    if self.is_constructor_call(call, name) {
                        (HighlightKind::Method, 0)
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
                self.push_expression_type_definition(span, id);
                let binding_hover = self
                    .binding_at_kind(name, span.lo, self.callees.contains_key(&id))
                    .and_then(|binding| binding.hover.clone());
                if let Some(value) = binding_hover {
                    self.push_hover(span, value);
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let expression_span = self.file.expr_spans[id.0 as usize];
                let lhs_span = self.file.expr_spans[lhs.0 as usize];
                if expression_span.lo != lhs_span.lo || expression_span.hi != lhs_span.hi {
                    self.mark_binary_operator(id, *op, *lhs, *rhs, None);
                }
            }
            Expr::Unary { op, operand } => self.mark_unary_operator(id, *op, *operand),
            Expr::IncDec {
                target,
                dec,
                prefix,
            } => self.mark_expression_inc_dec_operator(id, *target, *dec, *prefix),
            Expr::Member { receiver, name } => {
                let call = self.callees.get(&id).copied();
                let highlight = self.member_highlight(id, *receiver, name, call);
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
                    self.push_expression_type_definition(source_span, id);
                }
            }
            Expr::SafeCall {
                receiver,
                name,
                args,
            } => {
                let call = args.as_ref().map(|_| id);
                let highlight = self.member_highlight(id, *receiver, name, call);
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
                    self.push_expression_type_definition(source_span, id);
                }
            }
            Expr::CallableRef { receiver, name } if name != "class" => {
                let highlight = if let Some(receiver) = receiver {
                    self.member_highlight(id, *receiver, name, None)
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
                            Some(id),
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
            Expr::Return {
                label: Some(label), ..
            } => self.mark_parsed_label(span, label, true),
            Expr::Break { label: Some(label) } | Expr::Continue { label: Some(label) } => {
                self.mark_parsed_label(span, label, false);
            }
            Expr::Lambda { .. } => {}
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
                    let type_definition = self.type_definition_target_for_type_ref(
                        &catch.ty,
                        definition.map_or(catch.ty.span.lo, |span| span.lo),
                    );
                    self.set_last_binding_type_definition(type_definition);
                    if let (Some(definition), Some(target)) = (definition, type_definition) {
                        self.push_type_definition(definition, target);
                    }
                    let name = definition
                        .map(|span| source_name(self.source, span, &catch.name))
                        .unwrap_or(&catch.name);
                    self.set_last_binding_hover(format!("val {name}: {}", render_type(&catch.ty)));
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

    fn contextual_class_target(&self, name: &str, at: u32) -> Option<DefinitionTarget> {
        self.contextual_class_owner(name, at)
            .and_then(|owner| self.definition_symbols.class_target_for_owner(&owner))
    }

    fn contextual_class_owner(&self, name: &str, at: u32) -> Option<String> {
        if !name.contains('.') {
            for class in self.enclosing_classes(at) {
                let nested = format!("{}.{}", class.name, name);
                if let Some(owner) = self.definition_symbols.class_owner(self.file, &nested) {
                    return Some(owner);
                }
            }
        }
        self.definition_symbols.class_owner(self.file, name)
    }

    fn enclosing_classes(&self, at: u32) -> Vec<&ClassDecl> {
        let mut classes = self
            .file
            .decls
            .iter()
            .filter_map(|&declaration| match self.file.decl(declaration) {
                Decl::Class(class) if class.span.lo <= at && at <= class.span.hi => Some(class),
                _ => None,
            })
            .collect::<Vec<_>>();
        classes.sort_unstable_by_key(|class| class.span.hi.saturating_sub(class.span.lo));
        classes
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
                return self.contextual_class_target(name, at);
            }
        }
        if self.highlight_symbols.class_kinds.contains_key(name) {
            return self.contextual_class_target(name, at);
        }
        self.binding_at_kind(name, at, self.callees.contains_key(&expression))
            .and_then(|binding| binding.definition)
            .map(|span| DefinitionTarget {
                file: self.file_index,
                span,
            })
    }

    fn type_reference_target(&self, ty: &TypeRef, at: u32) -> Option<DefinitionTarget> {
        self.type_parameter_binding(ty, at)
            .and_then(|binding| binding.definition)
            .map(|span| DefinitionTarget {
                file: self.file_index,
                span,
            })
            .or_else(|| self.contextual_class_target(&ty.name, at))
    }

    fn type_parameter_binding(&self, ty: &TypeRef, at: u32) -> Option<&Binding> {
        if ty.name.contains('.') {
            return None;
        }
        self.binding_at_matching(&ty.name, at, |binding| {
            binding.kind == HighlightKind::TypeParameter
        })
    }

    fn push_expression_type_definition(&mut self, span: Span, expression: ExprId) {
        if self.type_definitions.len() >= self.type_definition_limit {
            return;
        }
        let expression = self.callees.get(&expression).copied().unwrap_or(expression);
        if let Some(target) = self.expression_type_definition_target(expression, span.lo) {
            self.push_type_definition(span, target);
        }
    }

    fn expression_type_definition_target(
        &self,
        expression: ExprId,
        at: u32,
    ) -> Option<DefinitionTarget> {
        let mut declared_target = None;
        if let Expr::Name(name) = self.file.expr(expression) {
            if let Some(binding) = self.binding_at(name, at) {
                match binding.type_definition {
                    BindingTypeDefinition::Unknown => {}
                    BindingTypeDefinition::Known(None) => return None,
                    BindingTypeDefinition::Known(target) => declared_target = target,
                }
            }
        }
        self.type_info
            .and_then(|types| types.expr_types.get(expression.0 as usize))
            .copied()
            .and_then(|ty| self.type_definition_target_for_ty(ty))
            .or(declared_target)
    }

    fn type_definition_target_for_ty(&self, ty: Ty) -> Option<DefinitionTarget> {
        let ty = ty.non_null();
        if matches!(ty, Ty::TyParam(_, _)) {
            return None;
        }
        ty.kotlin_class_internal()
            .and_then(|owner| self.definition_symbols.class_target_for_type(owner))
    }

    fn type_definition_target_for_type_ref(
        &self,
        ty: &TypeRef,
        at: u32,
    ) -> Option<DefinitionTarget> {
        if self.type_parameter_binding(ty, at).is_some() {
            return None;
        }
        self.contextual_class_target(&ty.name, at)
    }

    fn push_type_definition_for_type_ref(&mut self, span: Span, ty: &TypeRef) {
        let Some(target) = self.type_definition_target_for_type_ref(ty, span.lo) else {
            return;
        };
        self.push_type_definition(span, target);
    }

    fn push_type_definition(&mut self, span: Span, target: DefinitionTarget) {
        if self.type_definitions.len() >= self.type_definition_limit {
            return;
        }
        self.type_definitions.push(DefinitionOccurrence {
            span: definition_name_span(self.source, span),
            target,
        });
    }

    fn push_definition(&mut self, span: Span, target: DefinitionTarget) {
        let span = self.push_definition_only(span, target);
        if let Some(value) = self.definition_symbols.hover_value(target) {
            self.push_hover(span, value.to_owned());
        }
    }

    fn push_definition_only(&mut self, span: Span, target: DefinitionTarget) -> Span {
        let span = definition_name_span(self.source, span);
        if self.definitions.len() < self.definition_limit {
            self.definitions.push(DefinitionOccurrence { span, target });
        }
        let implementation_limit = self
            .implementation_limit
            .saturating_sub(self.definitions.len());
        self.implementations.truncate(implementation_limit);
        let remaining = implementation_limit.saturating_sub(self.implementations.len());
        self.implementations.extend(
            self.definition_symbols
                .implementation_targets(target)
                .iter()
                .copied()
                .take(remaining)
                .map(|target| DefinitionOccurrence { span, target }),
        );
        debug_assert!(
            self.definitions.len() + self.implementations.len()
                <= self.implementation_limit.max(self.definition_limit)
        );
        span
    }

    fn push_hover(&mut self, span: Span, value: String) {
        let span = definition_name_span(self.source, span);
        let value_index = self
            .hover_values
            .get(&value)
            .copied()
            .unwrap_or(self.hover_values.len() as u32);
        if !self.hover_entries.insert([span.lo, span.hi, value_index]) {
            return;
        }
        let new_value = !self.hover_values.contains_key(&value);
        let bytes = hover_wire_cost(&value, new_value);
        if self.hovers.len() >= self.hover_limit
            || bytes > self.hover_byte_limit.saturating_sub(self.hover_bytes)
        {
            self.hover_entries.remove(&[span.lo, span.hi, value_index]);
            return;
        }
        self.hover_bytes += bytes;
        if new_value {
            self.hover_values.insert(value.clone(), value_index);
        }
        self.hovers.push(HoverOccurrence { span, value });
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
        let types = self.type_info?;
        if let Some(member) = types.resolved_companion(expression) {
            if member.name != name {
                return None;
            }
            let owner = member.owner?.render();
            let owner = owner.strip_suffix("$Companion").unwrap_or(&owner);
            return self
                .definition_symbols
                .member_target(owner, name, kind, &member.params);
        }

        let (owner, resolved_name, params) = types.resolved_module_member_signature(expression)?;
        if resolved_name != name {
            return None;
        }
        let owner = owner.render();
        let owner = owner.strip_suffix("$Companion")?;
        self.definition_symbols
            .member_target(owner, name, kind, params)
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
            if let Some(target) = self
                .type_info
                .and_then(|types| types.source_extension_property(expression))
                .and_then(|property| property.source_key)
                .and_then(|(file, declaration)| {
                    self.definition_symbols
                        .declaration_target(file, declaration)
                })
            {
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
                        let nested = matches!(self.file.expr(expression), Expr::Call { .. })
                            .then(|| self.receiver_definition_owner(receiver))
                            .flatten()
                            .and_then(|owner| {
                                self.definition_symbols.nested_class_target(&owner, name)
                            })
                            .and_then(|target| {
                                self.definition_symbols
                                    .hover_value(target)
                                    .map(|value| (target, value.to_owned()))
                            });
                        let selected_hover = self
                            .definition_symbols
                            .hover_value(target)
                            .map(str::to_owned);
                        if let (Some((nested_target, nested)), Some(selected)) =
                            (nested, selected_hover)
                        {
                            let span = self.push_definition_only(source_span, nested_target);
                            self.push_definition_only(source_span, target);
                            self.push_hover(
                                span,
                                format!("{nested}\n````\n\n---\n````kotlin\n{selected}"),
                            );
                        } else {
                            self.push_definition(source_span, target);
                        }
                    }
                    return;
                }
            }
        }
        let Some(owner) = self.receiver_definition_owner(receiver) else {
            return;
        };
        let nested_target = resolved_expression
            .filter(|&expression| matches!(self.file.expr(expression), Expr::Call { .. }))
            .and_then(|_| self.definition_symbols.nested_class_target(&owner, name));
        let targets = self.definition_symbols.member_targets(&owner, name, kind);
        if !targets.is_empty() {
            if let Some((nested_target, nested)) = nested_target.and_then(|target| {
                self.definition_symbols
                    .hover_value(target)
                    .map(|value| (target, value.to_owned()))
            }) {
                let selected = targets
                    .iter()
                    .filter_map(|&target| self.definition_symbols.hover_value(target))
                    .collect::<Vec<_>>()
                    .join("\n````\n\n---\n````kotlin\n");
                self.push_definition_only(source_span, nested_target);
                for &target in &targets {
                    self.push_definition_only(source_span, target);
                }
                self.push_hover(
                    source_span,
                    format!("{nested}\n````\n\n---\n````kotlin\n{selected}"),
                );
            } else {
                for target in targets {
                    self.push_definition(source_span, target);
                }
            }
            return;
        }
        if let Some(target) = nested_target {
            self.push_definition(source_span, target);
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
                .contextual_class_owner(name, self.file.expr_spans[receiver.0 as usize].lo)
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
            if let Some(owner) =
                self.contextual_class_owner(name, self.file.expr_spans[receiver.0 as usize].lo)
            {
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
        expression: ExprId,
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
        if call.is_none() {
            if let Some(member) = self
                .type_info
                .and_then(|types| types.resolved_member(expression))
            {
                let default_library = member
                    .member
                    .owner
                    .is_some_and(|owner| default_library_member_owner(self.symbols, owner));
                return MemberHighlight {
                    kind: HighlightKind::Property,
                    modifiers: HighlightModifiers::READONLY
                        | if default_library {
                            HighlightModifiers::DEFAULT_LIBRARY
                        } else {
                            0
                        },
                };
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
                .is_some_and(|owner| default_library_member_owner(self.symbols, owner))
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
            if let Some(target) = self.type_reference_target(ty, source_span.lo) {
                self.push_definition(source_span, target);
                self.push_type_definition_for_type_ref(source_span, ty);
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
            type_definition: BindingTypeDefinition::Unknown,
            hover: None,
        });
    }

    fn set_last_binding_owner(&mut self, ty: &TypeRef) {
        let owner = self.contextual_class_owner(&ty.name, ty.span.lo);
        if let Some(binding) = self.bindings.last_mut() {
            binding.definition_owner = owner;
        }
    }

    fn set_last_binding_type_definition(&mut self, target: Option<DefinitionTarget>) {
        if let Some(binding) = self.bindings.last_mut() {
            binding.type_definition = BindingTypeDefinition::Known(target);
        }
    }

    fn set_last_binding_hover(&mut self, value: String) {
        let definition = self.bindings.last_mut().and_then(|binding| {
            binding.hover = Some(value.clone());
            binding.definition
        });
        if let Some(span) = definition {
            self.push_hover(span, value);
        }
    }

    fn set_last_binding_hover_at(&mut self, value: String, span: Span) {
        if let Some(binding) = self.bindings.last_mut() {
            binding.hover = Some(value.clone());
        }
        self.push_hover(span, value);
    }

    fn add_member_function_binding(
        &mut self,
        scope: Span,
        function: &FunDecl,
        static_member: bool,
    ) {
        let kind = if function.is_operator() {
            HighlightKind::Operator
        } else {
            HighlightKind::Method
        };
        let mut modifiers = if static_member {
            HighlightModifiers::STATIC
        } else {
            0
        };
        if function.is_abstract() {
            modifiers |= HighlightModifiers::ABSTRACT;
        }
        if function.is_suspend() {
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

    fn mark_index(&mut self, index: usize, kind: HighlightKind, modifiers: u16) {
        self.mark_index_span(index, self.tokens[index].span, kind, modifiers);
    }

    fn mark_index_span(&mut self, index: usize, span: Span, kind: HighlightKind, modifiers: u16) {
        self.classified[index] = Some(HighlightOccurrence {
            span,
            kind,
            modifiers: HighlightModifiers::from_bits(modifiers),
        });
    }

    fn mark_parsed_label(&mut self, owner: Span, label: &str, return_label: bool) {
        let Some(index) = self.parsed_label_index(owner, label) else {
            return;
        };
        if return_label {
            let span = Span::new(self.tokens[index - 1].span.lo, self.tokens[index].span.hi);
            self.mark_index_span(index, span, HighlightKind::Function, 0);
        } else {
            self.classified[index] = None;
        }
    }

    fn parsed_label_index(&self, owner: Span, label: &str) -> Option<usize> {
        self.tokens.iter().enumerate().find_map(|(index, token)| {
            let name = self.tokens.get(index + 1)?;
            (token.kind == FrontendNameTokenKind::At
                && token.span.lo >= owner.lo
                && name.span.hi <= owner.hi
                && token.span.hi == name.span.lo
                && name.kind == FrontendNameTokenKind::Ident
                && name.text(self.source) == label)
                .then_some(index + 1)
        })
    }

    fn mark_receiver_label(&mut self, receiver_span: Span, receiver: &str, label: &str) {
        let owner = self.receiver_name_span(receiver_span.lo);
        let supertype = receiver
            .strip_prefix("super<")
            .and_then(|receiver| receiver.strip_suffix('>'));
        let (receiver_kind, receiver_modifiers) = if let Some(supertype) = supertype {
            let (kind, modifiers) = self.type_token(supertype, receiver_span.lo);
            self.mark_named_in_span(owner, supertype, kind, modifiers, false);
            (kind, modifiers)
        } else {
            self.type_token(label, receiver_span.lo)
        };
        self.mark_exact(receiver_span, receiver_kind, receiver_modifiers);
        if let Some(index) = self.parsed_label_index(owner, label) {
            let (kind, modifiers) = self.type_token(label, self.tokens[index].span.lo);
            let span = Span::new(self.tokens[index - 1].span.lo, self.tokens[index].span.hi);
            self.mark_index_span(index, span, kind, modifiers);
        }
    }

    fn has_expression_label_before(&self, span: Span) -> bool {
        let Some(index) = self.tokens.iter().position(|token| token.span == span) else {
            return false;
        };
        index >= 2
            && self.tokens[index - 1].kind == FrontendNameTokenKind::At
            && self.tokens[index - 2].kind == FrontendNameTokenKind::Ident
            && self.tokens[index - 2].span.hi == self.tokens[index - 1].span.lo
            && self.tokens[index - 1].span.hi < span.lo
    }

    fn receiver_name_span(&self, start: u32) -> Span {
        let rest = &self.source[start as usize..];
        let len = rest
            .char_indices()
            .find_map(|(offset, character)| {
                (offset > 0
                    && (character.is_whitespace()
                        || matches!(
                            character,
                            '.' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
                        )))
                .then_some(offset)
            })
            .unwrap_or(rest.len());
        Span::new(start, start + len as u32)
    }
}

fn variable_modifier(is_var: bool) -> u16 {
    if is_var {
        HighlightModifiers::MODIFICATION
    } else {
        HighlightModifiers::READONLY
    }
}

fn default_library_member_owner(symbols: &FrontendSymbols, owner: krusty::types::TypeName) -> bool {
    symbols.libraries.is_default_library_owner(owner)
}

fn terminal_alias_target<'a>(
    mut target: &'a str,
    aliases: &HashMap<&'a str, &'a str>,
) -> Option<&'a str> {
    let mut seen = std::collections::HashSet::new();
    while let Some(next) = aliases.get(target).copied() {
        if !seen.insert(target) {
            return None;
        }
        target = next;
    }
    Some(target)
}

fn is_deprecated(annotations: &[String]) -> bool {
    annotations
        .iter()
        .any(|annotation| annotation == "Deprecated")
}

fn function_modifiers(function: &FunDecl) -> u16 {
    let mut modifiers = 0;
    if function.is_abstract() && matches!(function.body, FunBody::None) {
        modifiers |= HighlightModifiers::ABSTRACT;
    }
    if function.is_suspend() {
        modifiers |= HighlightModifiers::ASYNC;
    }
    if is_deprecated(&function.annotations) {
        modifiers |= HighlightModifiers::DEPRECATED;
    }
    modifiers
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
