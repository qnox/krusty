//! Exact callable identities in one stateful Wasm module.
//!
//! Source-module declarations use the frontend's stable [`CallableId`] while that module is being
//! lowered. Dependency declarations additionally publish their exact KLIB module plus declaration
//! signature, which survives independent analysis and is the only key an external call may use.
//! Wasm indices never come from a declaration spelling.

use crate::fx_hash::{HashMap, HashSet};

use crate::backend::{
    CheckedBackendCallables, DependencyExportSignatures, DependencyFunctionSignature,
    DependencyMemberReceiver, KlibDependencyCallable, SingletonExtensionUnreadReceiver,
    SingletonMemberReceiver,
};
use crate::fir::{CallableId, ExternalCallableId};
use crate::ir::{Callee, ExprId, IrExpr, IrFile};

use super::super::encode::Module;
use super::lower::Unsupported;

/// Whether checked common IR authenticated a singleton *member* call edge. The receiver
/// expression's syntax is deliberately irrelevant: generated dependency support may forward the
/// singleton through a parameter or local while retaining the exact per-expression fact. The
/// frozen callable kind prevents a receiver-reading extension with the same edge fact from being
/// routed through the member ABI after its unread-extension optimization does not apply.
pub(super) fn authenticated_singleton_member_call(
    dispatch_receiver: Option<ExprId>,
    has_dispatch_fact: bool,
    member: bool,
) -> bool {
    member && dispatch_receiver.is_some() && has_dispatch_fact
}

#[derive(Default)]
pub(super) struct CallableRegistry {
    source_module: Option<String>,
    module: HashMap<CallableId, u32>,
    module_defaults: HashMap<CallableId, u32>,
    dependencies: HashMap<KlibDependencyCallable, u32>,
    signatures: HashMap<KlibDependencyCallable, DependencyFunctionSignature>,
    member_receivers: HashMap<KlibDependencyCallable, DependencyMemberReceiver>,
    singleton_member_receivers: HashMap<KlibDependencyCallable, SingletonMemberReceiver>,
    singleton_extension_unread_receivers:
        HashMap<KlibDependencyCallable, SingletonExtensionUnreadReceiver>,
}

pub(super) struct FileCallTargets {
    module: HashMap<CallableId, u32>,
    module_defaults: HashMap<CallableId, u32>,
    external: HashMap<ExternalCallableId, u32>,
    signatures: HashMap<ExternalCallableId, DependencyFunctionSignature>,
    member_receivers: HashMap<ExternalCallableId, DependencyMemberReceiver>,
    singleton_member_receivers: HashMap<ExternalCallableId, SingletonMemberReceiver>,
    singleton_extension_unread_receivers:
        HashMap<ExternalCallableId, SingletonExtensionUnreadReceiver>,
}

impl CallableRegistry {
    /// Begin (or continue) one source module. `CallableId` belongs to one analysis and therefore
    /// cannot cross this boundary; exact module-qualified KLIB identities remain program-wide.
    pub(super) fn enter_source_module(&mut self, module_name: &str) {
        if self.source_module.as_deref() != Some(module_name) {
            self.source_module = Some(module_name.to_owned());
            self.module.clear();
            self.module_defaults.clear();
        }
    }

