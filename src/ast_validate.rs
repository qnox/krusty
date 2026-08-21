//! Structural validation for parser-produced arena ASTs.
//!
//! This pass is deliberately syntax-only: it checks arena references and source ranges without
//! resolving names or deciding whether a valid Kotlin construct is semantically supported.

use crate::ast::*;
use crate::diag::Span;

fn span(source: &str, label: &str, value: Span) -> Result<(), String> {
    let lo = value.lo as usize;
    let hi = value.hi as usize;
    if lo > hi || hi > source.len() {
        return Err(format!(
            "{label} span {}..{} is outside source length {}",
            value.lo,
            value.hi,
            source.len()
        ));
    }
    if !source.is_char_boundary(lo) || !source.is_char_boundary(hi) {
        return Err(format!(
            "{label} span {}..{} splits UTF-8",
            value.lo, value.hi
        ));
    }
    Ok(())
}

fn type_alias(source: &str, alias: &TypeAliasDecl, label: &str) -> Result<(), String> {
    span(source, label, alias.span)?;
    span(source, &format!("{label} target"), alias.target.span)?;
    if alias.target.span.lo < alias.span.lo || alias.target.span.hi > alias.span.hi {
        return Err(format!(
            "{label} target span {}..{} is outside declaration span {}..{}",
            alias.target.span.lo, alias.target.span.hi, alias.span.lo, alias.span.hi
        ));
    }
    Ok(())
}

fn expr(file: &File, id: ExprId, label: &str) -> Result<(), String> {
    if id.0 as usize >= file.expr_arena.len() {
        Err(format!(
            "{label} references missing expression {} (arena length {})",
            id.0,
            file.expr_arena.len()
        ))
    } else {
        Ok(())
    }
}

fn stmt(file: &File, id: StmtId, label: &str) -> Result<(), String> {
    if id.0 as usize >= file.stmt_arena.len() {
        Err(format!(
            "{label} references missing statement {} (arena length {})",
            id.0,
            file.stmt_arena.len()
        ))
    } else {
        Ok(())
    }
}

fn decl(file: &File, id: DeclId, label: &str) -> Result<(), String> {
    if id.0 as usize >= file.decl_arena.len() {
        Err(format!(
            "{label} references missing declaration {} (arena length {})",
            id.0,
            file.decl_arena.len()
        ))
    } else {
        Ok(())
    }
}

fn exprs(file: &File, ids: impl IntoIterator<Item = ExprId>, label: &str) -> Result<(), String> {
    for id in ids {
        expr(file, id, label)?;
    }
    Ok(())
}

fn annotation_args(file: &File, args: &[Vec<ExprId>], label: &str) -> Result<(), String> {
    exprs(file, args.iter().flatten().copied(), label)
}

fn fun_body(file: &File, body: &FunBody, label: &str) -> Result<(), String> {
    match body {
        FunBody::Expr(id) | FunBody::Block(id) => expr(file, *id, label),
        FunBody::None => Ok(()),
    }
}

fn params(file: &File, params: &[Param], label: &str) -> Result<(), String> {
    for param in params {
        if let Some(default) = param.default {
            expr(file, default, label)?;
        }
        annotation_args(file, &param.annotation_args, label)?;
    }
    Ok(())
}

fn function(file: &File, function: &FunDecl, label: &str) -> Result<(), String> {
    params(file, &function.params, label)?;
    annotation_args(file, &function.annotation_args, label)?;
    fun_body(file, &function.body, label)
}

fn property(file: &File, property: &PropDecl, label: &str) -> Result<(), String> {
    params(file, &property.context_params, label)?;
    annotation_args(file, &property.annotation_args, label)?;
    exprs(file, property.init, label)?;
    exprs(file, property.delegate, label)?;
    if let Some(getter) = &property.getter {
        fun_body(file, getter, label)?;
    }
    if let Some(setter) = &property.setter {
        if let Some(body) = &setter.body {
            fun_body(file, body, label)?;
        }
    }
    Ok(())
}

