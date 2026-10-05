//! kotlinc's checks on a `kotlin.contracts.contract { … }` statement: where one may be written and
//! what its description may refer to.
//!
//! A contract is declaration data, so it must be the first statement of a function's block body. A
//! local function, an open or overriding member, and a lambda cannot declare one, and a function
//! with an expression body cannot either. Inside the description, every reference must name one of
//! the owner's value parameters or its extension receiver, and a parameter is described by at most
//! one `callsInPlace`.

use super::{Checker, ExprLowering, ResolvedCall};
use crate::ast::{Decl, Expr, ExprId, File, FunBody, FunDecl, Stmt, StmtId};
use crate::contracts::{
    CallBinding, Description, DescriptionBinder, DescriptionOwner, InvocationKind, KindBinding,
    TermId, TermKind,
};
use crate::types::{Ty, TypeName};

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
        // kotlinc reads a contract block from a first statement whose callee is spelled `contract`;
        // any other call that selects the intrinsic, an import alias included, is misplaced.
        let owner = callee_spelled_contract(self.file, call)
            .then(|| first_statement_owner(self.file, statement))
            .flatten();
        match owner {
            None => self.diags.error(span, NOT_FIRST),
            Some(BodyOwner::Local) => self
                .diags
                .error(span, "contracts are not allowed for local functions."),
            Some(BodyOwner::OpenOrOverride) => self.diags.error(
                span,
                "contracts are not allowed for open or override functions.",
            ),
            Some(BodyOwner::Accessor) => {}
            Some(BodyOwner::Function(function)) => self.check_contract_description(function, call),
        }
    }

    /// Report kotlinc's "error in contract description" findings for the description of `call`,
    /// decoded from the declarations its checked calls selected.
    fn check_contract_description(&mut self, function: &FunDecl, call: ExprId) {
        let Some(description) = Description::read(self.file, call) else {
            return;
        };
        let params = function
            .params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let decoded = description.decode(
            &DescriptionOwner {
                params: &params,
                name: &function.name,
                has_receiver: function.receiver.is_some(),
            },
            &mut CheckedDescriptionBinder {
                checker: self,
                params: &params,
            },
        );
        for (anchor, message) in decoded.errors {
            let span = self.contract_element_span(anchor);
            self.diags
                .error(span, format!("error in contract description: {message}"));
        }
    }

    /// Where kotlinc reports a description element: a dot-qualified call at its selector.
    fn contract_element_span(&self, element: ExprId) -> crate::diag::Span {
        let span = self.span(element);
        match self.file.expr(element) {
            Expr::Call { callee, .. } if !self.file.infix_calls.contains(&element.0) => {
                match self.file.expr(*callee) {
                    Expr::Member { name, .. } => {
                        crate::diag::Span::new(self.member_name_span(*callee, name).lo, span.hi)
                    }
                    _ => span,
                }
            }
            _ => span,
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

/// Whether the callee of `call` is spelled `contract`, simple or qualified.
fn callee_spelled_contract(file: &File, call: ExprId) -> bool {
    let Expr::Call { callee, .. } = file.expr(call) else {
        return false;
    };
    matches!(
        file.expr(*callee),
        Expr::Name(name) | Expr::Member { name, .. } if name == "contract"
    )
}

/// Binds a checked description to the declarations its calls and invocation kinds selected.
struct CheckedDescriptionBinder<'c, 'a> {
    checker: &'c Checker<'a>,
    params: &'c [String],
}

impl DescriptionBinder for CheckedDescriptionBinder<'_, '_> {
    fn bind_call(&mut self, description: &Description, call: TermId) -> CallBinding {
        let TermKind::Call { name, .. } = &description.term(call).kind else {
            return CallBinding::Unresolved;
        };
        match self
            .checker
            .resolved_calls
            .get(&description.term(call).origin)
        {
            Some(ResolvedCall::Member(selected)) => {
                let Some(owner) = selected.member.owner else {
                    return CallBinding::Unresolved;
                };
                self.checker
                    .libraries
                    .contract_dsl_member(owner, name, selected.member.params.len())
                    .map_or_else(
                        || CallBinding::Foreign(member_callable_id(owner, &selected.member.name)),
                        CallBinding::Dsl,
                    )
            }
            Some(ResolvedCall::Companion(member)) => {
                member.owner.map_or(CallBinding::Unresolved, |owner| {
                    CallBinding::Foreign(member_callable_id(owner, &member.name))
                })
            }
            Some(ResolvedCall::TopLevel(selected)) => {
                CallBinding::Foreign(self.top_level_callable_id(
                    &selected.callable,
                    selected.stable_declaration,
                    &selected.callable.name,
                ))
            }
            Some(ResolvedCall::Extension(selected)) => {
                CallBinding::Foreign(self.top_level_callable_id(
                    &selected.callable,
                    selected.stable_declaration,
                    &selected.callable.name,
                ))
            }
            Some(ResolvedCall::MemberExtension { owner, name, .. }) => {
                CallBinding::Foreign(member_callable_id(*owner, name))
            }
            _ => CallBinding::Unresolved,
        }
    }

    fn bind_kind(&mut self, description: &Description, call: TermId, kind: TermId) -> KindBinding {
        let Some(ResolvedCall::Member(selected)) = self
            .checker
            .resolved_calls
            .get(&description.term(call).origin)
        else {
            return KindBinding::Unresolved;
        };
        let Some(expected) = selected
            .member
            .params
            .get(1)
            .copied()
            .and_then(Ty::kotlin_class_internal)
        else {
            return KindBinding::Unresolved;
        };
        let origin = description.term(kind).origin;
        if let Some(entry) = self.checker.resolved_enum_entries.get(&origin) {
            if entry.classifier == expected {
                if let Some(kind) = InvocationKind::of_entry(&entry.name) {
                    return KindBinding::Entry(kind);
                }
            }
        }
        self.foreign_kind(description, kind)
            .map_or(KindBinding::Unresolved, KindBinding::Foreign)
    }
}

impl CheckedDescriptionBinder<'_, '_> {
    /// kotlinc's callable id of top-level declaration `name`: `p/name`, with an empty package for
    /// the root. A source declaration's package is its file's; a library one's is the package of
    /// its platform `callable`.
    fn top_level_callable_id(
        &self,
        callable: &crate::libraries::LibraryCallable,
        declaration: Option<crate::fir::DeclarationId>,
        name: &str,
    ) -> String {
        let source_package =
            self.checker
                .resolved_index
                .zip(declaration)
                .and_then(|(index, declaration)| {
                    index.source_package(index.declaration_anchor(declaration)?.source)
                });
        let package = source_package
            .unwrap_or_else(|| self.checker.libraries.top_level_callable_package(callable));
        format!("{}/{name}", package.render())
    }

    /// kotlinc's rendering of a value that is not an invocation kind: a parameter (`R|<local>/k|`),
    /// a top-level property (`R|p/K|`), or a property of an object (`Q|p/O|.R|p/O.K|`).
    fn foreign_kind(&self, description: &Description, kind: TermId) -> Option<String> {
        let term = description.term(kind);
        if let TermKind::Name(segments) = &term.kind {
            if let [name] = segments.as_slice() {
                if self.params.contains(name) {
                    return Some(format!("R|<local>/{name}|"));
                }
            }
        }
        match self.checker.expr_lowers.get(&term.origin)? {
            ExprLowering::TopLevelPropertyGet(access) => Some(format!(
                "R|{}|",
                self.top_level_callable_id(
                    &access.property.getter,
                    access.property.stable_declaration,
                    &access.property.name,
                )
            )),
            ExprLowering::MemberPropertyRead { name, owner, .. } => {
                let Expr::Member { receiver, .. } = self.checker.file.expr(term.origin) else {
                    return None;
                };
                let Some(ExprLowering::SingletonValue(singleton)) =
                    self.checker.expr_lowers.get(receiver)
                else {
                    return None;
                };
                Some(format!(
                    "Q|{}|.R|{}|",
                    class_id(singleton.classifier),
                    member_callable_id(*owner, name)
                ))
            }
            _ => None,
        }
    }
}

/// kotlinc's callable id of a member of `owner`: `p/Outer.Inner.name`.
fn member_callable_id(owner: TypeName, name: &str) -> String {
    let (package, class) = class_id_parts(owner);
    format!("{package}/{class}.{name}")
}

/// kotlinc's class id of `classifier`: `p/Outer.Inner`, or `Outer.Inner` in the root package.
fn class_id(classifier: TypeName) -> String {
    match class_id_parts(classifier) {
        (package, class) if package.is_empty() => class,
        (package, class) => format!("{package}/{class}"),
    }
}

/// The package and the dot-separated nested class path of `classifier`.
fn class_id_parts(classifier: TypeName) -> (String, String) {
    let mut nested = Vec::new();
    let mut outer = classifier;
    while let Some(enclosing) = outer.nested_owner() {
        nested.push(
            outer
                .nested_segment_within(enclosing)
                .expect("a recorded nested owner must own the classifier segment"),
        );
        outer = enclosing;
    }
    let mut class = outer.segment_ref().to_string();
    for segment in nested.into_iter().rev() {
        class.push('.');
        class.push_str(segment);
    }
    (outer.package(), class)
}
