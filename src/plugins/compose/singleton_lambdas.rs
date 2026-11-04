//! The singletons kotlinc's `ComposerLambdaMemoization` makes of composable lambdas that capture
//! nothing. Each one is created once, in a property of the file's `ComposableSingletons` object,
//! and read from there wherever the lambda is written:
//!
//! ```text
//! object ComposableSingletons$FileKt {
//!     internal val `lambda$<key>`: (Composer, Int) -> Unit =
//!         composableLambdaInstance(<key>, false) { $composer, $changed -> … }
//! }
//! ```
//!
//! The object is the plugin's own declaration, with no Kotlin source behind it: its constructor
//! is public, its properties' fields are not final, and it has no declaration metadata.

use crate::ir::{
    ExprId, IrConst, IrEnclosure, IrExpr, IrField, IrFile, IrLiftingSequence, IrProperty, IrTypeOp,
};
use crate::types::{TypeName, Visibility};

use super::runtime::{self, ComposeRuntime};
use super::source_facts::{ComposableLambda, Memoization};

/// Hold those of `lambdas` that capture nothing in the file's singletons class `singletons` and
/// read each one from it.
pub(super) fn memoize(
    ir: &mut IrFile,
    runtime: &ComposeRuntime,
    lambdas: &[ComposableLambda],
    singletons: TypeName,
) {
    let lambdas = lambdas
        .iter()
        .filter(|lambda| lambda.memoization == Memoization::Singleton)
        .collect::<Vec<_>>();
    if lambdas.is_empty() {
        return;
    }
    let mut class = crate::plugins::synthetic_class_name(singletons);
    class.is_object = true;
    class.is_compiler_generated = true;
    let class_id = ir.add_class(class);
    ir.ctor_visibilities.insert(singletons, Visibility::Public);
    ir.mark_declarationless_class(singletons);
    let owner: Box<str> = format!("class:{}", singletons.nested_segment_ref()).into();
    let mut stores = Vec::with_capacity(lambdas.len());
    for (index, lambda) in (0u32..).zip(lambdas) {
        let name = format!("lambda${}", lambda.key);
        let initializer = instance(ir, runtime, lambda);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        let store = ir.add_expr(IrExpr::SetField {
            receiver,
            class: class_id,
            index,
            value: initializer,
        });
        ir.property_initializer_stores.insert(store);
        // The lambda is created on the line it starts.
        if lambda.opening_line != 0 {
            ir.expr_lines.insert(store, lambda.opening_line);
        }
        stores.push(store);
        let declaration = &mut ir.classes[class_id as usize];
        declaration
            .fields
            .push(IrField::new(name.clone(), runtime::composable_lambda()));
        declaration
            .properties
            .push(property(&name, index, initializer));
        declaration.methods.push(lambda.implementation);
        read_from_singletons(ir, lambda, singletons, &name);
        let (written, _) = &ir.lifted_functions[&lambda.implementation];
        let sequence = IrLiftingSequence {
            source: written.source,
            owner: owner.clone(),
            container: name.as_str().into(),
        };
        ir.relocate_lifted_lambda(
            lambda.implementation,
            sequence,
            index,
            IrEnclosure::ClassInitializer(class_id),
        );
    }
    let init = ir.add_expr(IrExpr::Block {
        stmts: stores,
        value: None,
    });
    ir.classes[class_id as usize].init_body = Some(init);
}

/// `composableLambdaInstance(key, false, lambda)`, cast to the composable calling convention.
fn instance(ir: &mut IrFile, runtime: &ComposeRuntime, lambda: &ComposableLambda) -> ExprId {
    let key = ir.add_expr(IrExpr::Const(IrConst::Int(lambda.key)));
    let tracked = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
    let created = runtime::call(
        ir,
        &runtime.composable_lambda_instance,
        None,
        vec![key, tracked, lambda.lambda],
    );
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: created,
        type_operand: runtime::composable_lambda(),
    })
}

/// The `internal val` holding one singleton.
fn property(name: &str, field: u32, initializer: ExprId) -> IrProperty {
    IrProperty {
        name: name.to_string(),
        diagnostic_span: None,
        getter_diagnostic_span: None,
        setter_diagnostic_span: None,
        context_params: Vec::new(),
        source_order: field,
        decl_line: 0,
        accessor_lines: crate::ir::IrAccessorLines::default(),
        ty: runtime::composable_lambda(),
        type_params: Vec::new(),
        visibility: Visibility::Internal,
        return_value_status: Default::default(),
        annotations: Box::new([]),
        initializer: Some(initializer),
        storage_ty: None,
        backing_field: Some(field),
        is_var: false,
        is_open: false,
        modifiers: Default::default(),
        delegate_field: None,
        has_constant_initializer: false,
        is_private: false,
        setter_visibility: Visibility::Internal,
        getter: None,
        setter: None,
        getter_jvm_name: None,
        setter_jvm_name: None,
        needs_access_bridge: false,
        accessor_annotations: Default::default(),
    }
}

/// Pass the singleton's property in the lambda's place. kotlinc builds the read without offsets.
fn read_from_singletons(
    ir: &mut IrFile,
    lambda: &ComposableLambda,
    singletons: TypeName,
    name: &str,
) {
    let instance = ir.add_expr(IrExpr::SingletonValue {
        classifier: singletons,
    });
    let read = ir.add_expr(IrExpr::PropertyRead {
        receiver: Some(instance),
        owner: singletons,
        name: name.to_string(),
        ty: runtime::composable_lambda(),
        interface: false,
        operation: None,
    });
    ir.mark_positionless(instance);
    ir.mark_positionless(read);
    let IrExpr::Call { args, .. } = &mut ir.exprs[lambda.call as usize] else {
        unreachable!("a singleton lambda is a call's argument");
    };
    let argument = args
        .iter_mut()
        .find(|argument| **argument == lambda.lambda)
        .expect("the call passes the lambda");
    *argument = read;
}
