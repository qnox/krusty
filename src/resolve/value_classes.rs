//! Value-class semantics the checker owns: which types are value classes, and the commitment of a
//! Java value used as a value class's dispatch receiver.
//!
//! A value class member receives its dispatch receiver as the class's value rather than as an
//! object that could be `null`: `list[0].f()` with `list: ArrayList<V>` runs `V.f` on the value
//! `list[0]` produced. kotlinc therefore commits a platform or enhanced receiver of a value-class
//! type to that non-null type, exactly as it commits an argument, and checks it where it enters
//! (`checkNotNullExpressionValue(list.get(0), "get(...)")`). An ordinary class receiver is left to
//! fail on its own dereference, so only a value-class receiver is narrowed here.

use super::{Checker, ExprLowering, PlatformNarrowing, ResolvedCall};
use crate::ast::{Expr, ExprId};
use crate::types::Ty;

impl Checker<'_> {
    /// True if `t` names a `@JvmInline value class`, independent of which symbol provider owns it.
    ///
    /// Keep this as the single checker-level semantic query. The module provider exports its
    /// `value_field` through the same `LibraryType::value_underlying` shape as decoded dependencies,
    /// so callers must not pair a source-table lookup with a separate classpath fallback. Besides
    /// duplicating policy, that split would fail to recognize value classes supplied by any future
    /// provider added to the federated resolver.
    pub(super) fn ty_is_value_class(&self, t: Ty) -> bool {
        t.non_null().kotlin_class_internal().is_some_and(|name| {
            self.resolver()
                .classifier(name)
                .is_some_and(|class| class.value_underlying.is_some())
        })
    }

    /// `ty` when it is an array the compiler constructs itself: the size constructor of an
    /// `IntArray` or `Array<T>` has no declaration of its own. A value class over an array
    /// (`UIntArray`) declares its constructors in metadata, and kotlinc calls them
    /// (`UIntArray.constructor-impl(I)[I`), so it is not one.
    pub(super) fn compiler_constructed_array(&self, ty: Option<Ty>) -> Option<Ty> {
        ty.filter(|ty| ty.is_array() && !self.ty_is_value_class(*ty))
    }

    /// The element a single-index read `receiver[i]` yields as the built-in array operation, or
    /// `None` when the subscript is a `get` convention call.
    ///
    /// An array classifier's element access is the array operation itself. A value class over an
    /// array (`UIntArray` over `IntArray`) is not an array: its `get` is the class's own member,
    /// which kotlinc calls (`UIntArray.get-pVg5ArA`). The declaration decides, through
    /// [`Self::ty_is_value_class`], never the classifier's spelling.
    pub(super) fn builtin_array_read_element(&self, receiver: Ty) -> Option<Ty> {
        receiver
            .array_read_elem()
            .filter(|_| !self.ty_is_value_class(receiver))
    }

    /// The element a single-index store `receiver[i] = v` writes as the built-in array operation,
    /// or `None` when it is a `set` convention call; see [`Self::builtin_array_read_element`].
    pub(super) fn builtin_array_store_element(&self, receiver: Ty) -> Option<Ty> {
        receiver
            .array_elem()
            .filter(|_| !self.ty_is_value_class(receiver))
    }

    /// Narrow the explicit receiver of every member call and member property read whose receiver
    /// type is a value class. A safe call has already tested its receiver.
    pub(super) fn narrow_value_class_receivers(&mut self) {
        let receivers: Vec<ExprId> = self
            .resolved_calls
            .iter()
            .filter(|(_, call)| matches!(call, ResolvedCall::Member(_)))
            .filter_map(|(&call, _)| match self.file.expr(call) {
                Expr::Call { callee, .. } => match self.file.expr(*callee) {
                    Expr::Member { receiver, .. } => Some(*receiver),
                    _ => None,
                },
                _ => None,
            })
            .chain(
                self.expr_lowers
                    .iter()
                    .filter(|(_, lowering)| {
                        matches!(lowering, ExprLowering::MemberPropertyRead { .. })
                    })
                    .filter_map(|(&read, _)| match self.file.expr(read) {
                        Expr::Member { receiver, .. } => Some(*receiver),
                        _ => None,
                    }),
            )
            .collect();
        for receiver in receivers {
            let received = self.expr_types[receiver.0 as usize].platform_lower_bound();
            if self.ty_is_value_class(received) && !received.admits_null() {
                self.narrow_platform_value(received, receiver, PlatformNarrowing::Argument);
            }
        }
    }
}
