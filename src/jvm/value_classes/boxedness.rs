//! Classifies whether a checked value-class expression currently occupies its JVM box.
//!
//! This operation consumes stable semantic and representation facts recorded on the IR. It does
//! not resolve declarations or recover value-class identity from source spelling.

use super::{repr_of_ty, target, Repr, ReprCtx, Target};
use crate::ir::{Callee, ExprId, IrExpr};
use crate::types::{Ty, TypeName};

/// Whether a JVM method descriptor's return type is the classfile form of `class`.
/// `()Lpkg/Foo;` and `[Lpkg/Foo;` both end in that spelling; the comparison uses the interned
/// identity instead of rendering it.
fn descriptor_returns_class(descriptor: &str, class: TypeName) -> bool {
    let Some(internal) = descriptor.strip_suffix(';') else {
        return false;
    };
    let Some(at) = internal.rfind('L') else {
        return false;
    };
    class.matches(&internal[at + 1..])
}

impl ReprCtx<'_> {
    /// Whether `id` reads a property whose declaration types it as the value class `x`, so its
    /// accessor returns `x`'s carrier rather than a box.
    fn declares_unboxed_property(&self, id: ExprId, x: TypeName) -> bool {
        let IrExpr::PropertyRead { operation, .. } = &self.exprs[id as usize] else {
            return false;
        };
        self.types
            .unboxed_declared_property(operation.unwrap_or(id), self.under)
            .is_some_and(|declared| declared.non_null().obj_internal() == Some(x))
    }

    /// Whether the expr at `id` produces a boxed value-class `x` object.
    pub(super) fn is_boxed_vc(&self, id: ExprId, x: TypeName) -> bool {
        let ReprCtx {
            exprs,
            funcs,
            slots,
            fields,
            under,
            types,
            physical,
            ..
        } = *self;
        let is_x = |t: &Ty| t.non_null().obj_internal().is_some_and(|n| n == x);
        if physical.get(&id).is_some_and(is_x) {
            return true;
        }
        // A callee declared to return `x` itself hands back `x`'s carrier, whatever its descriptor.
        if types.declared_result(id, under).is_some_and(
            |declared| matches!(repr_of_ty(&declared, under), Repr::Unboxed(c) if c == x),
        ) {
            return false;
        }
        // An erased-top physical slot holds the box only for a generic result. A carrier that is itself
        // `Object` (`value class Box(val item: Any)`) records the same physical type once this pass has
        // unboxed that result, so the recorded unbox of `x` decides. A property declared as `x`
        // (`val boxed: Boxed<String>`) returns the carrier, which a generic `x` erases to `Object`.
        if types.get(&id).is_some_and(is_x)
            && physical.get(&id).is_some_and(|ty| ty.is_erased_top())
            && types.declared_value_class(id, under) != Some(x)
            && !self.declares_unboxed_property(id, x)
            && self.carrier_unboxes.get(&id) != Some(&x)
        {
            return true;
        }
        match &exprs[id as usize] {
            // A local/param slot whose declared type is a boxed value class `x` (a nullable `X?`, e.g. the
            // `?.` receiver temp) holds a boxed `x` — so a `.field` on it `unbox-impl`s.
            IrExpr::GetValue(i) => {
                matches!(slots.get(i).map(|t| repr_of_ty(t, under)), Some(Repr::Boxed(c)) if c == x)
            }
            IrExpr::GetField { class, index, .. } => fields
                .get(*class as usize)
                .and_then(|fs| fs.get(*index as usize))
                .is_some_and(|t| matches!(repr_of_ty(t, under), Repr::Boxed(c) if c == x)),
            IrExpr::PropertyRead { ty, .. } => {
                if self.declares_unboxed_property(id, x) {
                    false
                } else {
                    (is_x(ty) && physical.get(&id).is_some_and(|ty| ty.is_erased_top()))
                        || matches!(repr_of_ty(ty, under), Repr::Boxed(c) if c == x)
                }
            }
            IrExpr::Call {
                callee: Callee::Static { owner, name, .. },
                ..
            } if *owner == x && name == "box-impl" => true,
            IrExpr::Call { callee, .. } if callee.source_function().is_some() => funcs
                .get(
                    callee
                        .source_function()
                        .expect("guarded same-file function call") as usize,
                )
                .is_some_and(|function| is_x(&function.ret)),
            // A cross-file call returning the value class `x` (or `x?`) hands back a boxed `x` — the sibling
            // facade/owner exposes the boxed wrapper across the file boundary (like a classpath member). So a
            // nullable-VC-return tail that is such a call is already boxed and must not be re-boxed.
            IrExpr::Call {
                callee: Callee::CrossFile { ret, .. },
                ..
            } => is_x(ret),
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        params: Some((_, ret)),
                        ..
                    },
                ..
            } => is_x(ret),
            // A function-value invocation (`fn.invoke(..)`) whose logical return is a value class `x`: the
            // generated `Function{N}.invoke` adapter returns a boxed `x` (the underlying `box-impl`'d back —
            // a `Function`'s reference type argument is the box), so a `.field` on the result `unbox-impl`s it.
            IrExpr::InvokeFunction { ret, .. } => is_x(ret),
            IrExpr::Call {
                callee: Callee::Static { descriptor, .. } | Callee::Virtual { descriptor, .. },
                ..
            } => descriptor_returns_class(descriptor, x),
            // A stdlib reference-array element read yields a boxed element.
            IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::ArrayGet,
                        ..
                    },
                ..
            } => true,
            // `e as X` / `e as X?` yields a boxed `X` (e.g. casting an `Any` returned by a value-class method
            // seen through a supertype) — the property access then `unbox-impl`s it. Except when the operand is
            // already an unboxed `X` (a generic value-class receiver erased to its underlying, with a no-op
            // `(X)v` self-cast common lowering inserts): there the cast is identity and the access is too.
            IrExpr::TypeOp {
                op:
                    crate::ir::IrTypeOp::Cast
                    | crate::ir::IrTypeOp::CastNonNull
                    | crate::ir::IrTypeOp::SafeCast,
                arg,
                type_operand,
            } => {
                is_x(type_operand)
                    && (self.is_boxed_vc(*arg, x)
                        || !matches!(self.repr(*arg), Repr::Unboxed(c) if c == x))
            }
            IrExpr::NotNullAssert { operand, .. } => self.is_boxed_vc(*operand, x),
            // A `when` whose non-null branch yields a boxed `x` (a nullable safe-call: `box-impl` vs `null`) is
            // a boxed `x`.
            IrExpr::When { branches } => branches.iter().any(|(_, r)| self.is_boxed_vc(*r, x)),
            // A sole-field access of a value class whose underlying is itself a boxed value class
            // (`ZN(val z: Z1?)`) reads as `ImplicitCoercion(ZN.unbox-impl(): LZ1;)` — transparently a boxed
            // `Z1`. Recurse into the coerced value so a further `.x` on it `unbox-impl`s.
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } => {
                // The coercion itself is the checked representation boundary. A non-null `X` target
                // promises the unboxed carrier after step 5, even when its operand is an erased generic
                // read such as `List<X>.get`. Treating the pre-rewrite operand as the coercion's result
                // makes a following sole-property access insert a second `unbox-impl`.
                // A surviving coercion to a boxed `X?` is one whose operand already was the box (an
                // unboxed operand was rewritten to `box-impl`), even an erased generic read.
                match target(type_operand, under) {
                    Target::UnboxedX(target) if target == x => false,
                    Target::Boxed if is_x(type_operand) => true,
                    _ => self.is_boxed_vc(*arg, x),
                }
            }
            IrExpr::Block { value: Some(v), .. } => self.is_boxed_vc(*v, x),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn descriptor_return_matches_the_interned_class_without_rendering() {
        let class = type_name("pkg/Foo$Bar");
        assert!(descriptor_returns_class("()Lpkg/Foo$Bar;", class));
        assert!(descriptor_returns_class("()[Lpkg/Foo$Bar;", class));
        assert!(!descriptor_returns_class("()I", class));
        assert!(!descriptor_returns_class("()Lpkg/Other;", class));
        assert!(!descriptor_returns_class("(Lpkg/Foo$Bar;)V", class));
        assert!(!descriptor_returns_class(
            "(Lpkg/Foo$Bar;)Lpkg/Other;",
            class
        ));
    }
}
