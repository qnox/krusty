//! The JVM lowering of a class's atomics: each one becomes a volatile field updated through a
//! static `java.util.concurrent.atomic` field updater, as kotlinc's `AtomicfuJvmIrTransformer`
//! lowers it.
//!
//! For `private val n = atomic(0)` in class `C` that is
//!
//! ```text
//! private volatile synthetic int n$volatile;                       // at the property's place
//! private static final synthetic AtomicIntegerFieldUpdater n$volatile$FU
//!     = AtomicIntegerFieldUpdater.newUpdater(C.class, "n$volatile");
//! ```
//!
//! with the private accessors kotlinc gives the generated `n$volatile` property, and a private
//! static getter of `n$volatile$FU` that every operation calls: `n.incrementAndGet()` becomes
//! `getN$volatile$FU().incrementAndGet(this)`.

use super::source_facts::{AtomicValue, AtomicfuFileFacts, FieldUpdater, UpdaterOperation};
use crate::ir::{
    Callee, ClassId, ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrFunction,
    IrGeneratedDeclarationDebug, IrGeneratedFunctionMetadataScope, IrGeneratedFunctionPublication,
    IrGeneratedMemberPublication, IrParameterIdentity,
};
use crate::plugins::runtime_declarations::BoundRuntimeFunction;
use crate::types::{type_name, EqualityMode, Ty, Visibility};

/// What [`lower_declaration`] made of one atomic, for its operations.
struct LoweredUpdater {
    class: ClassId,
    value: AtomicValue,
    /// The private static getter of the updater.
    getter: u32,
}

pub(super) fn transform(ir: &mut IrFile) {
    let Some(facts) = ir.atomicfu.take() else {
        return;
    };
    let AtomicfuFileFacts {
        updaters,
        operations,
    } = *facts;
    let lowered = updaters
        .iter()
        .map(|updater| lower_declaration(ir, updater))
        .collect::<Vec<_>>();
    for operation in &operations {
        lower_operation(ir, &lowered[operation.updater], operation);
    }
}

