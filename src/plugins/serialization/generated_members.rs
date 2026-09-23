//! Construction of methods owned by serialization-plugin generated classes.

use super::{class_ty, kserializer_of, unit};
use crate::ir::{
    Callee, ExprId, IrExpr, IrFile, IrFunction, IrGeneratedDeclarationDebug,
    IrGeneratedFunctionMetadata, IrGeneratedFunctionMetadataScope, IrGeneratedFunctionPublication,
    IrGeneratedMemberPublication,
};
use crate::types::{Ty, TypeName, Visibility};

const SERIALIZE_PARAMETER_NAMES: [&str; 2] = ["encoder", "value"];
const DESERIALIZE_PARAMETER_NAMES: [&str; 1] = ["decoder"];
const WRITE_SELF_KOTLIN_NAME: &str = "write$Self";
const WRITE_SELF_PARAMETER_NAMES: [&str; 3] = ["self", "output", "serialDesc"];

pub(super) fn write_self_publication(
    function: u32,
    owner_line: u32,
) -> IrGeneratedMemberPublication {
    IrGeneratedMemberPublication {
        metadata_scope: IrGeneratedFunctionMetadataScope::Additive,
        functions: vec![IrGeneratedFunctionPublication {
            function,
            parameter_names: WRITE_SELF_PARAMETER_NAMES.map(String::from).to_vec(),
            metadata: Some(IrGeneratedFunctionMetadata {
                source_name: WRITE_SELF_KOTLIN_NAME.to_string(),
                visibility: Visibility::Internal,
            }),
            debug: IrGeneratedDeclarationDebug::declaration_line(owner_line),
        }],
    }
}

pub(super) fn publish_write_self(ir: &mut IrFile, owner: TypeName, function: u32, owner_line: u32) {
    ir.function_annotations
        .insert(function, super::annotations::write_self_annotations());
    ir.publish_generated_members(owner, write_self_publication(function, owner_line));
}

pub(super) struct GeneratedSerializerMembers {
    pub(super) descriptor: u32,
    pub(super) serialize: u32,
    pub(super) deserialize: u32,
    pub(super) child_serializers: u32,
    pub(super) type_parameter_serializers: u32,
}

impl GeneratedSerializerMembers {
    pub(super) fn publication(
        &self,
        generic: bool,
        owner_line: u32,
        owner_end_line: u32,
    ) -> IrGeneratedMemberPublication {
        let line = IrGeneratedDeclarationDebug::declaration_line(owner_line);
        let metadata = |source_name: &str| {
            Some(IrGeneratedFunctionMetadata {
                source_name: source_name.to_string(),
                visibility: Visibility::Public,
            })
        };
        let member =
            |function, parameter_names: &[&str], metadata, debug| IrGeneratedFunctionPublication {
                function,
                parameter_names: parameter_names
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect(),
                metadata,
                debug,
            };
        let mut functions = vec![
            member(
                self.child_serializers,
                &[],
                metadata("childSerializers"),
                line,
            ),
            member(
                self.deserialize,
                &DESERIALIZE_PARAMETER_NAMES,
                metadata("deserialize"),
                line,
            ),
            member(
                self.serialize,
                &SERIALIZE_PARAMETER_NAMES,
                metadata("serialize"),
                IrGeneratedDeclarationDebug::declaration_line_with_fallthrough(
                    owner_line,
                    owner_end_line,
                ),
            ),
        ];
        if generic {
            functions.push(member(
                self.type_parameter_serializers,
                &[],
                metadata("typeParametersSerializers"),
                line,
            ));
        }
        functions.push(member(
            self.descriptor,
            &[],
            None,
            IrGeneratedDeclarationDebug::LocalsOnly,
        ));
        if !generic {
            functions.push(member(self.type_parameter_serializers, &[], None, line));
        }
        IrGeneratedMemberPublication {
            metadata_scope: IrGeneratedFunctionMetadataScope::Exclusive,
            functions,
        }
    }
}

