//! Bounded parser syntax retained for Pass-1-only executable work.
//!
//! Production signature extraction runs while one source is active. Afterwards only inline bodies
//! and compile-time constant initializers may still need parser syntax. This compactor follows those
//! roots, rewrites every retained parser identity densely, and drops neighboring ordinary bodies.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    ClassDecl, ClassInit, CtorDelegation, Decl, Expr, ExprId, File, FunBody, FunDecl, Param,
    PropDecl, Stmt, StmtId, TemplatePart, WhenCondition,
};

const MISSING_EXPR: ExprId = ExprId(u32::MAX);

#[derive(Default)]
struct Reachable {
    expressions: HashSet<ExprId>,
    statements: HashSet<StmtId>,
}

impl Reachable {
    fn expression(&mut self, file: &File, root: ExprId) {
        if !self.expressions.insert(root) {
            return;
        }
        let mut expressions = Vec::new();
        let mut statements = Vec::new();
        file.any_child_expr(
            root,
            &mut |child| {
                expressions.push(child);
                false
            },
            &mut |child| {
                statements.push(child);
                false
            },
        );
        for child in expressions {
            self.expression(file, child);
        }
        for child in statements {
            self.statement(file, child);
        }
        if let Some(&declaration) = file.anonymous_object_classes.get(&root) {
            self.declaration(file, declaration);
        }
    }

