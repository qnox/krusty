//! The use-site checks kotlinc runs on every resolved reference to a declaration, whichever
//! position it is written in: its opt-in requirements ([`super::opt_in_usage`]) and its
//! deprecation ([`super::deprecation_usage`]).

use super::*;

impl Checker<'_> {
    /// Check the declarations expression `expression` uses.
    pub(super) fn check_expression_usage(&mut self, scope: &CheckerScope<'_>, expression: ExprId) {
        self.check_expression_opt_in(scope, expression);
        self.check_expression_deprecation(expression);
    }

    /// Check a type reference the body check resolved.
    pub(super) fn check_type_reference_usage(&mut self, reference: &TypeRef, resolved: Ty) {
        self.check_type_reference_opt_in(reference, resolved);
        self.check_type_reference_deprecation(reference, resolved);
    }

    /// Check a declaration type finalized in Pass 1 and each type argument written in it.
    pub(super) fn check_finalized_type_reference_usage(
        &mut self,
        reference: &TypeRef,
        resolved: Ty,
    ) {
        self.check_finalized_type_reference_opt_in(reference, resolved);
        self.check_finalized_type_reference_deprecation(reference, resolved);
    }

    /// Check the supertypes class `class` lists, as their types were published with the class
    /// header in Pass 1.
    pub(super) fn check_supertype_usage(&mut self, class: &ClassDecl, owner: Option<TypeName>) {
        if self.discover_anonymous_captures {
            return;
        }
        for (reference, supertype) in self.declared_supertype_references(class, owner) {
            self.check_finalized_type_reference_usage(&reference, supertype);
        }
    }

    /// The classifier qualifier `expression` is selected through (`Outer` in `Outer.Nested()`,
    /// `Obj` in `Obj.f()`), with the classifier the selection proves it denotes. A qualifier is a
    /// receiver that was never typed as a value, or an object's singleton.
    pub(super) fn classifier_qualifier(&self, expression: ExprId) -> Option<(ExprId, TypeName)> {
        let (receiver, read) = match self.file.expr(expression) {
            Expr::Call { callee, .. } => match self.file.expr(*callee) {
                Expr::Member { receiver, .. } => (*receiver, false),
                _ => return None,
            },
            Expr::Member { receiver, .. } => (*receiver, true),
            _ => return None,
        };
        if !matches!(
            self.file.expr(receiver),
            Expr::Name(_) | Expr::Member { .. }
        ) {
            return None;
        }
        // An object qualifier is also its singleton value.
        if let Some(ExprLowering::SingletonValue(singleton)) = self.expr_lowers.get(&receiver) {
            return Some((receiver, singleton.classifier));
        }
        // A static member selected through its classifier (a Java static method, a companion-block
        // member) dispatches on no value, so its written receiver is the classifier qualifier even
        // though resolution typed it.
        if !read {
            if let Some(ResolvedCall::Companion(member)) = self.resolved_calls.get(&expression) {
                if member.singleton_dispatch.is_none() {
                    if let Some(owner) = member.owner {
                        return Some((receiver, owner));
                    }
                }
            }
        }
        if self.expr_types[receiver.0 as usize] != Ty::Error {
            return None;
        }
        let classifier = if read {
            match self.expr_lowers.get(&expression)? {
                ExprLowering::AssociatedPropertyRead { owner, .. }
                | ExprLowering::ClassifierPropertyRead { owner, .. } => *owner,
                ExprLowering::SingletonValue(singleton) => singleton.classifier.nested_owner()?,
                _ => return None,
            }
        } else if let Some(constructor) = self.resolved_constructors.get(&expression) {
            constructor.owner().nested_owner()?
        } else {
            match self.resolved_calls.get(&expression)? {
                ResolvedCall::Member(selected) => {
                    selected.receiver.non_null().kotlin_class_internal()?
                }
                ResolvedCall::Companion(member) => member.owner?,
                _ => return None,
            }
        };
        Some((receiver, classifier))
    }
}
