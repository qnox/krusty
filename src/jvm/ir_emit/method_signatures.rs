//! Which generic `Signature` attribute kotlinc writes for a method: the semantic shape that applies
//! to one emitted function (its own type parameters, the `suspend` CPS shape, a value-class
//! realization, or a parameterized concrete position), and whether the attribute is written given
//! its access word and declaration identity. The access word itself is computed by `method_access`.

use super::signature_formatter::MethodSignatureParts;
use super::*;
use crate::jvm::classfile::ACC_SYNTHETIC;

/// Whether a method keeps its generic `Signature` even when it is synthetic. kotlinc
/// (`FunctionCodegen.needsGenericSignature`) keeps it on an `inline` function, whose generics the
/// inliner reads, and on a suspend function's `$suspendImpl` static.
pub(super) fn keeps_signature_when_synthetic(ir: &IrFile, fid: u32) -> bool {
    ir.inline_fns.contains(&fid) || ir.jvm_suspend_impl_bodies.contains_key(&fid)
}

/// The generic `Signature` kotlinc writes for a method. It writes none on a synthetic method
/// unless `keeps_when_synthetic` says otherwise, and none that spells the erased descriptor back:
/// such an attribute says nothing the descriptor does not.
pub(super) fn written_signature(
    access: u16,
    keeps_when_synthetic: bool,
    descriptor: &str,
    signature: Option<String>,
) -> Option<String> {
    signature.filter(|signature| {
        (keeps_when_synthetic || access & ACC_SYNTHETIC == 0) && signature != descriptor
    })
}

/// A method's generic `Signature`, from whichever source applies: its own declared type parameters, the
/// `suspend` CPS shape, or a parameterized concrete parameter/return type. `None` when erasure loses
/// nothing. Shared by concrete and abstract emission — an abstract method has no body, which is not a
/// reason to drop its signature.
/// `Signature` for a member whose SEMANTIC types mention enclosing-class type parameters: each
/// position is a bare `T<name>;` reference or a plain non-generic descriptor. `None` when any
/// position needs deeper generic formatting (those flow through the ordinary formatters).
pub(super) fn method_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    fid: u32,
    f: &crate::ir::IrFunction,
) -> Option<String> {
    let signature = method_signature_parts(formatter, ir, fid, f)?.render();
    // A `Signature` attribute exists to say what the DESCRIPTOR cannot — a type variable, a type
    // argument, a wildcard. One that spells the descriptor back carries nothing, and kotlinc omits it:
    // `fun a(x: Array<String>)` signs `([Ljava/lang/String;)V`, which is already its descriptor, so
    // the attribute is absent. (Only equality is tested, so a shape whose descriptor is computed
    // differently — suspend, value-class mangling — simply keeps its attribute.)
    (signature != ir_method_desc(&f.params, &f.ret)).then_some(signature)
}

/// [`method_signature`] in its grammar's parts, before the descriptor comparison: the shape of the
/// method's whole frame, so a realization standing for it with fewer leading parameters (a value
/// class's interface entry) drops those elements.
pub(super) fn method_signature_parts(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    fid: u32,
    f: &crate::ir::IrFunction,
) -> Option<MethodSignatureParts> {
    // Pick the semantic signature category first. Formatting returns `None` both when no optional
    // Signature attribute is needed and when the selected shape is invalid, so chaining formatters
    // with `or_else` would incorrectly treat an encoding error as permission to try a less complete
    // shape. `JvmSignatureFormatter` records a precise emit error for the latter case.
    let vararg = ir.vararg_parameter_position(fid);
    if let (Some(generic), Some((_, declared_ret))) =
        (ir.signatures.get(&fid), ir.suspend_declared_sigs.get(&fid))
    {
        let ret = generic.ret.as_ref().unwrap_or(declared_ret);
        return suspend_generic_method_sig(formatter, generic, ret, vararg);
    }
    if let Some(generic) = ir.jvm_value_class_member_signatures.get(&fid) {
        // The recorded frame is the static's own parameter list, a moved receiver included, so
        // the vararg's position among its parameters is its position in the frame.
        return formatter.method_signature_parts(generic, f, vararg);
    }
    if let Some(generic) = ir.signatures.get(&fid) {
        let generic = value_class_signatures::physical_generic_signature(ir, fid, generic);
        return formatter.method_signature_parts(&generic, f, vararg);
    }
    if let (Some((params, ret)), Some(_)) = (
        ir.member_semantic_sigs.get(&fid),
        ir.suspend_declared_sigs.get(&fid),
    ) {
        return suspend_method_sig(formatter, params, ret, vararg);
    }
    if let Some((params, ret)) = value_class_signatures::physical_member_signature(ir, fid) {
        // A member using ENCLOSING-CLASS type parameters declares none: they belong to the class
        // header's own signature (`(TT;)TT;`).
        return formatter.method_positions_parts(&params, &ret, vararg);
    }
    if let Some((params, declared_ret)) = ir.suspend_declared_sigs.get(&fid) {
        // A value-class RETURN survives in the continuation's type argument as the value class
        // itself (`Continuation<? super OrganizationId>`), not its erasure. The VC pass ran
        // before the suspend pass and already erased `declared_ret` to the underlying, so recover the
        // declared return from `vc_declared_sigs` when this function had one.
        let ret = ir
            .vc_declared_sigs
            .get(&fid)
            .map(|(_, _, value_class_ret)| value_class_ret)
            .unwrap_or(declared_ret);
        return suspend_method_sig(formatter, params, ret, vararg);
    }
    if let Some((params, ret)) = value_class_signatures::declared_value_class_signature(ir, fid) {
        return formatter.method_positions_parts(&params, &ret, vararg);
    }
    method_parameterized_parts(
        formatter,
        &signature_function_params(ir, fid),
        &f.ret,
        vararg,
    )
}

