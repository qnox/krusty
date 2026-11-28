//! The external declarations a checked unit selects, recorded as each selection is committed.
//!
//! Every checker table whose entries are committed selections is a [`SelectionTable`]. Inserting
//! an entry records the dependency callables and properties it selected into the unit's one
//! [`SelectedExternalMembers`] ledger, so the ledger is complete by construction: a selection
//! cannot enter a table without entering the ledger. Builtin implementation facts are then derived
//! from that ledger while the checker still owns the normalized source provider.

use std::cell::RefCell;
use std::rc::Rc;

use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::fx_hash::{HashMap, HashSet};
use crate::libraries::{
    InlineBodyCall, InlineBodyPlan, InlineCollectionAppend, InlineCollectionCapacity,
    InlineIterationIndex, InlineIterationTraversal,
};

use super::super_calls::ResolvedSuperCall;
use super::{
    CallableReferenceTarget, CompoundAssignmentTarget, DelegateGetValueTarget, ExprLowering,
    ForRangeIteratorTarget, ImplicitPropertyWriteTarget, InvokeKind, IteratorProtocolTarget,
    ResolvedCall, ResolvedPropertyAccess, ResolvedSamConversion, StmtLowering,
};

/// One dependency declaration a committed selection names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum SelectedExternal {
    Callable(ExternalCallableId),
    /// The declaration that provides the defaults of a selected callable. A call that omits an
    /// argument calls it in place of the selected declaration.
    DefaultProvider(ExternalCallableId),
    /// A property selected by property syntax; its accessors are the callables it dispatches to.
    Property(ExternalPropertyId),
}

/// The unit-wide ledger every [`SelectionTable`] of one checker records into.
#[derive(Clone, Debug, Default)]
pub(super) struct SelectedExternalMembers(Rc<RefCell<HashSet<SelectedExternal>>>);

impl SelectedExternalMembers {
    pub(super) fn record(&self, selection: &impl SelectsExternalMembers) {
        let mut selected = self.0.borrow_mut();
        selection.selected_external_members(&mut |external| {
            selected.insert(external);
        });
    }

    /// Every declaration recorded so far, in a stable order.
    pub(super) fn snapshot(&self) -> Vec<SelectedExternal> {
        let mut selected = self.0.borrow().iter().copied().collect::<Vec<_>>();
        selected.sort_unstable_by_key(|external| match *external {
            SelectedExternal::Callable(identity) => (0, identity.raw()),
            SelectedExternal::DefaultProvider(identity) => (1, identity.raw()),
            SelectedExternal::Property(identity) => (2, identity.raw()),
        });
        selected
    }
}

/// A checker table of committed selections. Reads see the entries; every insert records the
/// entry's external declarations into the shared ledger. In-place mutation through
/// [`Self::get_mut`] and [`Self::values_mut`] refines a recorded selection (its storage or its
/// applied receiver) and must not change which declaration it selects.
#[derive(Debug)]
pub(super) struct SelectionTable<K, V> {
    entries: HashMap<K, V>,
    ledger: SelectedExternalMembers,
}

impl<K, V> Default for SelectionTable<K, V> {
    fn default() -> Self {
        Self {
            entries: HashMap::default(),
            ledger: SelectedExternalMembers::default(),
        }
    }
}

impl<K: std::hash::Hash + Eq, V: SelectsExternalMembers> SelectionTable<K, V> {
    pub(super) fn new(ledger: &SelectedExternalMembers) -> Self {
        Self {
            entries: HashMap::default(),
            ledger: ledger.clone(),
        }
    }

    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.ledger.record(&value);
        self.entries.insert(key, value)
    }

    pub(super) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.entries.remove(key)
    }

    pub(super) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.entries.get_mut(key)
    }

    pub(super) fn values_mut(&mut self) -> std::collections::hash_map::ValuesMut<'_, K, V> {
        self.entries.values_mut()
    }

    pub(super) fn into_entries(self) -> HashMap<K, V> {
        self.entries
    }
}

impl<K, V> std::ops::Deref for SelectionTable<K, V> {
    type Target = HashMap<K, V>;

    fn deref(&self) -> &HashMap<K, V> {
        &self.entries
    }
}

/// A committed checker decision that names the dependency declarations it selected. Each
/// implementation matches its variants exhaustively, so a new decision shape must say what it
/// selects before it compiles.
pub(super) trait SelectsExternalMembers {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal));
}

fn callable(identity: Option<ExternalCallableId>, record: &mut dyn FnMut(SelectedExternal)) {
    if let Some(identity) = identity {
        record(SelectedExternal::Callable(identity));
    }
}

