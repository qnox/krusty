//! Editor-neutral completion symbols derived from the parsed and checked frontend.

use std::collections::HashMap;

use krusty::ast::{
    ClassDecl, ClassKind, Decl, Expr, File, FunBody, FunDecl, Param, PropDecl, Stmt, TypeRef,
};
use krusty::diag::Span;
use krusty::types::{Ty, Visibility};

use super::FileAnalysis;

/// LSP 3.17 completion-item-kind discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum CompletionKind {
    Method = 2,
    Function = 3,
    Property = 10,
    Variable = 6,
    Class = 7,
    Interface = 8,
    Enum = 13,
    EnumMember = 20,
    Constant = 21,
    Struct = 22,
    Operator = 24,
    TypeParameter = 25,
}

#[derive(Clone)]
struct Symbol {
    label: String,
    detail: String,
    kind: CompletionKind,
    result_type: Option<String>,
}

struct GlobalSymbol {
    package: String,
    visibility: Visibility,
    symbol: Symbol,
}

/// One scoped symbol before it is interned into the long-lived completion snapshot.
pub(crate) struct ScopedCompletionSymbol {
    pub scope: Span,
    pub declared_at: u32,
    pub label: String,
    pub detail: String,
    pub kind: CompletionKind,
    pub result_type: Option<String>,
    pub priority: u8,
}

/// Source-set declaration catalog shared while building each document's compact snapshot.
pub(crate) struct CompletionSymbols {
    globals: Vec<GlobalSymbol>,
    members: HashMap<String, Vec<Symbol>>,
    class_owners: HashMap<(String, String), String>,
    simple_class_owners: HashMap<String, Vec<String>>,
}