/// Replace one atomic property by its volatile field, the field's updater, and their accessors.
fn lower_declaration(ir: &mut IrFile, updater: &FieldUpdater) -> LoweredUpdater {
    let class = updater.class;
    let owner = ir.classes[class as usize].fq_name;
    let atomic = ir.classes[class as usize].properties[updater.property].clone();
    let field = atomic
        .backing_field
        .expect("a recorded atomic property has a backing field");
    let value = updater.value.storage();
    let volatile_name = format!("{}$volatile", atomic.name);

    // The atomic property stays the class's declaration, which its metadata describes; the
    // storage it had becomes the generated `n$volatile` property's volatile field.
    ir.classes[class as usize].properties[updater.property].backing_field = None;
    let storage = &mut ir.classes[class as usize].fields[field as usize];
    storage.name.clone_from(&volatile_name);
    storage.ty = value;
    storage.flags = storage
        .flags
        .with_is_final(false)
        .with_compiler_generated(true);
    let (store, initial) = updater.initializer;
    let initial = stored_value(ir, updater.value, initial);
    if let IrExpr::SetField { value: stored, .. } = &mut ir.exprs[store as usize] {
        *stored = initial;
    }
    // kotlinc's transform stores the value in a new statement with no source position, so the
    // initializer's line is the value's own, entered where the value is computed.
    if let Some(line) = ir.expr_lines.remove(&store) {
        ir.expr_lines.entry(initial).or_insert(line);
    }

    let getter_name = accessor_name("get", &volatile_name);
    let setter_name = accessor_name("set", &volatile_name);
    let getter = {
        let this = ir.add_expr(IrExpr::GetValue(0));
        let read = ir.add_expr(IrExpr::GetField {
            receiver: this,
            class,
            index: field,
        });
        let body = returned(ir, Some(read));
        member(ir, owner, getter_name, vec![], value, body)
    };
    let setter = {
        let this = ir.add_expr(IrExpr::GetValue(0));
        let assigned = ir.add_expr(IrExpr::GetValue(1));
        let write = ir.add_expr(IrExpr::SetField {
            receiver: this,
            class,
            index: field,
            value: assigned,
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![write],
            value: None,
        });
        member(ir, owner, setter_name, vec![value], Ty::Unit, body)
    };
    for function in [getter, setter] {
        ir.fn_source_order.insert(function, atomic.source_order);
        ir.set_method_visibility(function, atomic.visibility);
        ir.classes[class as usize].methods.push(function);
    }
    publish(ir, owner, getter, vec![]);
    publish(
        ir,
        owner,
        setter,
        vec![IrParameterIdentity::producer_value("value")],
    );
    let mut volatile = atomic.clone();
    volatile.name = volatile_name.clone();
    volatile.ty = value;
    volatile.annotations = Box::new([type_name("kotlin/jvm/Volatile")]);
    volatile.initializer = None;
    volatile.is_var = true;
    volatile.backing_field = Some(field);
    volatile.getter = Some(getter);
    volatile.setter = Some(setter);
    volatile.setter_visibility = atomic.visibility;
    volatile.modifiers.declared_getter = true;
    volatile.modifiers.declared_setter = true;
    ir.classes[class as usize].properties.push(volatile);

    // `private static final n$volatile$FU = Updater.newUpdater(C.class, "n$volatile")`.
    let updater_ty = Ty::obj_name(updater.value.updater());
    let class_literal = ir.add_expr(IrExpr::ClassConst {
        internal: Some(owner),
    });
    let field_name = ir.add_expr(IrExpr::Const(IrConst::String(
        volatile_name.as_str().into(),
    )));
    let created = call(
        ir,
        &updater.new_updater,
        None,
        vec![class_literal, field_name],
    );
    let static_index = u32::try_from(ir.statics.len()).expect("too many statics");
    ir.statics.push(crate::ir::IrStatic {
        name: format!("{volatile_name}$FU"),
        ty: updater_ty,
        init: Some(created),
        is_var: false,
        is_const: false,
        is_lateinit: false,
        owner: Some(owner),
        visibility: Visibility::Private,
        getter_jvm_name: None,
        setter_jvm_name: None,
        erased_declared_ty: None,
        accessors: crate::ir::IrStaticAccessors::ABSENT,
        line: 0,
        accessor_lines: crate::ir::IrAccessorLines::default(),
        source_order: u32::MAX,
        property_delegate: false,
    });
    ir.mark_compiler_generated_static(static_index);
    let read = ir.add_expr(IrExpr::GetStatic(static_index));
    let body = returned(ir, Some(read));
    let getter = ir.add_fun(IrFunction {
        name: accessor_name("get", &format!("{volatile_name}$FU")),
        params: vec![],
        ret: updater_ty,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.synthetic_methods.insert(getter);
    ir.set_method_visibility(getter, Visibility::Private);
    ir.classes[class as usize].methods.push(getter);
    publish(ir, owner, getter, vec![]);
    LoweredUpdater {
        class,
        value: updater.value,
        getter,
    }
}

/// `n.op(args)` → `getN$volatile$FU().op(instance, args)`: the read of the atomic becomes the read
/// of its updater, and the instance moves to the arguments.
fn lower_operation(ir: &mut IrFile, updater: &LoweredUpdater, operation: &UpdaterOperation) {
    let owner = ir.classes[updater.class as usize].fq_name;
    ir.exprs[operation.read as usize] = IrExpr::Call {
        callee: Box::new(Callee::ClassStatic {
            owner,
            function: updater.getter,
        }),
        dispatch_receiver: None,
        args: vec![],
    };
    let arguments = operation
        .arguments
        .iter()
        .map(|&argument| stored_value(ir, updater.value, argument))
        .collect::<Vec<_>>();
    let args = std::iter::once(operation.instance)
        .chain(arguments)
        .collect();
    // The backend realizes a plugin's runtime calls among the expressions the plugin adds, so the
    // operation becomes a block whose value is a new call.
    let updated = call(ir, &operation.function, Some(operation.read), args);
    if let Some(&line) = ir.expr_lines.get(&operation.expression) {
        ir.expr_lines.insert(updated, line);
    }
    let value = if operation.function.ret == updater.value.storage() {
        loaded_value(ir, updater.value, updated)
    } else {
        updated
    };
    ir.exprs[operation.expression as usize] = IrExpr::Block {
        stmts: vec![],
        value: Some(value),
    };
}

/// `value` as the field stores it: a `Boolean` is `if (value) 1 else 0`, as kotlinc's `toInt`
/// builds it. kotlinc's code generator takes the branch of a constant condition, so a constant is
/// stored as its `Int` (and a constructor stores no default `0`).
fn stored_value(ir: &mut IrFile, kind: AtomicValue, value: ExprId) -> ExprId {
    if kind != AtomicValue::Boolean {
        return value;
    }
    if let IrExpr::Const(IrConst::Boolean(constant)) = ir.exprs[value as usize] {
        return ir.add_expr(IrExpr::Const(IrConst::Int(i32::from(constant))));
    }
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let converted = ir.add_expr(IrExpr::When {
        branches: vec![(Some(value), one), (None, zero)],
    });
    ir.whens.exhaustive.insert(converted, Ty::Int);
    converted
}

/// The atomic's value from what the field stores: a `Boolean` is `stored == 1`, as kotlinc's
/// `toBoolean` builds it.
fn loaded_value(ir: &mut IrFile, kind: AtomicValue, stored: ExprId) -> ExprId {
    if kind != AtomicValue::Boolean {
        return stored;
    }
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    ir.add_expr(IrExpr::Equality {
        op: IrBinOp::Eq,
        mode: EqualityMode::Primitive,
        lhs: stored,
        rhs: one,
    })
}

/// The JVM accessor name of a generated property: `get` + its capitalized name.
fn accessor_name(prefix: &str, property: &str) -> String {
    let mut characters = property.chars();
    let first = characters.next().map(|first| first.to_ascii_uppercase());
    format!(
        "{prefix}{}{}",
        first.into_iter().collect::<String>(),
        characters.as_str()
    )
}

fn returned(ir: &mut IrFile, value: Option<ExprId>) -> ExprId {
    let ret = ir.add_expr(IrExpr::Return(value));
    ir.add_expr(IrExpr::Block {
        stmts: vec![ret],
        value: None,
    })
}

fn member(
    ir: &mut IrFile,
    owner: crate::types::TypeName,
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    body: ExprId,
) -> u32 {
    let function = ir.add_fun(IrFunction {
        name,
        params,
        ret,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: Vec::new(),
    });
    ir.synthetic_methods.insert(function);
    function
}

/// Publish a generated function as kotlinc does: no Kotlin record, and locals without lines.
fn publish(
    ir: &mut IrFile,
    owner: crate::types::TypeName,
    function: u32,
    parameter_identities: Vec<IrParameterIdentity>,
) {
    let member = IrGeneratedFunctionPublication {
        function,
        parameter_identities,
        metadata: None,
        debug: IrGeneratedDeclarationDebug::LocalsOnly,
    };
    if ir.generated_member_publication(owner).is_some() {
        ir.publish_generated_debug_function(owner, member);
    } else {
        ir.publish_generated_members(
            owner,
            IrGeneratedMemberPublication {
                metadata_scope: IrGeneratedFunctionMetadataScope::Additive,
                functions: vec![member],
            },
        );
    }
}

fn external(function: &BoundRuntimeFunction) -> Callee {
    Callee::External {
        target: function.target,
        default_provider: None,
        params: function.params.clone(),
        ret: function.ret,
        substitutions: Vec::new(),
        defaults: Vec::new(),
        extension_receiver_parameter: None,
    }
}

fn call(
    ir: &mut IrFile,
    function: &BoundRuntimeFunction,
    receiver: Option<ExprId>,
    args: Vec<ExprId>,
) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Box::new(external(function)),
        dispatch_receiver: receiver,
        args,
    })
}
