//! Backend-neutral local-class naming contracts retained by common IR.

use std::collections::HashMap;

use crate::types::{Ty, TypeName};

use super::{
    Callee, ClassId, IrCallableReferenceTarget, IrCheckedArgument, IrCheckedOperation, IrExpr,
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
}

fn name(name: &mut TypeName, names: &HashMap<TypeName, TypeName>) {
    if let Some(replacement) = names.get(name) {
        *name = *replacement;
    }
}

fn ty(value: Ty, names: &HashMap<TypeName, TypeName>) -> Ty {
    match value {
        Ty::Obj(classifier, arguments) => Ty::obj_args_name(
            names.get(&classifier).copied().unwrap_or(classifier),
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
        other => other,
    }
}

fn tys(values: &mut [Ty], names: &HashMap<TypeName, TypeName>) {
    values
        .iter_mut()
        .for_each(|value| *value = ty(*value, names));
}

fn checked_arguments(values: &mut [IrCheckedArgument], names: &HashMap<TypeName, TypeName>) {
    for argument in values {
        if let IrCheckedArgument::Vararg { array_type, .. } = argument {
            *array_type = ty(*array_type, names);
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
        IrCheckedOperation::RangeContains { counter, .. }
        | IrCheckedOperation::RangeLoop { counter, .. } => {
            *counter = ty(*counter, names);
            return;
        }
        IrCheckedOperation::ProgressionMember { class, .. } => {
            *class = ty(*class, names);
            return;
        }
        IrCheckedOperation::CallableReference {
            function_type,
            substitutions,
            ..
        } => {
            *function_type = ty(*function_type, names);
            substitutions
        }
        IrCheckedOperation::PropertyReference { substitutions, .. } => substitutions,
        IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. } => return,
    };
    for substitution in substitutions {
        substitution.value = ty(substitution.value, names);
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
                | super::IrIntrinsic::PrimitiveCompare {
                    operand: classifier,
                    ..
                }
                | super::IrIntrinsic::UnsignedToString { source: classifier }
                | super::IrIntrinsic::PrimitiveArrayNew {
                    element: classifier,
                }
                | super::IrIntrinsic::DataClassFieldEquals { ty: classifier }
                | super::IrIntrinsic::DataClassFieldHash { ty: classifier }
                | super::IrIntrinsic::DataClassArrayToString { ty: classifier } => {
                    *classifier = ty(*classifier, names)
                }
                _ => {}
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

fn expression(expression: &mut IrExpr, names: &HashMap<TypeName, TypeName>) {
    match expression {
        IrExpr::Checked(operation) => checked_operation(operation, names),
        IrExpr::CallableReference(reference) => {
            match &mut reference.target {
                IrCallableReferenceTarget::Constructor { classifier } => name(classifier, names),
                IrCallableReferenceTarget::Local {
                    owner: Some(owner), ..
                } => name(owner, names),
                _ => {}
            }
            reference.function_type = ty(reference.function_type, names);
            tys(&mut reference.declaration_parameters, names);
            reference.declaration_result = ty(reference.declaration_result, names);
        }
        IrExpr::ClassConst {
            internal: Some(classifier),
        }
        | IrExpr::SingletonValue { classifier }
        | IrExpr::EnumEntry { classifier, .. }
        | IrExpr::EnumValues { classifier }
        | IrExpr::EnumValueOf { classifier, .. }
        | IrExpr::EnumEntries { classifier } => name(classifier, names),
        IrExpr::KClassLiteral {
            classifier: Some(classifier),
            ..
        }
        | IrExpr::LocalPropertyReference {
            property_type: classifier,
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
        }
        IrExpr::Try {
            catches, result, ..
        } => {
            *result = ty(*result, names);
            for catch in catches {
                name(&mut catch.exc_internal, names);
            }
        }
        _ => {}
    }
}

fn annotation_value(value: &mut super::AnnoValue, names: &HashMap<TypeName, TypeName>) {
    match value {
        super::AnnoValue::Enum(classifier, _) | super::AnnoValue::Class(classifier) => {
            name(classifier, names)
        }
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
    for retained in &mut annotations.0 {
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

fn type_alias(alias: &mut super::IrTypeAlias, names: &HashMap<TypeName, TypeName>) {
    alias.expansion = ty(alias.expansion, names);
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
        super::IrValueClassSuspendResult::Carrier(carrier) => *carrier = ty(*carrier, names),
    }
}

fn remap_keyed<V>(map: &mut HashMap<TypeName, V>, names: &HashMap<TypeName, TypeName>) {
    *map = std::mem::take(map)
        .into_iter()
        .map(|(key, value)| (names.get(&key).copied().unwrap_or(key), value))
        .collect();
}

fn remap_first_key<K: Eq + std::hash::Hash, V>(
    map: &mut HashMap<(TypeName, K), V>,
    names: &HashMap<TypeName, TypeName>,
) {
    *map = std::mem::take(map)
        .into_iter()
        .map(|((owner, key), value)| ((names.get(&owner).copied().unwrap_or(owner), key), value))
        .collect();
}

impl super::IrFile {
    /// Replace semantic classifier identities with target physical identities at a backend boundary.
    /// The map is exact and declaration-produced; this operation performs no spelling lookup.
    pub(crate) fn remap_classifier_identities(&mut self, names: &HashMap<TypeName, TypeName>) {
        if names.is_empty() {
            return;
        }

        annotations(&mut self.file_annotations, names);
        for function in &mut self.functions {
            tys(&mut function.params, names);
            function.ret = ty(function.ret, names);
            if let Some(owner) = &mut function.dispatch_receiver {
                name(owner, names);
            }
        }
        for class in &mut self.classes {
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
            }
            class
                .annotation_impl_of
                .iter_mut()
                .for_each(|value| name(value, names));
            class
                .sealed_subclasses
                .remap(|value| names.get(&value).copied().unwrap_or(value));
            name(&mut class.superclass, names);
            tys(&mut class.super_ctor_params, names);
            for entry in &mut class.enum_entries {
                tys(&mut entry.constructor_parameter_types, names);
                entry
                    .subclass
                    .iter_mut()
                    .for_each(|value| name(value, names));
            }
            if let Some(parameters) = &mut class.enum_entry_of {
                tys(parameters, names);
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
                tys(&mut bridge.erased_params, names);
                bridge.erased_ret = ty(bridge.erased_ret, names);
                tys(&mut bridge.concrete_params, names);
                bridge.concrete_ret = ty(bridge.concrete_ret, names);
                bridge.target_ret = bridge.target_ret.map(|value| ty(value, names));
                bridge
                    .box_ret
                    .iter_mut()
                    .for_each(|value| name(value, names));
                bridge
                    .unbox_params
                    .iter_mut()
                    .flatten()
                    .for_each(|value| name(value, names));
            }
            class
                .interfaces
                .remap(|value| names.get(&value).copied().unwrap_or(value));
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
                    super::CtorDelegateTarget::This { target_params, .. } => {
                        tys(target_params, names)
                    }
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
            expression(expression_value, names);
        }

        for callable in self.referenced_module_callables.values_mut() {
            name(&mut callable.source.package, names);
            callable
                .owner
                .iter_mut()
                .for_each(|value| name(value, names));
            tys(&mut callable.parameters, names);
            callable.result = ty(callable.result, names);
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
                .enclosing_class
                .iter_mut()
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
            function.equality_bound = function.equality_bound.map(|value| ty(value, names));
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
        for (parameters, result) in self.member_semantic_sigs.values_mut() {
            tys(parameters, names);
            *result = ty(*result, names);
        }
        for signature in self.class_signatures.values_mut() {
            generic_signature(signature, names);
        }
        for underlying in self.external_value_classes.values_mut() {
            *underlying = ty(*underlying, names);
        }
        for substitutions in self.reified_call_subst.values_mut() {
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
        for (classifier, _) in self.jvm_suspend_interface_bodies.values_mut() {
            name(classifier, names);
        }
        for (classifier, underlying) in self.erased_value_constructions.values_mut() {
            name(classifier, names);
            *underlying = ty(*underlying, names);
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
        self.logical_types
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.physical_types
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.exhaustive_whens
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.fn_equality_bounds
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.suspend_calls
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.ext_call_source_receiver
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
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
        }

        remap_keyed(&mut self.referenced_module_classifiers, names);
        remap_keyed(&mut self.classifier_hierarchies, names);
        remap_keyed(&mut self.property_overrides, names);
        remap_keyed(&mut self.function_overrides, names);
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
        remap_keyed(&mut self.class_ctor_defaults, names);
        remap_keyed(&mut self.class_signatures, names);
        remap_keyed(&mut self.field_signatures, names);
        remap_keyed(&mut self.external_value_classes, names);

        remap_first_key(&mut self.synthesized_data_class_members, names);
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
            .map(|value| names.get(&value).copied().unwrap_or(value))
            .collect();
        self.deprecated_classes = std::mem::take(&mut self.deprecated_classes)
            .into_iter()
            .map(|value| names.get(&value).copied().unwrap_or(value))
            .collect();
        self.public_synthetics = std::mem::take(&mut self.public_synthetics)
            .into_iter()
            .map(|value| names.get(&value).copied().unwrap_or(value))
            .collect();
        self.module_source_value_classes = std::mem::take(&mut self.module_source_value_classes)
            .into_iter()
            .map(|value| names.get(&value).copied().unwrap_or(value))
            .collect();
        self.module_readable_value_classes =
            std::mem::take(&mut self.module_readable_value_classes)
                .into_iter()
                .map(|value| names.get(&value).copied().unwrap_or(value))
                .collect();
    }
}
