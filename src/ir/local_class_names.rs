//! Backend-neutral local-class naming contracts retained by common IR.

mod reachable;

use std::collections::{HashMap, HashSet};

use crate::types::{Ty, TypeName};

/// A lambda implementation split from a shared source implementation while remapping one
/// reachable inline expansion. The lowering that owns that expansion attaches its call-site
/// specialization record before a backend assigns any class identity.
pub(crate) struct DetachedLambdaImplementation {
    pub(crate) source: FunId,
    pub(crate) target: FunId,
    pub(crate) parent: Option<FunId>,
    pub(crate) order: ExprId,
    /// Whether the detached edge is still a lambda value that may materialize as a class. A direct
    /// call to a consumed implementation is specialized for ownership/signature purposes but must
    /// not consume a class ordinal merely because it retains the source lambda identity.
    pub(crate) class_site: bool,
}

use super::{
    Callee, ClassId, EnclosingDeclaration, ExprId, FunId, IrCallableReferenceTarget,
    IrCheckedArgument, IrCheckedOperation, IrExpr,
};

/// The classifier that lexically owns a local classifier's executable context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IrLocalClassOwner {
    /// A classifier realized in this IR file.
    Class(ClassId),
    /// A classifier declared outside executable code in another source, reached when this file
    /// realizes an inline payload's local classifier. Its identity is already its physical name.
    External(TypeName),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IrLocalClassNameProvenance {
    /// Source file whose facade roots the name when there is no lexical owner. An inline payload
    /// classifier keeps its declaring source's facade, not the file that realizes the payload.
    pub source: super::IrModuleSource,
    /// Exact source classifier that lexically owns this executable context, or the file facade.
    pub lexical_owner: Option<IrLocalClassOwner>,
    /// Source declaration identities below the owner (callable/property/local/class names).
    pub segments: Box<[String]>,
    /// Shared generated-artifact position for an unnamed classifier.
    pub ordinal: Option<u32>,
    /// The declarations the classifier is nested in below `lexical_owner`, outermost first.
    pub parents: Box<[EnclosingDeclaration]>,
}

fn remapped_name(mut value: TypeName, names: &HashMap<TypeName, TypeName>) -> TypeName {
    for _ in 0..names.len() {
        let Some(next) = names.get(&value).copied() else {
            return value;
        };
        if next == value {
            return value;
        }
        value = next;
    }
    assert!(
        !names.contains_key(&value),
        "classifier identity remap contains a cycle"
    );
    value
}

fn name(name: &mut TypeName, names: &HashMap<TypeName, TypeName>) {
    *name = remapped_name(*name, names);
}

/// A captured enclosing instance names its class by identity, which follows the class's rename.
fn captured_receiver(
    receiver: &mut super::IrCapturedReceiver,
    names: &HashMap<TypeName, TypeName>,
) {
    match receiver {
        super::IrCapturedReceiver::Enclosing { classifier } => name(classifier, names),
        super::IrCapturedReceiver::Context { types, .. } => tys(types, names),
        super::IrCapturedReceiver::Callable { .. } | super::IrCapturedReceiver::Lambda(_) => {}
    }
}

fn ty(value: Ty, names: &HashMap<TypeName, TypeName>) -> Ty {
    match value {
        Ty::Obj(classifier, arguments) => Ty::obj_args_name(
            remapped_name(classifier, names),
            &arguments
                .iter()
                .map(|argument| ty(*argument, names))
                .collect::<Vec<_>>(),
        ),
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| ty(*parameter, names))
                .collect(),
            ty(signature.ret, names),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Nullable(inner) => Ty::nullable(ty(*inner, names)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(ty(*inner, names)),
        Ty::InProjection(inner) => Ty::in_projection(ty(*inner, names)),
        Ty::OutProjection(inner) => Ty::out_projection(ty(*inner, names)),
        Ty::StarProjection(inner) => Ty::star_projection(ty(*inner, names)),
        Ty::TyParam(parameter, bound) => Ty::ty_param(parameter, ty(*bound, names)),
        Ty::DefinitelyNotNull(inner) => {
            Ty::DefinitelyNotNull(crate::types::intern_ty(ty(*inner, names)))
        }
        Ty::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| ty(*part, names))
                .collect::<Vec<_>>(),
        ),
        Ty::Unit | Ty::Null | Ty::Nothing | Ty::Error | Ty::Pending => value,
    }
}

fn tys(values: &mut [Ty], names: &HashMap<TypeName, TypeName>) {
    values
        .iter_mut()
        .for_each(|value| *value = ty(*value, names));
}

fn resolved_ty(value: &mut crate::fir::ResolvedTy, names: &HashMap<TypeName, TypeName>) {
    *value = crate::fir::ResolvedTy::new(ty(value.get(), names))
        .expect("renaming a resolved classifier preserves a publishable type");
}

fn reference_adaptation(
    adaptation: &mut crate::fir::FirReferenceAdaptation,
    names: &HashMap<TypeName, TypeName>,
) {
    adaptation
        .parameter_types
        .iter_mut()
        .for_each(|value| resolved_ty(value, names));
    resolved_ty(&mut adaptation.result_type, names);
}

fn runtime_function(function: &mut super::IrRuntimeFunction, names: &HashMap<TypeName, TypeName>) {
    tys(&mut function.parameters, names);
    function.result = ty(function.result, names);
}

fn progression_source(
    source: &mut super::IrProgressionSource,
    names: &HashMap<TypeName, TypeName>,
) {
    match source {
        super::IrProgressionSource::Step {
            nested,
            last_element,
            ..
        } => {
            progression_source(nested, names);
            runtime_function(last_element, names);
        }
        super::IrProgressionSource::Reversed(nested) => progression_source(nested, names),
        super::IrProgressionSource::Value {
            stepped: Some(stepped),
            ..
        } => progression_source(stepped, names),
        super::IrProgressionSource::Literal { .. } | super::IrProgressionSource::Value { .. } => {}
    }
}

fn checked_arguments(values: &mut [IrCheckedArgument], names: &HashMap<TypeName, TypeName>) {
    for argument in values {
        if let IrCheckedArgument::Vararg { array_type, .. } = argument {
            *array_type = ty(*array_type, names);
        }
    }
}

fn inline_accessor_splice(
    splice: &mut Option<Box<crate::fir::FirInlineAccessorSplice>>,
    names: &HashMap<TypeName, TypeName>,
) {
    if let Some(splice) = splice {
        for substitution in &mut splice.substitutions {
            resolved_ty(&mut substitution.value, names);
        }
    }
}

fn property_target(
    target: &mut crate::fir::FirPropertyTarget,
    names: &HashMap<TypeName, TypeName>,
) {
    match target {
        crate::fir::FirPropertyTarget::Module { inline_splice, .. } => {
            inline_accessor_splice(inline_splice, names);
        }
        crate::fir::FirPropertyTarget::External {
            receiver,
            parameters,
            result,
            dispatch,
            ..
        } => {
            if let Some(receiver) = receiver {
                resolved_ty(receiver, names);
            }
            for parameter in parameters {
                resolved_ty(parameter, names);
            }
            resolved_ty(result, names);
            if let crate::fir::FirPropertyDispatch::Super { owner, .. } = dispatch {
                name(owner, names);
            }
        }
    }
}

fn property_reference_target(
    target: &mut crate::fir::FirPropertyReferenceTarget,
    names: &HashMap<TypeName, TypeName>,
) {
    match target {
        crate::fir::FirPropertyReferenceTarget::Module(_) => {}
        crate::fir::FirPropertyReferenceTarget::SpecializedModule {
            reflection_owner,
            receiver,
            property_type,
            declared_receiver,
            declared_property_type,
            getter_inline_splice,
            ..
        } => {
            if let Some(owner) = reflection_owner {
                name(owner, names);
            }
            if let Some(receiver) = receiver {
                resolved_ty(receiver, names);
            }
            resolved_ty(property_type, names);
            if let Some(receiver) = declared_receiver {
                resolved_ty(receiver, names);
            }
            resolved_ty(declared_property_type, names);
            inline_accessor_splice(getter_inline_splice, names);
        }
        crate::fir::FirPropertyReferenceTarget::Classifier {
            owner,
            property_type,
            ..
        } => {
            name(owner, names);
            resolved_ty(property_type, names);
        }
        crate::fir::FirPropertyReferenceTarget::External {
            reflection_owner,
            getter,
            setter,
            property_type,
            ..
        } => {
            if let Some(owner) = reflection_owner {
                resolved_ty(owner, names);
            }
            property_target(getter, names);
            if let Some(setter) = setter {
                property_target(setter, names);
            }
            resolved_ty(property_type, names);
        }
    }
}

