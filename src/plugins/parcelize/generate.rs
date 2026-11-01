//! The bodies kotlinc's `ParcelizeIrTransformer` gives a parcelized class's generated members, and
//! its static `CREATOR`: an instance of the local class `Creator`, which reads the class back.
//!
//! ```text
//! public final int describeContents() { return 0; }
//! public final void writeToParcel(Parcel dest, int flags) { dest.write…(this.p); … }
//! public static final Parcelable.Creator<C> CREATOR = new Creator();
//!
//! public static final class Creator implements Parcelable.Creator<C> {   // local to CREATOR
//!     public final C createFromParcel(Parcel parcel) { return new C(parcel.read…(), …); }
//!     public final C[] newArray(int size) { return new C[size]; }
//! }
//! ```

use crate::ir::{
    ExprId, FunId, IrConst, IrEnclosure, IrExpr, IrFile, IrFunction, IrGeneratedDeclarationDebug,
    IrGeneratedFunctionMetadataScope, IrGeneratedFunctionPublication, IrGeneratedMemberPublication,
    IrParameterCheck, IrParameterIdentity, IrStatic, IrStaticAccessors, IrTypeOp,
};
use crate::plugins::runtime_declarations::BoundRuntimeFunction;
use crate::types::{Ty, TypeName, Visibility};

use super::facts::{ParcelizeFileFacts, ParcelizedClass, GENERATED_BODY};

/// Complete every parcelized class of the file.
pub(super) fn complete(ir: &mut IrFile, facts: &ParcelizeFileFacts) {
    for class in &facts.classes {
        describe_contents(ir, class);
        write_to_parcel(ir, facts, class);
        creator(ir, facts, class);
    }
}