fn suspend_generic_method_sig(
    formatter: &JvmSignatureFormatter<'_>,
    generic: &crate::ir::IrGenericSig,
    ret: &Ty,
    vararg: Option<usize>,
) -> Option<MethodSignatureParts> {
    let type_parameters = jvm_type_params(formatter, generic)?;
    Some(MethodSignatureParts {
        type_parameters,
        ..suspend_method_sig(formatter, &generic.params, ret, vararg)?
    })
}

/// The generic `Signature` of a `suspend fun`'s CPS method. The declared return type survives only in
/// the trailing continuation's type argument — `suspend fun f(a: String)` compiles to
/// `f(String, Continuation): Object` but signs as
/// `(Ljava/lang/String;Lkotlin/coroutines/Continuation<-Lkotlin/Unit;>;)Ljava/lang/Object;`, the `-`
/// being `? super`. Takes the DECLARED parameters and return, not the rewritten ones.
fn suspend_method_sig(
    formatter: &JvmSignatureFormatter<'_>,
    params: &[Ty],
    ret: &Ty,
    vararg: Option<usize>,
) -> Option<MethodSignatureParts> {
    // The suspend return travels as `Continuation<-RET>`, a PARAMETER of the erased method, and
    // kotlinc wildcards inside it: `suspend fun <U> f(): Cont<U>` signs
    // `(Lkotlin/coroutines/Continuation<-LCont<+TU;>;>;)Ljava/lang/Object;`.
    let continuation = formatter.continuation(*ret, Wildcards::Declared)?;
    let mut parameters = formatter.parameter_parts(params, vararg)?;
    parameters.push(continuation);
    Some(MethodSignatureParts {
        type_parameters: String::new(),
        parameters,
        result: "Ljava/lang/Object;".to_string(),
    })
}

/// A method's generic `Signature` when a concrete param/return type is parameterized (`getXs()` →
/// `()Ljava/util/List<Ljava/lang/String;>;`); non-generic positions keep their erased descriptor,
/// `None` when none are parameterized.
pub(super) fn method_parameterized_sig(
    formatter: &JvmSignatureFormatter<'_>,
    params: &[Ty],
    ret: &Ty,
    vararg: Option<usize>,
) -> Option<String> {
    method_parameterized_parts(formatter, params, ret, vararg).map(|parts| parts.render())
}

/// [`method_parameterized_sig`] in its grammar's parts.
fn method_parameterized_parts(
    formatter: &JvmSignatureFormatter<'_>,
    params: &[Ty],
    ret: &Ty,
    vararg: Option<usize>,
) -> Option<MethodSignatureParts> {
    // Runs for every emitted method — bail before building any string when no position can carry one.
    let is_parameterized = |t: &Ty| {
        let inner = t.non_null();
        matches!(inner.kind(), TyKind::Fun(_) | TyKind::Intersection(_))
            || matches!(inner.kind(), TyKind::Obj(_, arguments) if !arguments.is_empty())
    };
    if !params
        .iter()
        .chain(std::iter::once(ret))
        .any(is_parameterized)
    {
        return None;
    }
    // A `Unit` parameter is the `kotlin.Unit` object, not `void`.
    let parts = formatter.method_positions_parts(params, ret, vararg)?;
    (parts.render() != ir_method_desc(params, ret)).then_some(parts)
}