/// A package-level declaration (a top-level or extension function or property) dispatches to
/// nothing; only the declarations its inline body plan calls are selected with it.
fn package_level(
    callable: &crate::libraries::LibraryCallable,
    record: &mut dyn FnMut(SelectedExternal),
) {
    if matches!(
        callable.declaration_owner,
        Some(crate::types::SemanticCallableOwner::Classifier(_))
    ) {
        callable.selected_external_members(record);
    } else {
        callable.inline_body_plan.selected_external_members(record);
    }
}

fn default_provider(
    identity: Option<ExternalCallableId>,
    record: &mut dyn FnMut(SelectedExternal),
) {
    if let Some(identity) = identity {
        record(SelectedExternal::DefaultProvider(identity));
    }
}

fn property(identity: Option<ExternalPropertyId>, record: &mut dyn FnMut(SelectedExternal)) {
    if let Some(identity) = identity {
        record(SelectedExternal::Property(identity));
    }
}

impl<T: SelectsExternalMembers + ?Sized> SelectsExternalMembers for Box<T> {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        (**self).selected_external_members(record);
    }
}

impl<T: SelectsExternalMembers> SelectsExternalMembers for Option<T> {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        if let Some(selection) = self {
            selection.selected_external_members(record);
        }
    }
}

impl<T: SelectsExternalMembers> SelectsExternalMembers for [T] {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        for selection in self {
            selection.selected_external_members(record);
        }
    }
}

impl SelectsExternalMembers for crate::libraries::LibraryMember {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        callable(self.external_identity, record);
        default_provider(self.external_default_provider, record);
        property(self.external_property_identity, record);
        self.inline_body_plan.selected_external_members(record);
    }
}

impl SelectsExternalMembers for crate::libraries::LibraryCallable {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        callable(self.external_identity, record);
        default_provider(self.external_default_provider, record);
        // An accessor selects its property, and a property access carries both accessors.
        property(self.external_property_identity, record);
        self.inline_body_plan.selected_external_members(record);
    }
}

impl SelectsExternalMembers for crate::libraries::PropertyInfo {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self.kind {
            crate::libraries::PropKind::Member | crate::libraries::PropKind::MemberExtension => {
                self.getter.selected_external_members(record);
                self.setter.selected_external_members(record);
            }
            crate::libraries::PropKind::Extension | crate::libraries::PropKind::TopLevel => {
                package_level(&self.getter, record);
                if let Some(setter) = &self.setter {
                    package_level(setter, record);
                }
            }
        }
    }
}

impl SelectsExternalMembers for crate::libraries::ClassifierProperty {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.getter.selected_external_members(record);
    }
}

impl SelectsExternalMembers for crate::symbol_resolver::ResolvedMember {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.member.selected_external_members(record);
    }
}

impl SelectsExternalMembers for crate::symbol_resolver::ResolvedPropertySetter {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.callable.selected_external_members(record);
    }
}

impl SelectsExternalMembers for crate::symbol_resolver::ResolvedPropertyRef {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        if self.extension_facade.is_some() {
            package_level(&self.getter, record);
            if let Some(setter) = &self.setter {
                package_level(setter, record);
            }
        } else {
            self.getter.selected_external_members(record);
            self.setter.selected_external_members(record);
        }
    }
}

impl SelectsExternalMembers for crate::symbol_resolver::SamSignature {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self.declaration {
            Some(crate::symbol_resolver::SamMethodDeclaration::External(identity)) => {
                record(SelectedExternal::Callable(identity));
            }
            Some(
                crate::symbol_resolver::SamMethodDeclaration::Module(_)
                | crate::symbol_resolver::SamMethodDeclaration::FunctionTypeInvoke,
            )
            | None => {}
        }
    }
}

impl SelectsExternalMembers for ResolvedSamConversion {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.signature.selected_external_members(record);
    }
}

impl SelectsExternalMembers for ResolvedPropertyAccess {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.property.selected_external_members(record);
    }
}

impl SelectsExternalMembers for ResolvedCall {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::Member(member) => member.selected_external_members(record),
            Self::TopLevel(call) => package_level(&call.callable, record),
            Self::Companion(member) => member.selected_external_members(record),
            Self::Extension(call) => package_level(&call.callable, record),
            Self::MemberExtension {
                external_identity,
                external_default_provider,
                inline_body_plan,
                ..
            } => {
                callable(*external_identity, record);
                default_provider(*external_default_provider, record);
                inline_body_plan.selected_external_members(record);
            }
            Self::LocalFunction(_) => {}
        }
    }
}

