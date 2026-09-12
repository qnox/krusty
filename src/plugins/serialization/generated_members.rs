//! Construction of methods owned by serialization-plugin generated classes.

use super::{class_ty, kserializer_of, unit};
use crate::ir::{ExprId, FnParamInfo, IrExpr, IrFile, IrFunction};
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
    let serializer_array = Ty::obj_args("kotlin/Array", &[kserializer_of(class_ty("kotlin/Any"))]);
    let child_serializers = add_instance_method(
        ir,
        owner,
        "childSerializers",
        vec![],
        serializer_array,
        None,
    );
    let empty_serializers = ir.add_expr(IrExpr::Vararg {
        array_type: serializer_array,
        spreads: vec![],
        elements: vec![],
    });
    let returned = ir.add_expr(IrExpr::Return(Some(empty_serializers)));
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
    // A plain `GeneratedSerializer` override is open and participates in bridge emission.
    ir.open_methods.insert(type_parameter_serializers);
    ir.bridge_methods.insert(type_parameter_serializers);
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
