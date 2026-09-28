//! Decoded inline-body plan caching for immutable classpath compositions.

use super::{Classpath, EntryKey};
use crate::name_tree::{FxBuildHasher, FxHasher};
use crate::types::{Ty, TypeName};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{Hash, Hasher};

/// The full input set a plan decode reads. The same physical method can surface through distinct
/// provider views whose semantic receiver, result, generic signature, physical parameter shape,
/// suspend shape, and default realization differ, so every decoder input participates in the key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PlanKey {
    owner: TypeName,
    name: String,
    body_descriptor: String,
    parameter_slots: Vec<u16>,
    physical_parameters: Vec<Ty>,
    context_count: usize,
    source_receiver: Option<Ty>,
    semantic_parameters: Vec<Ty>,
    semantic_result: Ty,
    suspend: bool,
    generic_signature: Option<(Option<Ty>, Ty)>,
    default_realization: Option<DefaultPlanKey>,
}

/// The complete part of a default realization read while decoding an inline body. Keep this typed
/// boundary beside `PlanKey`: passing only the bridge coordinates would silently alias views whose
/// mask layout, real parameter widths, or continuation slot differ.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DefaultPlanKey {
    declaration_owner: TypeName,
    name: String,
    descriptor: String,
    real_params: Vec<Ty>,
    mask_count: usize,
    suspend: bool,
}

/// Borrowed provider view used to construct a cache identity. Keeping the decoder inputs together
/// makes additions explicit at the cache boundary instead of extending parallel positional APIs.
#[derive(Clone, Copy)]
pub(in crate::jvm) struct InlinePlanCacheInput<'a> {
    pub(in crate::jvm) owner: TypeName,
    pub(in crate::jvm) name: &'a str,
    pub(in crate::jvm) body_descriptor: &'a str,
    pub(in crate::jvm) parameter_slots: &'a [u16],
    pub(in crate::jvm) physical_parameters: &'a [Ty],
    pub(in crate::jvm) context_count: usize,
    pub(in crate::jvm) source_receiver: Option<Ty>,
    pub(in crate::jvm) semantic_parameters: &'a [Ty],
    pub(in crate::jvm) semantic_result: Ty,
    pub(in crate::jvm) suspend: bool,
    pub(in crate::jvm) generic_signature: Option<(Option<Ty>, Ty)>,
    pub(in crate::jvm) default_realization: Option<&'a crate::libraries::DefaultCallRealization>,
}
pub(super) type PlanCache = std::sync::Arc<std::sync::RwLock<PlanMemo>>;

/// Process-global plans keyed by the complete archive/jimage composition. A decoded facade plan
/// can read a body from another entry, so an entry-local cache would be unsound under shadowing.
pub(super) fn global_plan_cache(key: &[EntryKey]) -> PlanCache {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<EntryKey>, PlanCache>>> =
        std::sync::OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    cache
        .entry(key.to_vec())
        .or_insert_with(|| std::sync::Arc::new(std::sync::RwLock::new(PlanMemo::unbounded())))
        .clone()
}

impl Classpath {
    /// The memoized plan for the exact provider view, or `None` on a cold miss. `Some(None)` is a
    /// remembered stable rejection, which avoids re-decoding the dominant no-plan case.
    pub(in crate::jvm) fn cached_inline_plan(
        &self,
        input: InlinePlanCacheInput<'_>,
    ) -> Option<Option<Box<crate::libraries::InlineBodyPlan>>> {
        if !self.plan_is_cacheable() {
            cache_stat!(inline_plans, false);
            return None;
        }
        if let Some(hit) = self.inline_plans.borrow_mut().touch(&input) {
            cache_stat!(inline_plans, true);
            return Some(hit);
        }
        if let Some(global) = self.shared_inline_plans.as_ref() {
            if let Some(hit) = global.read().unwrap().get(&input) {
                self.inline_plans.borrow_mut().insert(&input, hit.clone());
                cache_stat!(inline_plans, true);
                return Some(hit);
            }
        }
        cache_stat!(inline_plans, false);
        None
    }