fn checked_operation(operation: &mut IrCheckedOperation, names: &HashMap<TypeName, TypeName>) {
    let substitutions = match operation {
        IrCheckedOperation::Call {
            arguments,
            substitutions,
            ..
        } => {
            checked_arguments(arguments, names);
            substitutions
        }
        IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => substitutions,
        IrCheckedOperation::ConstructorDelegation {
            target,
            outer_parameter,
            arguments,
            substitutions,
            ..
        } => {
            if let super::IrCheckedConstructorTarget::External {
                classifier,
                parameters,
                ..
            } = target
            {
                name(classifier, names);
                tys(parameters, names);
            }
            if let Some(outer) = outer_parameter {
                *outer = ty(*outer, names);
            }
            checked_arguments(arguments, names);
            substitutions
        }
        IrCheckedOperation::ExternalPropertyRead {
            dispatch,
            parameters,
            result,
            source_receiver,
            ..
        }
        | IrCheckedOperation::ExternalPropertyWrite {
            dispatch,
            parameters,
            result,
            source_receiver,
            ..
        } => {
            if let super::IrPropertyDispatch::Super { owner, .. } = dispatch {
                name(owner, names);
            }
            tys(parameters, names);
            *result = ty(*result, names);
            if let Some(receiver) = source_receiver {
                *receiver = ty(*receiver, names);
            }
            return;
        }
        IrCheckedOperation::RangeConstruction {
            start_type,
            end_type,
            result,
            ..
        } => {
            *start_type = ty(*start_type, names);
            *end_type = ty(*end_type, names);
            *result = ty(*result, names);
            return;
        }
        IrCheckedOperation::RangeContains { counter, .. } => {
            *counter = ty(*counter, names);
            return;
        }
        IrCheckedOperation::RangeLoop {
            counter,
            source,
            unsigned_compare,
            ..
        } => {
            *counter = ty(*counter, names);
            progression_source(source, names);
            if let Some(compare) = unsigned_compare {
                runtime_function(compare, names);
            }
            return;
        }
        IrCheckedOperation::IllegalProgressionStep { .. } => return,
        IrCheckedOperation::PropertyReference {
            target,
            substitutions,
            adaptation,
            ..
        } => {
            property_reference_target(target, names);
            if let Some(adaptation) = adaptation {
                reference_adaptation(adaptation, names);
            }
            substitutions
        }
        IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. } => return,
    };
    for substitution in substitutions {
        substitution.value = ty(substitution.value, names);
        substitution.reified_runtime = ty(substitution.reified_runtime, names);
        tys(&mut substitution.additional_bounds, names);
    }
}

fn callee(callee: &mut Callee, names: &HashMap<TypeName, TypeName>) {
    match callee {
        Callee::ClassStatic { owner, .. }
        | Callee::ClassStaticWithDefaults { owner, .. }
        | Callee::ClassStaticDefault { owner, .. }
        | Callee::Static { owner, .. }
        | Callee::Special { owner, .. } => name(owner, names),
        Callee::Intrinsic { operation, ret } => {
            *ret = ty(*ret, names);
            match operation {
                super::IrIntrinsic::EnumValueOf { classifier }
                | super::IrIntrinsic::EnumEntries { classifier }
                | super::IrIntrinsic::PrimitiveCompare {
                    operand: classifier,
                    ..
                }
                | super::IrIntrinsic::UnsignedToString { source: classifier }
                | super::IrIntrinsic::PrimitiveArrayNew {
                    element: classifier,
                }
                | super::IrIntrinsic::GeneratedPropertyEquals { ty: classifier }
                | super::IrIntrinsic::GeneratedPropertyHash { ty: classifier }
                | super::IrIntrinsic::DataClassArrayToString { ty: classifier }
                | super::IrIntrinsic::TypeOf { ty: classifier }
                | super::IrIntrinsic::Ieee754Equals {
                    operand: classifier,
                } => *classifier = ty(*classifier, names),
                super::IrIntrinsic::Assert { .. }
                | super::IrIntrinsic::ArrayGet
                | super::IrIntrinsic::ArraySet
                | super::IrIntrinsic::ArraySize
                | super::IrIntrinsic::StringGet
                | super::IrIntrinsic::StringLength
                | super::IrIntrinsic::EnumName
                | super::IrIntrinsic::NullableAnyToString
                | super::IrIntrinsic::CoroutineContext => {}
            }
        }
        Callee::CrossFile {
            facade,
            params,
            ret,
            ..
        } => {
            name(facade, names);
            tys(params, names);
            *ret = ty(*ret, names);
        }
        Callee::Module { params, ret, .. } => {
            tys(params, names);
            *ret = ty(*ret, names);
        }
        Callee::External {
            params,
            ret,
            substitutions,
            ..
        } => {
            tys(params, names);
            *ret = ty(*ret, names);
            for substitution in substitutions {
                substitution.value = ty(substitution.value, names);
                substitution.reified_runtime = ty(substitution.reified_runtime, names);
                tys(&mut substitution.additional_bounds, names);
            }
        }
        Callee::ModuleWithDefaults {
            params,
            ret,
            dispatch_receiver_ty,
            ..
        } => {
            tys(params, names);
            *ret = ty(*ret, names);
            if let Some(receiver) = dispatch_receiver_ty {
                *receiver = ty(*receiver, names);
            }
        }
        Callee::Virtual { owner, params, .. } => {
            name(owner, names);
            if let Some((parameters, result)) = params {
                tys(parameters, names);
                *result = ty(*result, names);
            }
        }
        Callee::Super {
            owner,
            dispatch_owner,
            params,
            ret,
            ..
        } => {
            name(owner, names);
            name(dispatch_owner, names);
            tys(params, names);
            *ret = ty(*ret, names);
        }
        Callee::Local(_) | Callee::LocalDefault(_) | Callee::LocalWithDefaults { .. } => {}
    }
}

