//! The singletons kotlinc's `ComposerLambdaMemoization` makes of composable lambdas that capture
//! nothing. Each one is held by a property of the file's `ComposableSingletons` object and read
//! from there wherever the lambda is written. Since 2.5 the property's own getter creates the
//! singleton on the first read, into a field that starts empty:
//!
//! ```text
//! object ComposableSingletons$FileKt {
//!     private var `lambda$<key>`: ((Composer, Int) -> Unit)? = null
//!     internal val `lambda$<key>`: (Composer, Int) -> Unit
//!         get() {
//!             if (field == null) field = composableLambdaInstance(<key>, false) { … }
//!             return field
//!         }
//! }
//! ```
//!
//! Earlier releases create every singleton in the object's class initializer instead
//! ([`SingletonCreation::ClassInitializer`]), into a non-null field the property's default getter
//! reads.
//!
//! The object is the plugin's own declaration, with no Kotlin source behind it: its constructor
//! is public, its properties' fields are not final, and it has no declaration metadata.

use crate::ir::{
    ClassId, ExprId, FunId, IrBinOp, IrConst, IrEnclosure, IrExpr, IrField, IrFile, IrFunction,
    IrGeneratedDeclarationDebug, IrGeneratedFunctionMetadataScope, IrGeneratedFunctionPublication,
    IrGeneratedMemberPublication, IrLiftingSequence, IrProperty, IrTypeOp,
};
use crate::plugins::PluginRelease;
use crate::types::{Ty, TypeName, Visibility};

use super::runtime::{self, ComposeRuntime};
use super::source_facts::{ComposableLambda, Memoization};

/// Where the Compose plugin creates a file's composable singletons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SingletonCreation {
    /// In the `ComposableSingletons` class initializer, as the plugin before 2.5 does.
    ClassInitializer,
    /// In each property's getter, on the first read.
    FirstRead,
}

impl SingletonCreation {
    /// The creation of the plugin of `release`; the newest one when no jar named a release.
    pub(super) fn of_release(release: Option<PluginRelease>) -> Self {
        if release.is_some_and(|release| release < PluginRelease::V2_5_0) {
            Self::ClassInitializer
        } else {
            Self::FirstRead
        }
    }

    /// The name of the declaration a singleton lambda is written in, which names the lambda's
    /// implementation and its trace: the property's initializer, `lambda$<key>`, or the
    /// property's getter, `<get-lambda$<key>>`.
    pub(super) fn holder_name(self, key: i32) -> String {
        match self {
            Self::ClassInitializer => format!("lambda${key}"),
            Self::FirstRead => format!("<get-lambda${key}>"),
        }
    }
}

/// Hold those of `lambdas` that capture nothing in the file's singletons class `singletons` and
/// read each one from it.
pub(super) fn memoize(
    ir: &mut IrFile,
    runtime: &ComposeRuntime,
    lambdas: &[ComposableLambda],
    (singletons, creation): (TypeName, SingletonCreation),
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
    ir.mark_declarationless_class(singletons, crate::ir::IrDeclarationlessVisibility::Internal);
    let owner: Box<str> = format!("class:{}", singletons.nested_segment_ref()).into();
    let in_class_initializer = creation == SingletonCreation::ClassInitializer;
    let mut stores = Vec::with_capacity(lambdas.len());
    let mut getters = Vec::new();
    for (index, lambda) in (0u32..).zip(lambdas) {
        let name = format!("lambda${}", lambda.key);
        let ty = runtime::composable_function(&lambda.parameter_types());
        let initializer = instance(ir, runtime, lambda);
        let store = store(ir, class_id, index, initializer);
        // The lambda is created on the line it starts.
        if lambda.opening_line != 0 {
            ir.set_statement_line(store, lambda.opening_line);
        }
        let (getter, container) = if in_class_initializer {
            ir.property_initializer_stores.insert(store);
            stores.push(store);
            (None, IrEnclosure::ClassInitializer(class_id))
        } else {
            let getter = creating_getter(ir, singletons, (class_id, index), &name, store, ty);
            getters.push(IrGeneratedFunctionPublication {
                function: getter,
                parameter_identities: Vec::new(),
                metadata: None,
                debug: IrGeneratedDeclarationDebug::LocalsOnly,
            });
            (Some(getter), IrEnclosure::Function(getter))
        };
        let declaration = &mut ir.classes[class_id as usize];
        declaration.fields.push(IrField::new(
            name.clone(),
            if in_class_initializer {
                ty
            } else {
                Ty::nullable(ty)
            },
        ));
        declaration.properties.push(property(
            &name,
            index,
            getter.is_none().then_some(initializer),
            getter,
            (ty, !in_class_initializer),
        ));
        declaration.methods.extend(getter);
        declaration.methods.push(lambda.implementation);
        read_from_singletons(ir, lambda, singletons, &name);
        let (written, _) = &ir.lifted_functions[&lambda.implementation];
        let sequence = IrLiftingSequence {
            source: written.source,
            owner: owner.clone(),
            container: creation.holder_name(lambda.key).into(),
        };
        ir.relocate_lifted_lambda(lambda.implementation, sequence, index, container);
    }
    if !getters.is_empty() {
        // The class has no declaration metadata, so the getters publish only their locals.
        ir.publish_generated_members(
            singletons,
            IrGeneratedMemberPublication {
                metadata_scope: IrGeneratedFunctionMetadataScope::Exclusive,
                functions: getters,
            },
        );
    }
    if in_class_initializer {
        let init = ir.add_expr(IrExpr::Block {
            stmts: stores,
            value: None,
        });
        ir.classes[class_id as usize].init_body = Some(init);
    }
}

