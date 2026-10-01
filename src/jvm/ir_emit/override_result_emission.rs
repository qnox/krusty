//! Declarations of, and calls to, an override whose primitive result the JVM declares as its
//! wrapper.
//!
//! [`crate::jvm::override_results`] chooses the wrapper as such an override's JVM result; common IR
//! keeps the Kotlin result. The declaration names the wrapper in its descriptor and `Signature` and
//! boxes what it returns. A call names the wrapper in its descriptor and, where the value is used,
//! reads the primitive out of it, as kotlinc's codegen coerces a call's physical result to the type
//! its consumer wants; a reference consumer takes the wrapper as it is, and a discarded call pops it.

use super::inline_call::parse_descriptor_params;
use super::scalar_coercion::{box_prim_free, unbox_prim_from};
use super::signature_formatter::JvmSignatureFormatter;
use super::{
    debug_lines, discard, ir_method_desc, ir_ty_to_jvm, jvm_declared_ty, jvm_is_erased_top,
    method_descriptor, method_signature, type_descriptor, CodeBuilder, Emitter,
};
use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp};
use crate::jvm::override_results::OverrideResults;
use crate::types::Ty;

const NOT_NULL: &str = "Lorg/jetbrains/annotations/NotNull;";

/// The declaration annotation for a result after the boxed-override representation is selected.
///
/// Common IR keeps the non-null primitive Kotlin result, which ordinarily has no JVM nullability
/// annotation. When [`OverrideResults`] gives that declaration a wrapper result, the wrapper is a
/// non-null reference at the class-file boundary and kotlinc publishes `@NotNull` on it.
pub(super) fn declared_nullability(
    ir: &IrFile,
    override_results: &OverrideResults,
    function: u32,
) -> super::declared_nullability::DeclaredNullability {
    let mut declared = super::declared_nullability::declared_nullability(ir, function);
    if override_results.boxes(function) {
        declared.result = Some(NOT_NULL);
    }
    declared
}

