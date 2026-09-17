//! Construction of methods owned by serialization-plugin generated classes.

use super::{class_ty, kserializer_of, unit};
use crate::ir::{Callee, ExprId, FnParamInfo, IrExpr, IrFile, IrFunction};
use crate::types::{Ty, TypeName};

pub(super) struct GeneratedSerializerMembers {
    pub(super) descriptor: u32,
    pub(super) serialize: u32,
    pub(super) deserialize: u32,
    pub(super) child_serializers: u32,
    pub(super) type_parameter_serializers: u32,
}

/// Declare the complete member surface shared by generated serializer classes.
pub(super) fn add_serializer_members(
    ir: &mut IrFile,
    owner: TypeName,
    serialized_type: Ty,
    owner_line: u32,
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
                "encoder",
            ),
            GuardedParameter::new(serialized_type, "value"),
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
            "decoder",
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
    // The non-generic serializer delegates to the interface default and kotlinc publishes that
    // adapter as open/bridge. A generic serializer supplies its own array and publishes an ordinary
    // final override. This is a declaration-shape fact from the producer, not a JVM name check.
    if !has_type_parameters {
        ir.open_methods.insert(type_parameter_serializers);
        ir.bridge_methods.insert(type_parameter_serializers);
    }
    record_debug_tables(
        ir,
        owner_line,
        &[
            serialize,
            deserialize,
            child_serializers,
            type_parameter_serializers,
        ],
        &[descriptor],
    );
    GeneratedSerializerMembers {
        descriptor,
        serialize,
        deserialize,
        child_serializers,
        type_parameter_serializers,
    }
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
    let function = ir.add_fun(IrFunction {
        name: name.to_string(),
        params: parameter_types,
        ret,
        body,
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks,
    });
    ir.fn_params
        .insert(function, FnParamInfo::names(parameter_names));
    function
}

/// Attach the source/debug contract shared by methods of a generated serializer.
///
/// kotlinc gives every generated member a local-variable table. All members except the bare
/// `getDescriptor` field read also carry a line table rooted at the annotated class declaration.
pub(super) fn record_debug_tables(
    ir: &mut IrFile,
    owner_line: u32,
    line_and_locals: &[u32],
    locals_only: &[u32],
) {
    ir.fn_debug_locals
        .extend(line_and_locals.iter().chain(locals_only).copied());
    if owner_line != 0 {
        ir.fn_decl_lines.extend(
            line_and_locals
                .iter()
                .map(|function| (*function, owner_line)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn default_member_dispatch_stays_semantic_until_backend_realization() {
        let mut ir = IrFile::default();
        let owner = type_name("demo/Plain$$serializer");
        let members = add_serializer_members(&mut ir, owner, class_ty("demo/Plain"), 0, false);
        let body = ir.functions[members.type_parameter_serializers as usize]
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
                &[kserializer_of(Ty::star_projection(Ty::nullable(class_ty(
                    "kotlin/Any"
                ))))]
            )
        );
        assert!(*interface);
        assert_eq!(*realization, crate::libraries::MemberRealization::Dispatch);
        assert!(
            descriptor.is_empty(),
            "common IR must not carry a JVM descriptor"
        );
        assert!(args.is_empty());
        assert!(
            !ir.exprs.iter().any(|expression| matches!(
                expression,
                IrExpr::Call {
                    callee: Callee::Special { .. },
                    ..
                }
            )),
            "plugin IR must not choose the JVM invokespecial representation"
        );
    }
}
