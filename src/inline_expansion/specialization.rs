//! Specialization of an expression copied across an inline boundary to the call's type
//! arguments.

use crate::fx_hash::HashMap;

use crate::fir::{
    FirPropertyReferenceTarget, FirPropertyTarget, FirReferenceAdaptation, ResolvedTy,
};
use crate::ir::{
    Callee, ExprId, IrCheckedArgument, IrCheckedOperation, IrCheckedSubstitution, IrExpr,
    IrIntrinsic, IrSamTarget, IrValueClassSuspendResult,
};
use crate::types::{Ty, TyKind};

/// Specialize one expression copied across an inline boundary. A local delegated-property access
/// keeps the checker-selected declaration plan: kotlinc's local-delegate helper remains the erased
/// declaration helper and is reused (or rehomed by the JVM) across call-site substitutions.
pub(crate) fn specialize_inline_copy(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) -> Option<()> {
    // Keep the declaration type of an unnamed, non-reified generic temporary as provenance. The
    // copied expression itself is still specialized semantically; a backend can use the retained
    // declaration type when choosing the temporary's physical representation.
    let declared_temporary = match ir.expr(expression) {
        IrExpr::Variable {
            ty, named: false, ..
        } => match ty.non_null().kind() {
            TyKind::TyParam(name, _) if !runtime.contains_key(name.as_str()) => Some(*ty),
            TyKind::DefinitelyNotNull(inner)
                if inner
                    .ty_param_name()
                    .is_some_and(|name| !runtime.contains_key(name)) =>
            {
                Some(*ty)
            }
            _ => None,
        },
        _ => None,
    };
    if let Some(declared) = declared_temporary {
        ir.record_inline_declared_type(expression, declared);
    }
    specialize_recorded_facts(ir, expression, bindings, runtime);
    {
        let expression = ir.exprs.get_mut(expression as usize)?;
        specialize_typed_expression(expression, bindings, runtime);
        specialize_dependency_substitutions(expression, runtime);
    }
    Some(())
}

