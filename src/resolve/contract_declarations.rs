//! kotlinc's checks on a `kotlin.contracts.contract { … }` statement: where one may be written and
//! what its description may refer to.
//!
//! A contract is declaration data, so it must be the first statement of a function's block body. A
//! local function, an open or overriding member, and a lambda cannot declare one, and a function
//! with an expression body cannot either. Inside the description, every reference must name one of
//! the owner's value parameters or its extension receiver, and a parameter is described by at most
//! one `callsInPlace`.

use super::Checker;
use crate::ast::{Decl, Expr, ExprId, File, FunBody, FunDecl, Stmt, StmtId};

/// The declaration whose block body begins with a statement.
enum BodyOwner<'a> {
    /// A function that may declare a contract; its description is checked against it.
    Function(&'a FunDecl),
    /// A property accessor or a secondary constructor: a contract is allowed and not checked here.
    Accessor,
    OpenOrOverride,
    Local,
}

const NOT_FIRST: &str = "contract should be the first statement.";

/// Where a contract call sits in its block: an ordinary statement, or the block's trailing value
/// (a block's last expression statement).
#[derive(Clone, Copy)]
pub(super) enum BlockElement {
    Statement(StmtId),
    Trailing(ExprId),
}

impl Checker<'_> {
    /// Report a confirmed contract call `call`, written as block element `statement`, that kotlinc
    /// rejects.
    pub(super) fn check_contract_statement(&mut self, statement: BlockElement, call: ExprId) {
        let span = self.span(call);
        match first_statement_owner(self.file, statement) {
            None => self.diags.error(span, NOT_FIRST),
            Some(BodyOwner::Local) => self
                .diags
                .error(span, "contracts are not allowed for local functions."),
            Some(BodyOwner::OpenOrOverride) => self.diags.error(
                span,
                "contracts are not allowed for open or override functions.",
            ),
            Some(BodyOwner::Accessor) => {}
            Some(BodyOwner::Function(function)) => {
                for (anchor, message) in crate::contracts::description_errors(
                    self.file,
                    call,
                    &function
                        .params
                        .iter()
                        .map(|parameter| parameter.name.clone())
                        .collect::<Vec<_>>(),
                    &function.name,
                    function.receiver.is_some(),
                ) {
                    let span = self.span(anchor);
                    self.diags
                        .error(span, format!("error in contract description: {message}"));
                }
            }
        }
    }

    /// A function whose expression body is a contract call: kotlinc accepts one only as a block
    /// body's first statement.
    pub(super) fn check_contract_expression_body(&mut self, call: ExprId) {
        let span = self.span(call);
        self.diags
            .error(span, "contracts are only allowed in function body blocks.");
    }
}

/// The declaration whose block body `statement` is the first element of, or `None` when it is a
/// later statement, a statement of a nested block, or the first statement of a lambda or an `init`
/// block.
fn first_statement_owner(file: &File, statement: BlockElement) -> Option<BodyOwner<'_>> {
    let starts = |body: &FunBody| match body {
        FunBody::Block(block) => block_starts_with(file, *block, statement),
        FunBody::Expr(_) | FunBody::None => false,
    };
    for declaration in &file.decl_arena {
        match declaration {
            Decl::Fun(function) if starts(&function.body) => {
                return Some(BodyOwner::Function(function));
            }
            Decl::Fun(_) => {}
            Decl::Class(class) => {
                for method in &class.methods {
                    if !starts(&method.body) {
                        continue;
                    }
                    let open = method.is_open()
                        || method.is_override()
                        || method.is_abstract()
                        || (class.is_interface()
                            && method.visibility != crate::types::Visibility::Private);
                    return Some(if open {
                        BodyOwner::OpenOrOverride
                    } else {
                        BodyOwner::Function(method)
                    });
                }
                if class
                    .secondary_ctors
                    .iter()
                    .filter_map(|constructor| constructor.body)
                    .any(|body| block_starts_with(file, body, statement))
                    || class.body_props.iter().any(|property| {
                        property.getter.as_ref().is_some_and(starts)
                            || property
                                .setter
                                .as_ref()
                                .and_then(|setter| setter.body.as_ref())
                                .is_some_and(starts)
                    })
                {
                    return Some(BodyOwner::Accessor);
                }
            }
            Decl::Property(property) => {
                if property.getter.as_ref().is_some_and(starts)
                    || property
                        .setter
                        .as_ref()
                        .and_then(|setter| setter.body.as_ref())
                        .is_some_and(starts)
                {
                    return Some(BodyOwner::Accessor);
                }
            }
        }
    }
    file.stmt_arena
        .iter()
        .find_map(|candidate| match candidate {
            Stmt::LocalFun(function) if starts(&function.body) => Some(BodyOwner::Local),
            _ => None,
        })
}

fn block_starts_with(file: &File, block: ExprId, statement: BlockElement) -> bool {
    let Expr::Block { stmts, trailing } = file.expr(block) else {
        return false;
    };
    match statement {
        BlockElement::Statement(statement) => stmts.first() == Some(&statement),
        BlockElement::Trailing(expression) => stmts.is_empty() && *trailing == Some(expression),
    }
}
