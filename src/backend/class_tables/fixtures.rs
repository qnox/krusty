//! Hand-built IR files for testing class tables: classes, methods and the override edges the
//! frontend would record between them. Shared by every target's class-layout tests.

use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{ClassId, FunId, IrClass, IrField, IrFile, IrFunction, IrProperty, IrfFlags};
use crate::types::Ty;

pub(crate) fn function(
    name: &str,
    owner: &str,
    params: Vec<Ty>,
    ret: Ty,
    abstract_: bool,
) -> IrFunction {
    IrFunction {
        name: name.to_string(),
        params,
        ret,
        body: if abstract_ { None } else { Some(0) },
        is_static: false,
        dispatch_receiver: Some(crate::types::type_name(owner)),
        param_checks: Vec::new(),
    }
}

pub(crate) fn field(name: &str, ty: Ty) -> IrField {
    IrField {
        name: name.to_string(),
        ty,
        // A debug coordinate for a primary-constructor store, which a synthesized fixture has
        // no source line for; zero is what "none retained" is spelled as.
        constructor_store_line: 0,
        type_param: None,
        default: None,
        flags: IrfFlags::default(),
    }
}

pub(crate) fn stored_property(name: &str, ty: Ty, field: u32) -> IrProperty {
    IrProperty {
        name: name.to_string(),
        context_params: Vec::new(),
        source_order: 0,
        decl_line: 0,
        ty,
        type_params: Vec::new(),
        visibility: crate::types::Visibility::Public,
        return_value_status: Default::default(),
        annotations: Box::new([]),
        initializer: None,
        has_constant_initializer: false,
        storage_ty: None,
        backing_field: Some(field),
        is_var: false,
        is_open: false,
        modifiers: Default::default(),
        delegate_field: None,
        is_private: false,
        setter_visibility: crate::types::Visibility::Public,
        getter: None,
        setter: None,
        getter_jvm_name: None,
        setter_jvm_name: None,
        needs_access_bridge: false,
        accessor_annotations: Default::default(),
    }
}

pub(crate) fn class(ir: &mut IrFile, name: &str, superclass: &str, depth: u32) -> ClassId {
    let mut class = IrClass::synthetic(crate::types::type_name(name));
    class.superclass = crate::types::type_name(superclass);
    class.is_open = true;
    let id = ir.classes.len() as ClassId;
    ir.classes.push(class);
    let mut applied = vec![crate::ir::IrAppliedClassifier {
        classifier: crate::types::type_name(name),
        applied: Ty::obj(name),
        depth: 0,
    }];
    if depth > 0 {
        applied.push(crate::ir::IrAppliedClassifier {
            classifier: crate::types::type_name(superclass),
            applied: Ty::obj(superclass),
            depth,
        });
    }
    ir.classifier_hierarchies
        .insert(crate::types::type_name(name), applied);
    id
}

pub(crate) fn add_method(ir: &mut IrFile, class: ClassId, function: IrFunction) -> FunId {
    let fid = ir.functions.len() as FunId;
    ir.functions.push(function);
    ir.classes[class as usize].methods.push(fid);
    fid
}

pub(crate) fn record_override(
    ir: &mut IrFile,
    class: ClassId,
    implementation: FunId,
    overridden: FunId,
) {
    // The frontend records overrides by callable identity; the test uses the function index
    // as that identity, which is what `checked_callable_functions` maps back.
    use crate::fir::CallableId;
    let implementation_id = CallableId::from_raw(1000 + implementation);
    let overridden_id = CallableId::from_raw(1000 + overridden);
    ir.checked_callable_functions
        .insert(implementation_id, implementation);
    ir.checked_callable_functions
        .insert(overridden_id, overridden);
    let name = ir.functions[implementation as usize].name.clone();
    let owner = ir.classes[class as usize].fq_name_id();
    ir.function_overrides
        .entry(owner)
        .or_default()
        .push(crate::ir::IrFunctionOverride {
            implementation: ResolvedFunctionOverrideTarget::Module(implementation_id),
            implementation_function: None,
            implementation_owner: owner,
            overridden: ResolvedFunctionOverrideTarget::Module(overridden_id),
            overridden_owner: ir.functions[overridden as usize]
                .dispatch_receiver
                .expect("instance method"),
            overridden_semantic_role: None,
            collection_barrier: None,
            overridden_is_interface: false,
            name,
            declared_parameters: Vec::new(),
            declared_result: Ty::Unit,
            applied_parameters: Vec::new(),
            applied_result: Ty::Unit,
            implementation_parameters: Vec::new(),
            implementation_parameter_identities: Vec::new(),
            overridden_parameter_identities: Vec::new(),
            implementation_result: Ty::Unit,
            suspend: false,
            has_kotlin_superclass_override: false,
            depth: 1,
        });
}

pub(crate) fn record_semantic_override(
    ir: &mut IrFile,
    class: ClassId,
    implementation: FunId,
    role: crate::types::SemanticCallRole,
) {
    use crate::fir::{CallableId, ExternalCallableId};
    let owner = ir.classes[class as usize].fq_name_id();
    let name = ir.functions[implementation as usize].name.clone();
    ir.function_overrides
        .entry(owner)
        .or_default()
        .push(crate::ir::IrFunctionOverride {
            implementation: ResolvedFunctionOverrideTarget::Module(CallableId::from_raw(
                3000 + implementation,
            )),
            implementation_function: Some(implementation),
            implementation_owner: owner,
            overridden: ResolvedFunctionOverrideTarget::External(ExternalCallableId::from_raw(2)),
            overridden_owner: crate::types::type_name("fixture/Root"),
            overridden_semantic_role: Some(role),
            collection_barrier: None,
            overridden_is_interface: false,
            name,
            declared_parameters: Vec::new(),
            declared_result: Ty::Unit,
            applied_parameters: Vec::new(),
            applied_result: Ty::Unit,
            implementation_parameters: Vec::new(),
            implementation_parameter_identities: Vec::new(),
            overridden_parameter_identities: Vec::new(),
            implementation_result: Ty::Unit,
            suspend: false,
            has_kotlin_superclass_override: false,
            depth: 1,
        });
}
