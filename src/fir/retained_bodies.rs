//! Checked bodies retained across Pass 2 sources: inline templates and signature defaults.

use super::signature::ResolvedModuleIndex;
use super::{
    BodyOwnerId, CallableId, DeclarationId, FirBody, ResolvedCallableHeader, SourceFileId,
};

/// Persistent checked bodies required by call-site inlining. The only insertion path checks the
/// resolved declaration header and rejects every ordinary body.
#[derive(Debug, Default)]
pub struct InlineBodyStore {
    bodies: std::collections::BTreeMap<CallableId, FirBody>,
    /// The checked defaults of the retained inline callables. A call from another source expands
    /// them inside the inlined body, after their owning source drained its own default store.
    defaults: std::collections::BTreeMap<CallableId, FirBody>,
    /// The retained inline callables each callable's checked defaults call, by the declaration
    /// that owns those defaults. They must be templates before the defaults are lowered.
    default_dependencies: std::collections::BTreeMap<CallableId, (BodyOwnerId, Vec<CallableId>)>,
}

/// Checked signature expressions retained from Pass 1 until their owning source is lowered. Unlike
/// an ordinary body, a default is callable signature payload: callers and the generated default ABI
/// need it even when the declaration body is reparsed later.
#[derive(Debug, Default)]
pub struct DefaultArgumentStore {
    bodies: std::collections::BTreeMap<CallableId, FirBody>,
}

impl InlineBodyStore {
    pub fn insert(&mut self, callable: ResolvedCallableHeader, body: FirBody) {
        assert!(
            callable.is_inline(),
            "only semantically inline declarations may enter InlineBodyStore"
        );
        assert_eq!(
            body.owner(),
            BodyOwnerId::from_raw(callable.declaration.raw()),
            "inline FIR must belong to the inserted declaration"
        );
        crate::trace_compiler!(
            "fir",
            "retain signature defaults callable={:?} declaration={:?} count={}",
            callable.id,
            callable.declaration,
            body.default_values().len(),
        );
        assert!(
            self.bodies.insert(callable.id, body).is_none(),
            "an inline callable body may be inserted only once"
        );
    }

    pub fn get(&self, callable: CallableId) -> Option<&FirBody> {
        self.bodies.get(&callable)
    }

    /// Keep a copy of each retained inline callable's checked defaults, and record the retained
    /// inline callables every default calls.
    pub(crate) fn retain_defaults(&mut self, defaults: &DefaultArgumentStore) {
        for (callable, body) in &defaults.bodies {
            if self.bodies.contains_key(callable) {
                self.defaults.insert(*callable, body.clone());
            }
            let mut referenced = std::collections::HashSet::new();
            body.collect_referenced_module_callables(&mut referenced);
            let mut dependencies = referenced
                .into_iter()
                .filter(|dependency| self.bodies.contains_key(dependency))
                .collect::<Vec<_>>();
            if !dependencies.is_empty() {
                dependencies.sort_unstable_by_key(|dependency| dependency.raw());
                self.default_dependencies
                    .insert(*callable, (body.owner(), dependencies));
            }
        }
    }

    /// The retained inline callables that the checked defaults declared in `source` call.
    pub(crate) fn default_dependencies_for_source<'a>(
        &'a self,
        index: &'a ResolvedModuleIndex,
        source: SourceFileId,
    ) -> impl Iterator<Item = CallableId> + 'a {
        self.default_dependencies
            .values()
            .filter(move |(owner, _)| {
                index
                    .declaration_anchor(DeclarationId::from_raw(owner.raw()))
                    .is_some_and(|anchor| anchor.source == source)
            })
            .flat_map(|(_, dependencies)| dependencies.iter().copied())
    }

    pub(crate) fn defaults(&self, callable: CallableId) -> Option<&FirBody> {
        self.defaults.get(&callable)
    }

    pub(crate) fn attach_nested_declaration_body(&mut self, callable: CallableId, body: FirBody) {
        self.bodies
            .get_mut(&callable)
            .expect("an inline root must be retained before its nested declarations")
            .attach_inline_nested_declaration_body(body);
    }

    pub(crate) fn retained_bodies_for_source<'a>(
        &'a self,
        index: &'a ResolvedModuleIndex,
        source: SourceFileId,
    ) -> impl Iterator<Item = &'a FirBody> + 'a {
        self.bodies.values().filter(move |body| {
            index
                .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                .is_some_and(|anchor| anchor.source == source)
        })
    }

    /// Clone the retained bodies for one source into a consuming lowering unit. The originals remain
    /// available for call-site inlining throughout Pass 2; only inline FIR is allowed to pay this
    /// retention/copy cost.
    pub fn bodies_for_source(
        &self,
        index: &ResolvedModuleIndex,
        source: SourceFileId,
    ) -> Vec<(CallableId, FirBody)> {
        self.bodies
            .iter()
            .filter(|(_, body)| {
                index
                    .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                    .is_some_and(|anchor| anchor.source == source)
            })
            .map(|(callable, body)| (*callable, body.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.bodies
            .values()
            .chain(self.defaults.values())
            .map(|body| std::mem::size_of::<CallableId>() + body.storage_payload_bytes())
            .sum()
    }
}

impl DefaultArgumentStore {
    pub fn insert(&mut self, callable: ResolvedCallableHeader, body: FirBody) {
        assert!(
            body.is_default_fragment(),
            "only checked defaults enter the store"
        );
        assert!(
            !body.default_values().is_empty(),
            "a default fragment is nonempty"
        );
        assert_eq!(
            body.owner(),
            BodyOwnerId::from_raw(callable.declaration.raw()),
            "checked defaults must belong to their surviving callable"
        );
        assert!(
            self.bodies.insert(callable.id, body).is_none(),
            "a callable's checked defaults may be inserted only once"
        );
    }

    pub fn take_for_source(
        &mut self,
        index: &ResolvedModuleIndex,
        source: SourceFileId,
    ) -> Vec<(CallableId, FirBody)> {
        let selected = self
            .bodies
            .iter()
            .filter_map(|(callable, body)| {
                index
                    .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                    .is_some_and(|anchor| anchor.source == source)
                    .then_some(*callable)
            })
            .collect::<Vec<_>>();
        selected
            .into_iter()
            .filter_map(|callable| self.bodies.remove(&callable).map(|body| (callable, body)))
            .collect()
    }

    pub(crate) fn retained_bodies_for_source<'a>(
        &'a self,
        index: &'a ResolvedModuleIndex,
        source: SourceFileId,
    ) -> impl Iterator<Item = &'a FirBody> + 'a {
        self.bodies.values().filter(move |body| {
            index
                .declaration_anchor(DeclarationId::from_raw(body.owner().raw()))
                .is_some_and(|anchor| anchor.source == source)
        })
    }

    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.bodies
            .values()
            .map(|body| std::mem::size_of::<CallableId>() + body.storage_payload_bytes())
            .sum()
    }
}
