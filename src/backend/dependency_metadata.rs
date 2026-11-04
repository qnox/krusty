//! Dependency metadata emitted beside target code.
//!
//! A non-JVM source module publishes a KLIB to later source modules while its target backend emits
//! code for the final program. Both products must observe the same checked IR. This boundary is
//! deliberately read-only: it records metadata and the exact public declaration identities before
//! target representation mutates the file, without cloning or re-resolving the program.

use crate::fx_hash::HashMap;

use super::{Artifact, CheckedIrFile, CompilationTarget};
use crate::diag::DiagSink;
use crate::ir::FunId;
pub use crate::libraries::KlibDependencyCallable;

/// Whether `callable` has a standalone body a target can publish for another source module.
///
/// KLIB metadata also describes declarations which intentionally have no callable target: an
/// `@InlineOnly` declaration is a template consumed by common lowering, while an external or
/// abstract declaration has no body in this IR unit. Those declarations remain in the library's
/// semantic metadata, but recording either one in [`DependencyExportSignatures`] would promise a
/// physical definition which no backend can emit.
pub(crate) fn has_standalone_dependency_code(ir: &crate::ir::IrFile, callable: FunId) -> bool {
    ir.functions
        .get(callable as usize)
        .is_some_and(|function| function.body.is_some())
        && !ir.inline_only_fns.contains(&callable)
}

/// Whether an exact checked call edge supplies the singleton receiver type an emitted dependency
/// body recorded. The receiver node may be a local read after inline callable-reference staging;
/// its syntax carries no authority. The per-expression singleton classifier frozen from common IR
/// is the semantic proof, while a present receiver preserves the source call's operand shape.
pub(crate) fn authenticated_singleton_receiver(
    receiver: Option<crate::ir::ExprId>,
    selected_classifier: Option<crate::types::TypeName>,
    expected: crate::types::Ty,
) -> bool {
    let crate::types::TyKind::Obj(expected_classifier, arguments) = expected.kind() else {
        return false;
    };
    receiver.is_some() && arguments.is_empty() && selected_classifier == Some(expected_classifier)
}

/// Staged callable-reference singleton receivers whose source access a target may omit.
///
/// Common lowering owns which declaration stages the bound receiver. This boundary additionally
/// proves that every read of its slot is used only as the dispatch receiver of an exact checked
/// dependency call, and that every such call joins to the same inert exported receiver effect.
/// A shared `GetValue` node used in any second position fails the edge-count equality and remains
/// evaluated.
pub(crate) fn inert_staged_singleton_bindings(
    ir: &crate::ir::IrFile,
    mut singleton_dispatch: impl FnMut(crate::ir::ExprId) -> Option<crate::types::TypeName>,
    mut receiver_is_inert: impl FnMut(crate::fir::ExternalCallableId, crate::types::TypeName) -> bool,
) -> crate::fx_hash::HashSet<crate::ir::ExprId> {
    use crate::ir::{Callee, IrExpr};

    let mut inert = crate::fx_hash::HashSet::default();
    for &declaration in &ir.inline_callable_reference_receiver_bindings {
        let IrExpr::Variable {
            index,
            init: Some(initializer),
            ..
        } = ir.expr(declaration)
        else {
            continue;
        };
        let IrExpr::SingletonValue { classifier } = ir.expr(*initializer) else {
            continue;
        };
        let Some(namespace) = ir
            .functions
            .iter()
            .filter_map(|function| function.body)
            .find_map(|body| {
                let expressions = crate::ir::value_namespace_expressions(ir, body);
                expressions.contains(&declaration).then_some(expressions)
            })
        else {
            continue;
        };
        let namespace = namespace
            .into_iter()
            .collect::<crate::fx_hash::HashSet<_>>();
        let reads = namespace
            .iter()
            .filter_map(|&expression| {
                let node = ir.expr(expression);
                matches!(node, IrExpr::GetValue(slot) if slot == index).then_some(expression)
            })
            .collect::<crate::fx_hash::HashSet<_>>();
        if reads.is_empty() {
            continue;
        }
        let mut child_uses = HashMap::<crate::ir::ExprId, usize>::default();
        for &parent in &namespace {
            crate::ir::for_each_child(&ir.exprs, parent, &mut |child| {
                if reads.contains(&child) {
                    *child_uses.entry(child).or_default() += 1;
                }
            });
        }
        let mut approved = HashMap::<crate::ir::ExprId, usize>::default();
        for &expression in &namespace {
            let node = ir.expr(expression);
            let IrExpr::Call {
                callee: call_callee,
                dispatch_receiver: Some(receiver),
                ..
            } = node
            else {
                continue;
            };
            let Callee::External { target, .. } = &**call_callee else {
                continue;
            };
            if !reads.contains(receiver) || singleton_dispatch(expression) != Some(*classifier) {
                continue;
            }
            if receiver_is_inert(*target, *classifier) {
                *approved.entry(*receiver).or_default() += 1;
            }
        }
        if reads.iter().all(|read| {
            approved.get(read).copied().unwrap_or(0) == child_uses.get(read).copied().unwrap_or(0)
                && approved.get(read).copied().unwrap_or(0) > 0
        }) {
            inert.insert(declaration);
        }
    }
    inert
}