    /// Declare every emitted function defined by `ir`, reusing an index reserved by an earlier
    /// cross-file call. Every checked callable identity that realizes the same `FunId` binds to
    /// that index. An inline-only function is a cloning template, not a declaration, and therefore
    /// receives no Wasm index and binds no callable identity.
    pub(super) fn declare_file(
        &mut self,
        ir: &IrFile,
        wasm: &mut Module,
    ) -> Result<Vec<Option<u32>>, Unsupported> {
        let mut identities = vec![Vec::new(); ir.functions.len()];
        for (&callable, &function) in &ir.checked_callable_functions {
            let Some(targets) = identities.get_mut(function as usize) else {
                return Err("an invalid source callable identity".to_string());
            };
            targets.push(callable);
        }

        let mut declared = HashSet::default();
        identities
            .into_iter()
            .enumerate()
            .map(|(function, targets)| {
                if ir.inline_only_fns.contains(&(function as crate::ir::FunId)) {
                    if targets.iter().any(|target| {
                        self.module
                            .get(target)
                            .is_some_and(|&index| !wasm.function_is_defined(index))
                    }) {
                        return Err(
                            "a source call reserved an inline-only Wasm function".to_string()
                        );
                    }
                    return Ok(None);
                }
                let mut reserved = targets
                    .iter()
                    .filter_map(|target| self.module.get(target).copied());
                let index = reserved.next().unwrap_or_else(|| wasm.declare());
                if reserved.any(|other| other != index) {
                    return Err(
                        "two source callable identities reserved different Wasm functions"
                            .to_string(),
                    );
                }
                // An inline template's local class is cloned into each file it is spliced into,
                // and every clone realizes the template's callables again.
                if wasm.function_is_defined(index) || !declared.insert(index) {
                    return Err("a callable realized by two functions".to_string());
                }
                for target in targets {
                    self.module.insert(target, index);
                }
                Ok(Some(index))
            })
            .collect()
    }

    /// Declare the default-argument entry owned by every emitted source function that records a
    /// default. A sibling file may already have reserved the entry by exact callable identity.
    pub(super) fn declare_file_defaults(
        &mut self,
        ir: &IrFile,
        wasm: &mut Module,
    ) -> Result<Vec<Option<u32>>, Unsupported> {
        let mut identities = vec![Vec::new(); ir.functions.len()];
        for (&callable, &function) in &ir.checked_callable_functions {
            let Some(targets) = identities.get_mut(function as usize) else {
                return Err("an invalid source callable identity".to_string());
            };
            targets.push(callable);
        }
        identities
            .into_iter()
            .enumerate()
            .map(|(function, targets)| {
                if !emits_default_entry(ir, function as crate::ir::FunId) {
                    return Ok(None);
                }
                let identities =
                    default_entry_identities(ir, function as crate::ir::FunId, targets);
                let mut reserved = identities
                    .iter()
                    .filter_map(|target| self.module_defaults.get(target).copied());
                let index = reserved.next().unwrap_or_else(|| wasm.declare());
                if reserved.any(|other| other != index) {
                    return Err(
                        "two source callable identities reserved different Wasm default entries"
                            .to_string(),
                    );
                }
                for target in identities {
                    self.module_defaults.insert(target, index);
                }
                Ok(Some(index))
            })
            .collect()
    }