/// Declare the complete member surface shared by generated serializer classes.
pub(super) fn add_serializer_members(
    ir: &mut IrFile,
    owner: TypeName,
    serialized_type: Ty,
    owner_line: u32,
    owner_end_line: u32,
    has_type_parameters: bool,
) -> GeneratedSerializerMembers {
    let descriptor = add_instance_method(
        ir,
        owner,
        "getDescriptor",
        vec![],
        class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
        None,
    );
    let serialize = add_guarded_instance_method(
        ir,
        owner,
        "serialize",
        vec![
            GuardedParameter::new(
                class_ty("kotlinx/serialization/encoding/Encoder"),
                SERIALIZE_PARAMETER_NAMES[0],
            ),
            GuardedParameter::new(serialized_type, SERIALIZE_PARAMETER_NAMES[1]),
        ],
        unit(),
        None,
    );
    let deserialize = add_guarded_instance_method(
        ir,
        owner,
        "deserialize",
        vec![GuardedParameter::new(
            class_ty("kotlinx/serialization/encoding/Decoder"),
            DESERIALIZE_PARAMETER_NAMES[0],
        )],
        serialized_type,
        None,
    );
    // Both GeneratedSerializer array methods publish `Array<KSerializer<*>>`: the serializers'
    // element types are unrelated. `KSerializer<Any>` is a different Kotlin contract despite the
    // same erasure. The stored star bound is a semantic read fact; metadata omits its nested type.
    let serializer_array = Ty::obj_args(
        "kotlin/Array",
        &[kserializer_of(Ty::star_projection(Ty::nullable(class_ty(
            "kotlin/Any",
        ))))],
    );
    let child_serializers = add_instance_method(
        ir,
        owner,
        "childSerializers",
        vec![],
        serializer_array,
        None,
    );
    // `GeneratedSerializer` owns the default implementation. Keep that semantic super-member call
    // in common IR; a target backend chooses its nonvirtual calling convention and descriptor.
    let this = ir.add_expr(IrExpr::GetValue(0));
    let inherited = ir.add_expr(IrExpr::Call {
        callee: Callee::Super {
            owner: crate::types::type_name(super::GENERATED_SERIALIZER_FQ),
            dispatch_owner: owner,
            enclosing_dispatch: false,
            kind: crate::ir::IrSuperCallKind::Function,
            name: "typeParametersSerializers".to_string(),
            params: Vec::new(),
            ret: serializer_array,
            interface: true,
            realization: crate::libraries::MemberRealization::Dispatch,
            descriptor: String::new(),
            source: None,
            defaults: Vec::new(),
            source_member: None,
        },
        dispatch_receiver: Some(this),
        args: Vec::new(),
    });
    let returned = ir.add_expr(IrExpr::Return(Some(inherited)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    let type_parameter_serializers = add_instance_method(
        ir,
        owner,
        "typeParametersSerializers",
        vec![],
        serializer_array,
        Some(body),
    );
    // A non-generic serializer delegates to the interface default and publishes that adapter as an
    // open bridge. A generic serializer supplies its own final array-returning override.
    if !has_type_parameters {
        ir.open_methods.insert(type_parameter_serializers);
        ir.bridge_methods.insert(type_parameter_serializers);
    }
    let members = GeneratedSerializerMembers {
        descriptor,
        serialize,
        deserialize,
        child_serializers,
        type_parameter_serializers,
    };
    ir.publish_generated_members(
        owner,
        members.publication(has_type_parameters, owner_line, owner_end_line),
    );
    members
}

/// A non-null generated-method parameter whose source-visible name is also used by the JVM entry
/// guard. Keeping the type and name paired prevents the guard, debug table, and Kotlin metadata
/// from drifting to different physical parameter positions.
pub(super) struct GuardedParameter {
    ty: Ty,
    name: &'static str,
}

impl GuardedParameter {
    pub(super) fn new(ty: Ty, name: &'static str) -> Self {
        Self { ty, name }
    }
}

/// Add an instance method to a plugin-generated class and return its `FunId`.
pub(super) fn add_instance_method(
    ir: &mut IrFile,
    owner: TypeName,
    name: &str,
    params: Vec<Ty>,
    ret: Ty,
    body: Option<ExprId>,
) -> u32 {
    ir.add_fun(IrFunction {
        name: name.to_string(),
        params,
        ret,
        body,
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: Vec::new(),
    })
}

/// Add an instance method whose non-null parameters have kotlinc-compatible entry guards.
///
/// The same names are recorded for Kotlin metadata and debug-table emission. A generated member is
/// public API, so a Java caller can pass `null`; kotlinc guards it just like a source declaration.
pub(super) fn add_guarded_instance_method(
    ir: &mut IrFile,
    owner: TypeName,
    name: &str,
    params: Vec<GuardedParameter>,
    ret: Ty,
    body: Option<ExprId>,
) -> u32 {
    let parameter_types = params.iter().map(|parameter| parameter.ty).collect();
    let parameter_names = params
        .iter()
        .map(|parameter| parameter.name.to_string())
        .collect::<Vec<_>>();
    let param_checks = parameter_names.iter().cloned().map(Some).collect();
    ir.add_fun(IrFunction {
        name: name.to_string(),
        params: parameter_types,
        ret,
        body,
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks,
    })
}

#[cfg(test)]
mod tests {
    use super::{write_self_publication, GeneratedSerializerMembers};
    use crate::ir::{
        Callee, IrExpr, IrFile, IrGeneratedDeclarationDebug, IrGeneratedFunctionMetadataScope,
        IrGeneratedMemberPublication,
    };
    use crate::types::{type_name, Ty, Visibility};

    fn serializer_members() -> GeneratedSerializerMembers {
        GeneratedSerializerMembers {
            descriptor: 0,
            serialize: 1,
            deserialize: 2,
            child_serializers: 3,
            type_parameter_serializers: 4,
        }
    }

    fn metadata_shape(
        publication: &IrGeneratedMemberPublication,
    ) -> Vec<(u32, String, Visibility, Vec<String>)> {
        publication
            .functions
            .iter()
            .filter_map(|member| {
                member.metadata.as_ref().map(|metadata| {
                    (
                        member.function,
                        metadata.source_name.clone(),
                        metadata.visibility,
                        member.parameter_names.clone(),
                    )
                })
            })
            .collect()
    }

    #[test]
    fn serializer_publication_states_exact_generic_and_non_generic_surfaces() {
        let non_generic = serializer_members().publication(false, 17, 23);
        assert_eq!(
            non_generic.metadata_scope,
            IrGeneratedFunctionMetadataScope::Exclusive
        );
        assert_eq!(
            metadata_shape(&non_generic),
            vec![
                (
                    3,
                    "childSerializers".to_string(),
                    Visibility::Public,
                    vec![]
                ),
                (
                    2,
                    "deserialize".to_string(),
                    Visibility::Public,
                    vec!["decoder".to_string()],
                ),
                (
                    1,
                    "serialize".to_string(),
                    Visibility::Public,
                    vec!["encoder".to_string(), "value".to_string()],
                ),
            ]
        );
        assert_eq!(
            non_generic
                .functions
                .iter()
                .map(|member| (member.function, member.metadata.is_some(), member.debug))
                .collect::<Vec<_>>(),
            vec![
                (3, true, IrGeneratedDeclarationDebug::declaration_line(17)),
                (2, true, IrGeneratedDeclarationDebug::declaration_line(17)),
                (
                    1,
                    true,
                    IrGeneratedDeclarationDebug::declaration_line_with_fallthrough(17, 23),
                ),
                (0, false, IrGeneratedDeclarationDebug::LocalsOnly),
                (4, false, IrGeneratedDeclarationDebug::declaration_line(17)),
            ]
        );

        let generic = serializer_members().publication(true, 19, 29);
        assert_eq!(
            metadata_shape(&generic),
            vec![
                (
                    3,
                    "childSerializers".to_string(),
                    Visibility::Public,
                    vec![]
                ),
                (
                    2,
                    "deserialize".to_string(),
                    Visibility::Public,
                    vec!["decoder".to_string()],
                ),
                (
                    1,
                    "serialize".to_string(),
                    Visibility::Public,
                    vec!["encoder".to_string(), "value".to_string()],
                ),
                (
                    4,
                    "typeParametersSerializers".to_string(),
                    Visibility::Public,
                    vec![],
                ),
            ]
        );
        assert_eq!(generic.functions.len(), 5);
        assert_eq!(generic.functions[4].function, 0);
        assert!(generic.functions[4].metadata.is_none());
        assert_eq!(
            generic.functions[4].debug,
            IrGeneratedDeclarationDebug::LocalsOnly
        );
    }

    #[test]
    fn write_self_publication_is_additive_internal_and_uses_canonical_names() {
        let publication = write_self_publication(7, 23);
        assert_eq!(
            publication.metadata_scope,
            IrGeneratedFunctionMetadataScope::Additive
        );
        assert_eq!(publication.functions.len(), 1);
        let member = &publication.functions[0];
        assert_eq!(member.function, 7);
        assert_eq!(member.parameter_names, ["self", "output", "serialDesc"]);
        let metadata = member.metadata.as_ref().expect("writeSelf metadata");
        assert_eq!(metadata.source_name, "write$Self");
        assert_eq!(metadata.visibility, Visibility::Internal);
        assert_eq!(
            member.debug,
            IrGeneratedDeclarationDebug::declaration_line(23)
        );
    }

    #[test]
    fn default_member_dispatch_stays_semantic_until_backend_realization() {
        let mut ir = IrFile::default();
        let owner = type_name("demo/Plain$$serializer");
        let members = super::add_serializer_members(
            &mut ir,
            owner,
            super::class_ty("demo/Plain"),
            0,
            0,
            false,
        );
        let function = members.type_parameter_serializers;
        assert!(ir.open_methods.contains(&function));
        assert!(ir.bridge_methods.contains(&function));
        let body = ir.functions[function as usize]
            .body
            .expect("generated default delegation body");
        let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
            panic!("default delegation is not a statement block");
        };
        let Some(IrExpr::Return(Some(call))) = stmts.first().map(|statement| ir.expr(*statement))
        else {
            panic!("default delegation does not return its call");
        };
        let IrExpr::Call {
            callee:
                Callee::Super {
                    owner: super_owner,
                    dispatch_owner,
                    params,
                    ret,
                    interface,
                    realization,
                    descriptor,
                    ..
                },
            dispatch_receiver: Some(_),
            args,
        } = ir.expr(*call)
        else {
            panic!("default delegation is not a semantic super-member call");
        };
        assert_eq!(
            *super_owner,
            type_name(super::super::GENERATED_SERIALIZER_FQ)
        );
        assert_eq!(*dispatch_owner, owner);
        assert!(params.is_empty());
        assert_eq!(
            *ret,
            Ty::obj_args(
                "kotlin/Array",
                &[super::kserializer_of(Ty::star_projection(Ty::nullable(
                    super::class_ty("kotlin/Any")
                )))],
            )
        );
        assert!(*interface);
        assert_eq!(*realization, crate::libraries::MemberRealization::Dispatch);
        assert!(descriptor.is_empty());
        assert!(args.is_empty());
        assert!(!ir.exprs.iter().any(|expression| matches!(
            expression,
            IrExpr::Call {
                callee: Callee::Special { .. },
                ..
            }
        )));
    }

    #[test]
    fn generic_type_parameter_serializers_is_final_and_not_a_bridge() {
        let mut ir = IrFile::default();
        let members = super::add_serializer_members(
            &mut ir,
            type_name("demo/Box$$serializer"),
            Ty::obj_args("demo/Box", &[Ty::ty_param("T", Ty::obj("kotlin/Any"))]),
            0,
            0,
            true,
        );
        assert!(!ir
            .open_methods
            .contains(&members.type_parameter_serializers));
        assert!(!ir
            .bridge_methods
            .contains(&members.type_parameter_serializers));
    }
}
