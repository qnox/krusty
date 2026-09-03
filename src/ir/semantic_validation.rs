//! Common-IR semantic boundary validation.
//!
//! `Ty::Pending` and `Ty::Error` are frontend/checker states, not Kotlin types. This module walks
//! every common-IR type carrier before the IR crosses into a backend. The matches over semantic
//! operation enums are deliberately exhaustive: adding a new IR node requires deciding where its
//! types live instead of silently creating an unchecked backend path.

use crate::types::Ty;

use super::{
    Callee, CtorDelegateTarget, FuncRef, IrCheckedArgument, IrCheckedConstructorTarget,
    IrCheckedOperation, IrCheckedSubstitution, IrClass, IrExpr, IrFile, IrGenericSig, IrIntrinsic,
    IrSamTarget, IrTypeParameter, MemberExtProp, PropRef,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UndeterminedIrType {
    pub location: &'static str,
    pub ty: Ty,
}

fn reject(location: &'static str, ty: Ty) -> Result<(), UndeterminedIrType> {
    if ty.mentions_pending() || ty.mentions_error() {
        Err(UndeterminedIrType { location, ty })
    } else {
        Ok(())
    }
}

fn reject_all(
    location: &'static str,
    types: impl IntoIterator<Item = Ty>,
) -> Result<(), UndeterminedIrType> {
    for ty in types {
        reject(location, ty)?;
    }
    Ok(())
}

fn validate_intrinsic(operation: &IrIntrinsic) -> Result<(), UndeterminedIrType> {
    match operation {
        IrIntrinsic::PrimitiveCompare { operand } => reject("intrinsic operand", *operand),
        IrIntrinsic::UnsignedToString { source } => reject("intrinsic source", *source),
        IrIntrinsic::PrimitiveArrayNew { element } => reject("intrinsic array element", *element),
        IrIntrinsic::DataClassFieldEquals { ty }
        | IrIntrinsic::DataClassFieldHash { ty }
        | IrIntrinsic::DataClassArrayToString { ty } => reject("intrinsic property", *ty),
        IrIntrinsic::ArrayGet
        | IrIntrinsic::ArraySet
        | IrIntrinsic::ArraySize
        | IrIntrinsic::StringGet
        | IrIntrinsic::StringLength
        | IrIntrinsic::StringPlus
        | IrIntrinsic::NullableAnyToString
        | IrIntrinsic::CoroutineContext => Ok(()),
    }
}

fn validate_callee(callee: &Callee) -> Result<(), UndeterminedIrType> {
    match callee {
        Callee::Intrinsic { operation, ret } => {
            validate_intrinsic(operation)?;
            reject("callee result", *ret)
        }
        Callee::External {
            params,
            ret,
            substitutions,
            ..
        } => {
            reject_all("callee parameter", params.iter().copied())?;
            reject("callee result", *ret)?;
            for substitution in substitutions {
                validate_substitution(substitution)?;
            }
            Ok(())
        }
        Callee::CrossFile { params, ret, .. }
        | Callee::Module { params, ret, .. }
        | Callee::Super { params, ret, .. } => {
            reject_all("callee parameter", params.iter().copied())?;
            reject("callee result", *ret)
        }
        Callee::Virtual { params, .. } => {
            if let Some((params, ret)) = params {
                reject_all("virtual callee parameter", params.iter().copied())?;
                reject("virtual callee result", *ret)?;
            }
            Ok(())
        }
        Callee::Local(_)
        | Callee::ClassStatic { .. }
        | Callee::ClassStaticDefault { .. }
        | Callee::LocalDefault(_)
        | Callee::Static { .. }
        | Callee::Special { .. } => Ok(()),
    }
}

fn validate_argument(argument: &IrCheckedArgument) -> Result<(), UndeterminedIrType> {
    match argument {
        IrCheckedArgument::Vararg { array_type, .. } => reject("checked vararg array", *array_type),
        IrCheckedArgument::Expression { .. } | IrCheckedArgument::Default { .. } => Ok(()),
    }
}

fn validate_substitution(substitution: &IrCheckedSubstitution) -> Result<(), UndeterminedIrType> {
    reject("checked substitution", substitution.value)?;
    reject_all(
        "checked substitution bound",
        substitution.additional_bounds.iter().copied(),
    )
}

fn validate_constructor_target(
    target: &IrCheckedConstructorTarget,
) -> Result<(), UndeterminedIrType> {
    match target {
        IrCheckedConstructorTarget::External { parameters, .. } => {
            reject_all("checked constructor parameter", parameters.iter().copied())
        }
        IrCheckedConstructorTarget::Module(_) => Ok(()),
    }
}

fn validate_checked_operation(operation: &IrCheckedOperation) -> Result<(), UndeterminedIrType> {
    match operation {
        IrCheckedOperation::Call {
            arguments,
            substitutions,
            ..
        } => {
            for argument in arguments {
                validate_argument(argument)?;
            }
            for substitution in substitutions {
                validate_substitution(substitution)?;
            }
            Ok(())
        }
        IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => {
            for substitution in substitutions {
                validate_substitution(substitution)?;
            }
            Ok(())
        }
        IrCheckedOperation::ExternalPropertyRead {
            parameters,
            result,
            source_receiver,
            ..
        }
        | IrCheckedOperation::ExternalPropertyWrite {
            parameters,
            result,
            source_receiver,
            ..
        } => {
            reject_all("external property parameter", parameters.iter().copied())?;
            reject("external property result", *result)?;
            if let Some(receiver) = source_receiver {
                reject("external property receiver", *receiver)?;
            }
            Ok(())
        }
        IrCheckedOperation::ConstructorDelegation {
            target,
            outer_parameter,
            arguments,
            substitutions,
            ..
        } => {
            validate_constructor_target(target)?;
            if let Some(outer) = outer_parameter {
                reject("constructor outer parameter", *outer)?;
            }
            for argument in arguments {
                validate_argument(argument)?;
            }
            for substitution in substitutions {
                validate_substitution(substitution)?;
            }
            Ok(())
        }
        IrCheckedOperation::RangeConstruction {
            start_type,
            end_type,
            result,
            ..
        } => {
            reject("range start", *start_type)?;
            reject("range end", *end_type)?;
            reject("range result", *result)
        }
        IrCheckedOperation::RangeContains { counter, .. }
        | IrCheckedOperation::RangeLoop { counter, .. } => reject("range counter", *counter),
        IrCheckedOperation::CallableReference {
            function_type,
            substitutions,
            ..
        } => {
            reject("callable-reference type", *function_type)?;
            for substitution in substitutions {
                validate_substitution(substitution)?;
            }
            Ok(())
        }
        IrCheckedOperation::PropertyReference { substitutions, .. } => {
            for substitution in substitutions {
                validate_substitution(substitution)?;
            }
            Ok(())
        }
        IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. } => Ok(()),
    }
}