fn remap_expression(expression: &mut IrExpr, names: &HashMap<TypeName, TypeName>) {
    match expression {
        IrExpr::Checked(operation) => checked_operation(operation, names),
        IrExpr::CallableReference(reference) => {
            match &mut reference.target {
                IrCallableReferenceTarget::Constructor { classifier }
                | IrCallableReferenceTarget::Classifier { classifier, .. } => {
                    name(classifier, names)
                }
                IrCallableReferenceTarget::Local {
                    owner: Some(owner), ..
                } => name(owner, names),
                IrCallableReferenceTarget::External {
                    receiver: Some(receiver),
                    ..
                } => *receiver = ty(*receiver, names),
                IrCallableReferenceTarget::Module(_)
                | IrCallableReferenceTarget::Local { owner: None, .. }
                | IrCallableReferenceTarget::External { receiver: None, .. }
                | IrCallableReferenceTarget::FunctionValueConversion { .. }
                | IrCallableReferenceTarget::FunctionInvoke => {}
            }
            reference.function_type = ty(reference.function_type, names);
            tys(&mut reference.declaration_parameters, names);
            reference.declaration_result = ty(reference.declaration_result, names);
            if let Some(adaptation) = &mut reference.adaptation {
                reference_adaptation(adaptation, names);
            }
            if let Some(owner) = &mut reference.reflection_owner {
                name(owner, names);
            }
        }
        IrExpr::ClassConst {
            internal: Some(classifier),
        }
        | IrExpr::SingletonValue { classifier }
        | IrExpr::EnumEntry { classifier, .. }
        | IrExpr::EnumValues { classifier }
        | IrExpr::EnumValueOf { classifier, .. }
        | IrExpr::EnumEntries { classifier } => name(classifier, names),
        IrExpr::LocalPropertyReference(reference) => {
            name(&mut reference.source.package, names);
            if let Some(class) = &mut reference.class {
                name(class, names);
            }
            reference.property_type = ty(reference.property_type, names);
        }
        IrExpr::LocalDelegateAccess(_) => {}
        IrExpr::KClassLiteral {
            classifier: Some(classifier),
            ..
        }
        | IrExpr::TypeOp {
            type_operand: classifier,
            ..
        }
        | IrExpr::Variable { ty: classifier, .. }
        | IrExpr::PrimitiveNeg { ty: classifier, .. }
        | IrExpr::RefNew {
            elem: classifier, ..
        }
        | IrExpr::RefGet {
            elem: classifier, ..
        }
        | IrExpr::RefSet {
            elem: classifier, ..
        }
        | IrExpr::Vararg {
            array_type: classifier,
            ..
        }
        | IrExpr::NewArray {
            array_type: classifier,
            ..
        } => *classifier = ty(*classifier, names),
        IrExpr::Call { callee: target, .. } => callee(target, names),
        IrExpr::PluginPlaceholder { data, types, .. } => {
            data.iter_mut().for_each(|value| name(value, names));
            tys(types, names);
        }
        IrExpr::PropertyRead {
            owner, ty: value, ..
        }
        | IrExpr::PropertyWrite {
            owner, ty: value, ..
        } => {
            name(owner, names);
            *value = ty(*value, names);
        }
        IrExpr::EnclosingInstance { inner, outer, .. } => {
            name(inner, names);
            name(outer, names);
        }
        IrExpr::New {
            internal,
            ctor_params,
            ..
        } => {
            name(internal, names);
            if let Some(parameters) = ctor_params {
                tys(parameters, names);
            }
        }
        IrExpr::ExternalStaticField { owner, .. }
        | IrExpr::ReifiedClassMarker { erased: owner, .. }
        | IrExpr::ReifiedTypeOp { erased: owner, .. } => name(owner, names),
        IrExpr::ExternalStaticInstance { owner, ty, .. } => {
            name(owner, names);
            name(ty, names);
        }
        IrExpr::InvokeFunction { params, ret, .. } => {
            tys(params, names);
            *ret = ty(*ret, names);
        }
        IrExpr::Lambda { sam: Some(sam), .. } => {
            name(&mut sam.classifier, names);
            tys(&mut sam.parameters, names);
            sam.result = ty(sam.result, names);
            tys(&mut sam.declared_parameters, names);
            sam.declared_result = ty(sam.declared_result, names);
            tys(&mut sam.overridden_results, names);
        }
        IrExpr::Try {
            catches, result, ..
        } => {
            *result = ty(*result, names);
            for catch in catches {
                catch.ty = ty(catch.ty, names);
            }
        }
        IrExpr::ClassConst { internal: None }
        | IrExpr::KClassLiteral {
            classifier: None, ..
        }
        | IrExpr::Lambda { sam: None, .. }
        | IrExpr::Const(_)
        | IrExpr::BottomValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::SetValue { .. }
        | IrExpr::Return(_)
        | IrExpr::Block { .. }
        | IrExpr::When { .. }
        | IrExpr::While { .. }
        | IrExpr::SetFrameResult { .. }
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::Equality { .. }
        | IrExpr::StringConcat(_)
        | IrExpr::GetField { .. }
        | IrExpr::LateinitInitialized { .. }
        | IrExpr::SetField { .. }
        | IrExpr::GetStatic(_)
        | IrExpr::SetStatic { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation
        | IrExpr::InlineFrameMarker
        | IrExpr::NotNullAssert { .. }
        | IrExpr::LateinitCheck { .. }
        | IrExpr::Throw { .. }
        | IrExpr::ForwardedSuperArgument { .. } => {}
    }
}

fn remap_expression_class_ids(expression: &mut IrExpr, classes: &HashMap<ClassId, ClassId>) {
    let remap = |class: &mut ClassId| {
        if let Some(mapped) = classes.get(class).copied() {
            *class = mapped;
        }
    };
    match expression {
        IrExpr::GetField { class, .. }
        | IrExpr::LateinitInitialized { class, .. }
        | IrExpr::SetField { class, .. }
        | IrExpr::MethodCall { class, .. } => remap(class),
        IrExpr::StaticInstance { owner, ty, .. } => {
            remap(owner);
            remap(ty);
        }
        _ => {}
    }
}

fn annotation_value(value: &mut super::AnnoValue, names: &HashMap<TypeName, TypeName>) {
    match value {
        super::AnnoValue::Enum(classifier, _) => name(classifier, names),
        super::AnnoValue::Class(classifier) => *classifier = ty(*classifier, names),
        super::AnnoValue::Annotation(annotation) => annotation_application(annotation, names),
        super::AnnoValue::Array(values) => values
            .iter_mut()
            .for_each(|value| annotation_value(value, names)),
        super::AnnoValue::Const(_) => {}
    }
}

fn annotation_application(
    annotation: &mut super::AppliedAnnotation,
    names: &HashMap<TypeName, TypeName>,
) {
    name(&mut annotation.internal, names);
    for (_, value) in &mut annotation.values {
        annotation_value(value, names);
    }
}

fn annotations(
    annotations: &mut super::DeclarationAnnotations,
    names: &HashMap<TypeName, TypeName>,
) {
    for retained in annotations.iter_mut() {
        annotation_application(&mut retained.annotation, names);
    }
}

fn type_parameters(parameters: &mut [super::IrTypeParameter], names: &HashMap<TypeName, TypeName>) {
    for parameter in parameters {
        for (bound, _) in &mut parameter.bounds {
            *bound = ty(*bound, names);
        }
    }
}

fn package_type_parameters(
    parameters: &mut [super::IrPackageTypeParameter],
    names: &HashMap<TypeName, TypeName>,
) {
    for parameter in parameters {
        tys(&mut parameter.bounds, names);
    }
}

fn generic_signature(signature: &mut super::IrGenericSig, names: &HashMap<TypeName, TypeName>) {
    type_parameters(&mut signature.type_params, names);
    tys(&mut signature.params, names);
    signature.ret = signature.ret.map(|value| ty(value, names));
    tys(&mut signature.supers, names);
}

fn spelled(spelling: &mut crate::spelling::Spelled, names: &HashMap<TypeName, TypeName>) {
    spelling
        .alias
        .iter_mut()
        .for_each(|value| name(value, names));
    for (value, spelling) in &mut spelling.alias_args {
        *value = ty(*value, names);
        spelled(spelling, names);
    }
    spelling
        .args
        .iter_mut()
        .for_each(|value| spelled(value, names));
}

fn declared_spellings(
    spellings: &mut crate::spelling::DeclaredSpellings,
    names: &HashMap<TypeName, TypeName>,
) {
    spelled(&mut spellings.ret, names);
    spellings
        .params
        .iter_mut()
        .for_each(|value| spelled(value, names));
    spelled(&mut spellings.receiver, names);
    spellings
        .type_param_bounds
        .iter_mut()
        .flatten()
        .for_each(|value| spelled(value, names));
    spelled(&mut spellings.superclass, names);
    spellings
        .supertypes
        .iter_mut()
        .for_each(|value| spelled(value, names));
}

fn type_alias(alias: &mut super::IrTypeAlias, names: &HashMap<TypeName, TypeName>) {
    alias.expansion = ty(alias.expansion, names);
    spelled(&mut alias.expansion_spelling, names);
}

fn local_property_layout(
    layout: &mut super::IrLocalPropertyLayout,
    names: &HashMap<TypeName, TypeName>,
) {
    match layout {
        super::IrLocalPropertyLayout::TopLevelStorage { qualifier, .. } => {
            qualifier.iter_mut().for_each(|value| name(value, names));
        }
        super::IrLocalPropertyLayout::TopLevelAccessor {
            receiver,
            context_parameters,
            ..
        } => {
            *receiver = receiver.map(|value| ty(value, names));
            tys(context_parameters, names);
        }
        super::IrLocalPropertyLayout::Member {
            owner,
            ty: value,
            context_parameters,
            ..
        } => {
            name(owner, names);
            *value = ty(*value, names);
            tys(context_parameters, names);
        }
        super::IrLocalPropertyLayout::MemberExtension {
            owner,
            receiver,
            ty: value,
            context_parameters,
            ..
        } => {
            name(owner, names);
            *receiver = ty(*receiver, names);
            *value = ty(*value, names);
            tys(context_parameters, names);
        }
    }
}

fn value_class_suspend_result(
    result: &mut super::IrValueClassSuspendResult,
    names: &HashMap<TypeName, TypeName>,
) {
    match result {
        super::IrValueClassSuspendResult::Boxed {
            classifier,
            carrier,
        } => {
            name(classifier, names);
            *carrier = ty(*carrier, names);
        }
        super::IrValueClassSuspendResult::Carrier {
            classifier,
            carrier,
        } => {
            name(classifier, names);
            *carrier = ty(*carrier, names);
        }
    }
}

fn remap_keyed<V>(map: &mut HashMap<TypeName, V>, names: &HashMap<TypeName, TypeName>) {
    *map = std::mem::take(map)
        .into_iter()
        .map(|(key, value)| (remapped_name(key, names), value))
        .collect();
}

fn remap_first_key<K: Eq + std::hash::Hash, V>(
    map: &mut HashMap<(TypeName, K), V>,
    names: &HashMap<TypeName, TypeName>,
) {
    *map = std::mem::take(map)
        .into_iter()
        .map(|((owner, key), value)| ((remapped_name(owner, names), key), value))
        .collect();
}

fn local_class_name_provenance(
    provenance: &mut IrLocalClassNameProvenance,
    names: &HashMap<TypeName, TypeName>,
) {
    name(&mut provenance.source.package, names);
    if let Some(IrLocalClassOwner::External(owner)) = &mut provenance.lexical_owner {
        name(owner, names);
    }
}

fn module_member_access(
    access: &mut super::IrModuleMemberAccess,
    names: &HashMap<TypeName, TypeName>,
) {
    match access {
        super::IrModuleMemberAccess::Callable {
            selected_parameters,
            ..
        }
        | super::IrModuleMemberAccess::Property {
            selected_parameters,
            ..
        } => tys(selected_parameters, names),
    }
}

fn declaration_argument_boundaries(
    boundaries: &mut [super::IrDeclarationArgumentBoundary],
    names: &HashMap<TypeName, TypeName>,
) {
    boundaries
        .iter_mut()
        .for_each(|boundary| boundary.declaration = ty(boundary.declaration, names));
}

fn remap_class(class: &mut super::IrClass, names: &HashMap<TypeName, TypeName>) {
    name(&mut class.fq_name, names);
    for (_, bound) in &mut class.type_param_bounds {
        *bound = ty(*bound, names);
    }
    tys(&mut class.supertypes, names);
    for property in &mut class.properties {
        for (_, _, parameter) in &mut property.context_params {
            *parameter = ty(*parameter, names);
        }
        property.ty = ty(property.ty, names);
        property.storage_ty = property.storage_ty.map(|value| ty(value, names));
        type_parameters(&mut property.type_params, names);
        property
            .annotations
            .iter_mut()
            .for_each(|annotation| name(annotation, names));
    }
    annotations(&mut class.applied_annotations, names);
    annotations(&mut class.primary_ctor_annotations, names);
    for parameter_annotations in &mut class.ctor_param_annotations {
        annotations(parameter_annotations, names);
    }
    for field in &mut class.field_annotations {
        annotations(&mut field.annotations, names);
    }
    for property in &mut class.property_annotations {
        annotations(&mut property.annotations, names);
    }
    for field in &mut class.fields {
        field.ty = ty(field.ty, names);
    }
    for argument in &mut class.ctor_args {
        argument.ty = ty(argument.ty, names);
        argument.declared_ty = argument.declared_ty.map(|value| ty(value, names));
        if let Some(receiver) = argument
            .capture
            .as_mut()
            .and_then(|capture| capture.receiver.as_mut())
        {
            captured_receiver(receiver, names);
        }
    }
    class
        .annotation_impl_of
        .iter_mut()
        .for_each(|value| name(value, names));
    class
        .sealed_subclasses
        .remap(|value| remapped_name(value, names));
    name(&mut class.superclass, names);
    tys(&mut class.super_ctor_params, names);
    for entry in &mut class.enum_entries {
        tys(&mut entry.constructor_parameter_types, names);
        entry
            .subclass
            .iter_mut()
            .for_each(|value| name(value, names));
    }
    if let Some(reference) = &mut class.prop_ref {
        reference
            .owner_internal
            .iter_mut()
            .for_each(|value| name(value, names));
        reference
            .call_owner_internal
            .iter_mut()
            .for_each(|value| name(value, names));
        reference.prop_ty = ty(reference.prop_ty, names);
        if let Some(Some(owner)) = &mut reference.ext_facade {
            name(owner, names);
        }
    }
    if let Some(reference) = &mut class.func_ref {
        reference
            .owner_class
            .iter_mut()
            .for_each(|value| name(value, names));
        reference
            .call_owner
            .iter_mut()
            .for_each(|value| name(value, names));
        reference.reflection_target_ret_ty = reference
            .reflection_target_ret_ty
            .map(|value| ty(value, names));
        if let Some(parameters) = &mut reference.reflection_target_param_tys {
            tys(parameters, names);
        }
        tys(&mut reference.param_tys, names);
        reference.ret_ty = ty(reference.ret_ty, names);
        tys(&mut reference.target_param_tys, names);
        reference.target_ret_ty = ty(reference.target_ret_ty, names);
        reference.function_type = ty(reference.function_type, names);
        reference
            .unbox_params
            .iter_mut()
            .flatten()
            .for_each(|value| name(value, names));
        reference
            .box_ret
            .iter_mut()
            .for_each(|value| name(value, names));
        reference
            .staticbound_recv_unbox
            .iter_mut()
            .for_each(|value| name(value, names));
    }
    for bridge in &mut class.bridges {
        bridge
            .overridden_owner
            .iter_mut()
            .for_each(|value| name(value, names));
        bridge
            .parameters
            .iter_mut()
            .for_each(|parameter| parameter.semantic = ty(parameter.semantic, names));
        tys(&mut bridge.erased_params, names);
        bridge.erased_ret = ty(bridge.erased_ret, names);
        tys(&mut bridge.concrete_params, names);
        bridge.concrete_ret = ty(bridge.concrete_ret, names);
        bridge.target_ret = bridge.target_ret.map(|value| ty(value, names));
    }
    class.interfaces.remap(|value| remapped_name(value, names));
    class
        .companion_class
        .iter_mut()
        .for_each(|value| name(value, names));
    for constructor in &mut class.secondary_ctors {
        annotations(&mut constructor.annotations, names);
        tys(&mut constructor.prefix_params, names);
        tys(&mut constructor.params, names);
        for (_, parameter) in &mut constructor.named_params {
            *parameter = ty(*parameter, names);
        }
        match &mut constructor.delegate {
            super::CtorDelegateTarget::This { target_params, .. } => tys(target_params, names),
            super::CtorDelegateTarget::Super {
                owner,
                target_params,
                ..
            } => {
                name(owner, names);
                tys(target_params, names);
            }
            super::CtorDelegateTarget::ImplicitEnumBase => {}
        }
    }
    if let Some(lambda) = &mut class.lambda {
        for receiver in &mut lambda.captured_receivers {
            captured_receiver(receiver, names);
        }
        lambda.function_type = ty(lambda.function_type, names);
        tys(&mut lambda.bridge.param_tys, names);
        lambda.bridge.ret_ty = ty(lambda.bridge.ret_ty, names);
        lambda
            .bridge
            .unbox_params
            .iter_mut()
            .flatten()
            .for_each(|value| name(value, names));
        lambda
            .bridge
            .box_ret
            .iter_mut()
            .for_each(|value| name(value, names));
    }
    if let Some(wrapper) = &mut class.sam_wrapper {
        name(&mut wrapper.interface, names);
    }
}

/// The exact classifier remap, applied to a class's declaration record.
struct RecordRenaming<'a>(&'a HashMap<TypeName, TypeName>);

