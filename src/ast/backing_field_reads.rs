//! Whether a property accessor reads its property's backing field.
//!
//! Inside an accessor, `field` binds to the property's backing field unless a local declaration
//! nearer to the use declares `field`: a local variable from the statement after it, a lambda,
//! local-function or catch parameter, or a loop variable. The binding reaches into lambdas, local
//! functions and their parameter defaults, and object expressions, where it binds before the
//! object's own members; a nested property's own accessor binds `field` to that property instead.

use super::{ClassInit, Decl, Expr, ExprId, File, FunBody, FunDecl, Stmt, StmtId};

const FIELD: &str = "field";

/// Whether a property's getter has a backing field `field` at all. kotlinc declares none in a
/// getter whose property has neither a declared type nor an initializer: that getter infers the
/// property's type, and `field` there resolves as any other name.
pub fn getter_declares_field(declared_type: bool, initialized: bool) -> bool {
    declared_type || initialized
}

/// Whether the accessor body `body` reads or writes the backing field of the property it belongs
/// to.
pub fn accessor_uses_backing_field(file: &File, body: ExprId) -> bool {
    expr_uses(file, body)
}

fn expr_uses(file: &File, expr: ExprId) -> bool {
    match file.expr(expr) {
        Expr::Name(name) => name == FIELD,
        Expr::Lambda { params, body } => {
            !params.iter().any(|parameter| parameter == FIELD) && expr_uses(file, *body)
        }
        Expr::Block { stmts, trailing } => {
            for &stmt in stmts {
                if stmt_uses(file, stmt) {
                    return true;
                }
                if declares_field(file, stmt) {
                    return false;
                }
            }
            trailing.is_some_and(|trailing| expr_uses(file, trailing))
        }
        Expr::Try {
            body,
            catches,
            finally,
        } => {
            expr_uses(file, *body)
                || catches
                    .iter()
                    .any(|catch| catch.name != FIELD && expr_uses(file, catch.body))
                || finally.is_some_and(|finally| expr_uses(file, finally))
        }
        Expr::Call { .. } if file.anonymous_object_classes.contains_key(&expr) => {
            let Decl::Class(class) = file.decl(file.anonymous_object_classes[&expr]) else {
                return false;
            };
            // A member property of the object named `field` does not shadow it: like a local,
            // the field binds before the object's members.
            class
                .base_args
                .iter()
                .any(|&argument| expr_uses(file, argument))
                || class.init_order.iter().any(|init| match init {
                    ClassInit::PropInit(index) => class.body_props[*index]
                        .init
                        .is_some_and(|init| expr_uses(file, init)),
                    ClassInit::Block(block) => expr_uses(file, *block),
                })
                || class
                    .methods
                    .iter()
                    .any(|method| function_uses(file, method))
        }
        _ => file.any_child_expr(expr, &mut |child| expr_uses(file, child), &mut |stmt| {
            stmt_uses(file, stmt)
        }),
    }
}

fn stmt_uses(file: &File, stmt: StmtId) -> bool {
    match file.stmt(stmt) {
        Stmt::Assign { name, value } => name == FIELD || expr_uses(file, *value),
        Stmt::IncDec { name, .. } => name == FIELD,
        Stmt::For {
            name, range, body, ..
        } => {
            expr_uses(file, range.start)
                || expr_uses(file, range.end)
                || (name != FIELD && expr_uses(file, *body))
        }
        Stmt::ForEach {
            name,
            iterable,
            body,
            ..
        } => expr_uses(file, *iterable) || (name != FIELD && expr_uses(file, *body)),
        Stmt::LocalFun(function) => function_uses(file, function),
        _ => file.any_child_stmt(stmt, &mut |child| expr_uses(file, child)),
    }
}

/// Whether a local function's parameter defaults or body read the field. A parameter named
/// `field` shadows it in the defaults after it and in the body.
fn function_uses(file: &File, function: &FunDecl) -> bool {
    for parameter in &function.params {
        if parameter
            .default
            .is_some_and(|default| expr_uses(file, default))
        {
            return true;
        }
        if parameter.name == FIELD {
            return false;
        }
    }
    matches!(
        &function.body,
        FunBody::Expr(body) | FunBody::Block(body) if expr_uses(file, *body)
    )
}

/// Whether `stmt` declares a local `field` for the statements after it.
fn declares_field(file: &File, stmt: StmtId) -> bool {
    match file.stmt(stmt) {
        Stmt::Local { name, .. }
        | Stmt::LocalLateinit { name, .. }
        | Stmt::LocalDelegate { name, .. } => name == FIELD,
        Stmt::Destructure { entries, .. } => entries
            .iter()
            .any(|entry| !entry.ignored && entry.name == FIELD),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::accessor_uses_backing_field;
    use crate::ast::{Decl, FunBody};
    use crate::diag::DiagSink;

    /// Whether the getter of each property of class `C` in `source` uses the backing field, in
    /// declaration order.
    fn getters(source: &str) -> Vec<(String, bool)> {
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = crate::parser::parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "{}",
            diagnostics.render("test", source)
        );
        let class = file
            .decl_arena
            .iter()
            .find_map(|declaration| match declaration {
                Decl::Class(class) if class.name == "C" => Some(class),
                _ => None,
            })
            .expect("class C parsed");
        class
            .body_props
            .iter()
            .map(|property| {
                let uses = match property.getter.as_ref().expect("a getter") {
                    FunBody::Expr(body) | FunBody::Block(body) => {
                        accessor_uses_backing_field(&file, *body)
                    }
                    FunBody::None => false,
                };
                (property.name.clone(), uses)
            })
            .collect()
    }

    #[test]
    fn field_binds_to_the_backing_field_unless_a_nearer_local_declares_it() {
        let source = "class C {
            val direct: Int get() = field
            val statement: Int get() { val copy = field; return copy }
            val shadowed: Int get() { val field = 8; return field }
            val initializerReads: Int get() { val field = field; return field }
            val throughLambda: Int get() = run { field }
            val lambdaParameter: Int get() = 9.let { field -> field }
            val throughLocalFunction: Int get() { fun read() = field; return read() }
            val localFunctionParameter: Int get() { fun read(field: Int) = field; return read(1) }
            val throughObject: Int get() = object { fun read() = field }.read()
            val catchParameter: Int get() = try { 1 } catch (field: Exception) { field.hashCode() }
            val loopVariable: Int get() { for (field in 1..2) { println(field) }; return 1 }
            val parameterDefault: Int get() { fun read(x: Int = field) = x; return read() }
            val shadowingParameterDefault: Int get() { fun read(field: Int = 1, x: Int = field) = x; return read() }
            val objectMember: Int get() = object { val field = 1; fun read() = field }.read()
        }";
        assert_eq!(
            getters(source),
            [
                ("direct", true),
                ("statement", true),
                ("shadowed", false),
                ("initializerReads", true),
                ("throughLambda", true),
                ("lambdaParameter", false),
                ("throughLocalFunction", true),
                ("localFunctionParameter", false),
                ("throughObject", true),
                ("catchParameter", false),
                ("loopVariable", false),
                ("parameterDefault", true),
                ("shadowingParameterDefault", false),
                ("objectMember", true),
            ]
            .map(|(name, uses)| (name.to_string(), uses))
        );
    }
}