/// `describeContents()`: `CONTENTS_FILE_DESCRIPTOR` for a class holding a file descriptor, which
/// no covered property type is.
///
/// kotlinc builds it at the declaration's offsets, which are the class's own: the value starts
/// where the class starts and the return is at its end.
fn describe_contents(ir: &mut IrFile, class: &ParcelizedClass) {
    let (start, end) = class_lines(ir, class);
    let flags = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let returned = ir.add_expr(IrExpr::Return(Some(flags)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    if start != 0 {
        ir.expr_lines.insert(flags, start);
    }
    if end != 0 {
        ir.mark_implicit_return_end_line(returned, end);
    }
    complete_body(ir, class.describe_contents, body);
}

/// The lines the class's declaration starts (annotations included) and ends on.
fn class_lines(ir: &IrFile, class: &ParcelizedClass) -> (u32, u32) {
    let class = &ir.classes[class.class as usize];
    (class.decl_start_line, class.decl_end_line)
}

/// `writeToParcel(dest, flags)`: each property in primary-constructor order.
///
/// kotlinc writes the properties without source offsets; only the return the body falls off into
/// maps to the declaration's end, which is the class's.
fn write_to_parcel(ir: &mut IrFile, facts: &ParcelizeFileFacts, class: &ParcelizedClass) {
    const DEST: u32 = 1;
    // `this`, `dest` and `flags` come first; each null-aware write keeps its value in a temporary.
    let mut temporary = 3;
    let mut stmts = Vec::with_capacity(class.properties.len());
    for property in &class.properties {
        let this = ir.add_expr(IrExpr::GetValue(0));
        let value = ir.add_expr(IrExpr::GetField {
            receiver: this,
            class: class.class,
            index: property.field,
        });
        stmts.push(facts.runtime.write(
            ir,
            property.serializer,
            DEST,
            value,
            property.ty,
            temporary,
        ));
        temporary += u32::from(property.serializer.null_aware);
    }
    let body = ir.add_expr(IrExpr::Block { stmts, value: None });
    let function = class.write_to_parcel;
    ir.fn_decl_lines.remove(&function);
    ir.fn_debug_locals.insert(function);
    let (_, end) = class_lines(ir, class);
    if end != 0 {
        ir.fn_close_lines.insert(function, end);
    }
    complete_body(ir, function, body);
}

/// The static `CREATOR` and its local `Creator` class.
fn creator(ir: &mut IrFile, facts: &ParcelizeFileFacts, class: &ParcelizedClass) {
    let parcelized = ir.classes[class.class as usize].fq_name_id();
    let parcelized_ty = Ty::obj_name(parcelized);
    let creator_interface = super::creator_interface();
    let creator_ty = Ty::obj_args_name(creator_interface, &[parcelized_ty]);
    let name = parcelized.nested_child("Creator");

    let create_from_parcel = create_from_parcel(ir, facts, class, name, parcelized_ty);
    let new_array = new_array(ir, name, parcelized_ty);

    let mut creator = crate::plugins::synthetic_class_name(name);
    creator.is_local_class = true;
    // Declared in `CREATOR`'s initializer, which belongs to no method: the class as a whole.
    creator.enclosure = Some(IrEnclosure::Classifier(class.class));
    creator.supertypes = vec![creator_ty];
    creator.interfaces = vec![creator_interface].into();
    creator.methods = vec![new_array, create_from_parcel];
    let array_ty = Ty::array(Ty::nullable(parcelized_ty));
    // The JVM sees `Parcelable.Creator`'s erased members; each bridge adapts its result and
    // calls the concrete member.
    creator.bridges = vec![
        bridge(
            new_array,
            &facts.runtime.new_array,
            "newArray",
            &["size"],
            Ty::array(Ty::nullable(Ty::obj("kotlin/Any"))),
            array_ty,
        ),
        bridge(
            create_from_parcel,
            &facts.runtime.create_from_parcel,
            "createFromParcel",
            &["source"],
            Ty::nullable(Ty::obj("kotlin/Any")),
            parcelized_ty,
        ),
    ];
    let creator_id = ir.add_class(creator);
    ir.local_class_name_provenance.insert(
        creator_id,
        crate::ir::IrLocalClassNameProvenance {
            source: facts.source,
            lexical_owner: Some(crate::ir::IrLocalClassOwner::Class(class.class)),
            segments: vec!["Creator".to_string()].into_boxed_slice(),
            ordinal: None,
            parents: vec![crate::ir::EnclosingDeclaration::StaticInitializer].into_boxed_slice(),
        },
    );
    ir.ctor_visibilities.insert(name, Visibility::Public);
    ir.mark_declarationless_class(name, crate::ir::IrDeclarationlessVisibility::Local);
    ir.insert_class_signature_name(
        name,
        crate::ir::IrGenericSig {
            type_params: Vec::new(),
            params: Vec::new(),
            ret: None,
            supers: vec![Ty::obj("kotlin/Any"), creator_ty],
        },
    );
    for function in [new_array, create_from_parcel] {
        ir.set_method_visibility(function, Visibility::Public);
    }
    ir.publish_generated_members(
        name,
        IrGeneratedMemberPublication {
            metadata_scope: IrGeneratedFunctionMetadataScope::Exclusive,
            functions: [(new_array, "size"), (create_from_parcel, "parcel")]
                .into_iter()
                .map(|(function, parameter)| IrGeneratedFunctionPublication {
                    function,
                    parameter_identities: vec![IrParameterIdentity::producer_value(parameter)],
                    metadata: None,
                    debug: IrGeneratedDeclarationDebug::LocalsOnly,
                })
                .collect(),
        },
    );

    let instance = ir.add_expr(IrExpr::New {
        internal: name,
        args: Vec::new(),
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    let init = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: instance,
        type_operand: creator_ty,
    });
    ir.statics.push(IrStatic {
        name: "CREATOR".to_string(),
        ty: creator_ty,
        init: Some(init),
        is_var: false,
        is_const: false,
        is_lateinit: false,
        owner: Some(parcelized),
        visibility: Visibility::Public,
        getter_jvm_name: None,
        setter_jvm_name: None,
        erased_declared_ty: None,
        accessors: IrStaticAccessors::ABSENT,
        line: 0,
        accessor_lines: crate::ir::IrAccessorLines::default(),
        source_order: u32::MAX,
        property_delegate: false,
    });
}

/// `createFromParcel(parcel)`: the primary constructor called with each property read back.
fn create_from_parcel(
    ir: &mut IrFile,
    facts: &ParcelizeFileFacts,
    class: &ParcelizedClass,
    owner: TypeName,
    parcelized: Ty,
) -> FunId {
    const PARCEL: u32 = 1;
    let args = class
        .properties
        .iter()
        .map(|property| {
            facts
                .runtime
                .read(ir, property.serializer, PARCEL, property.ty)
        })
        .collect();
    let constructed = ir.add_expr(IrExpr::New {
        internal: ir.classes[class.class as usize].fq_name_id(),
        args,
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    let body = returning(ir, constructed);
    ir.add_fun(IrFunction {
        name: "createFromParcel".to_string(),
        params: vec![Ty::obj(super::PARCEL)],
        ret: parcelized,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: vec![Some(IrParameterCheck::NonNull)],
    })
}

/// `newArray(size)`: an array of `size` nulls.
fn new_array(ir: &mut IrFile, owner: TypeName, parcelized: Ty) -> FunId {
    let size = ir.add_expr(IrExpr::GetValue(1));
    let array_type = Ty::array(Ty::nullable(parcelized));
    let array = ir.add_expr(IrExpr::NewArray { array_type, size });
    let body = returning(ir, array);
    ir.add_fun(IrFunction {
        name: "newArray".to_string(),
        params: vec![Ty::Int],
        ret: array_type,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: vec![None],
    })
}

/// The bridge from `overridden`'s erased slot to `target`, which returns `concrete`.
///
/// kotlinc's plugin declares `Parcelable.Creator` itself (`AndroidSymbols`) rather than reading it
/// from the classpath, so the bridge names its parameters after that declaration: `source` and
/// `size`, whatever names the platform's class file carries.
fn bridge(
    target: FunId,
    overridden: &BoundRuntimeFunction,
    name: &str,
    parameters: &[&str],
    erased_ret: Ty,
    concrete_ret: Ty,
) -> crate::ir::Bridge {
    crate::ir::Bridge {
        kind: crate::ir::BridgeKind::Function,
        target_function: Some(target),
        overridden_owner: None,
        collection_barrier: None,
        parameters: parameters
            .iter()
            .zip(&overridden.params)
            .map(|(&parameter, &semantic)| crate::ir::BridgeParameter {
                identity: crate::fir::ResolvedParameterIdentity::Source(parameter.into()),
                semantic: semantic.non_null(),
            })
            .collect(),
        name: name.to_string(),
        erased_params: overridden
            .params
            .iter()
            .map(|param| param.non_null())
            .collect(),
        erased_ret,
        concrete_params: overridden
            .params
            .iter()
            .map(|param| param.non_null())
            .collect(),
        concrete_ret,
        target_ret: None,
        barrier_plan: None,
        special: false,
        module_name_bridge: false,
        target_name: None,
        property_implementation: None,
    }
}

fn returning(ir: &mut IrFile, value: ExprId) -> ExprId {
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    })
}

/// Replace a generated member's placeholder with its body. The placeholder's slot is emptied
/// too: a target declines any placeholder left in the arena, reachable or not.
fn complete_body(ir: &mut IrFile, function: FunId, body: ExprId) {
    let placeholder = ir.functions[function as usize]
        .body
        .replace(body)
        .filter(|&placeholder| {
            matches!(
                ir.expr(placeholder),
                IrExpr::PluginPlaceholder {
                    plugin: "parcelize",
                    kind: GENERATED_BODY,
                    ..
                }
            )
        })
        .expect("a generated Parcelize member is completed once");
    ir.exprs[placeholder as usize] = IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    };
}
