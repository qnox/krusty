//! Backend-neutral local-class naming contracts retained by common IR.

use std::collections::{HashMap, HashSet};

use crate::types::{Ty, TypeName};

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
        IrCheckedOperation::IllegalProgressionStep { .. } => return,
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
                | super::IrIntrinsic::TypeOf { ty: classifier } => {
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
        IrExpr::LocalPropertyReference(reference) => {
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
        }
        IrExpr::Try {
            catches, result, ..
        } => {
            *result = ty(*result, names);
            for catch in catches {
                catch.ty = ty(catch.ty, names);
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
        for function in &mut self.functions {
            tys(&mut function.params, names);
            function.ret = ty(function.ret, names);
            if let Some(owner) = &mut function.dispatch_receiver {
                name(owner, names);
            }
        }
        for plan in &mut self.local_delegate_plans {
            if let Some(class) = &mut plan.reference.class {
                name(class, names);
            }
            plan.reference.property_type = ty(plan.reference.property_type, names);
            for accessor in std::iter::once(&mut plan.getter).chain(plan.setter.iter_mut()) {
                tys(&mut accessor.parameters, names);
                type_parameters(&mut accessor.type_parameters, names);
                accessor.result = ty(accessor.result, names);
            }
        }
        for class in &mut self.classes {
            remap_class(class, names);
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
        for underlying in self.external_value_classes.values_mut() {
            *underlying = ty(*underlying, names);
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
        self.logical_types
            .values_mut()
            .for_each(|value| *value = ty(*value, names));
        self.inline_operand_declared_types_mut()
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
            .for_each(|value| *value = names.get(value).copied().unwrap_or(*value));
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
        self.callable_reference_names
            .values_mut()
            .for_each(|value| name(value, names));
        self.lambda_class_names
            .values_mut()
            .for_each(|value| name(value, names));
        remap_keyed(&mut self.declaration_paths, names);

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

    /// Rename classifiers reachable from one inline expansion.
    ///
    /// [`Self::remap_classifier_identities`] rewrites the whole file, including the declaration a
    /// copy was taken from. This walk uses that same type, expression, and class-field contract on
    /// `roots`, on functions whose bodies those roots own, and on classes named by a value of
    /// `names`.
    ///
    /// Cloning an expression shares a lambda's `impl_fn` with the inline template. Remapping that
    /// function in place would retarget every call site. An implementation referenced from outside
    /// this walk is cloned once. `detached_impls` records those clones so a later walk of the same
    /// expansion updates them in place instead of cloning again.
    pub(crate) fn remap_reachable_classifier_identities(
        &mut self,
        names: &HashMap<TypeName, TypeName>,
        roots: impl IntoIterator<Item = ExprId>,
        detached_impls: &mut HashSet<FunId>,
    ) {
        if names.is_empty() {
            return;
        }
        let lambda_sites = self
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(index, expr)| match expr {
                IrExpr::Lambda { impl_fn, .. } => Some((index as ExprId, *impl_fn)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut closure = HashSet::new();
        let mut pending = roots.into_iter().collect::<Vec<_>>();
        while let Some(expression) = pending.pop() {
            if !closure.insert(expression) {
                continue;
            }
            super::for_each_child(&self.exprs, expression, &mut |child| pending.push(child));
        }

        let mut seen = HashSet::new();
        let mut pending = closure.iter().copied().collect::<Vec<_>>();
        let mut local_clones = HashMap::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            if let IrExpr::Lambda { impl_fn, .. } = self.exprs[expression as usize] {
                let impl_fn = if let Some(existing) = detached_impl(
                    impl_fn,
                    expression,
                    &lambda_sites,
                    &closure,
                    &local_clones,
                    detached_impls,
                ) {
                    existing
                } else {
                    let cloned = detach_lambda_impl(self, impl_fn);
                    local_clones.insert(impl_fn, cloned);
                    detached_impls.insert(cloned);
                    cloned
                };
                if let IrExpr::Lambda { impl_fn: slot, .. } = &mut self.exprs[expression as usize] {
                    *slot = impl_fn;
                }
                if let Some(body) = self.functions[impl_fn as usize].body {
                    pending.push(body);
                }
            }
            remap_expression(&mut self.exprs[expression as usize], names);
            let mut children = Vec::new();
            super::for_each_child(&self.exprs, expression, &mut |child| children.push(child));
            pending.extend(children);
        }

        let owned = self
            .functions
            .iter()
            .enumerate()
            .filter_map(|(index, function)| {
                function
                    .body
                    .is_some_and(|body| seen.contains(&body))
                    .then_some(index as FunId)
            })
            .collect::<Vec<_>>();
        for function in owned {
            remap_owned_function(self, function, names);
        }

        let placeholders = names.values().copied().collect::<HashSet<_>>();
        let copies = self
            .classes
            .iter()
            .enumerate()
            .filter_map(|(index, class)| {
                placeholders
                    .contains(&class.fq_name)
                    .then_some(index as ClassId)
            })
            .collect::<Vec<_>>();
        for class in copies {
            remap_class(&mut self.classes[class as usize], names);
            remap_class_value(&mut self.shared_class_capture_fields, class, names);
            remap_class_value(&mut self.shared_super_capture_parameters, class, names);
            remap_secondary_capture(self, class, names);
        }
        for expression_id in seen {
            remap_expression_facts(self, expression_id, names);
        }
    }
}

fn detached_impl(
    impl_fn: FunId,
    expression: ExprId,
    lambda_sites: &[(ExprId, FunId)],
    closure: &HashSet<ExprId>,
    local_clones: &HashMap<FunId, FunId>,
    detached_impls: &HashSet<FunId>,
) -> Option<FunId> {
    if detached_impls.contains(&impl_fn) {
        return Some(impl_fn);
    }
    if let Some(cloned) = local_clones.get(&impl_fn).copied() {
        return Some(cloned);
    }
    let shared = lambda_sites.iter().any(|(site, function)| {
        *function == impl_fn && *site != expression && !closure.contains(site)
    });
    if shared {
        None
    } else {
        Some(impl_fn)
    }
}

fn detach_lambda_impl(ir: &mut super::IrFile, source: FunId) -> FunId {
    let mut shape = ir.functions[source as usize].clone();
    shape.body = shape
        .body
        .map(|body| super::clone_expression_dag(ir, body).0);
    super::clone::clone_class_method(ir, source, shape, &HashMap::new())
}

fn remap_owned_function(
    ir: &mut super::IrFile,
    function: FunId,
    names: &HashMap<TypeName, TypeName>,
) {
    {
        let shape = &mut ir.functions[function as usize];
        tys(&mut shape.params, names);
        shape.ret = ty(shape.ret, names);
        if let Some(owner) = &mut shape.dispatch_receiver {
            name(owner, names);
        }
    }
    if let Some(applied) = ir.function_annotations.get_mut(&function) {
        annotations(applied, names);
    }
    if let Some(parameters) = ir.fn_param_annotations.get_mut(&function) {
        for parameter in parameters {
            annotations(parameter, names);
        }
    }
    if let Some(signature) = ir.signatures.get_mut(&function) {
        generic_signature(signature, names);
    }
    if let Some(parameters) = ir.callable_bound_type_parameters.get_mut(&function) {
        type_parameters(parameters, names);
    }
    if let Some((parameters, result)) = ir.member_semantic_sigs.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some((parameters, result)) = ir.suspend_declared_sigs.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some((_, parameters, result)) = ir.vc_declared_sigs.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some(parameters) = ir.default_stub_boxed_params.get_mut(&function) {
        for (_, parameter) in parameters {
            *parameter = ty(*parameter, names);
        }
    }
    if let Some(result) = ir.value_class_suspend_returns.get_mut(&function) {
        value_class_suspend_result(result, names);
    }
    if let Some((classifier, _)) = ir.jvm_suspend_impl_bodies.get_mut(&function) {
        name(classifier, names);
    }
    if let Some((parameters, result)) = ir.lambda_sam_signature.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some((parameters, result)) = ir.lambda_sam_jvm_signature.get_mut(&function) {
        tys(parameters, names);
        *result = ty(*result, names);
    }
    if let Some(origin) = ir.lambda_origins.get_mut(&function) {
        origin
            .lexical_owner
            .iter_mut()
            .for_each(|value| name(value, names));
    }
    if let Some(classifier) = ir.lambda_class_names.get_mut(&function) {
        name(classifier, names);
    }
    let captures = ir
        .shared_capture_parameters
        .iter()
        .filter(|((owner, _), _)| *owner == function)
        .map(|((_, ordinal), value)| (*ordinal, *value))
        .collect::<Vec<_>>();
    for (ordinal, value) in captures {
        ir.shared_capture_parameters
            .insert((function, ordinal), ty(value, names));
    }
}

fn remap_class_value<K>(
    map: &mut HashMap<(ClassId, K), Ty>,
    class: ClassId,
    names: &HashMap<TypeName, TypeName>,
) where
    K: Eq + std::hash::Hash + Copy,
{
    let keys = map
        .keys()
        .copied()
        .filter(|(owner, _)| *owner == class)
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(value) = map.get_mut(&key) {
            *value = ty(*value, names);
        }
    }
}

fn remap_secondary_capture(
    ir: &mut super::IrFile,
    class: ClassId,
    names: &HashMap<TypeName, TypeName>,
) {
    let keys = ir
        .shared_secondary_super_capture_parameters
        .keys()
        .copied()
        .filter(|(owner, _, _)| *owner == class)
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(value) = ir.shared_secondary_super_capture_parameters.get_mut(&key) {
            *value = ty(*value, names);
        }
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
    if let Some(value) = ir.inline_operand_declared_type_mut(expression) {
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
            .enclosing_class
            .iter_mut()
            .for_each(|value| name(value, names));
    }
    if let Some(classifier) = ir.callable_reference_names.get_mut(&expression) {
        name(classifier, names);
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