impl crate::metadata::class_declarations::ClassifierRenaming for RecordRenaming<'_> {
    fn name(&self, value: TypeName) -> TypeName {
        remapped_name(value, self.0)
    }

    fn ty(&self, value: Ty) -> Ty {
        ty(value, self.0)
    }

    fn spelled(&self, value: &mut crate::spelling::Spelled) {
        spelled(value, self.0);
    }

    fn spellings(&self, value: &mut crate::spelling::DeclaredSpellings) {
        declared_spellings(value, self.0);
    }

    fn annotation(&self, value: &mut super::AppliedAnnotation) {
        annotation_application(value, self.0);
    }

    fn type_parameters(&self, value: &mut [super::IrTypeParameter]) {
        type_parameters(value, self.0);
    }
}

impl super::IrFile {
    /// Replace semantic classifier identities with target physical identities at a backend boundary.
    /// The map is exact and declaration-produced; this operation performs no spelling lookup.
    pub(crate) fn remap_classifier_identities(&mut self, names: &HashMap<TypeName, TypeName>) {
        if names.is_empty() {
            return;
        }

        annotations(&mut self.file_annotations, names);
        self.remap_inline_copy_owners(names);
        for provenance in self.local_class_name_provenance.values_mut() {
            local_class_name_provenance(provenance, names);
        }
        for provenance in self.callable_reference_provenance.values_mut() {
            local_class_name_provenance(provenance, names);
        }
        for function in &mut self.functions {
            tys(&mut function.params, names);
            function.ret = ty(function.ret, names);
            if let Some(owner) = &mut function.dispatch_receiver {
                name(owner, names);
            }
        }
        for plan in &mut self.local_delegate_plans {
            name(&mut plan.reference.source.package, names);
            if let Some(class) = &mut plan.reference.class {
                name(class, names);
            }
            plan.reference.property_type = ty(plan.reference.property_type, names);
            for accessor in std::iter::once(&mut plan.getter).chain(plan.setter.iter_mut()) {
                for receiver in &mut accessor.captured_receivers {
                    captured_receiver(receiver, names);
                }
                tys(&mut accessor.parameters, names);
                type_parameters(&mut accessor.type_parameters, names);
                accessor.result = ty(accessor.result, names);
            }
        }
        for class in &mut self.classes {
            remap_class(class, names);
        }
        remap_keyed(&mut self.class_declarations, names);
        for record in self.class_declarations.values_mut().flatten() {
            record.rename_classifiers(&RecordRenaming(names));
        }
        for parameters in self.fn_params.values_mut() {
            for receiver in &mut parameters.captured_receivers {
                captured_receiver(receiver, names);
            }
        }
        for annotations_by_function in self.function_annotations.values_mut() {
            annotations(annotations_by_function, names);
        }
        for annotations_by_parameter in self.fn_param_annotations.values_mut() {
            for parameter_annotations in annotations_by_parameter {
                annotations(parameter_annotations, names);
            }
        }
        for property in &mut self.statics {
            property.ty = ty(property.ty, names);
            property
                .owner
                .iter_mut()
                .for_each(|value| name(value, names));
            property.erased_declared_ty = property.erased_declared_ty.map(|value| ty(value, names));
        }
        for expression_value in &mut self.exprs {
            remap_expression(expression_value, names);
        }

        for callable in self.referenced_module_callables.values_mut() {
            name(&mut callable.source.package, names);
            callable
                .owner
                .iter_mut()
                .for_each(|value| name(value, names));
            tys(&mut callable.parameters, names);
            for parameter in &mut callable.type_parameters {
                for (bound, _) in &mut parameter.bounds {
                    *bound = ty(*bound, names);
                }
            }
            callable.result = ty(callable.result, names);
            if let super::IrStaticPlacement::CompanionBlock { declaring_class } =
                &mut callable.placement
            {
                name(declaring_class, names);
            }
            for annotation in &mut callable.annotations {
                name(&mut annotation.identity, names);
            }
        }
        for property in self.referenced_module_properties.values_mut() {
            name(&mut property.source.package, names);
            property.ty = ty(property.ty, names);
            tys(&mut property.context_parameters, names);
            property.extension_receiver = property.extension_receiver.map(|value| ty(value, names));
            property
                .owner
                .iter_mut()
                .for_each(|value| name(value, names));
            property
                .companion_owner
                .iter_mut()
                .for_each(|value| name(value, names));
            property
                .annotations
                .iter_mut()
                .for_each(|value| name(value, names));
        }
        for classifier in self.referenced_module_classifiers.values_mut() {
            classifier
                .companion_owner
                .iter_mut()
                .for_each(|value| name(value, names));
        }
        for hierarchy in self.classifier_hierarchies.values_mut() {
            for classifier in hierarchy {
                name(&mut classifier.classifier, names);
                classifier.applied = ty(classifier.applied, names);
            }
        }
        for overrides in self.property_overrides.values_mut() {
            for edge in overrides {
                name(&mut edge.implementation_owner, names);
                name(&mut edge.overridden_owner, names);
                edge.declared_type = ty(edge.declared_type, names);
                edge.applied_type = ty(edge.applied_type, names);
                edge.implementation_type = ty(edge.implementation_type, names);
                edge.declared_receiver = edge.declared_receiver.map(|value| ty(value, names));
                edge.implementation_receiver =
                    edge.implementation_receiver.map(|value| ty(value, names));
            }
        }
        for defaults in self.inherited_defaults.values_mut() {
            for default in defaults {
                name(&mut default.declaring_interface, names);
                name(&mut default.dispatch_interface, names);
                tys(&mut default.parameters, names);
                default.result = ty(default.result, names);
                tys(&mut default.applied_parameters, names);
                default.applied_result = ty(default.applied_result, names);
            }
        }
        for overrides in self.function_overrides.values_mut() {
            for edge in overrides {
                name(&mut edge.implementation_owner, names);
                name(&mut edge.overridden_owner, names);
                tys(&mut edge.declared_parameters, names);
                edge.declared_result = ty(edge.declared_result, names);
                tys(&mut edge.applied_parameters, names);
                edge.applied_result = ty(edge.applied_result, names);
                tys(&mut edge.implementation_parameters, names);
                edge.implementation_result = ty(edge.implementation_result, names);
            }
        }
        for constructor in self.checked_constructor_bodies.values_mut() {
            annotations(&mut constructor.annotations, names);
            for (_, parameter) in &mut constructor.parameters {
                *parameter = ty(*parameter, names);
            }
        }
        for property in self.checked_properties.values_mut() {
            property.ty = ty(property.ty, names);
            property.storage_ty = property.storage_ty.map(|value| ty(value, names));
        }
        for layout in self.local_property_layouts.values_mut() {
            local_property_layout(layout, names);
        }
        for construction in self.annotation_constructions.values_mut() {
            name(&mut construction.interface, names);
            for (_, member) in &mut construction.members {
                *member = ty(*member, names);
            }
            construction
                .scopes
                .iter_mut()
                .flatten()
                .for_each(|value| name(value, names));
        }
        for aliases in self.class_type_aliases.values_mut() {
            aliases
                .iter_mut()
                .for_each(|alias| type_alias(alias, names));
        }
        for function in &mut self.package_functions {
            for (_, parameter) in &mut function.params {
                *parameter = ty(*parameter, names);
            }
            function.ret = ty(function.ret, names);
            function.receiver = function.receiver.map(|value| ty(value, names));
            package_type_parameters(&mut function.type_params, names);
            declared_spellings(&mut function.spellings, names);
        }
        for property in &mut self.package_properties {
            property.ty = ty(property.ty, names);
            package_type_parameters(&mut property.type_params, names);
            property.receiver = property.receiver.map(|value| ty(value, names));
            tys(&mut property.context_parameters, names);
            property
                .annotations
                .iter_mut()
                .for_each(|value| name(value, names));
            declared_spellings(&mut property.spellings, names);
        }
        for alias in &mut self.package_type_aliases {
            type_alias(alias, names);
        }
        for properties in self.member_ext_props.values_mut() {
            for property in properties {
                property.receiver = ty(property.receiver, names);
                property.ty = ty(property.ty, names);
                type_parameters(&mut property.type_params, names);
            }
        }
        for signature in self.signatures.values_mut() {
            generic_signature(signature, names);
        }
        for parameters in self.callable_bound_type_parameters.values_mut() {
            type_parameters(parameters, names);
        }
        for (parameters, result) in self.member_semantic_sigs.values_mut() {
            tys(parameters, names);
            *result = ty(*result, names);
        }
        for signature in self.class_signatures.values_mut() {
            generic_signature(signature, names);
        }
        self.companion_blocks
            .remap_classifier_identities(names, |value| ty(value, names));
        self.remap_type_reflection_classifier_identities(names, |value| ty(value, names));
        for splice in self.inline_property_access.splices.values_mut() {
            for substitution in &mut splice.substitutions {
                substitution.value = ty(substitution.value, names);
            }
        }
        for specialized in self.specialized_anonymous_classes.values_mut() {
            specialized
                .bindings
                .values_mut()
                .for_each(|value| *value = ty(*value, names));
            specialized
                .reified_bindings
                .values_mut()
                .for_each(|value| *value = ty(*value, names));
        }
        for underlying in self.external_value_classes.values_mut() {
            *underlying = ty(*underlying, names);
        }
        for declaration in self.external_value_class_declarations.values_mut() {
            declaration.underlying = ty(declaration.underlying, names);
            tys(&mut declaration.type_parameters, names);
        }
        for substitutions in self.reified_call_subst.values_mut() {
            for (_, substitution) in substitutions {
                *substitution = ty(*substitution, names);
            }
        }
        for substitutions in self.inline_call_type_arguments.values_mut() {
            for (_, substitution) in substitutions {
                *substitution = ty(*substitution, names);
            }
        }
        for result in self.value_class_suspend_returns.values_mut() {
            value_class_suspend_result(result, names);
        }
        for result in self.value_class_suspend_calls.values_mut() {
            value_class_suspend_result(result, names);
        }
        for (parameters, result) in self.suspend_declared_sigs.values_mut() {
            tys(parameters, names);
            *result = ty(*result, names);
        }
        for (_, parameters, result) in self.vc_declared_sigs.values_mut() {
            tys(parameters, names);
            *result = ty(*result, names);
        }
        for parameters in self.default_stub_boxed_params.values_mut() {
            for (_, parameter) in parameters {
                *parameter = ty(*parameter, names);
            }
        }
        for point in self.intrinsic_suspension_points.values_mut() {
            point.result = ty(point.result, names);
        }
        for (classifier, _) in self.jvm_suspend_impl_bodies.values_mut() {
            name(classifier, names);
        }
        for (classifier, underlying) in self.erased_value_constructions.values_mut() {
            name(classifier, names);
            *underlying = ty(*underlying, names);
        }
        for operation in self.value_class_type_operations.values_mut() {
            name(&mut operation.boxed_owner, names);
            operation.carrier = ty(operation.carrier, names);
        }
        for access in self.module_member_accesses.values_mut() {
            module_member_access(access, names);
        }
        for call in self.jvm_protected_dependency_calls.values_mut() {
            name(&mut call.owner, names);
            tys(&mut call.parameters, names);
            call.result = ty(call.result, names);
        }
        for realization in self.jvm_overridden_call_realizations.values_mut() {
            name(&mut realization.declaration_owner, names);
        }
        for boundaries in self.declaration_argument_boundaries.values_mut() {
            declaration_argument_boundaries(boundaries, names);
        }

        self.expression_owners
            .values_mut()
            .for_each(|owner| name(owner, names));
        self.shared_capture_parameters
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.shared_class_capture_fields
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.shared_super_capture_parameters
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.shared_secondary_super_capture_parameters
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.class_static_local_functions
            .values_mut()
            .for_each(|value| name(value, names));
        for parameters in self.physical_call_parameters.values_mut() {
            tys(parameters, names);
        }
        self.logical_types
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.deferred_local_types
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.inline_declared_types_mut()
            .for_each(|value| *value = ty(*value, names));
        self.physical_types
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.whens
            .exhaustive
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.suspend_calls
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.suspend_call_overridden_results
            .values_mut()
            .flat_map(|results| results.iter_mut())
            .for_each(|value| *value = ty(*value, names));
        self.ext_call_source_receiver
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.dispatch_classes
            .values_mut()
            .for_each(|value| *value = remapped_name(*value, names));
        self.call_declared_ret
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.property_declaration_types
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        for values in self.call_declared_params.values_mut() {
            tys(values, names);
        }
        for values in self.construction_declared_params.values_mut() {
            tys(values, names);
        }
        for (_, value) in self.property_selected_accessors.values_mut() {
            *value = ty(*value, names);
        }
        for (_, value) in self.property_accessor_jvm_realizations.values_mut() {
            *value = ty(*value, names);
        }
        for (parameters, result) in self.lambda_sam_signature.values_mut() {
            tys(parameters, names);
            *result = ty(*result, names);
        }
        for (parameters, result) in self.lambda_sam_jvm_signature.values_mut() {
            tys(parameters, names);
            *result = ty(*result, names);
        }
        for origin in self.lambda_origins.values_mut() {
            origin
                .lexical_owner
                .iter_mut()
                .for_each(|value| name(value, names));
            if let Some(provenance) = &mut origin.class_provenance {
                local_class_name_provenance(provenance, names);
            }
        }
        self.callable_reference_names
            .values_mut()
            .for_each(|value| name(value, names));
        self.lambda_class_names
            .values_mut()
            .for_each(|value| name(value, names));
        self.fn_declared_spellings
            .values_mut()
            .for_each(|value| declared_spellings(value, names));
        self.class_declared_spellings
            .values_mut()
            .for_each(|value| declared_spellings(value, names));
        self.prop_declared_spellings
            .values_mut()
            .for_each(|value| declared_spellings(value, names));
        remap_keyed(&mut self.declaration_paths, names);

        remap_keyed(&mut self.referenced_module_classifiers, names);
        remap_keyed(&mut self.classifier_hierarchies, names);
        remap_keyed(&mut self.property_overrides, names);
        remap_keyed(&mut self.function_overrides, names);
        remap_keyed(&mut self.inherited_defaults, names);
        remap_keyed(&mut self.generated_member_publications, names);
        remap_keyed(&mut self.class_type_aliases, names);
        remap_keyed(&mut self.jvm_value_class_secondary_ctors, names);
        remap_keyed(&mut self.member_ext_props, names);
        remap_keyed(&mut self.class_visibilities, names);
        remap_keyed(&mut self.ctor_close_lines, names);
        remap_keyed(&mut self.ctor_visibilities, names);
        remap_keyed(&mut self.declared_class_statics, names);
        remap_keyed(&mut self.super_constructor_default_arguments, names);
        remap_keyed(&mut self.external_super_constructors, names);
        remap_keyed(&mut self.class_declared_spellings, names);
        remap_keyed(&mut self.class_superclass_positions, names);
        remap_keyed(&mut self.class_ctor_defaults, names);
        remap_keyed(&mut self.class_signatures, names);
        remap_keyed(&mut self.field_signatures, names);
        remap_keyed(&mut self.external_value_classes, names);
        remap_keyed(&mut self.companion_clinit_bodies, names);
        remap_keyed(&mut self.classifier_roles, names);
        remap_keyed(&mut self.external_value_class_declarations, names);

        remap_first_key(&mut self.synthesized_data_class_members, names);
        remap_first_key(&mut self.generated_classes, names);
        remap_first_key(&mut self.generated_functions, names);
        remap_first_key(&mut self.generated_secondary_constructors, names);
        remap_first_key(&mut self.jvm_companion_property_statics, names);
        remap_first_key(&mut self.property_annotation_markers, names);
        remap_first_key(&mut self.external_secondary_super_constructors, names);
        remap_first_key(&mut self.prop_declared_spellings, names);
        remap_first_key(&mut self.prop_decl_lines, names);
        self.value_class_constructor_facts
            .remap_classifier_identities(names, |value| ty(value, names));

        self.synthetic_classes = std::mem::take(&mut self.synthetic_classes)
            .into_iter()
            .map(|value| remapped_name(value, names))
            .collect();
        self.deprecated_classes = std::mem::take(&mut self.deprecated_classes)
            .into_iter()
            .map(|value| remapped_name(value, names))
            .collect();
        self.public_synthetics = std::mem::take(&mut self.public_synthetics)
            .into_iter()
            .map(|value| remapped_name(value, names))
            .collect();
        self.module_source_value_classes = std::mem::take(&mut self.module_source_value_classes)
            .into_iter()
            .map(|value| remapped_name(value, names))
            .collect();
        self.module_readable_value_classes =
            std::mem::take(&mut self.module_readable_value_classes)
                .into_iter()
                .map(|value| remapped_name(value, names))
                .collect();
    }
}