impl SelectsExternalMembers for CompoundAssignmentTarget {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.call.selected_external_members(record);
    }
}

impl SelectsExternalMembers for InvokeKind {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::Function { .. } => {}
            Self::Operator { target, .. } => target.selected_external_members(record),
        }
    }
}

impl SelectsExternalMembers for ImplicitPropertyWriteTarget {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::Member { getter, setter, .. } => {
                getter.selected_external_members(record);
                setter.selected_external_members(record);
            }
            Self::Extension { access, .. } => access.selected_external_members(record),
        }
    }
}

impl SelectsExternalMembers for CallableReferenceTarget {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::Classifier(member) | Self::Member { member, .. } => {
                member.selected_external_members(record);
            }
            Self::ClassifierProperty { property, .. } => {
                property.selected_external_members(record);
            }
            Self::Extension { callable, .. } => package_level(callable, record),
            Self::Property(property) => property.selected_external_members(record),
            Self::TopLevelProperty(property) => property.selected_external_members(record),
        }
    }
}

impl SelectsExternalMembers for ExprLowering {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::ImplicitPropertyIncDec(target) => target.selected_external_members(record),
            Self::TopLevelPropertyIncDec(access)
            | Self::ExtensionPropertyGet { access }
            | Self::TopLevelPropertyGet(access) => access.selected_external_members(record),
            Self::SamConstructorReference { sam, .. } | Self::SamConstructor { sam, .. } => {
                sam.selected_external_members(record);
            }
            Self::ConstructorRef { constructor, .. } => {
                constructor.selected_external_members(record);
            }
            Self::TopLevelFunctionRef(reference) => package_level(&reference.target, record),
            Self::CallableReference { target, .. }
            | Self::AdaptedCallableReference { target, .. } => {
                target.selected_external_members(record);
            }
            Self::AdaptedRef { target, .. } => package_level(target, record),
            Self::AssociatedPropertyRead {
                external_identity, ..
            } => property(*external_identity, record),
            Self::ClassifierPropertyRead { property, .. } => {
                property.selected_external_members(record);
            }
            Self::IntrinsicProperty(member) => member.selected_external_members(record),
            Self::MemberPropertyRead {
                accessor,
                context_access,
                ..
            } => {
                accessor.selected_external_members(record);
                context_access.selected_external_members(record);
            }
            Self::MemberExtensionPropertyRead { getter, .. } => {
                getter.selected_external_members(record);
            }
            Self::Invoke { kind, .. } => kind.selected_external_members(record),
            Self::SafePropertyInvoke { property, kind, .. } => {
                property.selected_external_members(record);
                kind.selected_external_members(record);
            }
            Self::PluginExpression(plan) => plan.synthesized.selected_external_members(record),
            Self::ConstructorCaptures(_)
            | Self::BuiltinUnaryCall { .. }
            | Self::RuntimeTypeTest { .. }
            | Self::RuntimeCast { .. }
            | Self::ExtensionFunctionBinding { .. }
            | Self::ClassStorageRead(_)
            | Self::EnumEntryStorageRead { .. }
            | Self::EnumEntryPropertyRead { .. }
            | Self::BackingFieldRead
            | Self::LateinitInitialized { .. }
            | Self::LocalFunction { .. }
            | Self::AdaptedLocalFunctionRef { .. }
            | Self::FunctionInvokeReference { .. }
            | Self::UnavailableCallableReference { .. }
            | Self::Unavailable { .. }
            | Self::Erased
            | Self::IncDecAccessOperands(_)
            | Self::CompilerSynthetic(_)
            | Self::Lambda(_)
            | Self::SingletonValue(_)
            | Self::LabeledThisInner
            | Self::LabeledThisDispatch
            | Self::ClassLiteral { .. }
            | Self::ReceiverFnInvoke { .. } => {}
        }
    }
}

impl SelectsExternalMembers for StmtLowering {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::PlusAssign(target) => target.selected_external_members(record),
            Self::ImplicitPropertyWrite(target) => target.selected_external_members(record),
            Self::MemberPropertyWrite {
                setter,
                context_access,
                ..
            } => {
                setter.selected_external_members(record);
                context_access.selected_external_members(record);
            }
            Self::AssociatedPropertyWrite {
                external_identity, ..
            } => property(*external_identity, record),
            Self::SuperPropertyWrite { target } => target.selected_external_members(record),
            Self::MemberExtensionPropertyWrite { setter, .. } => {
                setter.selected_external_members(record);
            }
            Self::ExtensionPropertyWrite { access } | Self::TopLevelPropertySet(access) => {
                access.selected_external_members(record);
            }
            Self::LocalFunction(_)
            | Self::OverloadedAssignment { .. }
            | Self::BackingFieldWrite { .. }
            | Self::DeferredPropertyWrite { .. }
            | Self::Erased => {}
        }
    }
}