    /// Resolve every direct cross-file/dependency call before a body is emitted. A source target
    /// may reserve a forward function index; a dependency target must already have been published
    /// by an earlier source module.
    pub(super) fn file_calls(
        &mut self,
        ir: &IrFile,
        callables: &CheckedBackendCallables,
        wasm: &mut Module,
    ) -> Result<FileCallTargets, Unsupported> {
        let mut targets = FileCallTargets {
            module: HashMap::default(),
            module_defaults: HashMap::default(),
            external: HashMap::default(),
            signatures: HashMap::default(),
            member_receivers: HashMap::default(),
            singleton_member_receivers: HashMap::default(),
            singleton_extension_unread_receivers: HashMap::default(),
        };
        for expression in emitted_call_roots(ir)
            .into_iter()
            .flat_map(|root| crate::ir::value_namespace_expressions(ir, root))
        {
            let accessor_call;
            let (callee, dispatch_receiver) = match ir.expr(expression) {
                IrExpr::Call {
                    callee,
                    dispatch_receiver,
                    ..
                } => (&**callee, *dispatch_receiver),
                IrExpr::Checked(operation) => {
                    let Some(access) = callables.dependency_accessor(operation) else {
                        continue;
                    };
                    if !access
                        .callable
                        .dependency_callable
                        .as_ref()
                        .is_some_and(|identity| self.dependencies.contains_key(identity))
                    {
                        continue;
                    }
                    accessor_call = access.callee().map_err(str::to_owned)?;
                    (&accessor_call, access.receiver)
                }
                _ => continue,
            };
            match callee {
                Callee::Module { target, .. } if dispatch_receiver.is_none() => {
                    targets
                        .module
                        .insert(*target, self.module_index(*target, wasm));
                }
                Callee::CrossFile {
                    module_target: Some(target),
                    ..
                } if dispatch_receiver.is_none() => {
                    targets
                        .module
                        .insert(*target, self.module_index(*target, wasm));
                }
                Callee::ModuleWithDefaults {
                    target,
                    default_provider: crate::fir::ResolvedFunctionOverrideTarget::Module(provider),
                    dispatch_receiver_ty: None,
                    extension_receiver_parameter: None,
                    ..
                } if dispatch_receiver.is_none() => {
                    targets
                        .module_defaults
                        .insert(*provider, self.module_default_index(*provider, wasm));
                }
                Callee::External {
                    target,
                    params,
                    defaults,
                    extension_receiver_parameter,
                    ..
                } if defaults.is_empty() => {
                    let callable = callables.callable(*target).ok_or_else(|| {
                        "a dependency call without frozen callable facts".to_string()
                    })?;
                    if callable
                        .compiler_operation()
                        .is_some_and(super::lower::is_direct_operation)
                        && !callable
                            .dependency_callable
                            .as_ref()
                            .is_some_and(|identity| self.dependencies.contains_key(identity))
                    {
                        // Emission realizes this exact operation; there is no dependency entry.
                        continue;
                    }
                    let supported_shape = callable.matches_dependency_body_call(
                        params,
                        *extension_receiver_parameter,
                        dispatch_receiver.is_some(),
                    );
                    // A published exact export owns even a specialized generic call. Its actual
                    // operand roles are authenticated from checked substitutions during emission,
                    // not by comparing specialized parameters to the uninstantiated declaration.
                    let published = callable
                        .dependency_callable
                        .as_ref()
                        .is_some_and(|identity| self.dependencies.contains_key(identity));
                    let serialized_companion_candidate = dispatch_receiver.is_none()
                        && callable.is_top_level_function()
                        && extension_receiver_parameter
                            .and_then(|position| usize::try_from(position).ok())
                            .is_some_and(|position| {
                                callable.klib_body_companion_receiver_position() == Some(position)
                            });
                    if !published && !supported_shape && !serialized_companion_candidate {
                        continue;
                    }
                    let identity = callable.dependency_callable.as_ref().ok_or_else(|| {
                        "a dependency callable without an exact module-qualified KLIB identity"
                            .to_string()
                    })?;
                    let unread_receiver = self
                        .singleton_extension_unread_receivers
                        .get(identity)
                        .copied();
                    if !published
                        && !supported_shape
                        && callable
                            .serialized_companion_receiver_position(
                                params,
                                *extension_receiver_parameter,
                                dispatch_receiver.is_some(),
                                unread_receiver.map(|receiver| receiver.semantic_type()),
                            )
                            .is_none()
                    {
                        continue;
                    }
                    let index = self.dependencies.get(identity).copied().ok_or_else(|| {
                        "a dependency callable whose target code was not emitted".to_string()
                    })?;
                    targets.external.insert(*target, index);
                    targets.signatures.insert(
                        *target,
                        self.signatures
                            .get(identity)
                            .ok_or_else(|| {
                                "an emitted dependency without its declared signature".to_owned()
                            })?
                            .clone(),
                    );
                    if let Some(receiver) = self.member_receivers.get(identity).copied() {
                        targets.member_receivers.insert(*target, receiver);
                    }
                    if let Some(receiver) = self.singleton_member_receivers.get(identity).copied() {
                        targets.singleton_member_receivers.insert(*target, receiver);
                    }
                    if let Some(receiver) = unread_receiver {
                        targets
                            .singleton_extension_unread_receivers
                            .insert(*target, receiver);
                    }
                }
                _ => {}
            }
        }
        Ok(targets)
    }