    fn statement(&mut self, file: &File, statement: StmtId) {
        if !self.statements.insert(statement) {
            return;
        }
        let mut expressions = Vec::new();
        file.any_child_stmt(statement, &mut |child| {
            expressions.push(child);
            false
        });
        for child in expressions {
            self.expression(file, child);
        }
        match file.stmt(statement) {
            Stmt::LocalFun(function) => self.function(file, function),
            Stmt::LocalClass(_) => {
                if let Some(&declaration) = file.local_class_decls.get(&statement) {
                    self.declaration(file, declaration);
                    if let Some(nested) = file.local_class_nested.get(&statement) {
                        for &declaration in nested {
                            self.declaration(file, declaration);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn roots(&mut self, file: &File, roots: impl IntoIterator<Item = ExprId>) {
        for root in roots {
            self.expression(file, root);
        }
    }

    fn function(&mut self, file: &File, function: &FunDecl) {
        let mut roots = Vec::new();
        file.any_fun_expr(function, &mut |root| {
            roots.push(root);
            false
        });
        self.roots(file, roots);
    }

    fn property(&mut self, file: &File, property: &PropDecl) {
        self.roots(
            file,
            property
                .annotation_args
                .iter()
                .flatten()
                .copied()
                .chain(
                    property
                        .context_params
                        .iter()
                        .flat_map(parameter_expression_roots),
                )
                .chain(property.init)
                .chain(property.delegate)
                .chain(property.getter.as_ref().and_then(fun_body_root))
                .chain(
                    property
                        .setter
                        .as_ref()
                        .and_then(|setter| setter.body.as_ref())
                        .and_then(fun_body_root),
                ),
        );
    }

    fn declaration(&mut self, file: &File, declaration: crate::ast::DeclId) {
        let mut roots = Vec::new();
        file.any_decl_expr(declaration, &mut |root| {
            roots.push(root);
            false
        });
        self.roots(file, roots);
        let Decl::Class(class) = file.decl(declaration) else {
            return;
        };
        // Parser-hoisted nested declarations are not children of `ClassDecl`; source containment is
        // the structural ownership relation already used by compact header inventory.
        let nested = file
            .decls
            .iter()
            .copied()
            .filter(|candidate| *candidate != declaration)
            .filter(|candidate| match file.decl(*candidate) {
                Decl::Class(candidate) => {
                    class.span.lo <= candidate.span.lo && candidate.span.hi <= class.span.hi
                }
                Decl::Fun(_) | Decl::Property(_) => false,
            })
            .collect::<Vec<_>>();
        for nested in nested {
            self.declaration(file, nested);
        }
    }
}

fn parameter_expression_roots(parameter: &Param) -> impl Iterator<Item = ExprId> + '_ {
    parameter
        .annotation_args
        .iter()
        .flatten()
        .copied()
        .chain(parameter.default)
}

fn fun_body_root(body: &FunBody) -> Option<ExprId> {
    match body {
        FunBody::Expr(root) | FunBody::Block(root) => Some(*root),
        FunBody::None => None,
    }
}

fn retain_parameter_defaults<'a>(
    retained: &mut Reachable,
    file: &File,
    parameters: impl IntoIterator<Item = &'a Param>,
) {
    retained.roots(
        file,
        parameters
            .into_iter()
            .filter_map(|parameter| parameter.default),
    );
}

fn collect_pass_one_roots(file: &File) -> Reachable {
    let mut retained = Reachable::default();
    for declaration in &file.decl_arena {
        match declaration {
            Decl::Fun(function) => {
                retain_parameter_defaults(&mut retained, file, &function.params);
                if function.is_inline() {
                    retained.function(file, function);
                }
            }
            Decl::Property(property) => {
                if property.is_const {
                    retained.property(file, property);
                } else {
                    if property.getter_inline {
                        retained.roots(file, property.getter.as_ref().and_then(fun_body_root));
                    }
                    if property
                        .setter
                        .as_ref()
                        .is_some_and(|setter| setter.is_inline)
                    {
                        retained.roots(
                            file,
                            property
                                .setter
                                .as_ref()
                                .and_then(|setter| setter.body.as_ref())
                                .and_then(fun_body_root),
                        );
                    }
                }
            }
            Decl::Class(class) => {
                retained.roots(
                    file,
                    class.props.iter().filter_map(|parameter| parameter.default),
                );
                for method in &class.methods {
                    retain_parameter_defaults(&mut retained, file, &method.params);
                }
                for constructor in &class.secondary_ctors {
                    retain_parameter_defaults(&mut retained, file, &constructor.params);
                }
                for entry in &class.enum_entries {
                    for method in &entry.methods {
                        retain_parameter_defaults(&mut retained, file, &method.params);
                    }
                }
                let has_inline_body = class.methods.iter().any(FunDecl::is_inline)
                    || class.body_props.iter().any(|property| {
                        property.getter_inline
                            || property
                                .setter
                                .as_ref()
                                .is_some_and(|setter| setter.is_inline)
                    })
                    || class.enum_entries.iter().any(|entry| {
                        entry.methods.iter().any(FunDecl::is_inline)
                            || entry.props.iter().any(|property| {
                                property.getter_inline
                                    || property
                                        .setter
                                        .as_ref()
                                        .is_some_and(|setter| setter.is_inline)
                            })
                    });
                if has_inline_body {
                    // Checking a selected inline member enters its enclosing class through the
                    // ordinary class checker. That checker folds every class-owned declaration
                    // annotation before it reaches the selected body, so these are bounded header
                    // roots rather than neighboring ordinary bodies.
                    retained.roots(
                        file,
                        class
                            .annotation_args
                            .iter()
                            .flatten()
                            .copied()
                            .chain(class.props.iter().flat_map(|property| {
                                property.annotation_args.iter().flatten().copied()
                            }))
                            .chain(class.body_props.iter().flat_map(|property| {
                                property.annotation_args.iter().flatten().copied()
                            }))
                            .chain(
                                class.enum_entries.iter().flat_map(|entry| {
                                    entry.annotation_args.iter().flatten().copied()
                                }),
                            )
                            .chain(class.secondary_ctors.iter().flat_map(|constructor| {
                                constructor.annotation_args.iter().flatten().copied()
                            }))
                            .chain(class.primary_ctor_annotation_args.iter().flatten().copied()),
                    );
                }
                for method in &class.methods {
                    if method.is_inline() {
                        retained.function(file, method);
                    }
                }
                for property in &class.body_props {
                    if property.is_const {
                        retained.property(file, property);
                    } else {
                        if property.getter_inline {
                            retained.roots(file, property.getter.as_ref().and_then(fun_body_root));
                        }
                        if property
                            .setter
                            .as_ref()
                            .is_some_and(|setter| setter.is_inline)
                        {
                            retained.roots(
                                file,
                                property
                                    .setter
                                    .as_ref()
                                    .and_then(|setter| setter.body.as_ref())
                                    .and_then(fun_body_root),
                            );
                        }
                    }
                }
                for entry in &class.enum_entries {
                    for method in &entry.methods {
                        if method.is_inline() {
                            retained.function(file, method);
                        }
                    }
                    for property in &entry.props {
                        if property.is_const {
                            retained.property(file, property);
                        }
                    }
                }
            }
        }
    }
    // File and declaration-type-parameter annotations are header syntax. Inline checking may still
    // consult their suppression/contract arguments, so retain these bounded roots as well.
    retained.roots(
        file,
        file.file_annotations
            .iter()
            .flat_map(|(_, arguments)| arguments.iter().copied())
            .chain(
                file.declaration_type_parameter_annotations
                    .values()
                    .flatten()
                    .flat_map(|parameter| parameter.annotation_args.iter().flatten().copied()),
            ),
    );
    // A default declared inside a nested/local/anonymous classifier is evaluated in the lexical
    // scope where that classifier was introduced. Preserve its enclosing top-level declaration unit until the
    // default has become checked FIR: the Pass-1 checker must walk preceding locals, classifiers,
    // and receiver rungs rather than checking the default from an invented file scope. This syntax
    // remains Pass-1-only and is discarded immediately after default storage is complete.
    let local_default_units = file
        .decl_arena
        .iter()
        .filter_map(|declaration| {
            let Decl::Class(class) = declaration else {
                return None;
            };
            let has_defaults = class
                .props
                .iter()
                .any(|parameter| parameter.default.is_some())
                || class
                    .methods
                    .iter()
                    .flat_map(|method| &method.params)
                    .any(|parameter| parameter.default.is_some())
                || class
                    .secondary_ctors
                    .iter()
                    .flat_map(|constructor| &constructor.params)
                    .any(|parameter| parameter.default.is_some())
                || class
                    .enum_entries
                    .iter()
                    .flat_map(|entry| &entry.methods)
                    .flat_map(|method| &method.params)
                    .any(|parameter| parameter.default.is_some());
            has_defaults.then_some(class.span)
        })
        .filter_map(|local_span| {
            file.decls
                .iter()
                .copied()
                .filter(|declaration| !file.is_local_declaration(*declaration))
                .filter_map(|declaration| {
                    let span = match file.decl(declaration) {
                        Decl::Fun(function) => function.span,
                        Decl::Class(class) => class.span,
                        Decl::Property(property) => property.span,
                    };
                    (span != local_span && span.lo <= local_span.lo && local_span.hi <= span.hi)
                        .then_some((span.hi - span.lo, declaration))
                })
                // Parser-hoisted anonymous and nested classifiers can themselves appear in
                // `file.decls`. The bounded parser unit is the outermost containing declaration,
                // not the nearest hoisted classifier.
                .max_by_key(|(size, _)| *size)
                .map(|(_, declaration)| declaration)
        })
        .collect::<HashSet<_>>();
    crate::trace_compiler!(
        "fir",
        "Pass 1 retained nested-default units={:?}",
        local_default_units
    );
    for declaration in local_default_units {
        retained.declaration(file, declaration);
    }
    crate::trace_compiler!(
        "fir",
        "Pass 1 retained syntax expressions={} statements={}",
        retained.expressions.len(),
        retained.statements.len(),
    );
    retained
}

fn expr_map(reachable: &Reachable) -> (Vec<ExprId>, HashMap<ExprId, ExprId>) {
    let mut old = reachable.expressions.iter().copied().collect::<Vec<_>>();
    old.sort_by_key(|id| id.0);
    let map = old
        .iter()
        .enumerate()
        .map(|(new, old)| (*old, ExprId(new as u32)))
        .collect();
    (old, map)
}

fn stmt_map(reachable: &Reachable) -> (Vec<StmtId>, HashMap<StmtId, StmtId>) {
    let mut old = reachable.statements.iter().copied().collect::<Vec<_>>();
    old.sort_by_key(|id| id.0);
    let map = old
        .iter()
        .enumerate()
        .map(|(new, old)| (*old, StmtId(new as u32)))
        .collect();
    (old, map)
}

fn mapped_expr(map: &HashMap<ExprId, ExprId>, expression: ExprId) -> ExprId {
    map.get(&expression).copied().unwrap_or(MISSING_EXPR)
}

fn mapped_expr_opt(map: &HashMap<ExprId, ExprId>, expression: &mut Option<ExprId>) {
    *expression = expression.map(|expression| mapped_expr(map, expression));
}

fn remap_fun_body(body: &mut FunBody, expressions: &HashMap<ExprId, ExprId>) {
    match body {
        FunBody::Expr(root) | FunBody::Block(root) => *root = mapped_expr(expressions, *root),
        FunBody::None => {}
    }
}

fn remap_params(parameters: &mut [Param], expressions: &HashMap<ExprId, ExprId>) {
    for parameter in parameters {
        mapped_expr_opt(expressions, &mut parameter.default);
        for arguments in &mut parameter.annotation_args {
            for argument in arguments {
                *argument = mapped_expr(expressions, *argument);
            }
        }
    }
}

fn remap_function(function: &mut FunDecl, expressions: &HashMap<ExprId, ExprId>) {
    remap_params(&mut function.params, expressions);
    remap_fun_body(&mut function.body, expressions);
    for arguments in &mut function.annotation_args {
        for argument in arguments {
            *argument = mapped_expr(expressions, *argument);
        }
    }
}

fn remap_property(property: &mut PropDecl, expressions: &HashMap<ExprId, ExprId>) {
    remap_params(&mut property.context_params, expressions);
    mapped_expr_opt(expressions, &mut property.init);
    mapped_expr_opt(expressions, &mut property.delegate);
    if let Some(getter) = &mut property.getter {
        remap_fun_body(getter, expressions);
    }
    if let Some(setter) = &mut property.setter {
        if let Some(body) = &mut setter.body {
            remap_fun_body(body, expressions);
        }
    }
    for arguments in &mut property.annotation_args {
        for argument in arguments {
            *argument = mapped_expr(expressions, *argument);
        }
    }
}

fn remap_class(class: &mut ClassDecl, expressions: &HashMap<ExprId, ExprId>) {
    for arguments in &mut class.annotation_args {
        for argument in arguments {
            *argument = mapped_expr(expressions, *argument);
        }
    }
    for property in &mut class.props {
        mapped_expr_opt(expressions, &mut property.default);
        for arguments in &mut property.annotation_args {
            for argument in arguments {
                *argument = mapped_expr(expressions, *argument);
            }
        }
    }
    for method in &mut class.methods {
        remap_function(method, expressions);
    }
    for property in &mut class.body_props {
        remap_property(property, expressions);
    }
    for initializer in &mut class.init_order {
        if let ClassInit::Block(body) = initializer {
            *body = mapped_expr(expressions, *body);
        }
    }
    for entry in &mut class.enum_entries {
        for arguments in &mut entry.annotation_args {
            for argument in arguments {
                *argument = mapped_expr(expressions, *argument);
            }
        }
        for argument in &mut entry.args {
            *argument = mapped_expr(expressions, *argument);
        }
        for method in &mut entry.methods {
            remap_function(method, expressions);
        }
        for property in &mut entry.props {
            remap_property(property, expressions);
        }
        for initializer in &mut entry.init_order {
            if let ClassInit::Block(body) = initializer {
                *body = mapped_expr(expressions, *body);
            }
        }
    }
    for delegation in &mut class.interface_delegations {
        delegation.value = mapped_expr(expressions, delegation.value);
    }
    for argument in &mut class.base_args {
        *argument = mapped_expr(expressions, *argument);
    }
    for constructor in &mut class.secondary_ctors {
        remap_params(&mut constructor.params, expressions);
        for arguments in &mut constructor.annotation_args {
            for argument in arguments {
                *argument = mapped_expr(expressions, *argument);
            }
        }
        match &mut constructor.delegation {
            CtorDelegation::This(call) | CtorDelegation::Super(call) => {
                for argument in &mut call.args {
                    *argument = mapped_expr(expressions, *argument);
                }
            }
            CtorDelegation::None => {}
        }
        mapped_expr_opt(expressions, &mut constructor.body);
    }
    for arguments in &mut class.primary_ctor_annotation_args {
        for argument in arguments {
            *argument = mapped_expr(expressions, *argument);
        }
    }
}

fn remap_declarations(file: &mut File, expressions: &HashMap<ExprId, ExprId>) {
    for declaration in &mut file.decl_arena {
        match declaration {
            Decl::Fun(function) => remap_function(function, expressions),
            Decl::Class(class) => remap_class(class, expressions),
            Decl::Property(property) => remap_property(property, expressions),
        }
    }
    file.script_body = file
        .script_body
        .and_then(|body| expressions.get(&body).copied());
    for (_, arguments) in &mut file.file_annotations {
        for argument in arguments {
            *argument = mapped_expr(expressions, *argument);
        }
    }
    for parameter in file
        .declaration_type_parameter_annotations
        .values_mut()
        .flatten()
    {
        for arguments in &mut parameter.annotation_args {
            for argument in arguments {
                *argument = mapped_expr(expressions, *argument);
            }
        }
    }
}

fn remap_expression(
    expression: &mut Expr,
    expressions: &HashMap<ExprId, ExprId>,
    statements: &HashMap<StmtId, StmtId>,
) {
    let map = |expression| mapped_expr(expressions, expression);
    match expression {
        Expr::IntLit(_)
        | Expr::LongLit(_)
        | Expr::UIntLit(_)
        | Expr::ULongLit(_)
        | Expr::DoubleLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::CharLit(_)
        | Expr::NullLit
        | Expr::UnsupportedAnnotationArgument(_)
        | Expr::Name(_)
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
        Expr::AnnotationArrayLiteral(elements) => {
            elements
                .iter_mut()
                .for_each(|element| *element = map(*element));
        }
        Expr::CallableRef { receiver, .. } => mapped_expr_opt(expressions, receiver),
        Expr::Return { value, .. } => mapped_expr_opt(expressions, value),
        Expr::NotNull { operand }
        | Expr::Throw { operand }
        | Expr::Unary { operand, .. }
        | Expr::Is { operand, .. }
        | Expr::As { operand, .. }
        | Expr::Lambda { body: operand, .. } => *operand = map(*operand),
        Expr::Elvis { lhs, rhs } | Expr::Binary { lhs, rhs, .. } => {
            *lhs = map(*lhs);
            *rhs = map(*rhs);
        }
        Expr::RangeTo { lo, hi, .. } => {
            *lo = map(*lo);
            *hi = map(*hi);
        }
        Expr::IncDec { target, .. } => *target = map(*target),
        Expr::InRange {
            value, start, end, ..
        } => {
            *value = map(*value);
            *start = map(*start);
            *end = map(*end);
        }
        Expr::Member { receiver, .. } => *receiver = map(*receiver),
        Expr::ExtensionAccess { receiver, callable } => {
            *receiver = map(*receiver);
            *callable = map(*callable);
        }
        Expr::Index { array, indices } => {
            *array = map(*array);
            indices.iter_mut().for_each(|index| *index = map(*index));
        }
        Expr::Call { callee, args } => {
            *callee = map(*callee);
            args.iter_mut()
                .for_each(|argument| *argument = map(*argument));
        }
        Expr::SafeCall { receiver, args, .. } => {
            *receiver = map(*receiver);
            if let Some(args) = args {
                args.iter_mut()
                    .for_each(|argument| *argument = map(*argument));
            }
        }
        Expr::Template(parts) => {
            for part in parts {
                if let TemplatePart::Expr(part) = part {
                    *part = map(*part);
                }
            }
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            *cond = map(*cond);
            *then_branch = map(*then_branch);
            mapped_expr_opt(expressions, else_branch);
        }
        Expr::Block { stmts, trailing } => {
            stmts.iter_mut().for_each(|statement| {
                *statement = statements
                    .get(statement)
                    .copied()
                    .expect("a retained block must retain each child statement")
            });
            mapped_expr_opt(expressions, trailing);
        }
        Expr::Try {
            body,
            catches,
            finally,
        } => {
            *body = map(*body);
            for catch in catches {
                catch.body = map(catch.body);
            }
            mapped_expr_opt(expressions, finally);
        }
        Expr::When { subject, arms } => {
            mapped_expr_opt(expressions, subject);
            for arm in arms {
                for condition in &mut arm.conditions {
                    *condition = match *condition {
                        WhenCondition::SubjectEquals(expression) => {
                            WhenCondition::SubjectEquals(map(expression))
                        }
                        WhenCondition::Predicate(expression) => {
                            WhenCondition::Predicate(map(expression))
                        }
                    };
                }
                mapped_expr_opt(expressions, &mut arm.guard);
                arm.body = map(arm.body);
            }
        }
    }
}

fn remap_statement(statement: &mut Stmt, expressions: &HashMap<ExprId, ExprId>) {
    let map = |expression| mapped_expr(expressions, expression);
    match statement {
        Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::IncDec { .. }
        | Stmt::LocalLateinit { .. }
        | Stmt::LocalTypeAlias(_) => {}
        Stmt::Local { init, .. }
        | Stmt::Destructure { init, .. }
        | Stmt::Assign { value: init, .. }
        | Stmt::LocalDelegate { delegate: init, .. }
        | Stmt::Expr(init) => *init = map(*init),
        Stmt::Return(value, _) => mapped_expr_opt(expressions, value),
        Stmt::AssignMember {
            receiver, value, ..
        } => {
            *receiver = map(*receiver);
            *value = map(*value);
        }
        Stmt::AssignIndex {
            array,
            indices,
            value,
        } => {
            *array = map(*array);
            indices.iter_mut().for_each(|index| *index = map(*index));
            *value = map(*value);
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
            *cond = map(*cond);
            *body = map(*body);
        }
        Stmt::For { range, body, .. } => {
            range.start = map(range.start);
            range.end = map(range.end);
            *body = map(*body);
        }
        Stmt::ForEach { iterable, body, .. } => {
            *iterable = map(*iterable);
            *body = map(*body);
        }
        Stmt::LocalFun(function) => remap_function(function, expressions),
        Stmt::LocalClass(class) => remap_class(class, expressions),
        Stmt::CompoundAssign { target, value, .. } => {
            *target = map(*target);
            *value = map(*value);
        }
    }
}

fn remap_u32_map<T>(
    old: std::collections::HashMap<u32, T>,
    expressions: &HashMap<ExprId, ExprId>,
) -> std::collections::HashMap<u32, T> {
    old.into_iter()
        .filter_map(|(old, value)| expressions.get(&ExprId(old)).map(|new| (new.0, value)))
        .collect()
}

fn remap_u32_set(
    old: std::collections::HashSet<u32>,
    expressions: &HashMap<ExprId, ExprId>,
) -> std::collections::HashSet<u32> {
    old.into_iter()
        .filter_map(|old| expressions.get(&ExprId(old)).map(|new| new.0))
        .collect()
}

/// Drop every ordinary parser body while retaining dense syntax for inline bodies and `const`
/// initializers. Header/declaration structures remain available to the temporary legacy adapters.
pub(super) fn compact(file: &mut File) {
    let reachable = collect_pass_one_roots(file);
    let (old_expression_ids, expressions) = expr_map(&reachable);
    let (old_statement_ids, statements) = stmt_map(&reachable);

    let old_expressions = std::mem::take(&mut file.expr_arena);
    let old_expression_spans = std::mem::take(&mut file.expr_spans);
    let old_expression_lines = std::mem::take(&mut file.expr_lines);
    let old_expression_source_lines = std::mem::take(&mut file.expr_source_lines);
    let old_expression_end_lines = std::mem::take(&mut file.expr_end_lines);
    file.expr_arena = old_expression_ids
        .iter()
        .map(|old| {
            let mut expression = old_expressions[old.0 as usize].clone();
            remap_expression(&mut expression, &expressions, &statements);
            expression
        })
        .collect();
    file.expr_spans = old_expression_ids
        .iter()
        .map(|old| old_expression_spans[old.0 as usize])
        .collect();
    file.expr_lines = old_expression_ids
        .iter()
        .map(|old| {
            old_expression_lines
                .get(old.0 as usize)
                .copied()
                .unwrap_or(0)
        })
        .collect();
    file.expr_source_lines = old_expression_ids
        .iter()
        .map(|old| {
            old_expression_source_lines
                .get(old.0 as usize)
                .copied()
                .unwrap_or(0)
        })
        .collect();
    file.expr_end_lines = old_expression_ids
        .iter()
        .map(|old| {
            old_expression_end_lines
                .get(old.0 as usize)
                .copied()
                .unwrap_or(0)
        })
        .collect();

    let old_statements = std::mem::take(&mut file.stmt_arena);
    let old_statement_spans = std::mem::take(&mut file.stmt_spans);
    let old_statement_lines = std::mem::take(&mut file.stmt_lines);
    file.stmt_arena = old_statement_ids
        .iter()
        .map(|old| {
            let mut statement = old_statements[old.0 as usize].clone();
            remap_statement(&mut statement, &expressions);
            statement
        })
        .collect();
    file.stmt_spans = old_statement_ids
        .iter()
        .map(|old| old_statement_spans[old.0 as usize])
        .collect();
    file.stmt_lines = old_statement_ids
        .iter()
        .map(|old| {
            old_statement_lines
                .get(old.0 as usize)
                .copied()
                .unwrap_or(0)
        })
        .collect();

    remap_declarations(file, &expressions);
    file.retained_expr_spans.clear();
    file.value_operator_spans =
        remap_u32_map(std::mem::take(&mut file.value_operator_spans), &expressions);
    file.annotation_arg_names =
        remap_u32_map(std::mem::take(&mut file.annotation_arg_names), &expressions);
    file.call_arg_names = remap_u32_map(std::mem::take(&mut file.call_arg_names), &expressions);
    file.collection_literal_calls = remap_u32_set(
        std::mem::take(&mut file.collection_literal_calls),
        &expressions,
    );
    file.call_arg_name_spans =
        remap_u32_map(std::mem::take(&mut file.call_arg_name_spans), &expressions);
    file.empty_call_open_paren_spans = remap_u32_map(
        std::mem::take(&mut file.empty_call_open_paren_spans),
        &expressions,
    );
    file.exact_member_name_spans = remap_u32_map(
        std::mem::take(&mut file.exact_member_name_spans),
        &expressions,
    );
    file.nullable_callable_ref_receivers = remap_u32_set(
        std::mem::take(&mut file.nullable_callable_ref_receivers),
        &expressions,
    );
    file.non_adjacent_member_dot_spans = std::mem::take(&mut file.non_adjacent_member_dot_spans)
        .into_iter()
        .filter_map(|(old, span)| expressions.get(&ExprId(old)).map(|new| (new.0, span)))
        .collect();
    file.call_has_trailing_lambda = remap_u32_set(
        std::mem::take(&mut file.call_has_trailing_lambda),
        &expressions,
    );
    file.trailing_call_close_paren_ends = remap_u32_map(
        std::mem::take(&mut file.trailing_call_close_paren_ends),
        &expressions,
    );
    file.infix_calls = remap_u32_set(std::mem::take(&mut file.infix_calls), &expressions);
    file.call_type_args = remap_u32_map(std::mem::take(&mut file.call_type_args), &expressions);
    file.lambda_param_types =
        remap_u32_map(std::mem::take(&mut file.lambda_param_types), &expressions);
    file.lambda_explicit_arrows = remap_u32_set(
        std::mem::take(&mut file.lambda_explicit_arrows),
        &expressions,
    );
    file.anon_fun_lambdas = remap_u32_set(std::mem::take(&mut file.anon_fun_lambdas), &expressions);
    file.anon_fun_context_count = remap_u32_map(
        std::mem::take(&mut file.anon_fun_context_count),
        &expressions,
    );
    file.anon_fun_receivers =
        remap_u32_map(std::mem::take(&mut file.anon_fun_receivers), &expressions);
    file.suspend_lambdas = remap_u32_set(std::mem::take(&mut file.suspend_lambdas), &expressions);
    file.lambda_labels = remap_u32_map(std::mem::take(&mut file.lambda_labels), &expressions);
    file.base_arg_names = remap_u32_map(std::mem::take(&mut file.base_arg_names), &expressions);
    file.anon_fun_ret = remap_u32_map(std::mem::take(&mut file.anon_fun_ret), &expressions);
    file.spread_arg_ids = remap_u32_set(std::mem::take(&mut file.spread_arg_ids), &expressions);

    file.incdec_access_operands = std::mem::take(&mut file.incdec_access_operands)
        .into_iter()
        .filter_map(|(old, operands)| {
            let new = expressions.get(&old).copied()?;
            let operands = operands
                .into_iter()
                .filter_map(|operand| expressions.get(&operand).copied())
                .collect();
            Some((new, operands))
        })
        .collect();
    file.anonymous_object_classes = std::mem::take(&mut file.anonymous_object_classes)
        .into_iter()
        .filter_map(|(old, declaration)| {
            expressions.get(&old).copied().map(|new| (new, declaration))
        })
        .collect();
    file.local_class_decls = std::mem::take(&mut file.local_class_decls)
        .into_iter()
        .filter_map(|(old, declaration)| {
            statements.get(&old).copied().map(|new| (new, declaration))
        })
        .collect();
    file.local_class_nested = std::mem::take(&mut file.local_class_nested)
        .into_iter()
        .filter_map(|(old, nested)| statements.get(&old).copied().map(|new| (new, nested)))
        .collect();
    file.statement_suppressions = std::mem::take(&mut file.statement_suppressions)
        .into_iter()
        .filter_map(|(old, suppressions)| {
            statements.get(&old).copied().map(|new| (new, suppressions))
        })
        .collect();
    file.assignment_target_spans = std::mem::take(&mut file.assignment_target_spans)
        .into_iter()
        .filter_map(|(old, span)| statements.get(&StmtId(old)).map(|new| (new.0, span)))
        .collect();
    file.statement_labels = std::mem::take(&mut file.statement_labels)
        .into_iter()
        .filter_map(|(old, label)| statements.get(&old).copied().map(|new| (new, label)))
        .collect();
    file.destructure_source_props = std::mem::take(&mut file.destructure_source_props)
        .into_iter()
        .filter_map(|(old, properties)| statements.get(&StmtId(old)).map(|new| (new.0, properties)))
        .collect();
    file.local_property_context_params = std::mem::take(&mut file.local_property_context_params)
        .into_iter()
        .filter_map(|(old, mut parameters)| {
            statements.get(&old).copied().map(|new| {
                remap_params(&mut parameters, &expressions);
                (new, parameters)
            })
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_class_check_retains_enclosing_annotation_arguments() {
        let source = "@Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\")\n\
            class Owner {\n\
            \x20 private inline fun value(): Int = 1\n\
            }\n";
        let mut diagnostics = crate::diag::DiagSink::new();
        let mut file =
            crate::frontend::parse_source_with_detected_features(source, &mut diagnostics);
        assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);

        compact(&mut file);

        let Decl::Class(class) = file.decl(file.decls[0]) else {
            panic!("expected class declaration");
        };
        let arguments = class.annotation_args[0].clone();
        assert_eq!(arguments.len(), 2);
        assert!(arguments
            .iter()
            .all(|argument| (argument.0 as usize) < file.expr_arena.len()));
        assert_eq!(
            arguments
                .iter()
                .map(|argument| file.const_string_value(*argument).unwrap().to_lossy())
                .collect::<Vec<_>>(),
            ["INVISIBLE_MEMBER", "INVISIBLE_REFERENCE"],
        );
    }
}