fn remap_expression_facts(
    ir: &mut super::IrFile,
    expression: ExprId,
    names: &HashMap<TypeName, TypeName>,
) {
    if let Some(value) = ir.logical_types.get_mut(&expression) {
        *value = ty(*value, names);
    }
    if let Some(value) = ir.deferred_local_types.get_mut(&expression) {
        *value = ty(*value, names);
    }
    if let Some(value) = ir.physical_types.get_mut(&expression) {
        *value = ty(*value, names);
    }
    if let Some(value) = ir.call_declared_ret.get_mut(&expression) {
        *value = ty(*value, names);
    }
    if let Some(parameters) = ir.call_declared_params.get_mut(&expression) {
        tys(parameters, names);
    }
    if let Some(parameters) = ir.construction_declared_params.get_mut(&expression) {
        tys(parameters, names);
    }
    if let Some(parameters) = ir.physical_call_parameters.get_mut(&expression) {
        tys(parameters, names);
    }
    if let Some(access) = ir.module_member_accesses.get_mut(&expression) {
        module_member_access(access, names);
    }
    if let Some(call) = ir.jvm_protected_dependency_calls.get_mut(&expression) {
        name(&mut call.owner, names);
        tys(&mut call.parameters, names);
        call.result = ty(call.result, names);
    }
    if let Some(realization) = ir.jvm_overridden_call_realizations.get_mut(&expression) {
        name(&mut realization.declaration_owner, names);
    }
    if let Some(boundaries) = ir.declaration_argument_boundaries.get_mut(&expression) {
        declaration_argument_boundaries(boundaries, names);
    }
    if let Some(operation) = ir.value_class_type_operations.get_mut(&expression) {
        name(&mut operation.boxed_owner, names);
        operation.carrier = ty(operation.carrier, names);
    }
    if let Some(point) = ir.intrinsic_suspension_points.get_mut(&expression) {
        point.result = ty(point.result, names);
    }
    if let Some(splice) = ir.inline_property_access.splices.get_mut(&expression) {
        for substitution in &mut splice.substitutions {
            substitution.value = ty(substitution.value, names);
        }
    }
    if let Some(substitutions) = ir.reified_call_subst.get_mut(&expression) {
        for (_, value) in substitutions {
            *value = ty(*value, names);
        }
    }
    if let Some(substitutions) = ir.inline_call_type_arguments.get_mut(&expression) {
        for (_, value) in substitutions {
            *value = ty(*value, names);
        }
    }
    if let Some(value) = ir.inline_declared_type_mut(expression) {
        *value = ty(*value, names);
    }
    if let Some(owner) = ir.expression_owners.get_mut(&expression) {
        name(owner, names);
    }
    if let Some(owner) = ir.dispatch_classes.get_mut(&expression) {
        name(owner, names);
    }
    if let Some(value) = ir.suspend_calls.get_mut(&expression) {
        *value = ty(*value, names);
    }
    if let Some(results) = ir.suspend_call_overridden_results.get_mut(&expression) {
        for value in results.iter_mut() {
            *value = ty(*value, names);
        }
    }
    if let Some(value) = ir.ext_call_source_receiver.get_mut(&expression) {
        *value = ty(*value, names);
    }
    if let Some(value) = ir.whens.exhaustive.get_mut(&expression) {
        *value = ty(*value, names);
    }
    if let Some(result) = ir.value_class_suspend_calls.get_mut(&expression) {
        value_class_suspend_result(result, names);
    }
    if let Some((classifier, underlying)) = ir.erased_value_constructions.get_mut(&expression) {
        name(classifier, names);
        *underlying = ty(*underlying, names);
    }
    if let Some(construction) = ir.annotation_constructions.get_mut(&expression) {
        name(&mut construction.interface, names);
        for (_, member) in &mut construction.members {
            *member = ty(*member, names);
        }
        construction
            .scopes
            .iter_mut()
            .flatten()
            .for_each(|value| name(value, names));
    }
    if let Some(classifier) = ir.callable_reference_names.get_mut(&expression) {
        name(classifier, names);
    }
    if let Some(provenance) = ir.callable_reference_provenance.get_mut(&expression) {
        local_class_name_provenance(provenance, names);
    }
}

