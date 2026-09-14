//! Decoded inline-body plan caching for immutable classpath compositions.

use super::{Classpath, EntryKey};
use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// The full input set a plan decode reads. The same physical method can surface through distinct
/// provider views whose semantic receiver, result, generic signature, parameter slots, suspend
/// shape, and default target differ, so every decoder input participates in the key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct PlanKey {
    owner: TypeName,
    name: String,
    body_descriptor: String,
    parameter_slots: Vec<u16>,
    context_count: usize,
    source_receiver: Option<Ty>,
    semantic_parameters: Vec<Ty>,
    semantic_result: Ty,
    suspend: bool,
    generic_signature: Option<(Option<Ty>, Ty)>,
    default_target: Option<(TypeName, String, String)>,
}
type PlanMap = HashMap<PlanKey, Option<Box<crate::libraries::InlineBodyPlan>>>;
pub(super) type PlanCache = std::sync::Arc<std::sync::RwLock<PlanMap>>;

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
        .or_insert_with(|| std::sync::Arc::new(std::sync::RwLock::new(HashMap::new())))
        .clone()
}

impl Classpath {
    /// The memoized plan for the exact provider view, or `None` on a cold miss. `Some(None)` is a
    /// remembered stable rejection, which avoids re-decoding the dominant no-plan case.
    pub(crate) fn cached_inline_plan(
        &self,
        owner: TypeName,
        name: &str,
        body_descriptor: &str,
        parameter_slots: &[u16],
        context_count: usize,
        source_receiver: Option<Ty>,
        semantic_parameters: &[Ty],
        semantic_result: Ty,
        suspend: bool,
        generic_signature: Option<(Option<Ty>, Ty)>,
        default_target: Option<(TypeName, &str, &str)>,
    ) -> Option<Option<Box<crate::libraries::InlineBodyPlan>>> {
        if !self.plan_is_cacheable() {
            cache_stat!(inline_plans, false);
            return None;
        }
        let key = plan_key(
            owner,
            name,
            body_descriptor,
            parameter_slots,
            context_count,
            source_receiver,
            semantic_parameters,
            semantic_result,
            suspend,
            generic_signature,
            default_target,
        );
        if let Some(hit) = self.inline_plans.borrow_mut().get(&key) {
            cache_stat!(inline_plans, true);
            return Some(hit.clone());
        }
        if let Some(global) = self.shared_inline_plans.as_ref() {
            if let Some(hit) = global.read().unwrap().get(&key).cloned() {
                self.inline_plans.borrow_mut().insert(key, hit.clone());
                cache_stat!(inline_plans, true);
                return Some(hit);
            }
        }
        cache_stat!(inline_plans, false);
        None
    }

    /// Store one decoded inline-body plan (or its stable absence) for the exact provider view.
    pub(crate) fn memoize_inline_plan(
        &self,
        owner: TypeName,
        name: &str,
        body_descriptor: &str,
        parameter_slots: &[u16],
        context_count: usize,
        source_receiver: Option<Ty>,
        semantic_parameters: &[Ty],
        semantic_result: Ty,
        suspend: bool,
        generic_signature: Option<(Option<Ty>, Ty)>,
        default_target: Option<(TypeName, &str, &str)>,
        plan: Option<Box<crate::libraries::InlineBodyPlan>>,
    ) {
        if !self.plan_is_cacheable() {
            return;
        }
        let key = plan_key(
            owner,
            name,
            body_descriptor,
            parameter_slots,
            context_count,
            source_receiver,
            semantic_parameters,
            semantic_result,
            suspend,
            generic_signature,
            default_target,
        );
        if let Some(global) = self.shared_inline_plans.as_ref() {
            global.write().unwrap().insert(key.clone(), plan.clone());
        }
        self.inline_plans.borrow_mut().insert(key, plan);
    }

    fn plan_is_cacheable(&self) -> bool {
        self.catalog_complete() && self.stub_overlay.borrow().is_empty()
    }
}

fn plan_key(
    owner: TypeName,
    name: &str,
    body_descriptor: &str,
    parameter_slots: &[u16],
    context_count: usize,
    source_receiver: Option<Ty>,
    semantic_parameters: &[Ty],
    semantic_result: Ty,
    suspend: bool,
    generic_signature: Option<(Option<Ty>, Ty)>,
    default_target: Option<(TypeName, &str, &str)>,
) -> PlanKey {
    PlanKey {
        owner,
        name: name.to_owned(),
        body_descriptor: body_descriptor.to_owned(),
        parameter_slots: parameter_slots.to_vec(),
        context_count,
        source_receiver,
        semantic_parameters: semantic_parameters.to_vec(),
        semantic_result,
        suspend,
        generic_signature,
        default_target: default_target
            .map(|(owner, name, descriptor)| (owner, name.to_owned(), descriptor.to_owned())),
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
        default: Option<(TypeName, &str, &str)>,
    ) -> Option<Option<Box<crate::libraries::InlineBodyPlan>>> {
        cp.cached_inline_plan(
            owner,
            "run",
            "()V",
            &[0],
            context_count,
            None,
            &[],
            result,
            suspend,
            generic,
            default,
        )
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
            owner,
            "run",
            "()V",
            &[0],
            0,
            None,
            &[],
            Ty::Unit,
            false,
            Some((None, Ty::Unit)),
            None,
            Some(Box::new(plan)),
        );
        assert!(cached(&cp, owner, 0, Ty::Unit, false, Some((None, Ty::Unit)), None,).is_some());
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
            (
                0,
                Ty::Unit,
                false,
                Some((None, Ty::Unit)),
                Some((owner, "run$default", "()V")),
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
            owner,
            "unrelated",
            "()V",
            &[0],
            0,
            None,
            &[],
            Ty::Unit,
            false,
            None,
            None,
            None,
        );
        cp.clear_stub_overlay();
        assert!(cp
            .cached_inline_plan(
                owner,
                "unrelated",
                "()V",
                &[0],
                0,
                None,
                &[],
                Ty::Unit,
                false,
                None,
                None,
            )
            .is_none());
        assert!(cached(&cp, owner, 0, Ty::Unit, false, Some((None, Ty::Unit)), None,).is_some());
    }
}