    /// Store one decoded inline-body plan (or its stable absence) for the exact provider view.
    pub(in crate::jvm) fn memoize_inline_plan(
        &self,
        input: InlinePlanCacheInput<'_>,
        plan: Option<Box<crate::libraries::InlineBodyPlan>>,
    ) {
        if !self.plan_is_cacheable() {
            return;
        }
        if let Some(global) = self.shared_inline_plans.as_ref() {
            global.write().unwrap().insert(&input, plan.clone());
        }
        self.inline_plans.borrow_mut().insert(&input, plan);
    }

    fn plan_is_cacheable(&self) -> bool {
        self.catalog_complete() && self.stub_overlay.borrow().is_empty()
    }
}

fn plan_key(input: &InlinePlanCacheInput<'_>) -> PlanKey {
    PlanKey {
        owner: input.owner,
        name: input.name.to_owned(),
        body_descriptor: input.body_descriptor.to_owned(),
        parameter_slots: input.parameter_slots.to_vec(),
        physical_parameters: input.physical_parameters.to_vec(),
        context_count: input.context_count,
        source_receiver: input.source_receiver,
        semantic_parameters: input.semantic_parameters.to_vec(),
        semantic_result: input.semantic_result,
        suspend: input.suspend,
        generic_signature: input.generic_signature,
        default_realization: input.default_realization.map(|realization| DefaultPlanKey {
            declaration_owner: realization.declaration_owner,
            name: realization.name.clone(),
            descriptor: realization.descriptor.clone(),
            real_params: realization.real_params.clone(),
            mask_count: realization.mask_count,
            suspend: realization.suspend,
        }),
    }
}

impl PlanKey {
    fn matches(&self, input: &InlinePlanCacheInput<'_>) -> bool {
        self.owner == input.owner
            && self.name == input.name
            && self.body_descriptor == input.body_descriptor
            && self.parameter_slots == input.parameter_slots
            && self.physical_parameters == input.physical_parameters
            && self.context_count == input.context_count
            && self.source_receiver == input.source_receiver
            && self.semantic_parameters == input.semantic_parameters
            && self.semantic_result == input.semantic_result
            && self.suspend == input.suspend
            && self.generic_signature == input.generic_signature
            && default_matches(self.default_realization.as_ref(), input.default_realization)
    }
}

fn default_matches(
    stored: Option<&DefaultPlanKey>,
    probed: Option<&crate::libraries::DefaultCallRealization>,
) -> bool {
    match (stored, probed) {
        (None, None) => true,
        (Some(stored), Some(probed)) => {
            stored.declaration_owner == probed.declaration_owner
                && stored.name == probed.name
                && stored.descriptor == probed.descriptor
                && stored.real_params == probed.real_params
                && stored.mask_count == probed.mask_count
                && stored.suspend == probed.suspend
        }
        _ => false,
    }
}