impl super::IrFile {
    /// Rename every local classifier to the physical name a target gives it, from its provenance.
    ///
    /// A name is its lexical owner's physical name followed by its source segments and then its
    /// ordinal, each a nested component. A classifier with no lexical owner starts from
    /// `root(source, first)` instead, which names its FIRST component. That start is the only
    /// thing targets disagree on: the JVM nests the whole path in the declaring file's facade
    /// (`AKt$box$Local`), and a target without facade classes starts it in the package
    /// (`box$Local`).
    pub(crate) fn realize_local_class_names(
        &mut self,
        root: impl Fn(super::IrModuleSource, &str) -> TypeName,
    ) {
        let classes = self
            .local_class_name_provenance
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let mut physical = HashMap::new();
        for class in classes {
            physical_name(class, &root, self, &mut physical);
        }
        let identities = physical
            .into_iter()
            .map(|(class, physical)| (self.classes[class as usize].fq_name, physical))
            .collect();
        self.remap_classifier_identities(&identities);
    }

    /// [`Self::realize_local_class_names`] for a target without facade classes: a local
    /// classifier that no classifier owns starts in its declaring source's package, so the one
    /// local to a top-level `box` is `box$Local` and its first anonymous object `box$1`.
    pub(crate) fn realize_local_class_names_in_packages(&mut self) {
        self.realize_local_class_names(|source, first| {
            crate::types::type_name_child(source.package, first)
        });
    }
}

