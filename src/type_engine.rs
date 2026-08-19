//! The resolution engine's memo: one place that answers "what is the type of this declaration?",
//! on demand, once per declaration.
//!
//! krusty used to answer that question in several unrelated places, each a partial typer with its
//! own scope model and its own idea of when to give up (see `docs/RESOLUTION_ENGINE.md`). The
//! structural fault was not any one of them but the absence of a single engine: a pass that types
//! declarations in file-argument order answers a question whose answer depends on the order it was
//! asked in.
//!
//! The model is kotlinc's, verified against the compiler we vendor rather than described from
//! memory. `ReturnTypeCalculatorWithJump` types an implicitly-typed declaration by JUMPING to it and
//! running real body resolution, and `ImplicitBodyResolveComputationSession` holds exactly three
//! things: a memo keyed by declaration symbol, the stack of declarations currently being computed,
//! and the loops found. This module is that object; the "jump" — reconstructing a declaration's
//! resolution context and running the checker over its body — is the caller's `compute` closure, so
//! the memo stays independent of what it memoises and can be tested on its own.
//!
//! One invariant governs the whole engine: a WRONG declared type is a miscompile that runs green
//! (it becomes the field descriptor, the getter descriptor and `@Metadata`), while a decline is a
//! recoverable diagnostic. [`TypeEngine::resolve`] is therefore the single place that turns "no
//! answer" into [`Resolution::Declined`]; no caller invents a type when the computation gave none.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::types::Ty;

/// Which list within a class body a member index counts in.
///
/// Properties and methods are separate lists, so index 1 names a different declaration in each.
/// Without this, `class C { val a = 1; val b = 2; fun f() = 3; fun g() = 4 }` gives `b` and `g` the
/// same key: the memo answers a demand for one with the other's type, silently, and only when the
/// two happen to occupy the same position.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum MemberList {
    /// The declaration itself rather than one of its body members.
    Own,
    Property,
    Method,
}

/// A declaration whose type the engine can be asked for.
///
/// The key is positional rather than name-based on purpose: two files may legally declare the same
/// simple name in different packages, and a class body may declare several members that share a
/// name with an extension receiver distinguishing them. `member` is [`DeclKey::OWN`] for the
/// declaration itself and the member's index within its own body list otherwise.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct DeclKey {
    pub file: u32,
    pub decl: u32,
    pub member: u32,
    pub list: MemberList,
}

impl DeclKey {
    /// `member` value denoting the declaration itself rather than one of its body members.
    pub const OWN: u32 = u32::MAX;

    pub fn declaration(file: u32, decl: u32) -> Self {
        Self {
            file,
            decl,
            member: Self::OWN,
            list: MemberList::Own,
        }
    }

    /// A member PROPERTY, by its index among the class body's properties.
    pub fn member(file: u32, decl: u32, member: u32) -> Self {
        Self::in_list(file, decl, member, MemberList::Property)
    }

    /// A member FUNCTION, by its index among the class body's methods.
    pub fn method(file: u32, decl: u32, member: u32) -> Self {
        Self::in_list(file, decl, member, MemberList::Method)
    }

    fn in_list(file: u32, decl: u32, member: u32, list: MemberList) -> Self {
        debug_assert_ne!(
            member,
            Self::OWN,
            "member index u32::MAX is reserved for the declaration itself"
        );
        Self {
            file,
            decl,
            member,
            list,
        }
    }
}

/// Why the engine could not answer. The reason is carried rather than collapsed into a bare
/// "no type", because the two cases produce different diagnostics: recursion is reported at every
/// declaration on the loop (kotlinc: "type checking has run into a recursive problem"), while an
/// untypeable initializer is reported once at the declaration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeclineReason {
    /// The declaration's type depends on itself.
    Recursive,
    /// The computation ran and produced no type.
    Untypeable,
}

/// The engine's answer. There is no third state: a caller either has a type or has a reason it does
/// not, and cannot silently substitute one for the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Resolved(Ty),
    Declined(DeclineReason),
}

impl Resolution {
    /// The type, or `None` if the engine declined. Use at the boundary where a consumer genuinely
    /// has a fallback; never to paper over a decline with an invented type.
    pub fn ty(self) -> Option<Ty> {
        match self {
            Self::Resolved(ty) => Some(ty),
            Self::Declined(_) => None,
        }
    }

    pub fn declined(self) -> Option<DeclineReason> {
        match self {
            Self::Resolved(_) => None,
            Self::Declined(reason) => Some(reason),
        }
    }
}

/// The state of one declaration in the memo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// On the computing stack: reaching it again is a cycle.
    Computing,
    Resolved(Ty),
    Declined(DeclineReason),
}