/// Frozen proof that a dependency body does not read its exact extension-receiver parameter. A
/// target may supply an inert carrier only when the checked call's receiver is a singleton of this
/// exact type. `evaluate_source` remains true unless the provider also proved, from the exact
/// serialized object declaration, that accessing this platform singleton runs no initializer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SingletonExtensionUnreadReceiver {
    semantic_type: crate::types::Ty,
    evaluate_source: bool,
}

impl SingletonExtensionUnreadReceiver {
    /// The frozen semantic type of the retained dependency ABI parameter.
    pub fn semantic_type(self) -> crate::types::Ty {
        self.semantic_type
    }

    /// Whether an ordinary source singleton access must run before its value is substituted.
    pub fn evaluate_source(self) -> bool {
        self.evaluate_source
    }
}

/// Frozen source-evaluation effect for the exact singleton dispatch erased from an emitted
/// dependency member. The classifier authenticates the per-call singleton fact; source evaluation
/// may be omitted only when the provider proved that accessing this exact serialized object runs
/// no initializer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SingletonMemberReceiver {
    classifier: crate::types::TypeName,
    evaluate_source: bool,
}

/// Exact semantic dispatch receiver retained by an emitted ordinary dependency member.
///
/// Backends receive this as a closed handoff fact rather than importing the common type system in
/// their module/registry facades. The lowering boundary may project the semantic type when it
/// constructs the target ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyMemberReceiver(crate::types::Ty);

/// A source property's default getter, authenticated before target representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DependencyFieldGetter {
    class: crate::ir::ClassId,
    field: u32,
    receiver: DependencyMemberReceiver,
    signature: super::DependencyFunctionSignature,
}

impl DependencyFieldGetter {
    pub(crate) fn class(&self) -> crate::ir::ClassId {
        self.class
    }

    pub(crate) fn field(&self) -> u32 {
        self.field
    }

    pub(crate) fn receiver(&self) -> DependencyMemberReceiver {
        self.receiver
    }

    pub(crate) fn signature(&self) -> &super::DependencyFunctionSignature {
        &self.signature
    }
}

impl DependencyMemberReceiver {
    pub fn semantic_type(self) -> crate::types::Ty {
        self.0
    }
}

impl SingletonMemberReceiver {
    pub fn classifier(self) -> crate::types::TypeName {
        self.classifier
    }

    pub fn evaluate_source(self) -> bool {
        self.evaluate_source
    }
}

/// Exact module-qualified identities of this file's callable realizations.
///
/// The key is a stable common-IR declaration identity, not a spelling. The value joins the source
/// module identity with the declaration identity its generated KLIB publishes and a dependent
/// provider later places on `Callee::External`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DependencyExportSignatures {
    callables: HashMap<FunId, KlibDependencyCallable>,
    field_getters: HashMap<crate::fir::PropertyId, (DependencyFieldGetter, KlibDependencyCallable)>,
    /// Every visible function an interface of this file declares, abstract ones included: the
    /// identity a dependent module's implementation of the member is recorded under.
    interface_members: HashMap<FunId, KlibDependencyCallable>,
    interface_properties: HashMap<crate::fir::PropertyId, KlibDependencyCallable>,
    member_receivers: HashMap<FunId, DependencyMemberReceiver>,
    singleton_member_receivers: HashMap<FunId, SingletonMemberReceiver>,
    singleton_extension_unread_receivers: HashMap<FunId, SingletonExtensionUnreadReceiver>,
}

impl DependencyExportSignatures {
    pub(crate) fn field_getters(
        &self,
    ) -> impl Iterator<Item = (&DependencyFieldGetter, &KlibDependencyCallable)> {
        self.field_getters
            .values()
            .map(|(getter, identity)| (getter, identity))
    }