impl CompletionSymbols {
    pub fn from_source_set(sources: &[&str], files: &[FileAnalysis]) -> Self {
        let mut result = Self {
            globals: Vec::new(),
            members: HashMap::new(),
            class_owners: HashMap::new(),
            simple_class_owners: HashMap::new(),
        };
        for file in files {
            let package = file.file.package.clone().unwrap_or_default();
            let local_classes: std::collections::HashSet<_> = file
                .file
                .stmt_arena
                .iter()
                .filter_map(|statement| match statement {
                    Stmt::LocalClass(class) => {
                        Some((class.name.as_str(), class.span.lo, class.span.hi))
                    }
                    _ => None,
                })
                .collect();
            for &declaration in &file.file.decls {
                let Decl::Class(class) = file.file.decl(declaration) else {
                    continue;
                };
                if local_classes.contains(&(class.name.as_str(), class.span.lo, class.span.hi)) {
                    continue;
                }
                let owner = qualified_name(&package, &class.name);
                result
                    .class_owners
                    .insert((package.clone(), class.name.clone()), owner.clone());
                result
                    .simple_class_owners
                    .entry(class.name.clone())
                    .or_default()
                    .push(owner);
            }
        }
        let mut inheritance = Vec::new();
        for (source, file) in sources.iter().copied().zip(files) {
            let package = file.file.package.clone().unwrap_or_default();
            let local_classes: std::collections::HashSet<_> = file
                .file
                .stmt_arena
                .iter()
                .filter_map(|statement| match statement {
                    Stmt::LocalClass(class) => {
                        Some((class.name.as_str(), class.span.lo, class.span.hi))
                    }
                    _ => None,
                })
                .collect();
            for &declaration in &file.file.decls {
                match file.file.decl(declaration) {
                    Decl::Fun(function) => {
                        let symbol = function_symbol(source, function, false);
                        if function.receiver.is_none() {
                            result.globals.push(GlobalSymbol {
                                package: package.clone(),
                                visibility: function.visibility,
                                symbol,
                            });
                        }
                    }
                    Decl::Class(class) => {
                        if local_classes.contains(&(
                            class.name.as_str(),
                            class.span.lo,
                            class.span.hi,
                        )) {
                            continue;
                        }
                        let owner = result.owner_for_name(&file.file, &class.name);
                        result.add_class(source, &package, &owner, &file.file, class);
                        inheritance.extend(
                            class
                                .base_class
                                .iter()
                                .map(|base| {
                                    (owner.clone(), result.owner_for_name(&file.file, base))
                                })
                                .chain(class.supertypes.iter().map(|base| {
                                    (owner.clone(), result.owner_for_name(&file.file, &base.name))
                                })),
                        );
                    }
                    Decl::Property(property) => {
                        let mut symbol = property_symbol(property);
                        symbol.result_type = property
                            .ty
                            .as_ref()
                            .map(|ty| result.owner_for_type(&file.file, ty));
                        if property.receiver.is_none() {
                            result.globals.push(GlobalSymbol {
                                package: package.clone(),
                                visibility: property.visibility,
                                symbol,
                            });
                        }
                    }
                }
            }
            for (alias, target) in &file.file.type_aliases {
                result.globals.push(GlobalSymbol {
                    package: package.clone(),
                    visibility: Visibility::Public,
                    symbol: Symbol {
                        label: alias.clone(),
                        detail: format!("typealias {alias} = {target}"),
                        kind: CompletionKind::Class,
                        result_type: Some(format!("@{alias}")),
                    },
                });
            }
            for (alias, parameters, result_type) in &file.file.type_alias_fun {
                let params = parameters.join(", ");
                result.globals.push(GlobalSymbol {
                    package: package.clone(),
                    visibility: Visibility::Public,
                    symbol: Symbol {
                        label: alias.clone(),
                        detail: format!(
                            "typealias {alias} = ({params}) -> {}",
                            render_type(result_type)
                        ),
                        kind: CompletionKind::Interface,
                        result_type: Some(format!("@{alias}")),
                    },
                });
            }
        }
        for _ in 0..inheritance.len() {
            let mut changed = false;
            for (child, parent) in &inheritance {
                let inherited = result.members.get(parent).cloned().unwrap_or_default();
                let child_members = result.members.entry(child.clone()).or_default();
                for member in inherited {
                    if !child_members
                        .iter()
                        .any(|existing| existing.label == member.label)
                    {
                        child_members.push(member);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        result
    }

    fn add_class(
        &mut self,
        source: &str,
        package: &str,
        owner: &str,
        file: &File,
        class: &ClassDecl,
    ) {
        let kind = class_kind(class);
        self.globals.push(GlobalSymbol {
            package: package.to_string(),
            visibility: class.visibility,
            symbol: Symbol {
                label: class.name.clone(),
                detail: class_detail(class),
                kind,
                result_type: Some(format!("@{owner}")),
            },
        });

        let mut instance = Vec::new();
        for property in &class.props {
            if property.is_property
                && matches!(
                    property.visibility,
                    Visibility::Public | Visibility::Internal
                )
            {
                instance.push(Symbol {
                    label: property.name.clone(),
                    detail: format!(
                        "{} {}: {}",
                        if property.is_var { "var" } else { "val" },
                        property.name,
                        render_type(&property.ty)
                    ),
                    kind: CompletionKind::Property,
                    result_type: Some(self.owner_for_type(file, &property.ty)),
                });
            }
        }
        for property in &class.body_props {
            if matches!(
                property.visibility,
                Visibility::Public | Visibility::Internal
            ) {
                let mut symbol = property_symbol(property);
                symbol.result_type = property.ty.as_ref().map(|ty| self.owner_for_type(file, ty));
                instance.push(symbol);
            }
        }
        for function in &class.methods {
            if matches!(
                function.visibility,
                Visibility::Public | Visibility::Internal
            ) {
                instance.push(function_symbol(source, function, true));
            }
        }
        self.members
            .entry(owner.to_string())
            .or_default()
            .extend(instance);

        let static_owner = format!("@{owner}");
        let mut static_members = Vec::new();
        for property in &class.companion_props {
            if matches!(
                property.visibility,
                Visibility::Public | Visibility::Internal
            ) {
                let mut symbol = property_symbol(property);
                symbol.result_type = property.ty.as_ref().map(|ty| self.owner_for_type(file, ty));
                static_members.push(symbol);
            }
        }
        for function in &class.companion_methods {
            if matches!(
                function.visibility,
                Visibility::Public | Visibility::Internal
            ) {
                static_members.push(function_symbol(source, function, true));
            }
        }
        for entry in &class.enum_entries {
            static_members.push(Symbol {
                label: entry.name.clone(),
                detail: format!("enum entry {}.{}", class.name, entry.name),
                kind: CompletionKind::EnumMember,
                result_type: Some(owner.to_string()),
            });
        }
        self.members
            .entry(static_owner)
            .or_default()
            .extend(static_members);
    }

    pub(crate) fn members(&self) -> impl Iterator<Item = (&str, &str, &str, CompletionKind)> {
        self.members.iter().flat_map(|(owner, symbols)| {
            symbols.iter().map(|symbol| {
                (
                    owner.as_str(),
                    symbol.label.as_str(),
                    symbol.detail.as_str(),
                    symbol.kind,
                )
            })
        })
    }

    fn globals_for<'a>(&'a self, file: &'a File) -> impl Iterator<Item = &'a Symbol> {
        let package = file.package.as_deref().unwrap_or_default();
        self.globals
            .iter()
            .filter(move |global| {
                if global.visibility.is_private() {
                    return false;
                }
                if global.package == package {
                    return true;
                }
                let qualified = if global.package.is_empty() {
                    global.symbol.label.clone()
                } else {
                    format!("{}.{}", global.package, global.symbol.label)
                };
                let wildcard = format!("{}.*", global.package);
                file.imports
                    .iter()
                    .any(|import| import == &qualified || import == &wildcard)
            })
            .map(|global| &global.symbol)
    }

    fn owner_for_name(&self, file: &File, name: &str) -> String {
        if name.contains('.') {
            let dotted = name.replace('$', ".");
            if self.class_owners.values().any(|owner| owner == &dotted) {
                return dotted;
            }
        }
        let simple = simple_name(name);
        for import in &file.imports {
            if import.rsplit('.').next() == Some(simple.as_str())
                && self.class_owners.values().any(|owner| owner == import)
            {
                return import.clone();
            }
        }
        let package = file.package.clone().unwrap_or_default();
        if let Some(owner) = self.class_owners.get(&(package, simple.clone())) {
            return owner.clone();
        }
        let wildcard_owners: Vec<_> = file
            .imports
            .iter()
            .filter_map(|import| import.strip_suffix(".*"))
            .map(|package| qualified_name(package, &simple))
            .filter(|candidate| self.class_owners.values().any(|owner| owner == candidate))
            .collect();
        if let [owner] = wildcard_owners.as_slice() {
            return owner.clone();
        }
        match self.simple_class_owners.get(&simple).map(Vec::as_slice) {
            Some([owner]) => owner.clone(),
            _ => simple,
        }
    }

    fn owner_for_type(&self, file: &File, reference: &TypeRef) -> String {
        self.owner_for_name(file, &reference.name)
    }
}

impl FileAnalysis {
    pub(crate) fn scoped_completion_symbols(
        &self,
        source: &str,
        symbols: &CompletionSymbols,
    ) -> Vec<ScopedCompletionSymbol> {
        let file_span = Span::new(0, source.len() as u32);
        let mut result: Vec<_> = symbols
            .globals_for(&self.file)
            .map(|symbol| scoped(symbol, file_span, 0, 0))
            .collect();
        let block_scopes: Vec<_> = self
            .file
            .expr_arena
            .iter()
            .enumerate()
            .filter(|(_, expression)| matches!(expression, Expr::Block { .. }))
            .map(|(index, _)| self.file.expr_spans[index])
            .collect();

        for &declaration in &self.file.decls {
            match self.file.decl(declaration) {
                Decl::Fun(function) => {
                    add_function_scope(&mut result, function, &self.file, None, symbols);
                }
                Decl::Class(class) => {
                    let owner = symbols.owner_for_name(&self.file, &class.name);
                    for function in &class.methods {
                        add_function_scope(
                            &mut result,
                            function,
                            &self.file,
                            Some(&owner),
                            symbols,
                        );
                        add_type_parameters(
                            &mut result,
                            function_scope(function, &self.file),
                            &class.type_params,
                        );
                    }
                    let companion_owner = format!("@{owner}");
                    for function in &class.companion_methods {
                        add_function_scope(
                            &mut result,
                            function,
                            &self.file,
                            Some(&companion_owner),
                            symbols,
                        );
                    }
                    for function in class.enum_entries.iter().flat_map(|entry| &entry.methods) {
                        add_function_scope(
                            &mut result,
                            function,
                            &self.file,
                            Some(&owner),
                            symbols,
                        );
                    }
                }
                Decl::Property(_) => {}
            }
        }

        for (index, statement) in self.file.stmt_arena.iter().enumerate() {
            let statement_span = self.file.stmt_spans[index];
            let enclosing_scope = block_scopes
                .iter()
                .copied()
                .filter(|scope| scope.lo <= statement_span.lo && statement_span.hi <= scope.hi)
                .min_by_key(|scope| scope.hi.saturating_sub(scope.lo))
                .unwrap_or(file_span);
            match statement {
                Stmt::Local {
                    is_var,
                    name,
                    ty,
                    init,
                }
                | Stmt::LocalDelegate {
                    is_var,
                    name,
                    ty,
                    delegate: init,
                } => {
                    let inferred = ty
                        .as_ref()
                        .map(|ty| symbols.owner_for_type(&self.file, ty))
                        .or_else(|| self.expression_type_key(*init, symbols));
                    result.push(ScopedCompletionSymbol {
                        scope: enclosing_scope,
                        declared_at: statement_span.hi,
                        label: name.clone(),
                        detail: value_detail(*is_var, name, inferred.as_deref()),
                        kind: CompletionKind::Variable,
                        result_type: inferred,
                        priority: 3,
                    });
                }
                Stmt::LocalLateinit { name, ty } => {
                    result.push(ScopedCompletionSymbol {
                        scope: enclosing_scope,
                        declared_at: statement_span.hi,
                        label: name.clone(),
                        detail: format!("lateinit var {name}: {}", render_type(ty)),
                        kind: CompletionKind::Variable,
                        result_type: Some(symbols.owner_for_type(&self.file, ty)),
                        priority: 3,
                    });
                }
                Stmt::Destructure { entries, .. } => {
                    for (name, is_var) in entries {
                        if name != "_" {
                            result.push(ScopedCompletionSymbol {
                                scope: enclosing_scope,
                                declared_at: statement_span.hi,
                                label: name.clone(),
                                detail: value_detail(*is_var, name, None),
                                kind: CompletionKind::Variable,
                                result_type: None,
                                priority: 3,
                            });
                        }
                    }
                }
                Stmt::For { name, body, .. } | Stmt::ForEach { name, body, .. } => {
                    let scope = self.file.expr_spans[body.0 as usize];
                    result.push(ScopedCompletionSymbol {
                        scope,
                        declared_at: scope.lo,
                        label: name.clone(),
                        detail: format!("val {name}"),
                        kind: CompletionKind::Variable,
                        result_type: None,
                        priority: 3,
                    });
                }
                Stmt::LocalFun(function) => {
                    result.push(scoped(
                        &function_symbol(source, function, false),
                        enclosing_scope,
                        statement_span.lo,
                        3,
                    ));
                    add_function_scope(&mut result, function, &self.file, None, symbols);
                }
                Stmt::LocalClass(class) => {
                    result.push(ScopedCompletionSymbol {
                        scope: enclosing_scope,
                        declared_at: statement_span.lo,
                        label: class.name.clone(),
                        detail: class_detail(class),
                        kind: class_kind(class),
                        result_type: Some(format!("@{}", class.name)),
                        priority: 3,
                    });
                }
                _ => {}
            }
        }

        for (index, expression) in self.file.expr_arena.iter().enumerate() {
            match expression {
                Expr::Lambda { params, body } => {
                    let scope = self.file.expr_spans[body.0 as usize];
                    let types = self.file.lambda_param_types.get(&(index as u32));
                    for (parameter_index, name) in params.iter().enumerate() {
                        let ty = types
                            .and_then(|types| types.get(parameter_index))
                            .and_then(Option::as_ref);
                        result.push(ScopedCompletionSymbol {
                            scope,
                            declared_at: scope.lo,
                            label: name.clone(),
                            detail: ty.map_or_else(
                                || name.clone(),
                                |ty| format!("{name}: {}", render_type(ty)),
                            ),
                            kind: CompletionKind::Variable,
                            result_type: ty.map(|ty| symbols.owner_for_type(&self.file, ty)),
                            priority: 2,
                        });
                    }
                }
                Expr::Try { catches, .. } => {
                    for catch in catches {
                        let scope = self.file.expr_spans[catch.body.0 as usize];
                        result.push(ScopedCompletionSymbol {
                            scope,
                            declared_at: scope.lo,
                            label: catch.name.clone(),
                            detail: format!("{}: {}", catch.name, render_type(&catch.ty)),
                            kind: CompletionKind::Variable,
                            result_type: Some(symbols.owner_for_type(&self.file, &catch.ty)),
                            priority: 3,
                        });
                    }
                }
                _ => {}
            }
        }
        result
    }

    fn expression_type_key(
        &self,
        expression: krusty::ast::ExprId,
        symbols: &CompletionSymbols,
    ) -> Option<String> {
        self.types
            .as_ref()
            .and_then(|types| types.expr_types.get(expression.0 as usize))
            .and_then(ty_key)
            .or_else(|| self.constructor_result_type(expression, symbols))
    }

    fn constructor_result_type(
        &self,
        expression: krusty::ast::ExprId,
        symbols: &CompletionSymbols,
    ) -> Option<String> {
        let Expr::Call { callee, .. } = self.file.expr(expression) else {
            return None;
        };
        let Expr::Name(name) = self.file.expr(*callee) else {
            return None;
        };
        let callable_shadows_class = self
            .file
            .stmt_arena
            .iter()
            .any(|statement| match statement {
                Stmt::LocalFun(function) => {
                    function.name == *name
                        || function
                            .params
                            .iter()
                            .any(|parameter| parameter.name == *name)
                }
                Stmt::LocalClass(class) => {
                    class.name == *name
                        || class.props.iter().any(|property| property.name == *name)
                        || class
                            .secondary_ctors
                            .iter()
                            .flat_map(|constructor| &constructor.params)
                            .any(|parameter| parameter.name == *name)
                        || class
                            .methods
                            .iter()
                            .chain(&class.companion_methods)
                            .chain(
                                class
                                    .enum_entries
                                    .iter()
                                    .flat_map(|entry| entry.methods.iter()),
                            )
                            .flat_map(|function| &function.params)
                            .any(|parameter| parameter.name == *name)
                        || class
                            .body_props
                            .iter()
                            .chain(&class.companion_props)
                            .chain(
                                class
                                    .enum_entries
                                    .iter()
                                    .flat_map(|entry| entry.props.iter()),
                            )
                            .any(|property| {
                                property.name == *name
                                    || property
                                        .setter
                                        .as_ref()
                                        .and_then(|setter| setter.param.as_ref())
                                        == Some(name)
                            })
                }
                Stmt::Local { name: local, .. }
                | Stmt::LocalLateinit { name: local, .. }
                | Stmt::LocalDelegate { name: local, .. }
                | Stmt::For { name: local, .. }
                | Stmt::ForEach { name: local, .. } => local == name,
                Stmt::Destructure { entries, .. } => entries.iter().any(|(local, _)| local == name),
                _ => false,
            })
            || self
                .file
                .decls
                .iter()
                .any(|&declaration| match self.file.decl(declaration) {
                    Decl::Fun(function) => {
                        function.name == *name
                            || function
                                .params
                                .iter()
                                .any(|parameter| parameter.name == *name)
                    }
                    Decl::Property(property) => {
                        property.name == *name
                            || property
                                .setter
                                .as_ref()
                                .and_then(|setter| setter.param.as_ref())
                                == Some(name)
                    }
                    Decl::Class(class) => {
                        class.props.iter().any(|property| property.name == *name)
                            || class
                                .secondary_ctors
                                .iter()
                                .flat_map(|constructor| &constructor.params)
                                .any(|parameter| parameter.name == *name)
                            || class
                                .body_props
                                .iter()
                                .chain(&class.companion_props)
                                .chain(
                                    class
                                        .enum_entries
                                        .iter()
                                        .flat_map(|entry| entry.props.iter()),
                                )
                                .any(|property| {
                                    property.name == *name
                                        || property
                                            .setter
                                            .as_ref()
                                            .and_then(|setter| setter.param.as_ref())
                                            == Some(name)
                                })
                            || class
                                .methods
                                .iter()
                                .chain(&class.companion_methods)
                                .chain(
                                    class
                                        .enum_entries
                                        .iter()
                                        .flat_map(|entry| entry.methods.iter()),
                                )
                                .flat_map(|function| &function.params)
                                .any(|parameter| parameter.name == *name)
                    }
                })
            || self
                .file
                .expr_arena
                .iter()
                .any(|expression| match expression {
                    Expr::Lambda { params, .. } => params.iter().any(|parameter| parameter == name),
                    Expr::Try { catches, .. } => catches.iter().any(|catch| catch.name == *name),
                    _ => false,
                });
        if callable_shadows_class {
            return None;
        }
        let owner = symbols.owner_for_name(&self.file, name);
        symbols
            .class_owners
            .values()
            .any(|candidate| candidate == &owner)
            .then_some(owner)
    }
}

fn add_function_scope(
    output: &mut Vec<ScopedCompletionSymbol>,
    function: &FunDecl,
    file: &File,
    owner: Option<&str>,
    symbols: &CompletionSymbols,
) {
    let scope = function_scope(function, file);
    for parameter in &function.params {
        output.push(parameter_symbol(parameter, scope, file, symbols));
    }
    for name in &function.type_params {
        output.push(ScopedCompletionSymbol {
            scope,
            declared_at: scope.lo,
            label: name.clone(),
            detail: format!("type parameter {name}"),
            kind: CompletionKind::TypeParameter,
            result_type: None,
            priority: 2,
        });
    }
    if let Some(owner) = owner {
        let rendered_owner = owner
            .strip_prefix('@')
            .map_or_else(|| owner.to_string(), |owner| format!("{owner}.Companion"));
        output.push(ScopedCompletionSymbol {
            scope,
            declared_at: scope.lo,
            label: "this".to_string(),
            detail: format!("this: {rendered_owner}"),
            kind: CompletionKind::Variable,
            result_type: Some(owner.to_string()),
            priority: 2,
        });
        if let Some(members) = symbols.members.get(owner) {
            output.extend(
                members
                    .iter()
                    .map(|member| scoped(member, scope, scope.lo, 1)),
            );
        }
    }
}

fn add_type_parameters(
    output: &mut Vec<ScopedCompletionSymbol>,
    scope: Span,
    type_parameters: &[String],
) {
    output.extend(type_parameters.iter().map(|name| ScopedCompletionSymbol {
        scope,
        declared_at: scope.lo,
        label: name.clone(),
        detail: format!("type parameter {name}"),
        kind: CompletionKind::TypeParameter,
        result_type: None,
        priority: 2,
    }));
}

fn parameter_symbol(
    parameter: &Param,
    scope: Span,
    file: &File,
    symbols: &CompletionSymbols,
) -> ScopedCompletionSymbol {
    ScopedCompletionSymbol {
        scope,
        declared_at: scope.lo,
        label: parameter.name.clone(),
        detail: format!("{}: {}", parameter.name, render_type(&parameter.ty)),
        kind: CompletionKind::Variable,
        result_type: Some(symbols.owner_for_type(file, &parameter.ty)),
        priority: 2,
    }
}

fn scoped(symbol: &Symbol, scope: Span, declared_at: u32, priority: u8) -> ScopedCompletionSymbol {
    ScopedCompletionSymbol {
        scope,
        declared_at,
        label: symbol.label.clone(),
        detail: symbol.detail.clone(),
        kind: symbol.kind,
        result_type: symbol.result_type.clone(),
        priority,
    }
}

fn function_scope(function: &FunDecl, file: &File) -> Span {
    match function.body {
        FunBody::Expr(body) | FunBody::Block(body) => file.expr_spans[body.0 as usize],
        FunBody::None => function.span,
    }
}

fn function_symbol(source: &str, function: &FunDecl, member: bool) -> Symbol {
    let params = function
        .params
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name, render_type(&parameter.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    let result_type = function.ret.as_ref().map(type_key);
    let rendered_result = function
        .ret
        .as_ref()
        .map_or_else(|| "<inferred>".to_string(), render_type);
    Symbol {
        label: function.name.clone(),
        detail: format!("fun {}({params}): {rendered_result}", function.name),
        kind: if has_modifier(source, function.span, "operator") {
            CompletionKind::Operator
        } else if member {
            CompletionKind::Method
        } else {
            CompletionKind::Function
        },
        result_type,
    }
}

fn property_symbol(property: &PropDecl) -> Symbol {
    let result_type = property.ty.as_ref().map(type_key);
    Symbol {
        label: property.name.clone(),
        detail: value_detail(property.is_var, &property.name, result_type.as_deref()),
        kind: if property.is_const {
            CompletionKind::Constant
        } else {
            CompletionKind::Property
        },
        result_type,
    }
}

fn value_detail(is_var: bool, name: &str, ty: Option<&str>) -> String {
    match ty {
        Some(ty) => format!("{} {name}: {ty}", if is_var { "var" } else { "val" }),
        None => format!("{} {name}", if is_var { "var" } else { "val" }),
    }
}

fn class_kind(class: &ClassDecl) -> CompletionKind {
    match class.kind {
        ClassKind::Interface => CompletionKind::Interface,
        ClassKind::Enum => CompletionKind::Enum,
        ClassKind::Class if class.is_data => CompletionKind::Struct,
        ClassKind::Class | ClassKind::Object | ClassKind::Annotation => CompletionKind::Class,
    }
}

fn class_detail(class: &ClassDecl) -> String {
    let prefix = match class.kind {
        ClassKind::Interface => "interface",
        ClassKind::Enum => "enum class",
        ClassKind::Object => "object",
        ClassKind::Annotation => "annotation class",
        ClassKind::Class if class.is_data => "data class",
        ClassKind::Class => "class",
    };
    format!("{prefix} {}", class.name)
}

fn has_modifier(source: &str, span: Span, modifier: &str) -> bool {
    source
        .get(span.lo as usize..span.hi as usize)
        .is_some_and(|declaration| {
            declaration
                .split(|character: char| !character.is_alphanumeric() && character != '_')
                .any(|word| word == modifier)
        })
}

fn type_key(reference: &TypeRef) -> String {
    simple_name(&reference.name)
}

fn simple_name(name: &str) -> String {
    name.rsplit(['.', '/', '$'])
        .next()
        .unwrap_or(name)
        .to_string()
}

fn qualified_name(package: &str, name: &str) -> String {
    if package.is_empty() {
        name.to_string()
    } else {
        format!("{package}.{name}")
    }
}

fn ty_key(ty: &Ty) -> Option<String> {
    match ty.non_null() {
        Ty::Obj(name, _) => Some(name.render().replace(['/', '$'], ".")),
        Ty::Error | Ty::Null | Ty::Nothing => None,
        primitive => Some(format!("{primitive:?}")),
    }
}

fn render_type(reference: &TypeRef) -> String {
    if reference.name == "<fun>" {
        let params = reference
            .fun_params
            .iter()
            .map(render_type)
            .collect::<Vec<_>>()
            .join(", ");
        let result = reference
            .arg
            .as_deref()
            .map_or_else(|| "Unit".to_string(), render_type);
        return format!("({params}) -> {result}");
    }
    let mut result = reference.name.clone();
    if !reference.targs.is_empty() {
        result.push('<');
        result.push_str(
            &reference
                .targs
                .iter()
                .map(render_type)
                .collect::<Vec<_>>()
                .join(", "),
        );
        result.push('>');
    } else if let Some(argument) = &reference.arg {
        result.push('<');
        result.push_str(&render_type(argument));
        result.push('>');
    }
    if reference.nullable {
        result.push('?');
    }
    result
}