impl Emitter<'_> {
    /// The primitive Kotlin result of call `e`, when its callee's JVM result is the wrapper.
    fn boxed_call_result(&self, e: ExprId) -> Option<Ty> {
        self.override_results.boxed_call_result(self.ir, e)
    }

    /// The JVM result call `e` reads: the wrapper its callee is declared with, or `result`.
    pub(super) fn physical_call_result(&self, e: ExprId, result: Ty) -> Ty {
        match self.boxed_call_result(e) {
            Some(primitive) => jvm_declared_ty(&Ty::nullable(primitive)),
            None => result,
        }
    }

    /// `descriptor` of call `e`, naming the wrapper result its callee is declared with.
    pub(super) fn physical_call_descriptor(&self, e: ExprId, descriptor: &str) -> String {
        let Some(primitive) = self.boxed_call_result(e) else {
            return descriptor.to_string();
        };
        let parameters = parse_descriptor_params(descriptor)
            .unwrap_or_else(|| panic!("a call descriptor is valid: {descriptor}"));
        method_descriptor(&parameters, jvm_declared_ty(&Ty::nullable(primitive)))
    }

    /// Emit `node`, the expression `e`, leaving the value of its Kotlin type: a call whose callee
    /// returns the wrapper of its primitive result is unboxed.
    pub(super) fn emit_value_node(&mut self, e: ExprId, node: &IrExpr, code: &mut CodeBuilder) {
        self.emit_physical_value_node(e, node, code);
        if let Some(primitive) = self.boxed_call_result(e) {
            let wrapper = jvm_declared_ty(&Ty::nullable(primitive));
            unbox_prim_from(self.cw, code, wrapper, primitive);
        }
    }

    /// Emit `node`, the discarded expression `e`, when it is a call whose callee returns the
    /// wrapper of its primitive result: kotlinc pops the wrapper without unboxing it. `false`,
    /// having emitted nothing, for anything else.
    pub(super) fn emit_discarded_boxed_call(
        &mut self,
        e: ExprId,
        node: &IrExpr,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(primitive) = self.boxed_call_result(e) else {
            return false;
        };
        self.emit_physical_value_node(e, node, code);
        discard(jvm_declared_ty(&Ty::nullable(primitive)), code);
        true
    }

    /// Emit `value` as the result of the function being emitted, declared on the JVM to return
    /// `ret`. Common IR returns the Kotlin result; an override whose primitive result the JVM
    /// declares as its wrapper boxes it here, once, as kotlinc's body does.
    pub(super) fn emit_returned_value(&mut self, value: ExprId, ret: Ty, code: &mut CodeBuilder) {
        let returned = self.value_ty(value);
        if returned.is_jvm_scalar()
            && !returned.is_unsigned()
            && ret == jvm_declared_ty(&Ty::nullable(returned))
        {
            if self.emit_boxed_boundary_return(value, returned, ret, code) {
                return;
            }
            self.emit_value(value, code);
            box_prim_free(self.cw, code, returned);
        } else {
            self.emit_value_as(value, ret, code);
        }
    }

    /// Return the reference slot under a checked declaration-result coercion directly from a
    /// boxed-result override. A generic delegate may produce `Object`, so narrow it to the wrapper;
    /// a dependency override already produces that wrapper and needs no unbox/box round trip.
    fn emit_boxed_boundary_return(
        &mut self,
        value: ExprId,
        primitive: Ty,
        wrapper: Ty,
        code: &mut CodeBuilder,
    ) -> bool {
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = self.ir.expr(value)
        else {
            return false;
        };
        if type_operand.non_null() != primitive.non_null() {
            return false;
        }
        let arg = *arg;
        let Some(physical) = self.ir.physical_types.get(&arg).copied() else {
            return false;
        };
        let physical_jvm = ir_ty_to_jvm(&physical);
        let wrapper_jvm = ir_ty_to_jvm(&wrapper);
        if !physical_jvm.is_reference()
            || (physical_jvm != wrapper_jvm && !jvm_is_erased_top(physical_jvm))
        {
            return false;
        }
        self.emit_value(arg, code);
        self.narrow_on_stack(physical, wrapper, code);
        true
    }

    /// Emit `e` for a consumer that takes a reference, when it is a call whose callee returns the
    /// wrapper of its primitive result: the wrapper is the reference, so nothing unboxes it only to
    /// box it again. The wrapper's type, or `None`, having emitted nothing, for anything else.
    pub(super) fn emit_boxed_call_reference(
        &mut self,
        e: ExprId,
        code: &mut CodeBuilder,
    ) -> Option<Ty> {
        let primitive = self.boxed_call_result(e)?;
        let node = self.ir.expr(e).clone();
        debug_lines::mark_expression_start(self.ir, e, code);
        self.emit_physical_value_node(e, &node, code);
        Some(jvm_declared_ty(&Ty::nullable(primitive)))
    }
}

/// The descriptor the method declared for `function` has: its result is the wrapper where
/// `override_results` boxes it.
pub(super) fn declared_method_desc(
    ir: &IrFile,
    override_results: &OverrideResults,
    function: u32,
) -> String {
    ir_method_desc(
        &ir.functions[function as usize].params,
        &override_results.physical_result(ir, function),
    )
}

/// The generic `Signature` of the method declared for `function`, when it needs one: its result is
/// the wrapper where `override_results` boxes it. A boxed result is a primitive, so the signature
/// spells it exactly as the descriptor does.
pub(super) fn declared_method_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    override_results: &OverrideResults,
    function: u32,
) -> Option<String> {
    let declaration = &ir.functions[function as usize];
    let signature = method_signature(formatter, ir, function, declaration)?;
    if !override_results.boxes(function) {
        return Some(signature);
    }
    let primitive = type_descriptor(jvm_declared_ty(&declaration.ret));
    let parameters = signature
        .strip_suffix(primitive.as_str())
        .expect("a boxed override result is signed as its primitive");
    let wrapper = type_descriptor(jvm_declared_ty(&Ty::nullable(declaration.ret)));
    Some(format!("{parameters}{wrapper}"))
}