fn class(source: &str, file: &File, class: &ClassDecl, label: &str) -> Result<(), String> {
    annotation_args(file, &class.annotation_args, label)?;
    annotation_args(file, &class.primary_ctor_annotation_args, label)?;
    for param in &class.props {
        exprs(file, param.default, label)?;
        annotation_args(file, &param.annotation_args, label)?;
    }
    for method in &class.methods {
        function(file, method, label)?;
    }
    exprs(file, class.base_args.iter().copied(), label)?;
    exprs(
        file,
        class.delegation_exprs.iter().map(|(_, value)| *value),
        label,
    )?;
    for property_decl in &class.body_props {
        property(file, property_decl, label)?;
    }
    for init in &class.init_order {
        match init {
            ClassInit::PropInit(index) if *index >= class.body_props.len() => {
                return Err(format!(
                    "{label} references missing body property {index} (list length {})",
                    class.body_props.len()
                ));
            }
            ClassInit::PropInit(_) => {}
            ClassInit::Block(body) => expr(file, *body, label)?,
        }
    }
    for constructor in &class.secondary_ctors {
        params(file, &constructor.params, label)?;
        annotation_args(file, &constructor.annotation_args, label)?;
        match &constructor.delegation {
            CtorDelegation::None => {}
            CtorDelegation::This(call) | CtorDelegation::Super(call) => {
                exprs(file, call.args.iter().copied(), label)?;
                if call.names.len() != call.args.len() {
                    return Err(format!(
                        "{label} constructor argument names length {} differs from argument length {}",
                        call.names.len(),
                        call.args.len()
                    ));
                }
            }
        }
        exprs(file, constructor.body, label)?;
    }
    for (index, alias) in class.type_aliases.iter().enumerate() {
        type_alias(source, alias, &format!("{label} type alias {index}"))?;
    }
    for entry in &class.enum_entries {
        annotation_args(file, &entry.annotation_args, label)?;
        exprs(file, entry.args.iter().copied(), label)?;
        if entry.arg_names.len() != entry.args.len() {
            return Err(format!(
                "{label} enum argument names length {} differs from argument length {}",
                entry.arg_names.len(),
                entry.args.len()
            ));
        }
        for method in &entry.methods {
            function(file, method, label)?;
        }
        for property_decl in &entry.props {
            property(file, property_decl, label)?;
        }
        for init in &entry.init_order {
            match init {
                ClassInit::PropInit(index) if *index >= entry.props.len() => {
                    return Err(format!(
                        "{label} enum entry references missing body property {index} (list length {})",
                        entry.props.len()
                    ));
                }
                ClassInit::PropInit(_) => {}
                ClassInit::Block(body) => expr(file, *body, label)?,
            }
        }
    }
    if let Some(companion) = class.companion {
        decl(file, companion, label)?;
    }
    Ok(())
}