impl SelectsExternalMembers for ResolvedSuperCall {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        callable(self.external_declaration, record);
    }
}

impl SelectsExternalMembers for DelegateGetValueTarget {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::Member {
                external_identity,
                external_default_provider,
                ..
            } => {
                callable(*external_identity, record);
                default_provider(*external_default_provider, record);
            }
            Self::MemberExtension {
                external_identity,
                external_default_provider,
                inline_body_plan,
                ..
            } => {
                callable(*external_identity, record);
                default_provider(*external_default_provider, record);
                inline_body_plan.selected_external_members(record);
            }
            Self::Extension { callable, .. } => package_level(callable, record),
        }
    }
}

impl SelectsExternalMembers for IteratorProtocolTarget {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.iterator.selected_external_members(record);
        self.has_next.selected_external_members(record);
        self.next.selected_external_members(record);
    }
}

impl SelectsExternalMembers for ForRangeIteratorTarget {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.range.selected_external_members(record);
        self.protocol.selected_external_members(record);
    }
}

impl SelectsExternalMembers for crate::fir::ResolvedDelegateCall {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self.call.target {
            crate::fir::ResolvedDelegatedCallTarget::External(identity) => {
                record(SelectedExternal::Callable(identity));
            }
            crate::fir::ResolvedDelegatedCallTarget::Module { .. } => {}
        }
    }
}

impl SelectsExternalMembers for crate::fir::ResolvedDelegateMemberCalls {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::Function(call) => call.selected_external_members(record),
            Self::Property { getter, setter } => {
                getter.selected_external_members(record);
                setter.selected_external_members(record);
            }
        }
    }
}

impl SelectsExternalMembers for InlineBodyCall {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        self.callable.selected_external_members(record);
    }
}

impl SelectsExternalMembers for InlineIterationTraversal {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::Iterator {
                prepare,
                has_next,
                next,
            } => {
                prepare.selected_external_members(record);
                has_next.selected_external_members(record);
                next.selected_external_members(record);
            }
            Self::Counted { size, get } => {
                size.selected_external_members(record);
                get.selected_external_members(record);
            }
            Self::Array => {}
        }
    }
}

/// An inline dependency callable expanded at its call site selects every declaration its
/// provider-normalized body plan calls.
impl SelectsExternalMembers for InlineBodyPlan {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::ReifiedTypeTest { .. } => {}
            Self::InvokeLambda {
                prologue,
                cleanup,
                recovery,
                ..
            } => {
                prologue.selected_external_members(record);
                cleanup.selected_external_members(record);
                if let Some(recovery) = recovery {
                    recovery.constructor.selected_external_members(record);
                    recovery.failure.selected_external_members(record);
                }
            }
            Self::Iteration {
                index, traversal, ..
            } => {
                match index {
                    Some(InlineIterationIndex::Checked { overflow }) => {
                        overflow.selected_external_members(record);
                    }
                    Some(InlineIterationIndex::Unchecked) | None => {}
                }
                traversal.selected_external_members(record);
            }
            Self::CollectionTransform {
                traversal,
                factory,
                capacity,
                append,
                ..
            } => {
                traversal.selected_external_members(record);
                factory.selected_external_members(record);
                match capacity {
                    Some(InlineCollectionCapacity::Member(member)) => {
                        member.selected_external_members(record);
                    }
                    Some(InlineCollectionCapacity::Extension { callable, .. }) => {
                        callable.selected_external_members(record);
                    }
                    None => {}
                }
                match append {
                    InlineCollectionAppend::Member(member) => {
                        member.selected_external_members(record);
                    }
                    InlineCollectionAppend::Extension(callable) => {
                        callable.selected_external_members(record);
                    }
                }
            }
        }
    }
}

/// A compiler plugin's expression selects the dependency members its synthesized calls name.
impl SelectsExternalMembers for crate::plugins::PluginSynthesizedOperand {
    fn selected_external_members(&self, record: &mut dyn FnMut(SelectedExternal)) {
        match self {
            Self::SingletonCall { call, arguments } => {
                call.selected.member.selected_external_members(record);
                arguments.selected_external_members(record);
            }
            Self::Operation { operands, .. } => operands.selected_external_members(record),
        }
    }
}

#[cfg(test)]
#[path = "selected_external_members_tests.rs"]
mod tests;