fn physical_name(
    class: ClassId,
    root: &impl Fn(super::IrModuleSource, &str) -> TypeName,
    ir: &super::IrFile,
    cache: &mut HashMap<ClassId, TypeName>,
) -> TypeName {
    if let Some(name) = cache.get(&class) {
        return *name;
    }
    // An owner declared outside executable code (a top-level or member classifier) already has its
    // physical identity; only local and anonymous classifiers are named from provenance.
    let Some(provenance) = ir.local_class_name_provenance.get(&class) else {
        return ir.classes[class as usize].fq_name;
    };
    let ordinal = provenance.ordinal.map(|ordinal| ordinal.to_string());
    let mut components = provenance
        .segments
        .iter()
        .map(String::as_str)
        .chain(ordinal.as_deref());
    let mut name = match provenance.lexical_owner {
        Some(IrLocalClassOwner::Class(owner)) => physical_name(owner, root, ir, cache),
        Some(IrLocalClassOwner::External(owner)) => owner,
        None => {
            let first = components
                .next()
                .expect("a local classifier's provenance names at least one component");
            root(provenance.source, first)
        }
    };
    for component in components {
        name = crate::types::type_name_nested_child(name, component);
    }
    cache.insert(class, name);
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        IrCallableReference, IrConst, IrDeclarationArgumentBoundary, IrFunction,
        IrModuleMemberAccess, IrProgressionSource, IrRuntimeFunction, IrSamMethod, IrSamTarget,
        IrValueClassTypeOperation, IrValueClassTypeRole,
    };

    fn resolved(value: Ty) -> crate::fir::ResolvedTy {
        crate::fir::ResolvedTy::new(value).expect("test type is publishable")
    }

    #[test]
    fn classifier_remap_follows_a_specialization_chain_to_its_terminal_identity() {
        let declaration = crate::types::type_name("sample/Declaration");
        let intermediate = crate::types::type_name("sample/Intermediate");
        let terminal = crate::types::type_name("sample/Terminal");
        let names = HashMap::from([(declaration, intermediate), (intermediate, terminal)]);

        assert_eq!(remapped_name(declaration, &names), terminal);
        assert_eq!(
            ty(Ty::obj_name(declaration), &names),
            Ty::obj_name(terminal)
        );
    }

    #[test]
    fn remaps_every_type_carrier_inside_expressions() {
        let source = crate::types::type_name("sample/Source");
        let target = crate::types::type_name("sample/Target");
        let source_ty = Ty::obj_name(source);
        let target_ty = Ty::obj_name(target);
        let names = HashMap::from([(source, target)]);

        let mut callable = IrExpr::CallableReference(IrCallableReference {
            target: IrCallableReferenceTarget::FunctionValueConversion { ordinal: 0 },
            adapter: 0,
            captures: Vec::new(),
            bound_receiver: None,
            function_type: source_ty,
            declaration_parameters: vec![source_ty].into_boxed_slice(),
            declaration_result: source_ty,
            declaration_suspend: false,
            adaptation: Some(Box::new(crate::fir::FirReferenceAdaptation {
                arguments: Vec::new().into_boxed_slice(),
                parameter_types: vec![resolved(source_ty)].into_boxed_slice(),
                result_type: resolved(source_ty),
                suspend_conversion: false,
            })),
            reflection_owner: Some(source),
        });
        remap_expression(&mut callable, &names);
        let IrExpr::CallableReference(callable) = callable else {
            unreachable!()
        };
        assert_eq!(callable.function_type, target_ty);
        assert_eq!(callable.declaration_parameters.as_ref(), &[target_ty]);
        assert_eq!(callable.declaration_result, target_ty);
        assert_eq!(callable.reflection_owner, Some(target));
        let adaptation = callable.adaptation.expect("adaptation retained");
        assert_eq!(adaptation.parameter_types[0].get(), target_ty);
        assert_eq!(adaptation.result_type.get(), target_ty);

        let mut lambda = IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures: Vec::new(),
            sam: Some(IrSamTarget {
                classifier: source,
                method: "apply".to_string(),
                method_target: IrSamMethod::FunctionTypeInvoke,
                parameters: vec![source_ty],
                contravariant_parameters: vec![false],
                result: source_ty,
                declared_parameters: vec![source_ty],
                declared_result: source_ty,
                context_count: 0,
                has_receiver: false,
                suspend: false,
                source_suspend: false,
                overridden_results: vec![source_ty],
                function_adapter: false,
                wraps_function_value: false,
                nullable: false,
                kotlin_interface: true,
                parameter_identities: Vec::new(),
            }),
            inline_body: None,
        };
        remap_expression(&mut lambda, &names);
        let IrExpr::Lambda { sam: Some(sam), .. } = lambda else {
            unreachable!()
        };
        assert_eq!(sam.classifier, target);
        assert_eq!(sam.overridden_results, vec![target_ty]);

        let runtime = || IrRuntimeFunction {
            function: crate::fir::ExternalCallableId::from_raw(1),
            parameters: vec![source_ty],
            result: source_ty,
        };
        let mut range = IrCheckedOperation::RangeLoop {
            variable: 0,
            variable_name: None,
            counter: source_ty,
            source: IrProgressionSource::Step {
                nested: Box::new(IrProgressionSource::Literal {
                    operation: crate::fir::FirRangeOperation::Through,
                    start: 0,
                    end: 1,
                }),
                step: 2,
                last_element: runtime(),
            },
            unsigned_compare: Some(runtime()),
            body: 3,
            label: String::new(),
            with_index: None,
        };
        checked_operation(&mut range, &names);
        let IrCheckedOperation::RangeLoop {
            counter,
            source: IrProgressionSource::Step { last_element, .. },
            unsigned_compare: Some(unsigned_compare),
            ..
        } = range
        else {
            unreachable!()
        };
        assert_eq!(counter, target_ty);
        assert_eq!(last_element.parameters, vec![target_ty]);
        assert_eq!(last_element.result, target_ty);
        assert_eq!(unsigned_compare.parameters, vec![target_ty]);
        assert_eq!(unsigned_compare.result, target_ty);
    }

    #[test]
    fn remaps_expression_side_tables_and_file_classifier_keys() {
        let source = crate::types::type_name("sample/Source");
        let target = crate::types::type_name("sample/Target");
        let source_ty = Ty::obj_name(source);
        let target_ty = Ty::obj_name(target);
        let names = HashMap::from([(source, target)]);
        let mut ir = super::super::IrFile::default();
        let expression = ir.add_expr(IrExpr::Const(IrConst::Int(0)));

        ir.deferred_local_types.insert(expression, source_ty);
        ir.module_member_accesses.insert(
            expression,
            IrModuleMemberAccess::Callable {
                target: crate::fir::CallableId::from_raw(1),
                selected_parameters: vec![source_ty].into_boxed_slice(),
            },
        );
        ir.declaration_argument_boundaries.insert(
            expression,
            vec![IrDeclarationArgumentBoundary {
                argument: expression,
                parameter: 0,
                declaration: source_ty,
                retarget_coercion: false,
            }]
            .into_boxed_slice(),
        );
        ir.value_class_type_operations.insert(
            expression,
            IrValueClassTypeOperation {
                boxed_owner: source,
                carrier: source_ty,
                role: IrValueClassTypeRole::Box,
            },
        );
        remap_expression_facts(&mut ir, expression, &names);

        assert_eq!(ir.deferred_local_types[&expression], target_ty);
        let IrModuleMemberAccess::Callable {
            selected_parameters,
            ..
        } = &ir.module_member_accesses[&expression]
        else {
            unreachable!()
        };
        assert_eq!(selected_parameters.as_ref(), &[target_ty]);
        assert_eq!(
            ir.declaration_argument_boundaries[&expression][0].declaration,
            target_ty
        );
        assert_eq!(
            ir.value_class_type_operations[&expression],
            IrValueClassTypeOperation {
                boxed_owner: target,
                carrier: target_ty,
                role: IrValueClassTypeRole::Box,
            }
        );

        ir.classifier_roles
            .insert(source, crate::types::ClassifierRole::FunctionOfArity(0));
        ir.companion_clinit_bodies.insert(source, expression);
        ir.class_static_local_functions.insert(0, source);
        ir.remap_classifier_identities(&names);
        assert!(ir.classifier_roles.contains_key(&target));
        assert_eq!(ir.companion_clinit_bodies[&target], expression);
        assert_eq!(ir.class_static_local_functions[&0], target);
    }

    #[test]
    fn reachable_remap_reports_a_lambda_implementation_detached_from_another_site() {
        let source = crate::types::type_name("sample/Source");
        let target = crate::types::type_name("sample/Target");
        let names = HashMap::from([(source, target)]);
        let mut ir = super::super::IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "invoke".to_string(),
            params: vec![Ty::obj_name(source)],
            ret: Ty::obj_name(source),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let reachable_inline_body = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(implementation),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let reachable = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(reachable_inline_body),
        });
        let other_inline_body = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(implementation),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let other = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(other_inline_body),
        });
        ir.inline_only_fns.insert(implementation);
        ir.must_inline_lambdas.insert(implementation);

        let detached = ir.remap_reachable_classifier_identities(
            &names,
            [reachable],
            &HashSet::from([reachable, reachable_inline_body]),
            &mut HashSet::new(),
        );

        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].source, implementation);
        assert_eq!(detached[0].parent, None);
        assert_eq!(detached[0].order, reachable);
        assert!(detached[0].class_site);
        assert!(matches!(
            ir.expr(reachable),
            IrExpr::Lambda { impl_fn, .. } if *impl_fn == detached[0].target
        ));
        assert!(matches!(
            ir.expr(other),
            IrExpr::Lambda { impl_fn, .. } if *impl_fn == implementation
        ));
        assert!(matches!(
            ir.expr(reachable_inline_body),
            IrExpr::Call { callee: Callee::Local(function), .. }
                if *function == detached[0].target
        ));
        assert!(matches!(
            ir.expr(other_inline_body),
            IrExpr::Call { callee: Callee::Local(function), .. }
                if *function == implementation
        ));
        assert!(ir.inline_only_fns.contains(&implementation));
        assert!(ir.must_inline_lambdas.contains(&implementation));
        assert!(!ir.inline_only_fns.contains(&detached[0].target));
        assert!(!ir.must_inline_lambdas.contains(&detached[0].target));
        assert_eq!(
            ir.functions[detached[0].target as usize].params,
            vec![Ty::obj_name(target)]
        );
        assert_eq!(
            ir.functions[detached[0].target as usize].ret,
            Ty::obj_name(target)
        );
    }

    #[test]
    fn reachable_remap_detaches_an_implementation_owned_by_the_remapped_class() {
        let source = crate::types::type_name("sample/Source");
        let target = crate::types::type_name("sample/Target");
        let names = HashMap::from([(source, target)]);
        let mut ir = super::super::IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "invoke".to_string(),
            params: vec![Ty::obj_name(source)],
            ret: Ty::obj_name(source),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut declaration = crate::plugins::synthetic_class("sample/Source");
        declaration.methods.push(implementation);
        let declaration = ir.add_class(declaration);
        ir.note_class_method(declaration, implementation);
        ir.add_class(crate::plugins::synthetic_class("sample/Target"));
        let reachable = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });

        let detached = ir.remap_reachable_classifier_identities(
            &names,
            [reachable],
            &HashSet::from([reachable]),
            &mut HashSet::new(),
        );

        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].source, implementation);
        assert!(matches!(
            ir.expr(reachable),
            IrExpr::Lambda { impl_fn, .. } if *impl_fn == detached[0].target
        ));
        assert!(!ir.class_method_owners.contains_key(&detached[0].target));
        assert_eq!(
            ir.functions[detached[0].target as usize].params,
            vec![Ty::obj_name(target)]
        );
    }

    #[test]
    fn reachable_remap_detaches_a_consumed_lambda_direct_call() {
        let source = crate::types::type_name("sample/Source");
        let target = crate::types::type_name("sample/Target");
        let names = HashMap::from([(source, target)]);
        let mut ir = super::super::IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "invoke".to_string(),
            params: vec![Ty::obj_name(source)],
            ret: Ty::obj_name(source),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.lambda_origins.insert(
            implementation,
            crate::ir::IrLambdaOrigin {
                identity: 0,
                lexical_owner: None,
                enclosing_name: "box".to_string(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "box".to_string(),
                implementation_ordinal: 0,
                receiver_parameter: None,
                label: None,
                form: crate::ir::IrLambdaForm::Literal,
                explicit_suspend: false,
                class_provenance: None,
            },
        );
        let reachable = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(implementation),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let other = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(implementation),
            dispatch_receiver: None,
            args: Vec::new(),
        });

        let detached = ir.remap_reachable_classifier_identities(
            &names,
            [reachable],
            &HashSet::from([reachable]),
            &mut HashSet::new(),
        );

        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].source, implementation);
        assert_eq!(detached[0].order, reachable);
        assert!(!detached[0].class_site);
        assert!(matches!(
            ir.expr(reachable),
            IrExpr::Call { callee: Callee::Local(function), .. }
                if *function == detached[0].target
        ));
        assert!(matches!(
            ir.expr(other),
            IrExpr::Call { callee: Callee::Local(function), .. }
                if *function == implementation
        ));
        assert_eq!(
            ir.functions[detached[0].target as usize].params,
            vec![Ty::obj_name(target)]
        );
        assert_eq!(
            ir.functions[detached[0].target as usize].ret,
            Ty::obj_name(target)
        );
    }

    #[test]
    fn reachable_remap_specializes_a_consumed_lambda_with_no_other_site() {
        let source = crate::types::type_name("sample/Source");
        let target = crate::types::type_name("sample/Target");
        let names = HashMap::from([(source, target)]);
        let mut ir = super::super::IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "invoke".to_string(),
            params: vec![Ty::obj_name(source)],
            ret: Ty::obj_name(source),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.lambda_origins.insert(
            implementation,
            crate::ir::IrLambdaOrigin {
                identity: 0,
                lexical_owner: None,
                enclosing_name: "box".to_string(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "box".to_string(),
                implementation_ordinal: 0,
                receiver_parameter: None,
                label: None,
                form: crate::ir::IrLambdaForm::Literal,
                explicit_suspend: false,
                class_provenance: None,
            },
        );
        let reachable = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(implementation),
            dispatch_receiver: None,
            args: Vec::new(),
        });

        let detached = ir.remap_reachable_classifier_identities(
            &names,
            [reachable],
            &HashSet::from([reachable]),
            &mut HashSet::new(),
        );

        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].source, implementation);
        assert!(!detached[0].class_site);
        assert!(matches!(
            ir.expr(reachable),
            IrExpr::Call { callee: Callee::Local(function), .. }
                if *function == detached[0].target
        ));
        assert_eq!(
            ir.functions[detached[0].target as usize].params,
            vec![Ty::obj_name(target)]
        );
        assert_eq!(
            ir.functions[detached[0].target as usize].ret,
            Ty::obj_name(target)
        );
    }

    #[test]
    fn reachable_remap_does_not_take_ownership_of_a_substituted_caller_lambda() {
        let source = crate::types::type_name("sample/Source");
        let target = crate::types::type_name("sample/Target");
        let names = HashMap::from([(source, target)]);
        let mut ir = super::super::IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "invoke".to_string(),
            params: vec![Ty::obj_name(source)],
            ret: Ty::obj_name(source),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let caller_lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });
        let copied_root = ir.add_expr(IrExpr::Block {
            stmts: vec![caller_lambda],
            value: None,
        });
        ir.inline_only_fns.insert(implementation);
        ir.must_inline_lambdas.insert(implementation);

        let detached = ir.remap_reachable_classifier_identities(
            &names,
            [copied_root],
            &HashSet::from([copied_root]),
            &mut HashSet::new(),
        );

        assert!(detached.is_empty());
        assert!(matches!(
            ir.expr(caller_lambda),
            IrExpr::Lambda { impl_fn, .. } if *impl_fn == implementation
        ));
        assert_eq!(
            ir.functions[implementation as usize].params,
            vec![Ty::obj_name(source)]
        );
        assert_eq!(
            ir.functions[implementation as usize].ret,
            Ty::obj_name(source)
        );
        assert!(ir.inline_only_fns.contains(&implementation));
        assert!(ir.must_inline_lambdas.contains(&implementation));
    }
}
