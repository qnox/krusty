//! Mechanical bridge from authoritative common metadata to private JVM builtins records.
//!
//! KLIB decoding and semantic type conversion are owned by [`crate::metadata::semantic`]. The JVM
//! classpath still has private `Builtin*` consumers; this explicitly scoped bridge adapts those
//! records without creating another KLIB decoder or provider path.

use super::BuiltinPackage;
use crate::metadata::semantic as common;

pub(crate) use common::PackageFragmentDecodeError;

/// Decode one dependency-owned fragment atomically. Structural and semantic parsing are both
/// fallible: a nested declaration is published only after all of its names, types, parameters and
/// table references have resolved successfully.
pub(crate) fn parse_package_fragment_checked(
    bytes: &[u8],
) -> Result<BuiltinPackage, PackageFragmentDecodeError> {
    common::parse_package_fragment_checked(bytes).map(package_from_common)
}

pub(super) fn ty_from_common(ty: common::KotlinType) -> super::BuiltinTy {
    match ty {
        common::KotlinType::Class {
            internal,
            args,
            nullable,
            shape,
        } => super::BuiltinTy::Class {
            internal,
            args: args.into_iter().map(ty_from_common).collect(),
            nullable,
            shape,
        },
        common::KotlinType::Param { name, nullable } => super::BuiltinTy::Param { name, nullable },
        common::KotlinType::InProjection(inner) => {
            super::BuiltinTy::InProjection(Box::new(ty_from_common(*inner)))
        }
        common::KotlinType::OutProjection(inner) => {
            super::BuiltinTy::OutProjection(Box::new(ty_from_common(*inner)))
        }
    }
}

pub(crate) fn ty_to_common(ty: &super::BuiltinTy) -> common::KotlinType {
    match ty {
        super::BuiltinTy::Class {
            internal,
            args,
            nullable,
            shape,
        } => common::KotlinType::Class {
            internal: internal.clone(),
            args: args.iter().map(ty_to_common).collect(),
            nullable: *nullable,
            shape: *shape,
        },
        super::BuiltinTy::Param { name, nullable } => common::KotlinType::Param {
            name: name.clone(),
            nullable: *nullable,
        },
        super::BuiltinTy::InProjection(inner) => {
            common::KotlinType::InProjection(Box::new(ty_to_common(inner)))
        }
        super::BuiltinTy::OutProjection(inner) => {
            common::KotlinType::OutProjection(Box::new(ty_to_common(inner)))
        }
    }
}

fn type_param_from_common(parameter: common::KotlinTypeParameter) -> super::BuiltinTypeParam {
    super::BuiltinTypeParam {
        name: parameter.name,
        bounds: parameter.bounds.into_iter().map(ty_from_common).collect(),
        variance: parameter.variance,
        only_input: parameter.only_input,
    }
}

fn package_from_common(package: common::KotlinPackage) -> BuiltinPackage {
    BuiltinPackage {
        classes: package
            .classes
            .into_iter()
            .map(|(name, class)| {
                let flags = class.metadata_flags;
                (
                    name,
                    super::BuiltinClass {
                        supertypes: class.supertypes,
                        supertype_tys: class
                            .supertype_tys
                            .into_iter()
                            .map(ty_from_common)
                            .collect(),
                        members: class
                            .members
                            .into_iter()
                            .map(|member| super::BuiltinMember {
                                name: member.name,
                                params: member.params.into_iter().map(ty_from_common).collect(),
                                ret: ty_from_common(member.ret),
                                is_property: member.is_property,
                                is_operator: member.is_operator,
                                is_infix: member.is_infix,
                                is_abstract: member.is_abstract,
                                formals: member
                                    .formals
                                    .into_iter()
                                    .map(type_param_from_common)
                                    .collect(),
                                ret_nullable: member.ret_nullable,
                                constant: member.constant,
                                param_names: member.param_names,
                                param_defaults: member.param_defaults,
                                vararg: member.vararg,
                            })
                            .collect(),
                        constructors: class
                            .constructors
                            .into_iter()
                            .map(|constructor| super::BuiltinConstructor {
                                params: constructor
                                    .params
                                    .into_iter()
                                    .map(ty_from_common)
                                    .collect(),
                                param_names: constructor.param_names,
                                param_defaults: constructor.param_defaults,
                                vararg: constructor.vararg,
                                visibility: constructor.visibility,
                            })
                            .collect(),
                        companion_name: class.companion_name,
                        type_params: class
                            .type_params
                            .into_iter()
                            .map(type_param_from_common)
                            .collect(),
                        kind: class.kind,
                        is_fun_interface: class.is_fun_interface,
                        visibility: class.visibility,
                        is_expect: class.is_expect,
                        enum_entries: class.enum_entries,
                        sealed_subclasses: class.sealed_subclasses,
                        inline_class_property: class.inline_class_property,
                        modality: class.modality,
                        is_nested: class.is_nested,
                        access: super::builtin_class_access(flags),
                        nullable_member_returns: class.nullable_member_returns,
                    },
                )
            })
            .collect(),
        properties: package
            .properties
            .into_iter()
            .map(|property| super::BuiltinProperty {
                name: property.name,
                receiver: property.receiver.map(ty_from_common),
                ty: ty_from_common(property.ty),
                formals: property
                    .formals
                    .into_iter()
                    .map(type_param_from_common)
                    .collect(),
                visibility: property.visibility,
                is_var: property.is_var,
                context_count: property.context_count,
                constant: property.constant,
            })
            .collect(),
        functions: package
            .functions
            .into_iter()
            .map(|function| super::BuiltinFunction {
                name: function.name,
                receiver: function.receiver.map(ty_from_common),
                params: function.params.into_iter().map(ty_from_common).collect(),
                ret: ty_from_common(function.ret),
                formals: function
                    .formals
                    .into_iter()
                    .map(type_param_from_common)
                    .collect(),
                param_names: function.param_names,
                param_defaults: function.param_defaults,
                vararg: function.vararg,
                visibility: function.visibility,
                is_inline: function.is_inline,
                has_reified_type_params: function.has_reified_type_params,
                is_suspend: function.is_suspend,
                is_operator: function.is_operator,
                is_infix: function.is_infix,
                context_count: function.context_count,
            })
            .collect(),
    }
}