/// `this.<fields[index]> = value`.
fn store(ir: &mut IrFile, class: ClassId, index: u32, value: ExprId) -> ExprId {
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.add_expr(IrExpr::SetField {
        receiver,
        class,
        index,
        value,
    })
}

/// The getter that creates the singleton on its first read:
/// `if (field == null) <store>; return field`. kotlinc builds both statements without offsets,
/// so only the store's creation has a line.
fn creating_getter(
    ir: &mut IrFile,
    singletons: TypeName,
    (class, index): (ClassId, u32),
    property: &str,
    store: ExprId,
    ty: Ty,
) -> FunId {
    let field = |ir: &mut IrFile| {
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.add_expr(IrExpr::GetField {
            receiver,
            class,
            index,
        })
    };
    let current = field(ir);
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let empty = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::RefEq,
        lhs: current,
        rhs: null,
    });
    let create = ir.add_when(vec![(Some(empty), store)], Ty::Unit);
    let created = field(ir);
    let returned = ir.add_expr(IrExpr::Return(Some(created)));
    for statement in [create, returned] {
        ir.mark_positionless(statement);
    }
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![create, returned],
        value: None,
    });
    let name = crate::names::property_getter_name(property);
    let getter = ir.add_fun(IrFunction {
        name: name.clone(),
        params: Vec::new(),
        ret: ty,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(singletons),
        param_checks: Vec::new(),
    });
    ir.fn_source_names.insert(getter, name);
    ir.set_method_visibility(getter, Visibility::Internal);
    getter
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
        type_operand: runtime::composable_function(&lambda.parameter_types()),
    })
}

/// The `internal val` holding one singleton.
fn property(
    name: &str,
    field: u32,
    initializer: Option<ExprId>,
    getter: Option<FunId>,
    (ty, nullable_field): (Ty, bool),
) -> IrProperty {
    IrProperty {
        origin: crate::ir::IrPropertyOrigin::Declaration,
        name: name.to_string(),
        diagnostic_span: None,
        getter_diagnostic_span: None,
        setter_diagnostic_span: None,
        context_params: Vec::new(),
        source_order: field,
        decl_line: 0,
        accessor_lines: crate::ir::IrAccessorLines::default(),
        ty,
        type_params: Vec::new(),
        visibility: Visibility::Internal,
        return_value_status: Default::default(),
        annotations: Box::new([]),
        initializer,
        storage_ty: nullable_field.then(|| Ty::nullable(ty)),
        backing_field: Some(field),
        is_var: false,
        is_open: false,
        modifiers: Default::default(),
        delegate_field: None,
        has_constant_initializer: false,
        is_private: false,
        setter_visibility: Visibility::Internal,
        getter,
        setter: None,
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
        ty: runtime::composable_function(&lambda.parameter_types()),
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

#[cfg(test)]
mod tests {
    use super::SingletonCreation;
    use crate::plugins::PluginRelease;

    #[test]
    fn the_plugin_before_2_5_creates_singletons_in_the_class_initializer() {
        assert_eq!(
            SingletonCreation::of_release(Some(PluginRelease::V2_4_21)),
            SingletonCreation::ClassInitializer
        );
        assert_eq!(
            SingletonCreation::of_release(Some(PluginRelease::V2_5_0)),
            SingletonCreation::FirstRead
        );
        assert_eq!(
            SingletonCreation::of_release(None),
            SingletonCreation::FirstRead
        );
    }

    #[test]
    fn a_singleton_lambda_is_named_after_the_declaration_that_creates_it() {
        assert_eq!(
            SingletonCreation::ClassInitializer.holder_name(-39200502),
            "lambda$-39200502"
        );
        assert_eq!(
            SingletonCreation::FirstRead.holder_name(-39200502),
            "<get-lambda$-39200502>"
        );
    }
}