/// Demand-driven, memoised declaration typing with cycle detection.
///
/// Interior mutability is deliberate. A resolution runs INSIDE a checker that holds `&SymbolTable`,
/// so the memo cannot live in the symbol table and be mutated through it; results are published into
/// the table at seam points instead.
#[derive(Default)]
pub struct TypeEngine {
    memo: RefCell<HashMap<DeclKey, State>>,
    computing: RefCell<Vec<DeclKey>>,
    /// Declarations that took part in a cycle. Every declaration on the loop is recorded, not only
    /// the one the recursion happened to re-enter, so the diagnostic can be reported at each of them
    /// the way kotlinc reports it.
    cycles: RefCell<HashSet<DeclKey>>,
}

impl TypeEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `key` is on the computing stack right now.
    ///
    /// A demand that CHOOSES between candidates must not choose one that is already being computed:
    /// asking for it closes a cycle that is an artefact of the choice, not of the program, and the
    /// decline is memoised for good. `fun foo() = foo(1)` beside `fun foo(i: Int) = "O"` is the
    /// case — an argument-less demand arriving while `foo()` is in flight would pick `foo()`.
    pub fn is_computing(&self, key: DeclKey) -> bool {
        matches!(self.memo.borrow().get(&key), Some(State::Computing))
    }

    /// The type of `key`, computing it at most once.
    ///
    /// `compute` performs the jump: it reconstructs the declaration's resolution context and runs the
    /// real expression typer over its body, returning `None` when that produced no type. It is not
    /// called if the answer is already known, and it is not called re-entrantly for the same
    /// declaration — a second request while the first is in flight is a cycle and declines instead,
    /// which is what makes termination structural rather than a depth budget.
    pub fn resolve(&self, key: DeclKey, compute: impl FnOnce() -> Option<Ty>) -> Resolution {
        match self.memo.borrow().get(&key).copied() {
            Some(State::Resolved(ty)) => return Resolution::Resolved(ty),
            Some(State::Declined(reason)) => return Resolution::Declined(reason),
            Some(State::Computing) => {
                self.record_cycle(key);
                return Resolution::Declined(DeclineReason::Recursive);
            }
            None => {}
        }
        self.memo.borrow_mut().insert(key, State::Computing);
        self.computing.borrow_mut().push(key);
        let computed = compute();
        let popped = self.computing.borrow_mut().pop();
        debug_assert_eq!(
            popped,
            Some(key),
            "the computing stack must unwind in the order it was pushed"
        );
        // A declaration reached again while it was computing keeps its decline: the value `compute`
        // produced was built on top of the recursive answer, so publishing it would make the type
        // depend on which member of the loop was asked for first — the order dependence the engine
        // exists to remove.
        if self.cycles.borrow().contains(&key) {
            let declined = Resolution::Declined(DeclineReason::Recursive);
            self.memo
                .borrow_mut()
                .insert(key, State::Declined(DeclineReason::Recursive));
            return declined;
        }
        let resolution = match computed {
            Some(ty) => Resolution::Resolved(ty),
            None => Resolution::Declined(DeclineReason::Untypeable),
        };
        self.memo.borrow_mut().insert(
            key,
            match resolution {
                Resolution::Resolved(ty) => State::Resolved(ty),
                Resolution::Declined(reason) => State::Declined(reason),
            },
        );
        resolution
    }

    /// Whether `key` was found to be part of a resolution cycle.
    pub fn in_cycle(&self, key: DeclKey) -> bool {
        self.cycles.borrow().contains(&key)
    }

    /// Every declaration found to be on a resolution cycle, in a stable order so a diagnostic sweep
    /// over them does not depend on hash iteration order.
    pub fn cycle_members(&self) -> Vec<DeclKey> {
        let mut members = self.cycles.borrow().iter().copied().collect::<Vec<_>>();
        members.sort_unstable();
        members
    }

    /// The already-computed answer for `key`, without starting a computation.
    ///
    /// Used by consumers that must not trigger resolution: a diagnostic pass reading what the
    /// engine concluded, and a scope being built for one declaration — which lists its siblings,
    /// and where demanding the declaration the scope belongs to would record a FALSE cycle and
    /// decline it permanently.
    pub fn known(&self, key: DeclKey) -> Option<Resolution> {
        match self.memo.borrow().get(&key).copied() {
            Some(State::Resolved(ty)) => Some(Resolution::Resolved(ty)),
            Some(State::Declined(reason)) => Some(Resolution::Declined(reason)),
            Some(State::Computing) | None => None,
        }
    }

    /// Record the loop that re-entering `key` closed: `key` itself and every declaration above it on
    /// the computing stack.
    fn record_cycle(&self, key: DeclKey) {
        let computing = self.computing.borrow();
        let start = computing
            .iter()
            .position(|entry| *entry == key)
            .unwrap_or(computing.len());
        let mut cycles = self.cycles.borrow_mut();
        cycles.insert(key);
        for entry in &computing[start..] {
            cycles.insert(*entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn key(decl: u32) -> DeclKey {
        DeclKey::declaration(0, decl)
    }

    #[test]
    fn computes_once_and_memoises() {
        let engine = TypeEngine::new();
        let calls = Cell::new(0);
        let compute = || {
            calls.set(calls.get() + 1);
            Some(Ty::Int)
        };
        assert_eq!(
            engine.resolve(key(1), compute),
            Resolution::Resolved(Ty::Int)
        );
        assert_eq!(
            engine.resolve(key(1), || {
                calls.set(calls.get() + 1);
                Some(Ty::String)
            }),
            Resolution::Resolved(Ty::Int),
            "a memoised declaration must not be recomputed, and must not change its answer"
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_declaration_that_produces_no_type_declines() {
        let engine = TypeEngine::new();
        assert_eq!(
            engine.resolve(key(1), || None),
            Resolution::Declined(DeclineReason::Untypeable)
        );
        assert_eq!(
            engine.resolve(key(1), || Some(Ty::Int)),
            Resolution::Declined(DeclineReason::Untypeable),
            "a decline is memoised like any other answer"
        );
    }

    #[test]
    fn self_recursion_declines_instead_of_recursing() {
        let engine = TypeEngine::new();
        let inner = engine.resolve(key(1), || engine.resolve(key(1), || Some(Ty::Int)).ty());
        assert_eq!(inner, Resolution::Declined(DeclineReason::Recursive));
        assert!(engine.in_cycle(key(1)));
    }

    #[test]
    fn mutual_recursion_declines_every_declaration_on_the_loop() {
        let engine = TypeEngine::new();
        // `val a = b` / `val b = a`: resolving `a` demands `b`, which demands `a` again.
        let a = engine.resolve(key(1), || {
            engine
                .resolve(key(2), || engine.resolve(key(1), || Some(Ty::Int)).ty())
                .ty()
        });
        assert_eq!(a, Resolution::Declined(DeclineReason::Recursive));
        assert_eq!(engine.cycle_members(), vec![key(1), key(2)]);
        assert_eq!(
            engine.known(key(2)),
            Some(Resolution::Declined(DeclineReason::Recursive)),
            "the inner declaration keeps the decline rather than a type built on the recursion"
        );
    }

    #[test]
    fn three_way_recursion_declines_every_declaration_on_the_loop() {
        let engine = TypeEngine::new();
        let a = engine.resolve(key(1), || {
            engine
                .resolve(key(2), || {
                    engine
                        .resolve(key(3), || engine.resolve(key(1), || Some(Ty::Int)).ty())
                        .ty()
                })
                .ty()
        });
        assert_eq!(a, Resolution::Declined(DeclineReason::Recursive));
        assert_eq!(engine.cycle_members(), vec![key(1), key(2), key(3)]);
    }

    #[test]
    fn a_declaration_reached_twice_without_a_cycle_still_resolves() {
        let engine = TypeEngine::new();
        // `val a = c` / `val b = c` / `val c = 1`: `c` is demanded twice, which is not a loop.
        let c = || engine.resolve(key(3), || Some(Ty::Int));
        let a = engine.resolve(key(1), || c().ty());
        let b = engine.resolve(key(2), || c().ty());
        assert_eq!(a, Resolution::Resolved(Ty::Int));
        assert_eq!(b, Resolution::Resolved(Ty::Int));
        assert!(engine.cycle_members().is_empty());
    }

    #[test]
    fn resolution_order_does_not_change_the_answer() {
        // The A.kt/B.kt case in memo form: whichever declaration is asked for first, both answers
        // are the same, because the dependency is resolved on demand rather than by whichever pass
        // ran first.
        let base = |engine: &TypeEngine| engine.resolve(key(1), || Some(Ty::Int));
        let derived = |engine: &TypeEngine| engine.resolve(key(2), || base(engine).ty());

        let forward = TypeEngine::new();
        let base_first = base(&forward);
        let derived_second = derived(&forward);

        let backward = TypeEngine::new();
        let derived_first = derived(&backward);
        let base_second = base(&backward);

        assert_eq!(base_first, base_second);
        assert_eq!(derived_second, derived_first);
        assert_eq!(derived_first, Resolution::Resolved(Ty::Int));
    }
}