fn validate_sam(sam: &IrSamTarget) -> Result<(), UndeterminedIrType> {
    reject_all("SAM parameter", sam.parameters.iter().copied())?;
    reject("SAM result", sam.result)?;
    reject_all(
        "SAM declared parameter",
        sam.declared_parameters.iter().copied(),
    )?;
    reject("SAM declared result", sam.declared_result)
}

fn validate_expr(expression: &IrExpr) -> Result<(), UndeterminedIrType> {
    match expression {
        IrExpr::Checked(operation) => validate_checked_operation(operation),
        IrExpr::KClassLiteral { classifier, .. } => {
            classifier.map_or(Ok(()), |ty| reject("class literal", ty))
        }
        IrExpr::LocalPropertyReference { property_type, .. } => {
            reject("local property reference", *property_type)
        }
        IrExpr::Call { callee, .. } => validate_callee(callee),
        IrExpr::TypeOp { type_operand, .. } => reject("type operation", *type_operand),
        IrExpr::Variable { ty, .. } => reject("local variable", *ty),
        IrExpr::PrimitiveNeg { ty, .. } => reject("primitive negation", *ty),
        IrExpr::PropertyRead { ty, .. } | IrExpr::PropertyWrite { ty, .. } => {
            reject("property operation", *ty)
        }
        IrExpr::New { ctor_params, .. } => {
            if let Some(params) = ctor_params {
                reject_all("constructor parameter", params.iter().copied())?;
            }
            Ok(())
        }
        IrExpr::Lambda { sam, .. } => sam.as_ref().map_or(Ok(()), validate_sam),
        IrExpr::InvokeFunction { params, ret, .. } => {
            reject_all("function-value parameter", params.iter().copied())?;
            reject("function-value result", *ret)
        }
        IrExpr::RefNew { elem, .. } | IrExpr::RefGet { elem, .. } | IrExpr::RefSet { elem, .. } => {
            reject("shared local element", *elem)
        }
        IrExpr::Vararg { array_type, .. } | IrExpr::NewArray { array_type, .. } => {
            reject("array type", *array_type)
        }
        IrExpr::Try { result, .. } => reject("try result", *result),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::SingletonValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::SetValue { .. }
        | IrExpr::PluginPlaceholder { .. }
        | IrExpr::Return(_)
        | IrExpr::Block { .. }
        | IrExpr::When { .. }
        | IrExpr::While { .. }
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::StringConcat(_)
        | IrExpr::EnclosingInstance { .. }
        | IrExpr::GetField { .. }
        | IrExpr::LateinitInitialized { .. }
        | IrExpr::SetField { .. }
        | IrExpr::GetStatic(_)
        | IrExpr::SetStatic { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::EnumValueOf { .. }
        | IrExpr::EnumEntries { .. }
        | IrExpr::ReifiedClassMarker { .. }
        | IrExpr::ReifiedTypeOp { .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation
        | IrExpr::NotNullAssert { .. }
        | IrExpr::LateinitCheck { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::Throw { .. } => Ok(()),
    }
}

fn validate_type_parameter(parameter: &IrTypeParameter) -> Result<(), UndeterminedIrType> {
    reject_all(
        "type-parameter bound",
        parameter.bounds.iter().map(|(ty, _)| *ty),
    )
}

fn validate_generic_signature(signature: &IrGenericSig) -> Result<(), UndeterminedIrType> {
    for parameter in &signature.type_params {
        validate_type_parameter(parameter)?;
    }
    reject_all(
        "generic signature parameter",
        signature.params.iter().copied(),
    )?;
    if let Some(ret) = signature.ret {
        reject("generic signature result", ret)?;
    }
    reject_all(
        "generic signature supertype",
        signature.supers.iter().copied(),
    )
}

fn validate_func_ref(reference: &FuncRef) -> Result<(), UndeterminedIrType> {
    if let Some(ty) = reference.reflection_target_ret_ty {
        reject("function-reference reflected result", ty)?;
    }
    if let Some(params) = &reference.reflection_target_param_tys {
        reject_all(
            "function-reference reflected parameter",
            params.iter().copied(),
        )?;
    }
    reject_all(
        "function-reference parameter",
        reference.param_tys.iter().copied(),
    )?;
    reject("function-reference result", reference.ret_ty)?;
    reject_all(
        "function-reference target parameter",
        reference.target_param_tys.iter().copied(),
    )?;
    reject("function-reference target result", reference.target_ret_ty)
}

fn validate_prop_ref(reference: &PropRef) -> Result<(), UndeterminedIrType> {
    reject("property-reference type", reference.prop_ty)
}

fn validate_member_extension(property: &MemberExtProp) -> Result<(), UndeterminedIrType> {
    reject("member-extension receiver", property.receiver)?;
    reject("member-extension property", property.ty)?;
    for parameter in &property.type_params {
        validate_type_parameter(parameter)?;
    }
    Ok(())
}

fn validate_class(class: &IrClass) -> Result<(), UndeterminedIrType> {
    reject_all(
        "class type-parameter bound",
        class.type_param_bounds.iter().map(|(_, ty)| *ty),
    )?;
    reject_all("class supertype", class.supertypes.iter().copied())?;
    for property in &class.properties {
        reject_all(
            "property context parameter",
            property.context_params.iter().map(|(_, ty)| *ty),
        )?;
        reject("property declaration", property.ty)?;
        if let Some(storage) = property.storage_ty {
            reject("property storage", storage)?;
        }
    }
    for field in &class.fields {
        reject("class field", field.ty)?;
    }
    for argument in &class.ctor_args {
        reject("constructor argument", argument.ty)?;
        if let Some(declared) = argument.declared_ty {
            reject("declared constructor argument", declared)?;
        }
    }
    reject_all(
        "super-constructor parameter",
        class.super_ctor_params.iter().copied(),
    )?;
    if let Some(types) = &class.enum_entry_of {
        reject_all("enum-entry constructor parameter", types.iter().copied())?;
    }
    if let Some(reference) = &class.prop_ref {
        validate_prop_ref(reference)?;
    }
    if let Some(reference) = &class.func_ref {
        validate_func_ref(reference)?;
    }
    for bridge in &class.bridges {
        reject_all(
            "bridge erased parameter",
            bridge.erased_params.iter().copied(),
        )?;
        reject("bridge erased result", bridge.erased_ret)?;
        reject_all(
            "bridge concrete parameter",
            bridge.concrete_params.iter().copied(),
        )?;
        reject("bridge concrete result", bridge.concrete_ret)?;
    }
    for constructor in &class.secondary_ctors {
        reject_all(
            "secondary constructor prefix",
            constructor.prefix_params.iter().copied(),
        )?;
        reject_all(
            "secondary constructor parameter",
            constructor.params.iter().copied(),
        )?;
        reject_all(
            "declared secondary constructor parameter",
            constructor.named_params.iter().map(|(_, ty)| *ty),
        )?;
        match &constructor.delegate {
            CtorDelegateTarget::This { target_params, .. }
            | CtorDelegateTarget::Super { target_params, .. } => reject_all(
                "secondary constructor delegation parameter",
                target_params.iter().copied(),
            )?,
        }
    }
    Ok(())
}

impl IrFile {
    /// Prove that common IR contains only semantic Kotlin types before a backend is invoked.
    pub fn validate_determined_types(&self) -> Result<(), UndeterminedIrType> {
        for function in &self.functions {
            reject_all("function parameter", function.params.iter().copied())?;
            reject("function result", function.ret)?;
        }
        for constructor in self.checked_constructor_bodies.values() {
            reject_all(
                "checked constructor-body parameter",
                constructor.parameters.iter().map(|(_, ty)| *ty),
            )?;
        }
        for property in self.checked_properties.values() {
            reject("checked property", property.ty)?;
            if let Some(storage) = property.storage_ty {
                reject("checked property storage", storage)?;
            }
        }
        reject_all(
            "shared capture parameter",
            self.shared_capture_parameters.values().copied(),
        )?;
        reject_all(
            "shared class capture field",
            self.shared_class_capture_fields.values().copied(),
        )?;
        for class in &self.classes {
            validate_class(class)?;
        }
        for aliases in self.class_type_aliases.values() {
            reject_all(
                "type-alias expansion",
                aliases.iter().map(|alias| alias.expansion),
            )?;
        }
        for function in &self.package_functions {
            reject_all(
                "package-function parameter",
                function.params.iter().map(|(_, ty)| *ty),
            )?;
            reject("package-function result", function.ret)?;
            if let Some(receiver) = function.receiver {
                reject("package-function receiver", receiver)?;
            }
            reject_all(
                "package-function type-parameter bound",
                function
                    .type_params
                    .iter()
                    .flat_map(|parameter| parameter.bounds.iter().copied()),
            )?;
            if let Some(bound) = function.equality_bound {
                reject("package-function equality bound", bound)?;
            }
        }
        for property in &self.package_properties {
            reject("package-property type", property.ty)?;
            if let Some(receiver) = property.receiver {
                reject("package-property receiver", receiver)?;
            }
            reject_all(
                "package-property context parameter",
                property.context_parameters.iter().copied(),
            )?;
            reject_all(
                "package-property type-parameter bound",
                property
                    .type_params
                    .iter()
                    .flat_map(|parameter| parameter.bounds.iter().copied()),
            )?;
        }
        for property in self.referenced_module_properties.values() {
            reject("referenced module property", property.ty)?;
            reject_all(
                "referenced module property context parameter",
                property.context_parameters.iter().copied(),
            )?;
            if let Some(receiver) = property.extension_receiver {
                reject("referenced module property receiver", receiver)?;
            }
        }
        for hierarchy in self.classifier_hierarchies.values() {
            reject_all(
                "applied classifier hierarchy",
                hierarchy.iter().map(|entry| entry.applied),
            )?;
        }
        for overrides in self.property_overrides.values() {
            for edge in overrides {
                reject("overridden property declaration type", edge.declared_type)?;
                reject("overridden property applied type", edge.applied_type)?;
                reject("overriding property type", edge.implementation_type)?;
            }
        }
        for overrides in self.function_overrides.values() {
            for edge in overrides {
                reject_all(
                    "overridden function declaration parameter",
                    edge.declared_parameters.iter().copied(),
                )?;
                reject(
                    "overridden function declaration result",
                    edge.declared_result,
                )?;
                reject_all(
                    "overridden function applied parameter",
                    edge.applied_parameters.iter().copied(),
                )?;
                reject("overridden function applied result", edge.applied_result)?;
                reject_all(
                    "overriding function parameter",
                    edge.implementation_parameters.iter().copied(),
                )?;
                reject("overriding function result", edge.implementation_result)?;
            }
        }
        reject_all(
            "package type-alias expansion",
            self.package_type_aliases
                .iter()
                .map(|alias| alias.expansion),
        )?;
        for constructors in self.jvm_value_class_secondary_ctors.values() {
            for constructor in constructors {
                reject_all(
                    "value-class secondary constructor parameter",
                    constructor.params.iter().map(|(_, ty)| *ty),
                )?;
            }
        }
        for property in &self.statics {
            reject("static property", property.ty)?;
        }
        for expression in &self.exprs {
            validate_expr(expression)?;
        }
        for construction in self.annotation_constructions.values() {
            reject_all(
                "annotation member",
                construction.members.iter().map(|(_, ty)| *ty),
            )?;
        }
        reject_all("logical expression", self.logical_types.values().copied())?;
        reject_all("exhaustive when", self.exhaustive_whens.values().copied())?;
        reject_all("physical expression", self.physical_types.values().copied())?;
        for properties in self.member_ext_props.values() {
            for property in properties {
                validate_member_extension(property)?;
            }
        }
        reject_all("equality bound", self.fn_equality_bounds.values().copied())?;
        reject_all(
            "default-stub boxed parameter",
            self.default_stub_boxed_params
                .values()
                .flatten()
                .map(|(_, ty)| *ty),
        )?;
        reject_all(
            "value-class constructor parameter",
            self.vc_ctor_declared_params.values().flatten().copied(),
        )?;
        for (params, ret) in self.suspend_declared_sigs.values() {
            reject_all("suspend declaration parameter", params.iter().copied())?;
            reject("suspend declaration result", *ret)?;
        }
        for (_, params, ret) in self.vc_declared_sigs.values() {
            reject_all("value-class declaration parameter", params.iter().copied())?;
            reject("value-class declaration result", *ret)?;
        }
        reject_all("suspend call", self.suspend_calls.values().copied())?;
        reject_all(
            "intrinsic suspension point",
            self.intrinsic_suspension_points
                .values()
                .map(|point| point.result),
        )?;
        for signature in self
            .signatures
            .values()
            .chain(self.class_signatures.values())
        {
            validate_generic_signature(signature)?;
        }
        for (params, ret) in self.member_semantic_sigs.values() {
            reject_all("member semantic parameter", params.iter().copied())?;
            reject("member semantic result", *ret)?;
        }
        reject_all(
            "external value-class storage",
            self.external_value_classes.values().copied(),
        )?;
        reject_all(
            "erased value construction",
            self.erased_value_constructions.values().map(|(_, ty)| *ty),
        )?;
        reject_all(
            "reified call substitution",
            self.reified_call_subst
                .values()
                .flatten()
                .map(|(_, ty)| *ty),
        )?;
        reject_all(
            "extension call receiver",
            self.ext_call_source_receiver.values().copied(),
        )?;
        reject_all(
            "declared call result",
            self.call_declared_ret.values().copied(),
        )?;
        reject_all(
            "declared call parameter",
            self.call_declared_params.values().flatten().copied(),
        )?;
        reject_all(
            "property declaration type",
            self.property_declaration_types.values().copied(),
        )?;
        reject_all(
            "selected property accessor",
            self.property_selected_accessors.values().map(|(_, ty)| *ty),
        )?;
        reject_all(
            "property accessor realization",
            self.property_accessor_jvm_realizations
                .values()
                .map(|(_, ty)| *ty),
        )?;
        for (params, ret) in self
            .lambda_sam_signature
            .values()
            .chain(self.lambda_sam_jvm_signature.values())
        {
            reject_all("lambda SAM parameter", params.iter().copied())?;
            reject("lambda SAM result", *ret)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrExpr, IrFunction};

    #[test]
    fn rejects_an_undetermined_declaration_signature() {
        let mut ir = IrFile::default();
        ir.functions.push(IrFunction {
            name: "broken".to_owned(),
            params: vec![Ty::Pending],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });

        assert_eq!(
            ir.validate_determined_types(),
            Err(UndeterminedIrType {
                location: "function parameter",
                ty: Ty::Pending,
            })
        );
    }

    #[test]
    fn rejects_an_undetermined_nested_operation_type() {
        let mut ir = IrFile::default();
        ir.exprs.push(IrExpr::Call {
            callee: Callee::CrossFile {
                facade: crate::types::type_name("OtherKt"),
                name: "broken".to_owned(),
                params: vec![Ty::String],
                ret: Ty::Error,
            },
            dispatch_receiver: None,
            args: Vec::new(),
        });

        assert_eq!(
            ir.validate_determined_types(),
            Err(UndeterminedIrType {
                location: "callee result",
                ty: Ty::Error,
            })
        );
    }
}
