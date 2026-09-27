//! The inferred type of an unannotated local initialized by a callable reference: kotlinc infers the
//! reflection type (`val f = A::b` is a `KFunction`, not a plain function type), which is why
//! `f.returnType` resolves on it.
//!
//! Only UNBOUND references widen, because that is the set krusty realizes as a real Kotlin reference
//! object (`FunctionReferenceImpl`, hence a `KFunction`). A BOUND reference on a value receiver
//! (`E.A::foo`) can still lower to an `invokedynamic` lambda, which implements `Function{N}` and
//! nothing else; typing its binding as a `KFunction` would `ClassCastException` on the first store.
//! Widening those needs the backend to realize EVERY reference as a reference class.

use super::*;

impl Checker<'_> {
    /// The type an unannotated local takes from its checked initializer `init` of type `ty`.
    pub(super) fn callable_reference_local_ty(&self, init: ExprId, ty: Ty) -> Ty {
        if self.unbound_callable_reference(init) {
            self.libraries.function_reference_type(ty).unwrap_or(ty)
        } else {
            ty
        }
    }

    /// Whether `e` is an UNBOUND callable reference: `::foo`, or `Type::member` whose recorded selection
    /// captures no value receiver. The receiver's role is the checker's recorded binding, never a
    /// second resolution of its spelling (`Alias<Any>::foo` is a type, `Alias::foo` its object).
    fn unbound_callable_reference(&self, e: ExprId) -> bool {
        let Expr::CallableRef { receiver, name } = self.file.expr(e) else {
            return false;
        };
        match receiver {
            _ if name == "class" => false,
            None => true,
            Some(receiver) if !matches!(self.file.expr(*receiver), Expr::Name(_)) => false,
            Some(_) => match self.expr_lowers.get(&e) {
                Some(
                    ExprLowering::CallableReference { binding, .. }
                    | ExprLowering::AdaptedCallableReference { binding, .. },
                ) => !matches!(
                    binding,
                    CallableReferenceBinding::Bound | CallableReferenceBinding::ImplicitThis
                ),
                Some(
                    ExprLowering::LocalFunction { bound_receiver, .. }
                    | ExprLowering::AdaptedLocalFunctionRef { bound_receiver, .. },
                ) => !bound_receiver,
                Some(ExprLowering::ConstructorRef { outer, .. }) => {
                    !matches!(outer, ConstructorReferenceOuter::Expression(_))
                }
                Some(ExprLowering::FunctionInvokeReference { .. }) | None => false,
                Some(_) => true,
            },
        }
    }
}