impl File {
    /// Validate the arena and exact-source-range invariants required of a successful parse.
    pub fn validate_integrity(&self, source: &str) -> Result<(), String> {
        if self.expr_arena.len() != self.expr_spans.len() {
            return Err(format!(
                "expression arena length {} differs from span length {}",
                self.expr_arena.len(),
                self.expr_spans.len()
            ));
        }
        if self.stmt_arena.len() != self.stmt_spans.len() {
            return Err(format!(
                "statement arena length {} differs from span length {}",
                self.stmt_arena.len(),
                self.stmt_spans.len()
            ));
        }
        for (index, value) in self.expr_spans.iter().copied().enumerate() {
            span(source, &format!("expression {index}"), value)?;
        }
        for (index, value) in self.stmt_spans.iter().copied().enumerate() {
            span(source, &format!("statement {index}"), value)?;
        }
        for (index, declaration) in self.decl_arena.iter().enumerate() {
            let label = format!("declaration {index}");
            match declaration {
                Decl::Fun(value) => {
                    span(source, &label, value.span)?;
                    span(source, "function signature", value.signature_span)?;
                    function(self, value, &label)?;
                }
                Decl::Class(value) => {
                    span(source, &label, value.span)?;
                    class(source, self, value, &label)?;
                }
                Decl::Property(value) => {
                    span(source, &label, value.span)?;
                    property(self, value, &label)?;
                }
            }
        }
        for (index, expression) in self.expr_arena.iter().enumerate() {
            let label = format!("expression {index}");
            match expression {
                Expr::AnnotationArrayLiteral(values) => {
                    exprs(self, values.iter().copied(), &label)?
                }
                Expr::NotNull { operand }
                | Expr::Throw { operand }
                | Expr::Is { operand, .. }
                | Expr::As { operand, .. }
                | Expr::IncDec {
                    target: operand, ..
                }
                | Expr::Unary { operand, .. } => expr(self, *operand, &label)?,
                Expr::Elvis { lhs, rhs } | Expr::Binary { lhs, rhs, .. } => {
                    expr(self, *lhs, &label)?;
                    expr(self, *rhs, &label)?;
                }
                Expr::Template(parts) => exprs(
                    self,
                    parts.iter().filter_map(|part| match part {
                        TemplatePart::Expr(id) => Some(*id),
                        TemplatePart::Str(_) => None,
                    }),
                    &label,
                )?,
                Expr::SafeCall { receiver, args, .. } => {
                    expr(self, *receiver, &label)?;
                    exprs(self, args.iter().flatten().copied(), &label)?;
                }
                Expr::Return { value, .. } => exprs(self, value.iter().copied(), &label)?,
                Expr::Lambda { body, .. } => expr(self, *body, &label)?,
                Expr::Try {
                    body,
                    catches,
                    finally,
                } => {
                    expr(self, *body, &label)?;
                    exprs(self, catches.iter().map(|catch| catch.body), &label)?;
                    exprs(self, finally.iter().copied(), &label)?;
                }
                Expr::InRange {
                    value, start, end, ..
                } => {
                    expr(self, *value, &label)?;
                    expr(self, *start, &label)?;
                    expr(self, *end, &label)?;
                }
                Expr::RangeTo { lo, hi, .. } => {
                    expr(self, *lo, &label)?;
                    expr(self, *hi, &label)?;
                }
                Expr::Member { receiver, .. } => expr(self, *receiver, &label)?,
                Expr::ExtensionAccess { receiver, callable } => {
                    expr(self, *receiver, &label)?;
                    expr(self, *callable, &label)?;
                }
                Expr::Index { array, indices } => {
                    expr(self, *array, &label)?;
                    exprs(self, indices.iter().copied(), &label)?;
                }
                Expr::Call { callee, args } => {
                    expr(self, *callee, &label)?;
                    exprs(self, args.iter().copied(), &label)?;
                }
                Expr::If {
                    cond,
                    then_branch,
                    else_branch,
                } => {
                    expr(self, *cond, &label)?;
                    expr(self, *then_branch, &label)?;
                    exprs(self, else_branch.iter().copied(), &label)?;
                }
                Expr::Block { stmts, trailing } => {
                    for id in stmts {
                        stmt(self, *id, &label)?;
                    }
                    exprs(self, trailing.iter().copied(), &label)?;
                }
                Expr::When { subject, arms } => {
                    exprs(self, subject.iter().copied(), &label)?;
                    for arm in arms {
                        exprs(self, arm.conditions.iter().copied(), &label)?;
                        exprs(self, arm.guard.iter().copied(), &label)?;
                        expr(self, arm.body, &label)?;
                    }
                }
                Expr::CallableRef { receiver, .. } => {
                    exprs(self, receiver.iter().copied(), &label)?;
                }
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
            }
            if let Expr::Binary { operator_span, .. } = expression {
                span(source, &format!("{label} operator"), *operator_span)?;
            }
        }
        for (index, statement) in self.stmt_arena.iter().enumerate() {
            let label = format!("statement {index}");
            match statement {
                Stmt::Local { init, .. }
                | Stmt::LocalDelegate { delegate: init, .. }
                | Stmt::Destructure { init, .. }
                | Stmt::Assign { value: init, .. }
                | Stmt::Expr(init) => expr(self, *init, &label)?,
                Stmt::AssignMember {
                    receiver, value, ..
                } => {
                    expr(self, *receiver, &label)?;
                    expr(self, *value, &label)?;
                }
                Stmt::AssignIndex {
                    array,
                    indices,
                    value,
                } => {
                    expr(self, *array, &label)?;
                    exprs(self, indices.iter().copied(), &label)?;
                    expr(self, *value, &label)?;
                }
                Stmt::Return(value, _) => exprs(self, value.iter().copied(), &label)?,
                Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
                    expr(self, *cond, &label)?;
                    expr(self, *body, &label)?;
                }
                Stmt::For { range, body, .. } => {
                    expr(self, range.start, &label)?;
                    expr(self, range.end, &label)?;
                    expr(self, *body, &label)?;
                }
                Stmt::ForEach { iterable, body, .. } => {
                    expr(self, *iterable, &label)?;
                    expr(self, *body, &label)?;
                }
                Stmt::LocalFun(value) => function(self, value, &label)?,
                Stmt::LocalClass(value) => class(source, self, value, &label)?,
                Stmt::CompoundAssign { target, value, .. } => {
                    expr(self, *target, &label)?;
                    expr(self, *value, &label)?;
                }
                Stmt::LocalTypeAlias(alias) => {
                    type_alias(source, alias, &format!("{label} type alias"))?;
                }
                Stmt::LocalLateinit { .. }
                | Stmt::IncDec { .. }
                | Stmt::Break(_)
                | Stmt::Continue(_) => {}
            }
        }
        for id in &self.decls {
            decl(self, *id, "top-level declaration list")?;
        }
        for id in &self.expect_decls {
            decl(self, *id, "expect declaration list")?;
        }
        for (index, alias) in self.type_alias_decls.iter().enumerate() {
            type_alias(source, alias, &format!("file type alias {index}"))?;
        }
        exprs(self, self.script_body, "script body")?;
        annotation_args(
            self,
            &self
                .file_annotations
                .iter()
                .map(|(_, args)| args.clone())
                .collect::<Vec<_>>(),
            "file annotation",
        )?;
        for (expression, declaration) in &self.anonymous_object_classes {
            expr(self, *expression, "anonymous object map")?;
            decl(self, *declaration, "anonymous object map")?;
        }
        for (declaration, enclosing) in &self.anonymous_object_enclosing_functions {
            decl(self, *declaration, "anonymous enclosing map")?;
            match enclosing {
                AnonymousEnclosingFunction::TopLevel(function) => {
                    decl(self, *function, "anonymous enclosing function")?;
                }
                AnonymousEnclosingFunction::Member { class, method } => {
                    decl(self, *class, "anonymous enclosing class")?;
                    let Decl::Class(class) = &self.decl_arena[class.0 as usize] else {
                        return Err("anonymous enclosing member owner is not a class".into());
                    };
                    if *method as usize >= class.methods.len() {
                        return Err(format!(
                            "anonymous enclosing member {} is outside method list length {}",
                            method,
                            class.methods.len()
                        ));
                    }
                }
            }
        }
        for (statement, declaration) in &self.local_class_decls {
            stmt(self, *statement, "local class map")?;
            decl(self, *declaration, "local class map")?;
        }
        for (statement, (_, label_span)) in &self.statement_labels {
            stmt(self, *statement, "statement label map")?;
            span(source, "statement label", *label_span)?;
        }
        for (statement, context_params) in &self.local_property_context_params {
            stmt(self, *statement, "local property context map")?;
            params(self, context_params, "local property context map")?;
        }
        Ok(())
    }
}