    pub(crate) fn record_field_getter(
        &mut self,
        ir: &crate::ir::IrFile,
        property: crate::fir::PropertyId,
        identity: KlibDependencyCallable,
    ) -> Result<(), &'static str> {
        let Some(crate::ir::IrLocalPropertyLayout::Member {
            class,
            owner,
            backing_field: Some(field),
            getter: None,
            ty,
            property: ordinal,
            context_parameters,
            ..
        }) = ir.local_property_layouts.get(&property)
        else {
            return Err("a default getter export has no declared field-property role");
        };
        let declared = ir
            .classes
            .get(*class as usize)
            .ok_or("a default getter export has no declaring class")?;
        let member = declared
            .properties
            .get(*ordinal as usize)
            .ok_or("a default getter export has no declaring property")?;
        let storage = declared
            .fields
            .get(*field as usize)
            .ok_or("a default getter export has no declared backing field")?;
        if declared.fq_name_id() != *owner
            || member.backing_field != Some(*field)
            || member.ty != *ty
            || storage.ty != member.storage_ty.unwrap_or(member.ty)
            || !context_parameters.is_empty()
        {
            return Err("a default getter export disagrees with its declared property storage");
        }
        let receiver = DependencyMemberReceiver(
            ir.declared_class_self_type(*owner)
                .ok_or("a default getter export has no declared class application")?,
        );
        let getter = DependencyFieldGetter {
            class: *class,
            field: *field,
            receiver,
            signature: super::DependencyFunctionSignature::field_getter(receiver, *ty),
        };
        if self
            .field_getters
            .insert(property, (getter, identity))
            .is_some()
        {
            return Err("one default getter cannot publish two declaration identities");
        }
        Ok(())
    }

    /// The public dependency identity exported for `callable`, when the declaration is visible to
    /// another module and has a callable realization.
    pub fn callable(&self, callable: FunId) -> Option<&KlibDependencyCallable> {
        self.callables.get(&callable)
    }

    /// Iterate every callable identity this file publishes so a target can install its complete
    /// signature-to-code registry before lowering dependent call sites.
    pub fn callables(&self) -> impl ExactSizeIterator<Item = (FunId, &KlibDependencyCallable)> {
        self.callables
            .iter()
            .map(|(&callable, signature)| (callable, signature))
    }

    /// The published identity of a function an interface of this file declares, with or without
    /// a body: what another module's class implementing it names it by.
    pub fn interface_member(&self, function: FunId) -> Option<&KlibDependencyCallable> {
        self.interface_members.get(&function)
    }

    /// The published getter identity of a property an interface of this file declares, with or
    /// without accessor bodies: what another module's class implementing the property names its
    /// getter and setter by.
    pub fn interface_property(
        &self,
        property: crate::fir::PropertyId,
    ) -> Option<&KlibDependencyCallable> {
        self.interface_properties.get(&property)
    }

    /// Exact unread extension receiver and its separately authenticated source-evaluation effect.
    pub fn singleton_extension_unread_receiver(
        &self,
        callable: FunId,
    ) -> Option<SingletonExtensionUnreadReceiver> {
        self.singleton_extension_unread_receivers
            .get(&callable)
            .copied()
    }

    /// Exact erased singleton-member receiver and its source-evaluation effect.
    pub fn singleton_member_receiver(&self, callable: FunId) -> Option<SingletonMemberReceiver> {
        self.singleton_member_receivers.get(&callable).copied()
    }

    /// Exact semantic dispatch receiver retained by an emitted ordinary member body.
    pub fn member_receiver(&self, callable: FunId) -> Option<DependencyMemberReceiver> {
        self.member_receivers.get(&callable).copied()
    }

    /// Record one declaration at the metadata owner that computed its exact identity.
    pub(crate) fn record_callable(&mut self, callable: FunId, identity: KlibDependencyCallable) {
        let previous = self.callables.insert(callable, identity);
        assert!(
            previous.is_none(),
            "one common-IR callable cannot export two KLIB declaration identities"
        );
    }

    pub(crate) fn record_interface_member(
        &mut self,
        function: FunId,
        identity: KlibDependencyCallable,
    ) {
        let previous = self.interface_members.insert(function, identity);
        assert!(
            previous.is_none(),
            "one interface member cannot publish two KLIB declaration identities"
        );
    }

    pub(crate) fn record_interface_property(
        &mut self,
        property: crate::fir::PropertyId,
        getter: KlibDependencyCallable,
    ) {
        let previous = self.interface_properties.insert(property, getter);
        assert!(
            previous.is_none(),
            "one interface property cannot publish two KLIB declaration identities"
        );
    }

    pub(crate) fn record_member_receiver(&mut self, callable: FunId, receiver: crate::types::Ty) {
        let previous = self
            .member_receivers
            .insert(callable, DependencyMemberReceiver(receiver));
        assert!(
            previous.is_none(),
            "one common-IR callable cannot publish two member receiver types"
        );
    }

    pub(crate) fn record_singleton_extension_unread_receiver(
        &mut self,
        callable: FunId,
        receiver: crate::types::Ty,
        evaluate_source: bool,
    ) {
        let previous = self.singleton_extension_unread_receivers.insert(
            callable,
            SingletonExtensionUnreadReceiver {
                semantic_type: receiver,
                evaluate_source,
            },
        );
        assert!(
            previous.is_none(),
            "one common-IR callable cannot publish two singleton-extension receiver effects"
        );
    }

    pub(crate) fn record_singleton_member_receiver(
        &mut self,
        callable: FunId,
        classifier: crate::types::TypeName,
        evaluate_source: bool,
    ) {
        let previous = self.singleton_member_receivers.insert(
            callable,
            SingletonMemberReceiver {
                classifier,
                evaluate_source,
            },
        );
        assert!(
            previous.is_none(),
            "one common-IR callable cannot publish two singleton-member receiver effects"
        );
    }
}