/// `bindings` substitutes static signatures. `runtime` substitutes operations that execute at run
/// time (`is`, `as`, `as?`, `typeOf`, `T::class`). An ordinary type parameter is in `bindings` and
/// not in `runtime`, so a lambda copied for a reified sibling does not reify the erased parameter.
fn specialize_typed_expression(
    expression: &mut IrExpr,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) {
    match expression {
        IrExpr::Checked(operation) => specialize_checked_operation(operation, bindings),
        IrExpr::CallableReference(reference) => {
            if let crate::ir::IrCallableReferenceTarget::External {
                receiver: Some(receiver),
                ..
            } = &mut reference.target
            {
                specialize_ty(receiver, bindings);
            }
            specialize_ty(&mut reference.function_type, bindings);
            specialize_tys(&mut reference.declaration_parameters, bindings);
            specialize_ty(&mut reference.declaration_result, bindings);
            specialize_reference_adaptation(&mut reference.adaptation, bindings);
        }
        IrExpr::KClassLiteral { classifier, .. } => specialize_optional_ty(classifier, runtime),
        IrExpr::LocalPropertyReference(reference) => {
            specialize_ty(&mut reference.property_type, bindings)
        }
        IrExpr::LocalDelegateAccess(_) => {}
        IrExpr::Call { callee, .. } => specialize_callee(callee, bindings, runtime),
        IrExpr::TypeOp {
            op, type_operand, ..
        } => {
            let declaration_generic_target = type_operand.non_null().is_ty_param();
            specialize_ty(type_operand, runtime);
            // `as T` is checked against T's declaration bound, so an unconstrained T initially
            // admits null. Once an inline call fixes a reified T to a concrete non-null type, the
            // checked operation has non-null cast semantics at that use site. An ordinary type
            // parameter stays a type parameter: the cast remains erased.
            if declaration_generic_target
                && *op == crate::ir::IrTypeOp::Cast
                && !type_operand.non_null().is_ty_param()
                && !type_operand.is_nullable()
            {
                *op = crate::ir::IrTypeOp::CastNonNull;
            }
        }
        IrExpr::Variable {
            ty: type_operand, ..
        }
        | IrExpr::PrimitiveNeg {
            ty: type_operand, ..
        }
        | IrExpr::PropertyRead {
            ty: type_operand, ..
        }
        | IrExpr::PropertyWrite {
            ty: type_operand, ..
        }
        | IrExpr::AbsentArgument { ty: type_operand }
        | IrExpr::RefNew {
            elem: type_operand, ..
        }
        | IrExpr::RefGet {
            elem: type_operand, ..
        }
        | IrExpr::RefSet {
            elem: type_operand, ..
        }
        | IrExpr::Vararg {
            array_type: type_operand,
            ..
        }
        | IrExpr::NewArray {
            array_type: type_operand,
            ..
        } => specialize_ty(type_operand, bindings),
        IrExpr::Try {
            result: type_operand,
            catches,
            ..
        } => {
            specialize_ty(type_operand, bindings);
            // The semantic type is the only catch fact. A reified parameter stays a parameter
            // until this substitution; emission derives the JVM class from whatever `ty` is then.
            for catch in catches {
                specialize_ty(&mut catch.ty, runtime);
            }
        }
        IrExpr::New {
            ctor_params: Some(parameters),
            ..
        } => specialize_tys(parameters, bindings),
        IrExpr::InvokeFunction { params, ret, .. } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        IrExpr::PluginPlaceholder { types, .. } => specialize_tys(types, bindings),
        IrExpr::Lambda { sam: Some(sam), .. } => specialize_sam_target(sam, bindings),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::SingletonValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::ForwardedSuperArgument { .. }
        | IrExpr::SetValue { .. }
        | IrExpr::SetFrameResult { .. }
        | IrExpr::Return(_)
        | IrExpr::Block { .. }
        | IrExpr::When { .. }
        | IrExpr::While { .. }
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::Equality { .. }
        | IrExpr::StringConcat(_)
        | IrExpr::EnclosingInstance { .. }
        | IrExpr::GetField { .. }
        | IrExpr::LateinitInitialized { .. }
        | IrExpr::BottomValue { .. }
        | IrExpr::SetField { .. }
        | IrExpr::GetStatic(_)
        | IrExpr::SetStatic { .. }
        | IrExpr::New {
            ctor_params: None, ..
        }
        | IrExpr::MethodCall { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::EnumValueOf { .. }
        | IrExpr::EnumEntries { .. }
        | IrExpr::EnumConstant { .. }
        | IrExpr::EnumAllocation { .. }
        | IrExpr::EnumEntriesList { .. }
        | IrExpr::NoSuchEnumConstant { .. }
        | IrExpr::EnumSwitchOrdinal { .. }
        | IrExpr::EnumSwitchKey { .. }
        | IrExpr::ReifiedClassMarker { .. }
        | IrExpr::ReifiedTypeOp { .. }
        | IrExpr::Lambda { sam: None, .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation
        | IrExpr::InlineFrameMarker
        | IrExpr::NotNullAssert { .. }
        | IrExpr::LateinitCheck { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::Throw { .. } => {}
    }
}

fn specialize_recorded_facts(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) {
    if let Some(ty) = ir.logical_type_mut(expression) {
        specialize_ty(ty, bindings);
    }
    if let Some(ty) = ir.physical_type_mut(expression) {
        specialize_ty(ty, bindings);
    }
    for ty in [
        ir.whens.exhaustive.get_mut(&expression),
        ir.ext_call_source_receiver.get_mut(&expression),
        ir.call_declared_ret.get_mut(&expression),
        ir.suspend_calls.get_mut(&expression),
    ]
    .into_iter()
    .flatten()
    {
        specialize_ty(ty, bindings);
    }
    if let Some(parameters) = ir.call_declared_params.get_mut(&expression) {
        specialize_tys(parameters, bindings);
    }
    if let Some(access) = ir.module_member_accesses.get_mut(&expression) {
        let parameters = match access {
            crate::ir::IrModuleMemberAccess::Callable {
                selected_parameters,
                ..
            }
            | crate::ir::IrModuleMemberAccess::Property {
                selected_parameters,
                ..
            } => selected_parameters,
        };
        specialize_tys(parameters, bindings);
    }
    if let IrExpr::New {
        declared_params: Some(parameters),
        ..
    } = &mut ir.exprs[expression as usize]
    {
        specialize_tys(parameters, bindings);
    }
    // A call the copied body makes binds the callee's own type parameters to types written in terms
    // of this splice's formals; the callee's formal keys stay, the bound types are specialized.
    if let Some(arguments) = ir.inline_call_type_arguments.get_mut(&expression) {
        for (_, ty) in arguments {
            specialize_ty(ty, bindings);
        }
    }
    if let Some(substitutions) = ir.reified_call_subst.get_mut(&expression) {
        for (_, ty) in substitutions {
            specialize_ty(ty, runtime);
        }
    }
    if let Some(construction) = ir.annotation_constructions.get_mut(&expression) {
        for (_, ty) in &mut construction.members {
            specialize_ty(ty, bindings);
        }
    }
    if let Some(result) = ir.value_class_suspend_calls.get_mut(&expression) {
        match result {
            IrValueClassSuspendResult::Boxed { carrier, .. }
            | IrValueClassSuspendResult::Carrier { carrier, .. } => {
                specialize_ty(carrier, bindings)
            }
        }
    }
    if let Some(point) = ir.intrinsic_suspension_points.get_mut(&expression) {
        specialize_ty(&mut point.result, bindings);
    }
}

fn specialize_ty(ty: &mut Ty, bindings: &HashMap<String, Ty>) {
    *ty = crate::types::ty_subst_keep_unbound(*ty, bindings).canonical_semantic();
}

fn specialize_optional_ty(ty: &mut Option<Ty>, bindings: &HashMap<String, Ty>) {
    if let Some(ty) = ty {
        specialize_ty(ty, bindings);
    }
}

fn specialize_tys(types: &mut [Ty], bindings: &HashMap<String, Ty>) {
    for ty in types {
        specialize_ty(ty, bindings);
    }
}

fn specialize_resolved_ty(ty: &mut ResolvedTy, bindings: &HashMap<String, Ty>) {
    *ty = ResolvedTy::new(
        crate::types::ty_subst_keep_unbound(ty.get(), bindings).canonical_semantic(),
    )
    .expect("inline specialization preserves resolved types");
}

fn specialize_resolved_tys(types: &mut [ResolvedTy], bindings: &HashMap<String, Ty>) {
    for ty in types {
        specialize_resolved_ty(ty, bindings);
    }
}

fn specialize_checked_substitution(
    substitution: &mut IrCheckedSubstitution,
    bindings: &HashMap<String, Ty>,
) {
    specialize_ty(&mut substitution.value, bindings);
    specialize_ty(&mut substitution.reified_runtime, bindings);
    specialize_tys(&mut substitution.additional_bounds, bindings);
}

fn specialize_checked_argument(argument: &mut IrCheckedArgument, bindings: &HashMap<String, Ty>) {
    if let IrCheckedArgument::Vararg { array_type, .. } = argument {
        specialize_ty(array_type, bindings);
    }
}

fn specialize_intrinsic(operation: &mut IrIntrinsic, bindings: &HashMap<String, Ty>) {
    match operation {
        IrIntrinsic::PrimitiveCompare { operand, .. }
        | IrIntrinsic::UnsignedCompare { operand }
        | IrIntrinsic::UnsignedToString { source: operand }
        | IrIntrinsic::PrimitiveArrayNew { element: operand }
        | IrIntrinsic::EnumValueOf {
            classifier: operand,
        }
        | IrIntrinsic::EnumValues {
            classifier: operand,
        }
        | IrIntrinsic::EnumEntries {
            classifier: operand,
        }
        | IrIntrinsic::GeneratedPropertyEquals { ty: operand }
        | IrIntrinsic::Ieee754Equals { operand }
        | IrIntrinsic::GeneratedPropertyHash { ty: operand }
        | IrIntrinsic::GeneratedPropertyArrayToString { ty: operand }
        | IrIntrinsic::TypeOf { ty: operand } => specialize_ty(operand, bindings),
        IrIntrinsic::Assert { .. }
        | IrIntrinsic::ArrayGet
        | IrIntrinsic::ArraySet
        | IrIntrinsic::ArraySize
        | IrIntrinsic::StringGet
        | IrIntrinsic::StringLength
        | IrIntrinsic::EnumOrdinal
        | IrIntrinsic::EnumName
        | IrIntrinsic::NullableAnyToString
        | IrIntrinsic::CoroutineContext => {}
    }
}

fn specialize_callee(
    callee: &mut Callee,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) {
    match callee {
        Callee::Intrinsic { operation, ret } => {
            specialize_intrinsic(operation, runtime);
            specialize_ty(ret, bindings);
        }
        Callee::CrossFile { params, ret, .. }
        | Callee::Module { params, ret, .. }
        | Callee::Super { params, ret, .. } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::ModuleConstructorDefaultEntry {
            outer, params, ret, ..
        } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
            if let Some(outer) = outer {
                specialize_ty(outer, bindings);
            }
        }
        Callee::ModuleWithDefaults {
            params,
            ret,
            dispatch_receiver_ty,
            ..
        }
        | Callee::ModuleDefaultEntry {
            params,
            ret,
            dispatch_receiver_ty,
            ..
        } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
            if let Some(receiver) = dispatch_receiver_ty {
                specialize_ty(receiver, bindings);
            }
        }
        // The dependency's own substitutions are specialized separately, by the reified
        // arguments alone (`specialize_dependency_substitutions`).
        Callee::External { params, ret, .. } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::Virtual {
            params: Some((params, ret)),
            ..
        } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::Local(_)
        | Callee::LocalWithDefaults { .. }
        | Callee::ClassStatic { .. }
        | Callee::ClassStaticWithDefaults { .. }
        | Callee::ClassStaticDefault { .. }
        | Callee::LocalDefault(_)
        | Callee::Static { .. }
        | Callee::Virtual { params: None, .. }
        | Callee::ValueClassAdapter { .. }
        | Callee::Special { .. } => {}
    }
}

/// Specialize the type arguments a dependency call selected. Only these reach a dependency's
/// reified operations, which see the reified arguments of the expanded declaration and nothing
/// else.
fn specialize_dependency_substitutions(
    expression: &mut IrExpr,
    reified_bindings: &HashMap<String, Ty>,
) {
    if let IrExpr::Call {
        callee: call_callee,
        ..
    } = expression
    {
        if let Callee::External { substitutions, .. } = &mut **call_callee {
            for substitution in substitutions {
                specialize_checked_substitution(substitution, reified_bindings);
            }
        }
    }
}

fn specialize_sam_target(target: &mut IrSamTarget, bindings: &HashMap<String, Ty>) {
    specialize_tys(&mut target.parameters, bindings);
    specialize_ty(&mut target.result, bindings);
    specialize_tys(&mut target.declared_parameters, bindings);
    specialize_ty(&mut target.declared_result, bindings);
    for frame in target
        .dispatch_methods
        .iter_mut()
        .chain(&mut target.default_methods)
    {
        specialize_tys(&mut frame.parameters, bindings);
        specialize_ty(&mut frame.result, bindings);
    }
}

fn specialize_reference_adaptation(
    adaptation: &mut Option<Box<FirReferenceAdaptation>>,
    bindings: &HashMap<String, Ty>,
) {
    let Some(adaptation) = adaptation else {
        return;
    };
    specialize_resolved_tys(&mut adaptation.parameter_types, bindings);
    specialize_resolved_ty(&mut adaptation.result_type, bindings);
}

fn specialize_property_target(target: &mut FirPropertyTarget, bindings: &HashMap<String, Ty>) {
    if let FirPropertyTarget::External {
        receiver,
        parameters,
        result,
        ..
    } = target
    {
        if let Some(receiver) = receiver {
            specialize_resolved_ty(receiver, bindings);
        }
        specialize_resolved_tys(parameters, bindings);
        specialize_resolved_ty(result, bindings);
    }
}

fn specialize_property_reference_target(
    target: &mut FirPropertyReferenceTarget,
    bindings: &HashMap<String, Ty>,
) {
    match target {
        FirPropertyReferenceTarget::Module(_) => {}
        FirPropertyReferenceTarget::SpecializedModule {
            receiver,
            property_type,
            getter_inline_splice,
            ..
        } => {
            if let Some(receiver) = receiver {
                specialize_resolved_ty(receiver, bindings);
            }
            specialize_resolved_ty(property_type, bindings);
            if let Some(splice) = getter_inline_splice {
                for substitution in &mut splice.substitutions {
                    specialize_resolved_ty(&mut substitution.value, bindings);
                }
            }
        }
        FirPropertyReferenceTarget::Classifier { property_type, .. } => {
            specialize_resolved_ty(property_type, bindings)
        }
        FirPropertyReferenceTarget::External {
            reflection_owner,
            getter,
            setter,
            property_type,
            ..
        } => {
            if let Some(owner) = reflection_owner {
                specialize_resolved_ty(owner, bindings);
            }
            specialize_property_target(getter, bindings);
            if let Some(setter) = setter {
                specialize_property_target(setter, bindings);
            }
            specialize_resolved_ty(property_type, bindings);
        }
    }
}

fn specialize_checked_operation(
    operation: &mut IrCheckedOperation,
    bindings: &HashMap<String, Ty>,
) {
    match operation {
        IrCheckedOperation::Call {
            arguments,
            substitutions,
            ..
        } => {
            for argument in arguments {
                specialize_checked_argument(argument, bindings);
            }
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        IrCheckedOperation::ConstructorDelegation {
            target,
            outer_parameter,
            capture_arguments,
            arguments,
            substitutions,
            ..
        } => {
            // The declared parameters name the dependency constructor's own formals, which an
            // inline call's bindings never substitute.
            if let crate::ir::IrCheckedConstructorTarget::External { parameters, .. } = target {
                specialize_tys(parameters, bindings);
            }
            specialize_optional_ty(outer_parameter, bindings);
            for (_, ty) in capture_arguments.iter_mut().flatten() {
                specialize_ty(ty, bindings);
            }
            for argument in arguments {
                specialize_checked_argument(argument, bindings);
            }
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => {
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        IrCheckedOperation::ExternalPropertyRead {
            parameters,
            result,
            substitutions,
            source_receiver,
            ..
        }
        | IrCheckedOperation::ExternalPropertyWrite {
            parameters,
            result,
            substitutions,
            source_receiver,
            ..
        } => {
            specialize_tys(parameters, bindings);
            specialize_ty(result, bindings);
            specialize_optional_ty(source_receiver, bindings);
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        IrCheckedOperation::RangeConstruction {
            start_type,
            end_type,
            result,
            ..
        } => {
            specialize_ty(start_type, bindings);
            specialize_ty(end_type, bindings);
            specialize_ty(result, bindings);
        }
        IrCheckedOperation::RangeContains { counter, .. }
        | IrCheckedOperation::RangeLoop { counter, .. } => specialize_ty(counter, bindings),
        IrCheckedOperation::IllegalProgressionStep { .. }
        | IrCheckedOperation::AtomicField { .. }
        | IrCheckedOperation::AtomicArrayElement { .. }
        | IrCheckedOperation::ComparableRangeContains { .. } => {}
        IrCheckedOperation::PropertyReference {
            target,
            substitutions,
            adaptation,
            function_type,
            ..
        } => {
            if let Some(function_type) = function_type {
                specialize_ty(function_type, bindings);
            }
            specialize_property_reference_target(target, bindings);
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
            specialize_reference_adaptation(adaptation, bindings);
        }
        IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. } => {}
    }
}
