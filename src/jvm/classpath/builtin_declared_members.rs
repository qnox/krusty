//! The declarations of a `.kotlin_builtins` classifier as the fragment states them: its
//! constructors as classless library members, and its own members for a consumer that shows
//! declarations.

use super::{Classpath, Ty, TypeName};
use crate::libraries::{
    DeclaredConstructor, DeclaredFunction, DeclaredMembers, DeclaredParameter, DeclaredProperty,
};

impl Classpath {
    /// Constructors declared by a classless Kotlin builtin. Their descriptor is deliberately empty:
    /// the declaration is semantic metadata, while a backend intrinsic owns the platform realization
    /// (`Array(size, init)` has no JVM `<init>` method).
    pub fn builtin_constructors_name(
        &self,
        internal: TypeName,
    ) -> Vec<crate::libraries::LibraryMember> {
        let file = self.builtins_file_for_package(Self::builtins_package_for(internal));
        let Some(class) = file.get_name(internal) else {
            return Vec::new();
        };
        class
            .constructors
            .iter()
            .map(|constructor| {
                let mut member = crate::libraries::LibraryMember::new(
                    "<init>".to_string(),
                    constructor.params.clone(),
                    Ty::Unit,
                    String::new(),
                );
                member.visibility = constructor.visibility;
                member.generic_sig = Some(crate::libraries::GenericSig {
                    formals: Vec::new(),
                    formal_bounds: Vec::new(),
                    receiver: None,
                    params: constructor.params.clone(),
                    ret: Ty::obj_name(internal),
                    return_policy: Default::default(),
                });
                member.call_sig = crate::libraries::CallSig::metadata_member(
                    constructor.params.len(),
                    constructor.param_names.clone(),
                    constructor.param_defaults.clone(),
                    constructor.vararg,
                );
                member
            })
            .collect()
    }

    /// The members the Kotlin builtin `internal` declares, in fragment order. `None` when the
    /// fragment declares no such classifier, or when the classifier declares a nested one: the
    /// fragment keeps nested classifiers in no order.
    pub(in crate::jvm) fn builtin_declared_members(
        &self,
        internal: TypeName,
    ) -> Option<DeclaredMembers> {
        let file = self.builtins_file_for_package(Self::builtins_package_for(internal));
        let class = file.get_name(internal)?;
        if file
            .classes
            .keys()
            .any(|name| name.nested_owner() == Some(internal))
        {
            return None;
        }
        let parameters =
            |types: &[Ty], names: &[String], defaults: &[bool], vararg: Option<usize>| {
                types
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        DeclaredParameter::new(
                            names.get(index)?.clone(),
                            *ty,
                            vararg == Some(index),
                            *defaults.get(index)?,
                        )
                    })
                    .collect::<Option<Vec<_>>>()
            };
        let mut members = DeclaredMembers {
            functions: Vec::new(),
            properties: Vec::new(),
            static_properties: Vec::new(),
            constructors: class
                .constructors
                .iter()
                .map(|constructor| {
                    Some(DeclaredConstructor {
                        parameters: parameters(
                            &constructor.params,
                            &constructor.param_names,
                            &constructor.param_defaults,
                            constructor.vararg,
                        )?,
                        visibility: constructor.visibility,
                    })
                })
                .collect::<Option<_>>()?,
            nested: Vec::new(),
        };
        for member in &class.members {
            let signature = &member.generic_sig;
            let ret = match member.ret_nullable {
                true => Ty::nullable(signature.ret),
                false => signature.ret,
            };
            if member.is_property {
                members.properties.push(DeclaredProperty {
                    name: member.name.clone(),
                    ty: ret,
                    is_var: member.is_var,
                    visibility: member.visibility,
                    modality: member.modality,
                    from_java_method: false,
                    getter_annotations: Vec::new(),
                    annotated: !member.annotations.is_empty(),
                });
            } else {
                members.functions.push(DeclaredFunction {
                    name: member.name.clone(),
                    parameters: parameters(
                        &signature.params,
                        &member.param_names,
                        &member.param_defaults,
                        member.vararg,
                    )?,
                    ret,
                    visibility: member.visibility,
                    modality: member.modality,
                    is_operator: member.is_operator,
                    is_infix: member.is_infix,
                    is_generic: !signature.formals.is_empty(),
                    is_extension: signature.receiver.is_some(),
                    annotated: !member.annotations.is_empty(),
                });
            }
        }
        Some(members)
    }
}