    /// Publish this file's exact module-qualified dependency identities only after its functions
    /// have been defined.
    pub(super) fn publish(
        &mut self,
        ir: &IrFile,
        exports: &DependencyExportSignatures,
        functions: &[Option<u32>],
    ) -> Result<(), Unsupported> {
        for (function, identity) in exports.callables() {
            let emitted = functions
                .get(function as usize)
                .copied()
                .ok_or_else(|| "an invalid exported callable identity".to_string())?;
            let index = emitted
                .ok_or_else(|| "an inline-only function exported as Wasm code".to_string())?;
            let signature = DependencyFunctionSignature::from_ir(
                ir,
                function,
                exports.member_receiver(function),
            )
            .map_err(str::to_owned)?;
            if self
                .signatures
                .get(identity)
                .is_some_and(|previous| previous != &signature)
            {
                return Err("two functions exported incompatible dependency signatures".to_owned());
            }
            match self.dependencies.insert(identity.clone(), index) {
                Some(previous) if previous != index => {
                    return Err("two functions exported one KLIB declaration identity".to_string())
                }
                _ => {}
            }
            self.signatures.insert(identity.clone(), signature);
            if let Some(receiver) = exports.member_receiver(function) {
                match self.member_receivers.insert(identity.clone(), receiver) {
                    Some(previous) if previous != receiver => {
                        return Err(
                            "two functions exported incompatible member receiver types".to_string()
                        )
                    }
                    _ => {}
                }
            }
            if let Some(receiver) = exports.singleton_extension_unread_receiver(function) {
                match self
                    .singleton_extension_unread_receivers
                    .insert(identity.clone(), receiver)
                {
                    Some(previous) if previous != receiver => return Err(
                        "two functions exported incompatible singleton-extension receiver effects"
                            .to_string(),
                    ),
                    _ => {}
                }
            }
            if let Some(receiver) = exports.singleton_member_receiver(function) {
                match self
                    .singleton_member_receivers
                    .insert(identity.clone(), receiver)
                {
                    Some(previous) if previous != receiver => {
                        return Err(
                            "two functions exported incompatible singleton-member receiver effects"
                                .to_string(),
                        )
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// The function `target` of this source module is, reserving its index when the file that
    /// declares it has not been lowered yet.
    pub(super) fn module_index(&mut self, target: CallableId, wasm: &mut Module) -> u32 {
        *self.module.entry(target).or_insert_with(|| wasm.declare())
    }

    /// The function an earlier source module published under `identity`.
    pub(super) fn dependency(&self, identity: &KlibDependencyCallable) -> Option<u32> {
        self.dependencies.get(identity).copied()
    }

    fn module_default_index(&mut self, target: CallableId, wasm: &mut Module) -> u32 {
        *self
            .module_defaults
            .entry(target)
            .or_insert_with(|| wasm.declare())
    }
}

/// Roots of every expression body this Wasm file physically emits. Constructor expressions are
/// not `IrFunction` bodies: class lowering emits the primary delegation/initializer and each
/// secondary delegation/body separately, so call discovery must include those exact roots while
/// still excluding cloning-only inline templates.
fn emitted_call_roots(ir: &IrFile) -> Vec<ExprId> {
    let mut roots = crate::ir::emitted_body_roots(ir);
    for function in 0..ir.functions.len() {
        let function = function as crate::ir::FunId;
        if !emits_default_entry(ir, function) {
            continue;
        }
        roots.extend(
            ir.param_defaults(function)
                .into_iter()
                .flatten()
                .filter_map(|default| *default),
        );
    }
    roots
}

/// Whether `function` owns the exact top-level source bridge that this Wasm backend can route.
pub(super) fn emits_default_entry(ir: &IrFile, function: crate::ir::FunId) -> bool {
    let declaration = &ir.functions[function as usize];
    declaration.body.is_some()
        && declaration.dispatch_receiver.is_none()
        && !ir.inline_only_fns.contains(&function)
        && !ir.extension_receiver_fns.contains(&function)
        && ir
            .checked_callable_functions
            .values()
            .any(|&candidate| candidate == function)
        && ir
            .param_defaults(function)
            .is_some_and(|defaults| defaults.iter().any(Option::is_some))
}

fn default_entry_identities(
    ir: &IrFile,
    function: crate::ir::FunId,
    declared: Vec<CallableId>,
) -> Vec<CallableId> {
    match ir.inherited_param_default_provider(function) {
        Some(crate::fir::ResolvedFunctionOverrideTarget::Module(provider)) => vec![provider],
        Some(crate::fir::ResolvedFunctionOverrideTarget::External(_)) => Vec::new(),
        None => declared,
    }
}

impl FileCallTargets {
    pub(super) fn module(&self, target: CallableId) -> Option<u32> {
        self.module.get(&target).copied()
    }

    pub(super) fn external(&self, target: ExternalCallableId) -> Option<u32> {
        self.external.get(&target).copied()
    }

    pub(super) fn signature(
        &self,
        target: ExternalCallableId,
    ) -> Option<&DependencyFunctionSignature> {
        self.signatures.get(&target)
    }

    pub(super) fn member_receiver(
        &self,
        target: ExternalCallableId,
    ) -> Option<DependencyMemberReceiver> {
        self.member_receivers.get(&target).copied()
    }

    pub(super) fn singleton_extension_unread_receiver(
        &self,
        target: ExternalCallableId,
    ) -> Option<SingletonExtensionUnreadReceiver> {
        self.singleton_extension_unread_receivers
            .get(&target)
            .copied()
    }

    pub(super) fn singleton_member_receiver(
        &self,
        target: ExternalCallableId,
    ) -> Option<SingletonMemberReceiver> {
        self.singleton_member_receivers.get(&target).copied()
    }

    pub(super) fn module_default(&self, target: CallableId) -> Option<u32> {
        self.module_defaults.get(&target).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendCallableFact;
    use crate::ir::IrFunction;
    use crate::libraries::{
        ExternalCallableKind, InlineKind, KlibBodyCompanionReceiver, KlibDeclarationSignature,
        MemberRealization,
    };
    use crate::metadata::id_signature::{
        callable_signature, CallableShape, DeclarationContainer, Placement,
    };
    use crate::metadata::semantic::KotlinType;
    use crate::types::{type_name, Ty};
    use crate::wasm::encode::{Code, Function};

    #[test]
    fn a_recorded_singleton_member_edge_does_not_require_literal_receiver_syntax() {
        let forwarded_local = Some(7);

        assert!(authenticated_singleton_member_call(
            forwarded_local,
            true,
            true
        ));
        assert!(!authenticated_singleton_member_call(
            forwarded_local,
            false,
            true
        ));
        assert!(!authenticated_singleton_member_call(None, true, true));
    }

    #[test]
    fn a_singleton_bound_receiver_reading_extension_is_not_a_member_call() {
        let forwarded_local = Some(7);

        assert!(!authenticated_singleton_member_call(
            forwarded_local,
            true,
            false,
        ));
    }

    fn function(name: &str, body: Option<crate::ir::ExprId>) -> IrFunction {
        IrFunction {
            name: name.to_string(),
            param_checks: Vec::new(),
            params: Vec::new(),
            ret: Ty::Unit,
            body,
            is_static: true,
            dispatch_receiver: None,
        }
    }

    fn signature(name: &str) -> KlibDeclarationSignature {
        let package = vec!["demo".to_string()];
        let container = DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        };
        KlibDeclarationSignature::Public(
            callable_signature(
                container,
                &CallableShape {
                    name,
                    contexts: Vec::new(),
                    receiver: None,
                    params: Vec::new(),
                    vararg: None,
                    type_parameters: Vec::new(),
                    expect: false,
                    placement: Placement::Ordinary,
                },
            )
            .expect("a parameterless function has a public signature"),
        )
    }

    fn companion_callable() -> BackendCallableFact {
        BackendCallableFact {
            name: "fromBits".to_string(),
            physical_name: None,
            reflection_name: None,
            physical_owner: type_name("kotlin/Float"),
            visibility: crate::types::Visibility::Public,
            declaration_owner: None,
            singleton_dispatch: None,
            kind: ExternalCallableKind::TopLevel,
            owner_is_interface: false,
            compiler_intrinsic: None,
            semantic_role: None,
            overridden_declarations: Box::new([]),
            member_realization: MemberRealization::Dispatch,
            params: vec![Ty::String, Ty::Int],
            physical_params: Vec::new(),
            parameter_identities: Box::new([]),
            physical_parameter_plan: None,
            ret: Ty::Float,
            physical_ret: Ty::Float,
            descriptor: String::new(),
            inline: InlineKind::None,
            source_receiver: None,
            context_count: 1,
            declared_params: None,
            declared_ret: None,
            inline_modifiers: Box::new([]),
            default_realization: None,
            nonvirtual_realization: None,
            overridden_call_realizations: Box::new([]),
            generic_sig: None,
            reified_type_parameter_ordinals: Box::new([]),
            declaration_signature: None,
            dependency_callable: None,
            direct_c_function: None,
            klib_body_companion_receiver: Some(KlibBodyCompanionReceiver::new(type_name(
                "kotlin/Float",
            ))),
            klib_body_type_parameters: Box::new([]),
            klib_body_declaration_modifiers: None,
            klib_body_class_contract: None,
            klib_body_accessor: None,
            klib_body_vararg_index: None,
            klib_body_parameter_defaults: None,
            annotations: Box::new([]),
        }
    }

    #[test]
    fn a_serialized_companion_call_keeps_exactly_its_authenticated_receiver_slot() {
        let callable = companion_callable();
        let receiver = Ty::obj_name(type_name("kotlin/Float"));
        let params = [Ty::String, receiver, Ty::Int];

        assert_eq!(
            callable.serialized_companion_receiver_position(
                &params,
                Some(1),
                false,
                Some(receiver),
            ),
            Some(1)
        );
        assert_eq!(
            callable.serialized_companion_receiver_position(
                &params,
                Some(0),
                false,
                Some(receiver)
            ),
            None
        );
        assert_eq!(
            callable.serialized_companion_receiver_position(&params, Some(1), false, None),
            None
        );
        assert_eq!(
            callable.serialized_companion_receiver_position(
                &[Ty::String, Ty::Int, Ty::Int],
                Some(1),
                false,
                Some(receiver),
            ),
            None
        );
        assert_eq!(
            callable
                .serialized_companion_receiver_position(&params, Some(1), true, Some(receiver),),
            None
        );
    }

    #[test]
    fn source_identities_reset_between_modules_but_exact_exports_remain() {
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        let local = CallableId::from_raw(0);

        registry.enter_source_module("lib");
        let library_index = registry.module_index(local, &mut wasm);
        let signature = signature("answer");
        let identity = KlibDependencyCallable {
            module: "lib".to_string(),
            declaration: signature,
        };
        let mut exports = DependencyExportSignatures::default();
        exports.record_callable(0, identity.clone());
        exports.record_singleton_member_receiver(0, type_name("demo/Companion"), false);
        exports.record_singleton_extension_unread_receiver(
            0,
            Ty::obj_name(type_name("demo/Companion")),
            true,
        );
        let mut ir = IrFile::default();
        ir.add_fun(function("answer", None));
        registry
            .publish(&ir, &exports, &[Some(library_index)])
            .expect("the library export is valid");
        assert_eq!(
            registry.singleton_extension_unread_receivers[&identity].semantic_type(),
            Ty::obj_name(type_name("demo/Companion"))
        );
        assert!(registry.singleton_extension_unread_receivers[&identity].evaluate_source());
        assert_eq!(
            registry.singleton_member_receivers[&identity].classifier(),
            type_name("demo/Companion")
        );
        assert!(!registry.singleton_member_receivers[&identity].evaluate_source());

        registry.enter_source_module("app");
        let application_index = registry.module_index(local, &mut wasm);
        assert_ne!(application_index, library_index);
        assert_eq!(registry.dependencies[&identity], library_index);
    }

    #[test]
    fn a_dependency_export_retains_one_declared_abi_and_rejects_contradictions() {
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        let mut ir = IrFile::default();
        let formal = Ty::ty_param("provider-owned-T", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut declaration = function("identity", None);
        declaration.params = vec![formal];
        declaration.ret = formal;
        ir.add_fun(declaration);
        let identity = KlibDependencyCallable {
            module: "lib".to_owned(),
            declaration: signature("identity"),
        };
        let mut exports = DependencyExportSignatures::default();
        exports.record_callable(0, identity.clone());
        let index = wasm.declare();
        registry.publish(&ir, &exports, &[Some(index)]).unwrap();
        assert_eq!(registry.dependencies[&identity], index);
        assert_eq!(registry.signatures[&identity].parameters(), [formal]);
        assert_eq!(registry.signatures[&identity].result(), formal);
        let foreign = KlibDependencyCallable {
            module: "other-lib".to_owned(),
            declaration: identity.declaration.clone(),
        };
        assert!(!registry.dependencies.contains_key(&foreign));
        assert!(!registry.signatures.contains_key(&foreign));
        ir.functions[0].params = vec![Ty::Int];
        ir.functions[0].ret = Ty::Int;
        assert_eq!(
            registry.publish(&ir, &exports, &[Some(index)]),
            Err("two functions exported incompatible dependency signatures".to_owned())
        );
        assert_eq!(registry.signatures[&identity].parameters(), [formal]);
        assert_eq!(registry.signatures[&identity].result(), formal);
    }

    #[test]
    fn an_inline_only_template_binds_no_callable_and_declares_no_function() {
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        let callable = CallableId::from_raw(0);
        let mut ir = IrFile::default();
        let function = ir.add_fun(crate::ir::IrFunction {
            name: "template".to_string(),
            param_checks: Vec::new(),
            params: Vec::new(),
            ret: crate::types::Ty::String,
            body: None,
            is_static: true,
            dispatch_receiver: None,
        });
        ir.checked_callable_functions.insert(callable, function);
        ir.inline_only_fns.insert(function);

        registry.enter_source_module("app");
        let functions = registry
            .declare_file(&ir, &mut wasm)
            .expect("the inline template is a valid non-emitted function");

        assert_eq!(functions, [None]);
        assert!(!registry.module.contains_key(&callable));
        assert_eq!(wasm.declare(), 0);
    }

    #[test]
    fn calls_owned_only_by_an_inline_template_reserve_no_wasm_target() {
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        let dead_target = CallableId::from_raw(2);
        let mut ir = IrFile::default();
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Module {
                target: dead_target,
                name: "dead".to_string(),
                params: Vec::new(),
                ret: Ty::Unit,
            }),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let template = ir.add_fun(function("template", Some(call)));
        ir.inline_only_fns.insert(template);
        let entry_body = ir.add_expr(IrExpr::UnitInstance);
        ir.add_fun(function("entry", Some(entry_body)));

        registry.enter_source_module("app");
        let functions = registry
            .declare_file(&ir, &mut wasm)
            .expect("the emitted entry has one declaration");
        let calls = registry
            .file_calls(&ir, &CheckedBackendCallables::default(), &mut wasm)
            .expect("a dead inline template cannot affect emitted calls");

        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0], None);
        assert!(functions[1].is_some());
        assert_eq!(calls.module(dead_target), None);
        assert!(!registry.module.contains_key(&dead_target));
    }

    #[test]
    fn a_primary_constructor_initializer_registers_its_module_call() {
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        let target = CallableId::from_raw(0);
        let mut ir = IrFile::default();
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Module {
                target,
                name: "initialize".to_string(),
                params: Vec::new(),
                ret: Ty::Unit,
            }),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let mut class = crate::ir::IrClass::synthetic(crate::types::type_name("demo/Box"));
        class.init_body = Some(call);
        ir.classes.push(class);

        registry.enter_source_module("app");
        let calls = registry
            .file_calls(&ir, &CheckedBackendCallables::default(), &mut wasm)
            .expect("a supported constructor initializer has registered calls");

        assert_eq!(calls.module(target), registry.module.get(&target).copied());
        assert!(calls.module(target).is_some());
    }

    #[test]
    fn an_inline_template_accepts_defined_identity_but_rejects_a_forward_reservation() {
        fn inline_file(callable: CallableId) -> IrFile {
            let mut ir = IrFile::default();
            let template = ir.add_fun(function("template", None));
            ir.checked_callable_functions.insert(callable, template);
            ir.inline_only_fns.insert(template);
            ir
        }

        let callable = CallableId::from_raw(0);
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        registry.enter_source_module("app");
        let physical = registry.module_index(callable, &mut wasm);
        let ty = wasm.func_type(Vec::new(), Vec::new());
        wasm.define(
            physical,
            Function {
                ty,
                locals: Vec::new(),
                code: Code::default(),
            },
        );
        assert_eq!(
            registry
                .declare_file(&inline_file(callable), &mut wasm)
                .expect("an existing physical definition remains authoritative"),
            [None]
        );

        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        registry.enter_source_module("app");
        registry.module_index(callable, &mut wasm);
        assert_eq!(
            registry.declare_file(&inline_file(callable), &mut wasm),
            Err("a source call reserved an inline-only Wasm function".to_string())
        );
    }

    #[test]
    fn inherited_default_entry_is_keyed_by_provider_and_calls_the_actual_function() {
        let target = CallableId::from_raw(1);
        let provider = CallableId::from_raw(2);
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        registry.enter_source_module("app");

        let mut caller = IrFile::default();
        let call = caller.add_expr(IrExpr::Call {
            callee: Box::new(Callee::ModuleWithDefaults {
                target,
                default_provider: crate::fir::ResolvedFunctionOverrideTarget::Module(provider),
                name: "answer".to_string(),
                params: vec![Ty::String],
                ret: Ty::String,
                defaults: vec![0].into_boxed_slice(),
                dispatch_receiver_ty: None,
                extension_receiver_parameter: None,
            }),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        caller.add_fun(function("box", Some(call)));
        let calls = registry
            .file_calls(&caller, &CheckedBackendCallables::default(), &mut wasm)
            .expect("the inherited provider reserves its exact entry");
        let reserved = calls
            .module_default(provider)
            .expect("the provider owns the entry");
        assert_eq!(calls.module_default(target), None);

        let mut declaration = IrFile::default();
        let default = declaration.add_expr(IrExpr::Const(crate::ir::IrConst::String(
            crate::kt_string::KtString::from("OK"),
        )));
        let body = declaration.add_expr(IrExpr::GetValue(0));
        let function = declaration.add_fun(IrFunction {
            name: "answer".to_string(),
            param_checks: Vec::new(),
            params: vec![Ty::String],
            ret: Ty::String,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
        });
        declaration
            .checked_callable_functions
            .insert(target, function);
        let mut parameters =
            crate::ir::FnParamInfo::source_defaults(vec!["value".to_string()], vec![Some(default)]);
        parameters.inherited_default_provider =
            Some(crate::fir::ResolvedFunctionOverrideTarget::Module(provider));
        declaration.fn_params.insert(function, parameters);

        let entries = registry
            .declare_file_defaults(&declaration, &mut wasm)
            .expect("the actual declaration realizes the provider-owned entry");
        assert_eq!(entries[function as usize], Some(reserved));
        assert_eq!(registry.module_defaults.get(&provider), Some(&reserved));
        assert!(!registry.module_defaults.contains_key(&target));
    }
}