/// A read-only producer of one source module's dependency metadata.
///
/// A fresh [`Self::State`] belongs to exactly one source module. The target backend state has a
/// longer lifetime and may consume several such modules before it is finalized.
pub trait DependencyMetadata {
    type State: Default;

    /// The source semantics this metadata describes. It must match both the analysis and target
    /// backend that consume it.
    fn compilation_target(&self) -> CompilationTarget;

    /// Observe one checked file before target lowering and return the exact identities its target
    /// code must export. No provider or resolver is reachable through this boundary.
    fn record_ir_file(
        &self,
        file: &CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> DependencyExportSignatures;

    /// Validate the complete source module before its metadata is finalized.
    fn check_module(&self, _state: &Self::State, _diags: &mut DiagSink) {}

    /// Emit this source module's distinct dependency artifact set.
    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact>;
}

#[cfg(test)]
mod tests {
    use super::{
        authenticated_singleton_receiver, has_standalone_dependency_code,
        inert_staged_singleton_bindings,
    };
    use crate::ir::{Callee, IrExpr, IrFile, IrFunction};
    use crate::types::{type_name, Ty};

    fn function(name: &str, body: Option<u32>) -> IrFunction {
        IrFunction {
            name: name.to_owned(),
            params: Vec::new(),
            ret: Ty::Unit,
            body,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        }
    }

    #[test]
    fn only_a_function_with_standalone_code_can_be_a_dependency_export() {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let bodyless = ir.add_fun(function("bodyless", None));
        let emitted = ir.add_fun(function("emitted", Some(body)));
        let inline_template = ir.add_fun(function("inline_template", Some(body)));
        ir.inline_only_fns.insert(inline_template);

        assert!(!has_standalone_dependency_code(&ir, bodyless));
        assert!(has_standalone_dependency_code(&ir, emitted));
        assert!(!has_standalone_dependency_code(&ir, inline_template));
        assert!(!has_standalone_dependency_code(&ir, 99));
    }

    #[test]
    fn an_authenticated_singleton_receiver_uses_the_edge_fact_not_receiver_syntax() {
        let classifier = type_name("demo/Companion");
        let forwarded_local = Some(7);

        assert!(authenticated_singleton_receiver(
            forwarded_local,
            Some(classifier),
            Ty::obj_name(classifier),
        ));
        assert!(!authenticated_singleton_receiver(
            forwarded_local,
            None,
            Ty::obj_name(classifier),
        ));
        assert!(!authenticated_singleton_receiver(
            forwarded_local,
            Some(classifier),
            Ty::obj("demo/Other"),
        ));
        assert!(!authenticated_singleton_receiver(
            None,
            Some(classifier),
            Ty::obj_name(classifier),
        ));
    }

    #[test]
    fn an_inert_staged_singleton_requires_every_local_read_to_be_an_authenticated_receiver() {
        let classifier = type_name("demo/Companion");
        let target = crate::fir::ExternalCallableId::from_raw(7);
        let mut ir = IrFile::default();
        let singleton = ir.add_expr(IrExpr::SingletonValue { classifier });
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::obj_name(classifier),
            init: Some(singleton),
            named: false,
        });
        ir.inline_callable_reference_receiver_bindings
            .insert(declaration);
        let read = ir.add_expr(IrExpr::GetValue(0));
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::External {
                target,
                default_provider: None,
                params: Vec::new(),
                ret: Ty::Unit,
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            }),
            dispatch_receiver: Some(read),
            args: Vec::new(),
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, call],
            value: None,
        });
        let function = ir.add_fun(function("owner", Some(body)));
        assert_eq!(
            inert_staged_singleton_bindings(
                &ir,
                |expression| (expression == call).then_some(classifier),
                |candidate, selected| candidate == target && selected == classifier,
            ),
            crate::fx_hash::HashSet::from_iter([declaration])
        );
        assert!(inert_staged_singleton_bindings(
            &ir,
            |expression| (expression == call).then_some(classifier),
            |_, _| false,
        )
        .is_empty());

        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, read, call],
            value: None,
        });
        ir.functions[function as usize].body = Some(body);
        assert!(inert_staged_singleton_bindings(
            &ir,
            |expression| (expression == call).then_some(classifier),
            |candidate, selected| candidate == target && selected == classifier,
        )
        .is_empty());
    }
}