fn fingerprint(input: &InlinePlanCacheInput<'_>) -> u64 {
    let mut hasher = FxHasher::default();
    input.owner.hash(&mut hasher);
    input.name.hash(&mut hasher);
    input.body_descriptor.hash(&mut hasher);
    input.parameter_slots.hash(&mut hasher);
    input.physical_parameters.hash(&mut hasher);
    input.context_count.hash(&mut hasher);
    input.source_receiver.hash(&mut hasher);
    input.semantic_parameters.hash(&mut hasher);
    input.semantic_result.hash(&mut hasher);
    input.suspend.hash(&mut hasher);
    input.generic_signature.hash(&mut hasher);
    match input.default_realization {
        None => 0u8.hash(&mut hasher),
        Some(realization) => {
            1u8.hash(&mut hasher);
            realization.declaration_owner.hash(&mut hasher);
            realization.name.hash(&mut hasher);
            realization.descriptor.hash(&mut hasher);
            realization.real_params.hash(&mut hasher);
            realization.mask_count.hash(&mut hasher);
            realization.suspend.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Inline-plan memo whose lookup hashes and compares the borrowed provider view.
///
/// The owned key is built only when a new view is stored. A hit copies the memoized plan and
/// leaves the name, descriptor, and parameter vectors on the stack.
pub(super) struct PlanMemo {
    cap: Option<usize>,
    tick: u64,
    buckets: HashMap<u64, Vec<PlanSlot>, FxBuildHasher>,
    recency: BinaryHeap<Reverse<PlanRecency>>,
    len: usize,
}

struct PlanSlot {
    key: PlanKey,
    value: Option<Box<crate::libraries::InlineBodyPlan>>,
    tick: u64,
}

struct PlanRecency {
    tick: u64,
    fingerprint: u64,
    key: PlanKey,
}

impl PartialEq for PlanRecency {
    fn eq(&self, other: &Self) -> bool {
        self.tick == other.tick
    }
}

impl Eq for PlanRecency {}

impl PartialOrd for PlanRecency {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PlanRecency {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.tick.cmp(&other.tick)
    }
}

impl PlanMemo {
    /// A bounded memo using the repository-wide cache-cap override when configured.
    pub(super) fn new(default_cap: usize) -> Self {
        let cap = std::env::var("KRUSTY_CACHE_CAP")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(default_cap);
        Self::new_fixed(cap)
    }

    fn new_fixed(cap: usize) -> Self {
        Self {
            cap: Some(cap.max(1)),
            tick: 0,
            buckets: HashMap::default(),
            recency: BinaryHeap::new(),
            len: 0,
        }
    }

    pub(super) fn unbounded() -> Self {
        Self {
            cap: None,
            tick: 0,
            buckets: HashMap::default(),
            recency: BinaryHeap::new(),
            len: 0,
        }
    }

    fn get(
        &self,
        input: &InlinePlanCacheInput<'_>,
    ) -> Option<Option<Box<crate::libraries::InlineBodyPlan>>> {
        let slots = self.buckets.get(&fingerprint(input))?;
        let slot = slots.iter().find(|slot| slot.key.matches(input))?;
        Some(slot.value.clone())
    }

    fn touch(
        &mut self,
        input: &InlinePlanCacheInput<'_>,
    ) -> Option<Option<Box<crate::libraries::InlineBodyPlan>>> {
        let fingerprint = fingerprint(input);
        let slots = self.buckets.get_mut(&fingerprint)?;
        let slot = slots.iter_mut().find(|slot| slot.key.matches(input))?;
        self.tick += 1;
        slot.tick = self.tick;
        Some(slot.value.clone())
    }

    fn insert(
        &mut self,
        input: &InlinePlanCacheInput<'_>,
        plan: Option<Box<crate::libraries::InlineBodyPlan>>,
    ) {
        let fingerprint = fingerprint(input);
        if let Some(slots) = self.buckets.get_mut(&fingerprint) {
            if let Some(slot) = slots.iter_mut().find(|slot| slot.key.matches(input)) {
                self.tick += 1;
                slot.tick = self.tick;
                slot.value = plan;
                return;
            }
        }
        if self.cap.is_some_and(|cap| self.len >= cap) {
            self.evict();
        }
        let key = plan_key(input);
        self.tick += 1;
        let tick = self.tick;
        if self.cap.is_some() {
            self.recency.push(Reverse(PlanRecency {
                tick,
                fingerprint,
                key: key.clone(),
            }));
        }
        self.buckets.entry(fingerprint).or_default().push(PlanSlot {
            key,
            value: plan,
            tick,
        });
        self.len += 1;
    }

    fn evict(&mut self) {
        while let Some(Reverse(candidate)) = self.recency.pop() {
            let Some(slots) = self.buckets.get_mut(&candidate.fingerprint) else {
                continue;
            };
            let Some(position) = slots.iter().position(|slot| slot.key == candidate.key) else {
                continue;
            };
            if slots[position].tick != candidate.tick {
                let tick = slots[position].tick;
                self.recency.push(Reverse(PlanRecency {
                    tick,
                    fingerprint: candidate.fingerprint,
                    key: candidate.key,
                }));
                continue;
            }
            slots.swap_remove(position);
            if slots.is_empty() {
                self.buckets.remove(&candidate.fingerprint);
            }
            self.len -= 1;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    type GenericShape = Option<(Option<Ty>, Ty)>;

    fn cached(
        cp: &Classpath,
        owner: TypeName,
        context_count: usize,
        result: Ty,
        suspend: bool,
        generic: GenericShape,
        default: Option<&crate::libraries::DefaultCallRealization>,
    ) -> Option<Option<Box<crate::libraries::InlineBodyPlan>>> {
        cp.cached_inline_plan(InlinePlanCacheInput {
            owner,
            name: "run",
            body_descriptor: "()V",
            parameter_slots: &[0],
            physical_parameters: &[Ty::String],
            context_count,
            source_receiver: None,
            semantic_parameters: &[],
            semantic_result: result,
            suspend,
            generic_signature: generic,
            default_realization: default,
        })
    }

    #[test]
    fn plan_memo_is_scoped_to_the_complete_semantic_view_and_overlay() {
        let owner = type_name("p/Widget");
        let plan = crate::libraries::InlineBodyPlan::InvokeLambda {
            lambda_parameter: 0,
            arguments: Vec::new(),
            prologue: Vec::new(),
            cleanup: Vec::new(),
            cause: None,
            recovery: None,
            defaults: Vec::new(),
            result: None,
        };
        let cp = Classpath::new(vec![]);
        cp.memoize_inline_plan(
            InlinePlanCacheInput {
                owner,
                name: "run",
                body_descriptor: "()V",
                parameter_slots: &[0],
                physical_parameters: &[Ty::String],
                context_count: 0,
                source_receiver: None,
                semantic_parameters: &[],
                semantic_result: Ty::Unit,
                suspend: false,
                generic_signature: Some((None, Ty::Unit)),
                default_realization: None,
            },
            Some(Box::new(plan)),
        );
        assert!(cached(&cp, owner, 0, Ty::Unit, false, Some((None, Ty::Unit)), None,).is_some());
        let owned_name = "run".to_string();
        let owned_descriptor = "()V".to_string();
        assert!(cp
            .cached_inline_plan(InlinePlanCacheInput {
                owner,
                name: &owned_name,
                body_descriptor: &owned_descriptor,
                parameter_slots: &[0],
                physical_parameters: &[Ty::String],
                context_count: 0,
                source_receiver: None,
                semantic_parameters: &[],
                semantic_result: Ty::Unit,
                suspend: false,
                generic_signature: Some((None, Ty::Unit)),
                default_realization: None,
            })
            .is_some());
        for (context, result, suspend, generic, default) in [
            (1, Ty::Unit, false, Some((None, Ty::Unit)), None),
            (0, Ty::String, false, Some((None, Ty::Unit)), None),
            (0, Ty::Unit, true, Some((None, Ty::Unit)), None),
            (
                0,
                Ty::Unit,
                false,
                Some((Some(Ty::String), Ty::String)),
                None,
            ),
        ] {
            assert!(
                cached(&cp, owner, context, result, suspend, generic, default).is_none(),
                "a distinct semantic callable view must not reuse another view's plan"
            );
        }

        let stubs = crate::jvm::java_stub::stub_classes(
            &[("W.java".into(), "package p; public class Widget {}".into())],
            crate::jvm::java_stub::StubMode::Lenient,
            &|candidate| candidate == "java/lang/Object",
        )
        .expect("stub");
        cp.set_stub_overlay(stubs);
        assert!(cached(&cp, owner, 0, Ty::Unit, false, Some((None, Ty::Unit)), None,).is_none());
        cp.memoize_inline_plan(
            InlinePlanCacheInput {
                owner,
                name: "unrelated",
                body_descriptor: "()V",
                parameter_slots: &[0],
                physical_parameters: &[Ty::String],
                context_count: 0,
                source_receiver: None,
                semantic_parameters: &[],
                semantic_result: Ty::Unit,
                suspend: false,
                generic_signature: None,
                default_realization: None,
            },
            None,
        );
        cp.clear_stub_overlay();
        assert!(cp
            .cached_inline_plan(InlinePlanCacheInput {
                owner,
                name: "unrelated",
                body_descriptor: "()V",
                parameter_slots: &[0],
                physical_parameters: &[Ty::String],
                context_count: 0,
                source_receiver: None,
                semantic_parameters: &[],
                semantic_result: Ty::Unit,
                suspend: false,
                generic_signature: None,
                default_realization: None,
            })
            .is_none());
        assert!(cached(&cp, owner, 0, Ty::Unit, false, Some((None, Ty::Unit)), None,).is_some());
    }

    #[test]
    fn plan_memo_distinguishes_physical_parameters_and_complete_default_realization() {
        let owner = type_name("p/CompleteInlinePlanKey");
        let plan = crate::libraries::InlineBodyPlan::InvokeLambda {
            lambda_parameter: 0,
            arguments: Vec::new(),
            prologue: Vec::new(),
            cleanup: Vec::new(),
            cause: None,
            recovery: None,
            defaults: Vec::new(),
            result: None,
        };
        let default = crate::libraries::DefaultCallRealization {
            owner,
            name: "run$default".to_owned(),
            descriptor: "(Ljava/lang/String;ILjava/lang/Object;)V".to_owned(),
            declaration_owner: owner,
            real_params: vec![Ty::String],
            mask_count: 1,
            ret: Ty::Unit,
            suspend: false,
        };
        let cp = Classpath::new(vec![]);
        cp.memoize_inline_plan(
            InlinePlanCacheInput {
                owner,
                name: "run",
                body_descriptor: "()V",
                parameter_slots: &[0],
                physical_parameters: &[Ty::String],
                context_count: 0,
                source_receiver: None,
                semantic_parameters: &[],
                semantic_result: Ty::Unit,
                suspend: false,
                generic_signature: None,
                default_realization: Some(&default),
            },
            Some(Box::new(plan)),
        );

        fn lookup(
            cp: &Classpath,
            owner: TypeName,
            physical: &[Ty],
            realization: &crate::libraries::DefaultCallRealization,
        ) -> Option<Option<Box<crate::libraries::InlineBodyPlan>>> {
            cp.cached_inline_plan(InlinePlanCacheInput {
                owner,
                name: "run",
                body_descriptor: "()V",
                parameter_slots: &[0],
                physical_parameters: physical,
                context_count: 0,
                source_receiver: None,
                semantic_parameters: &[],
                semantic_result: Ty::Unit,
                suspend: false,
                generic_signature: None,
                default_realization: Some(realization),
            })
        }
        assert!(lookup(&cp, owner, &[Ty::String], &default).is_some());
        assert!(
            lookup(&cp, owner, &[Ty::obj("java/lang/String")], &default,).is_none(),
            "equal-width physical parameter views must not share a plan"
        );

        let mut distinct = default.clone();
        distinct.declaration_owner = type_name("p/OtherInlineBodies");
        assert!(lookup(&cp, owner, &[Ty::String], &distinct).is_none());
        distinct = default.clone();
        distinct.name = "other$default".to_owned();
        assert!(lookup(&cp, owner, &[Ty::String], &distinct).is_none());
        distinct = default.clone();
        distinct.descriptor = "(Ljava/lang/String;II)V".to_owned();
        assert!(lookup(&cp, owner, &[Ty::String], &distinct).is_none());
        distinct = default.clone();
        distinct.real_params = vec![Ty::Long];
        assert!(lookup(&cp, owner, &[Ty::String], &distinct).is_none());
        distinct = default.clone();
        distinct.mask_count = 2;
        assert!(lookup(&cp, owner, &[Ty::String], &distinct).is_none());
        distinct = default.clone();
        distinct.suspend = true;
        assert!(lookup(&cp, owner, &[Ty::String], &distinct).is_none());
    }

    #[test]
    fn capped_plan_memo_drops_the_least_recently_stored_view() {
        fn view<'a>(owner: TypeName, name: &'a str) -> InlinePlanCacheInput<'a> {
            InlinePlanCacheInput {
                owner,
                name,
                body_descriptor: "()V",
                parameter_slots: &[0],
                physical_parameters: &[Ty::String],
                context_count: 0,
                source_receiver: None,
                semantic_parameters: &[],
                semantic_result: Ty::Unit,
                suspend: false,
                generic_signature: None,
                default_realization: None,
            }
        }
        let owner = type_name("sample/PlanCap");
        let mut memo = PlanMemo::new_fixed(1);
        let first = view(owner, "first");
        let second = view(owner, "second");
        memo.insert(&first, None);
        memo.insert(&first, None);
        assert!(memo.touch(&first).is_some());
        memo.insert(&second, None);
        assert!(memo.touch(&first).is_none());
        assert!(memo.touch(&second).is_some());
    }
}
