//! Lexical scopes for the checker.
//!
//! A scope is created per binding construct and linked to its parent by BORROW, so the chain lives
//! on the call stack: a scope cannot outlive its parent, cannot be left open by an early return,
//! and nothing here borrows the checker, so `&Scope` and `&mut Checker` never conflict.
//!
//! The chain holds only what is genuinely lexical — locals, parameters, type parameters, and the
//! implicit-receiver rungs. Class members, top-level declarations and the classpath are NOT scopes:
//! they are indexed namespaces reached by fall-through AFTER the chain misses (a class scope would
//! otherwise have to materialize its whole inherited member set on entry, and member visibility is
//! order-free while local visibility is not).
//!
//! The binding payload `B` stays generic so this module does not depend on the checker's `Local`.
//! The flow frame is concrete: narrowings are a property of lexical scopes, so they live here.

use crate::diag::Span;
use crate::types::{Ty, TypeName};
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ScopeKind {
    /// File level: the root of every chain. Holds no bindings of its own — imports and top-level
    /// declarations are fall-through namespaces, not lexical rungs.
    File,
    /// A class or object body.
    Class {
        /// Applied semantic receiver (`C<T>`), not merely the classifier name. Erasing the arguments
        /// here makes `this@C` disagree with an explicitly written `C<T>` in the same body.
        ty: Ty,
        /// FALSE for a plain nested `class`, a named `object` and a companion: those cut the
        /// implicit-receiver chain, so neither `this@Outer` nor the outer class's type parameters
        /// are reachable. TRUE for `inner class`, a local class and an anonymous object, which
        /// capture the enclosing instance.
        carries_outer: bool,
    },
    /// A function body's signature scope: type parameters and value parameters. The block body is a
    /// CHILD of this, which is what makes `fun f(x: Int) { val x = 1 }` shadowing (a warning)
    /// rather than a redeclaration (an error).
    Function {
        /// The extension receiver, when there is one.
        receiver: Option<Ty>,
    },
    /// Any other binding block: `if`/`when` branch, loop body, lambda body, `try`.
    Block,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextReceiver {
    pub(crate) ty: Ty,
    pub(crate) name: String,
    pub(crate) label: Option<String>,
}

impl ContextReceiver {
    pub(crate) fn new(ty: Ty, name: impl Into<String>, label: Option<String>) -> Self {
        Self {
            ty,
            name: name.into(),
            label,
        }
    }
}

/// A STABLE ACCESS PATH a flow narrowing (smart cast) applies to: an immutable ROOT binding
/// (`this`, a local `val`/parameter) followed by immutable property segments (`a.b.c`). A root-only
/// path is the classic name narrowing; segments extend the same proofs (`==`/`!=` null checks,
/// `is`/`!is` type tests, contract conclusions) to property reads, like kotlinc. One machinery
/// serves every condition shape and application site — nothing keys on the condition's syntax.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NarrowPath {
    pub(crate) root: String,
    pub(crate) segments: Vec<String>,
}

impl NarrowPath {
    pub(crate) fn root_only(root: &str) -> Self {
        NarrowPath {
            root: root.to_string(),
            segments: Vec::new(),
        }
    }
}

/// A recorded property-path narrowing. A `this`-rooted path names a property of one specific
/// receiver object, and a `this` rebind — a receiver lambda, an inner class, an extension receiver,
/// even to the SAME type — is a different object. That is expressed by WHERE the narrowing lives:
/// it sits in the frame of the scope that proved it, and `lookup_path_narrowing` stops walking
/// outward at the rung that established the receiver.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct PathNarrowing {
    pub(crate) ty: Ty,
}

/// A value or classifier a stable access path cannot denote on the current control-flow edge.
/// These are body-local semantic facts: exhaustiveness consumes them while checking a `when`, and
/// no complement/intersection pseudo-type can escape into checked FIR.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum FlowExclusion {
    Boolean(bool),
    EnumEntry { classifier: TypeName, name: String },
    Singleton(TypeName),
    Classifier(TypeName),
    Null,
}

/// Flow facts proven within one scope. Both kinds live HERE rather than on the binding they talk
/// about, which is what makes them die with the scope that proved them — in both directions, with
/// no reset step. A fact about a binding declared further out is still the inner scope's fact.
#[derive(Clone, Default)]
pub(crate) struct Flow {
    /// Property paths (`a.b`) proven non-null or `is T` by an enclosing condition.
    paths: HashMap<NarrowPath, PathNarrowing>,
    /// All incomparable types proved for one stable access path. A Kotlin smart cast may produce
    /// an intersection (`x is A && x is B`) even though the compact, globally shared [`Ty`] model
    /// deliberately has no body-flow-only intersection variant. These facts stay in the lexical
    /// flow frame and are discarded with the active body; a use site selects an ordinary bound and
    /// publishes only that checked receiver type to FIR.
    intersections: HashMap<NarrowPath, Vec<Ty>>,
    /// Values and classifier regions excluded by preceding conditions on stable paths.
    exclusions: HashMap<NarrowPath, Vec<FlowExclusion>>,
    /// Straight-line READ types for a `var` assigned a non-null value (`var x: Int?; x = 10`
    /// reads as `Int`). A nested lexical scope starts on the same execution edge and therefore sees
    /// an enclosing fact until it shadows or writes that binding; branch-local facts still die with
    /// the frame that established them.
    locals: HashMap<String, Ty>,
    /// Smart casts a condition ATTEMPTED but the stability gate declined because the root is a
    /// local `var` a capturing closure mutates: the variable's name and the target type the
    /// condition tried to prove. The fact dies with the guarded region like a real narrowing; a
    /// later nullable-receiver use reports kotlinc's SMARTCAST_IMPOSSIBLE instead of the plain
    /// unsafe-call error.
    declined: Vec<(String, Ty)>,
}

/// A short-lived copy of the flow frames on one lexical scope chain. Control-flow constructs use
/// this while checking sibling execution paths; it never survives body checking or crosses FIR.
#[derive(Clone)]
pub(crate) struct FlowSnapshot {
    frames: Vec<Flow>,
}

/// Kotlin's namespaces are distinct: `fun Foo()` and `val Foo` may coexist in one scope, and a
/// call site looks only in the function namespace. Redeclaration is an error only within the same
/// scope AND the same namespace.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum Ns {
    Value,
    Function,
    /// Types introduced lexically: a generic type parameter (`class C<T>`, `fun <T> f()`). One
    /// namespace, different declaring RUNG — a class's parameters sit on its class rung, a
    /// function's on its function rung, so each retires with the declaration that introduced it.
    Classifier,
}

pub(crate) enum RootValue<B> {
    Binding(B),
    Receiver,
    External,
}

/// One value available for implicit context-parameter binding. Scope owns the precedence between
/// receiver rungs and lexical values; the resolver only checks type applicability.
pub(crate) enum ContextValue {
    ImplicitReceiver {
        ty: Ty,
        current: bool,
        receiver_depth: usize,
        context_name: Option<String>,
        context_shadow_depth: usize,
    },
    Binding {
        name: String,
        shadow_depth: usize,
    },
}

struct ScopedReceiver {
    ty: Ty,
    identity: (usize, usize),
    extension_declaration: Option<Span>,
    class_receiver: bool,
    context_name: Option<String>,
    context_shadow_depth: usize,
}

impl ScopedReceiver {
    fn plain(ty: Ty, identity: (usize, usize)) -> Self {
        Self {
            ty,
            identity,
            extension_declaration: None,
            class_receiver: true,
            context_name: None,
            context_shadow_depth: 0,
        }
    }
}

struct Binding<B> {
    name: String,
    ns: Ns,
    payload: B,
}

pub(crate) struct Scope<'p, B> {
    parent: Option<&'p Scope<'p, B>>,
    kind: ScopeKind,
    /// Context receivers belonging to this function rung, outermost first. The ordinary
    /// extension/current receiver stays in [`ScopeKind::Function`]; keeping the remaining
    /// receivers on the same rung lets every scope consumer (member lookup and context-argument
    /// selection alike) observe the exact lambda shape.
    context_receivers: RefCell<Vec<ContextReceiver>>,
    /// Runtime binding of the function rung's current receiver. Ordinary extension receivers are
    /// addressed as `this`; when the last context parameter occupies this slot, lowering binds it
    /// under its declared name instead.
    current_receiver_name: Option<String>,
    /// Source declaration that introduced this rung's ordinary extension receiver. Receiver
    /// lambdas have no declaration; extension functions and properties retain the exact span so a
    /// selected outer receiver marks that declaration used without reconstructing identity from a
    /// type or label.
    extension_receiver_declaration: Option<Span>,
    /// Source-declared receiver type label (`String` in `fun String.f`). This cannot be recovered
    /// from the semantic type when the source used a type alias.
    extension_receiver_label: Option<String>,
    /// Bindings on this rung that are the lexical names of receiver entries owned by its parent
    /// function rung. Member parameters live on a child rung so they can shadow properties; named
    /// context parameters therefore need an explicit alias marker instead of being counted as a
    /// second value with the same name.
    parent_receiver_aliases: HashMap<String, usize>,
    /// Bindings introduced by THIS scope, in declaration order. Declaration order is what makes
    /// `fun g(a: Int, b: Int = a)` resolve and `fun g(a: Int = b, b: Int)` not.
    ///
    /// Interior-mutable because a narrowing shadow is declared into the CURRENT scope while
    /// checking a condition, when child scopes may already borrow this one: `&mut Scope` and a
    /// live child cannot coexist.
    bindings: RefCell<Vec<Binding<B>>>,
    /// Flow facts recorded in this scope. Dropped with the scope, which is the Kotlin rule: a smart
    /// cast established inside a branch does not survive it.
    flow: RefCell<Flow>,
}

impl<B> Scope<'static, B> {
    pub(crate) fn root() -> Scope<'static, B> {
        Scope::with_parent(None, ScopeKind::File)
    }
}

impl<'p, B> Scope<'p, B> {
    fn with_parent(parent: Option<&'p Scope<'p, B>>, kind: ScopeKind) -> Scope<'p, B> {
        Scope {
            parent,
            kind,
            context_receivers: RefCell::new(Vec::new()),
            current_receiver_name: None,
            extension_receiver_declaration: None,
            extension_receiver_label: None,
            parent_receiver_aliases: HashMap::new(),
            bindings: RefCell::new(Vec::new()),
            flow: RefCell::new(Flow::default()),
        }
    }

    pub(crate) fn child(&'p self, kind: ScopeKind) -> Scope<'p, B> {
        debug_assert!(
            !matches!(kind, ScopeKind::File),
            "the file scope is the root of the chain"
        );
        Scope::with_parent(Some(self), kind)
    }

    pub(crate) fn parameter_child(&'p self, context_receivers: &[ContextReceiver]) -> Scope<'p, B> {
        let mut child = Scope::with_parent(Some(self), ScopeKind::Block);
        for receiver in context_receivers {
            if receiver.name != "_" {
                *child
                    .parent_receiver_aliases
                    .entry(receiver.name.clone())
                    .or_default() += 1;
            }
        }
        child
    }

    /// Capture the flow state of this scope chain before checking a sibling execution path.
    pub(crate) fn flow_snapshot(&self) -> FlowSnapshot {
        FlowSnapshot {
            frames: self
                .ancestors()
                .map(|scope| scope.flow.borrow().clone())
                .collect(),
        }
    }

    /// Restore a previously captured state for the same live scope chain.
    pub(crate) fn restore_flow(&self, snapshot: &FlowSnapshot) {
        let scopes = self.ancestors().collect::<Vec<_>>();
        assert_eq!(
            scopes.len(),
            snapshot.frames.len(),
            "flow snapshot must belong to the same lexical scope chain"
        );
        for (scope, flow) in scopes.into_iter().zip(&snapshot.frames) {
            *scope.flow.borrow_mut() = flow.clone();
        }
    }

    /// Keep only facts that hold on every supplied normal-exit edge, then install that joined state.
    pub(crate) fn restore_common_flow(&self, snapshots: &[FlowSnapshot]) {
        let Some(first) = snapshots.first() else {
            return;
        };
        let mut common = first.clone();
        for snapshot in &snapshots[1..] {
            assert_eq!(
                common.frames.len(),
                snapshot.frames.len(),
                "joined flow snapshots must share one lexical scope chain"
            );
            for (frame, other) in common.frames.iter_mut().zip(&snapshot.frames) {
                frame
                    .paths
                    .retain(|path, fact| other.paths.get(path) == Some(fact));
                frame
                    .intersections
                    .retain(|path, facts| other.intersections.get(path) == Some(facts));
                frame
                    .exclusions
                    .retain(|path, facts| other.exclusions.get(path) == Some(facts));
                frame
                    .locals
                    .retain(|name, ty| other.locals.get(name) == Some(ty));
                frame.declined.retain(|fact| other.declined.contains(fact));
            }
        }
        self.restore_flow(&common);
    }

    pub(crate) fn function_child(
        &'p self,
        receiver: Option<Ty>,
        receiver_name: Option<String>,
        context_receivers: &[ContextReceiver],
    ) -> Scope<'p, B> {
        let mut child = Scope::with_parent(Some(self), ScopeKind::Function { receiver });
        child.current_receiver_name = receiver_name;
        child
            .context_receivers
            .get_mut()
            .extend_from_slice(context_receivers);
        child
    }

    pub(crate) fn declaration_function_child_with_context(
        &'p self,
        receiver: Option<Ty>,
        extension_declaration: Option<(Span, String)>,
        context_receivers: &[ContextReceiver],
    ) -> Scope<'p, B> {
        debug_assert_eq!(receiver.is_some(), extension_declaration.is_some());
        let mut child = Scope::with_parent(Some(self), ScopeKind::Function { receiver });
        if let Some((declaration, label)) = extension_declaration {
            child.extension_receiver_declaration = Some(declaration);
            child.extension_receiver_label = Some(label);
        }
        child
            .context_receivers
            .get_mut()
            .extend_from_slice(context_receivers);
        child
    }

    /// Attach classifier-owned context receivers after its type parameters have entered the class
    /// rung and their types can be resolved. This mutates only the current lexical scope frame.
    pub(crate) fn declare_context_receivers(&self, receivers: &[ContextReceiver]) {
        self.context_receivers
            .borrow_mut()
            .extend_from_slice(receivers);
    }

    pub(crate) fn kind(&self) -> ScopeKind {
        self.kind
    }

    /// Bind `name`, replacing any binding of the same namespace in THIS scope. Kotlin has no such
    /// declaration; it exists for the narrowing shadow, which re-binds the SAME runtime value under
    /// its proven type.
    pub(crate) fn rebind(&self, name: &str, ns: Ns, payload: B) {
        let mut bindings = self.bindings.borrow_mut();
        if let Some(slot) = bindings.iter_mut().find(|b| b.ns == ns && b.name == name) {
            slot.payload = payload;
        } else {
            bindings.push(Binding {
                name: name.to_string(),
                ns,
                payload,
            });
        }
    }

    /// This scope and every enclosing one, innermost first.
    pub(crate) fn ancestors<'s>(&'s self) -> impl Iterator<Item = &'s Scope<'p, B>> {
        std::iter::successors(Some(self), |s: &&'s Scope<'p, B>| s.parent)
    }

    /// The rungs a CLASSIFIER lookup may consult, innermost first. Same cut as
    /// [`Self::implicit_receivers`]: a class rung that does not carry its outer instance ends the
    /// walk, because the outer declaration's type parameters are not reachable from it —
    /// `class A<T> { class B { fun g(): T } }` is rejected by kotlinc, while an `inner class`, a
    /// local class and an anonymous object all keep `T`.
    ///
    /// The FILE rung is always consulted, cut or not: it is the root of every chain, and nothing
    /// that severs the receiver walk can put a declaration outside the file it was written in.
    pub(crate) fn classifier_rungs<'s>(&'s self) -> impl Iterator<Item = &'s Scope<'p, B>> {
        let mut cut = false;
        self.ancestors().filter(move |rung| {
            if matches!(rung.kind, ScopeKind::File) {
                return true;
            }
            if cut {
                return false;
            }
            cut = matches!(
                rung.kind,
                ScopeKind::Class {
                    carries_outer: false,
                    ..
                }
            );
            true
        })
    }

    /// A binding introduced by THIS scope only. The caller walks `ancestors` itself, because an
    /// unqualified name must consult each rung's MEMBERS between rungs — an inner class's members
    /// shadow an enclosing function's locals, so lookup cannot be "all bindings, then all
    /// receivers".
    pub(crate) fn own_binding(&self, name: &str, ns: Ns) -> Option<B>
    where
        B: Clone,
    {
        self.bindings
            .borrow()
            .iter()
            .rev()
            .find(|b| b.ns == ns && b.name == name)
            .map(|b| b.payload.clone())
    }

    fn own_value_root(&self, name: &str) -> Option<B>
    where
        B: Clone,
    {
        self.bindings
            .borrow()
            .iter()
            .rev()
            .find(|binding| {
                binding.name == name
                    && (binding.ns == Ns::Value
                        || (binding.ns == Ns::Function
                            && matches!(self.kind, ScopeKind::File | ScopeKind::Block)))
            })
            .map(|binding| binding.payload.clone())
    }

    /// Resolve the value half of an unqualified first segment through one scope-tower query. The
    /// caller supplies provider-backed predicates; scope owns precedence and returns one normalized
    /// value origin.
    pub(crate) fn root_value(
        &self,
        name: &str,
        mut receiver_has_value: impl FnMut(Ty) -> bool,
        external_has_value: impl FnOnce() -> bool,
    ) -> Option<RootValue<B>>
    where
        B: Clone,
    {
        for rung in self.ancestors() {
            let cuts_outer = matches!(
                rung.kind,
                ScopeKind::Class {
                    carries_outer: false,
                    ..
                }
            );
            if let Some(binding) = rung.own_value_root(name) {
                return Some(RootValue::Binding(binding));
            }
            match rung.kind {
                ScopeKind::Function {
                    receiver: Some(receiver),
                } if receiver_has_value(receiver) => {
                    return Some(RootValue::Receiver);
                }
                ScopeKind::Class { ty, .. } => {
                    if receiver_has_value(ty) {
                        return Some(RootValue::Receiver);
                    }
                }
                _ => {}
            }
            if !rung.context_receivers.borrow().is_empty() {
                for receiver in rung.context_receivers.borrow().iter().rev() {
                    if receiver_has_value(receiver.ty) {
                        return Some(RootValue::Receiver);
                    }
                }
            }
            if cuts_outer {
                break;
            }
        }
        external_has_value().then_some(RootValue::External)
    }

    /// Whether THIS scope already binds `name` in `ns` — Kotlin's "conflicting declarations".
    /// Shadowing an OUTER scope is legal, so this deliberately does not walk parents.
    pub(crate) fn declared_here(&self, name: &str, ns: Ns) -> bool {
        self.bindings
            .borrow()
            .iter()
            .any(|b| b.ns == ns && b.name == name)
    }

    /// Every live binding in the chain, innermost scope first. Used by capture discovery, which
    /// needs the whole visible set rather than one name.
    pub(crate) fn visit_bindings(&self, ns: Ns, mut visit: impl FnMut(&str, &B)) {
        for scope in self.ancestors() {
            for binding in scope.bindings.borrow().iter().rev() {
                if binding.ns == ns {
                    visit(&binding.name, &binding.payload);
                }
            }
        }
    }

    /// Select the nearest value that may fill one context parameter. Implicit receivers are ordered
    /// innermost-first, then lexical bindings are ordered by their ordinary scope chain.
    pub(crate) fn find_context_value(
        &self,
        mut receiver_matches: impl FnMut(Ty) -> bool,
        mut binding_matches: impl FnMut(&B) -> bool,
    ) -> Option<ContextValue>
    where
        B: Clone,
    {
        for (index, receiver) in self.implicit_receiver_values().into_iter().enumerate() {
            let ty = receiver.ty;
            if receiver_matches(ty) {
                return Some(ContextValue::ImplicitReceiver {
                    ty,
                    current: index == 0,
                    receiver_depth: index,
                    context_name: receiver.context_name,
                    context_shadow_depth: receiver.context_shadow_depth,
                });
            }
        }
        let mut same_name_depths = HashMap::<String, usize>::new();
        for scope in self.ancestors() {
            for binding in scope.bindings.borrow().iter().rev() {
                if binding.ns != Ns::Value {
                    continue;
                }
                let shadow_depth = *same_name_depths.get(&binding.name).unwrap_or(&0);
                if binding_matches(&binding.payload) {
                    return Some(ContextValue::Binding {
                        name: binding.name.clone(),
                        shadow_depth,
                    });
                }
                *same_name_depths.entry(binding.name.clone()).or_default() += 1;
            }
        }
        None
    }

    /// Every binding THIS scope introduces in `ns`, in declaration order. The per-rung view a
    /// namespace whose lookup is rung-sensitive needs (classifiers stop at [`Self::classifier_rungs`]).
    pub(crate) fn own_bindings(&self, ns: Ns, mut visit: impl FnMut(&str, &B)) {
        for binding in self.bindings.borrow().iter() {
            if binding.ns == ns {
                visit(&binding.name, &binding.payload);
            }
        }
    }

    /// Prove a property path in THIS scope. The proof dies with the scope, which is the Kotlin
    /// rule: a smart cast established inside a branch does not survive it.
    pub(crate) fn narrow_path(&self, path: NarrowPath, ty: Ty) {
        self.flow
            .borrow_mut()
            .paths
            .insert(path, PathNarrowing { ty });
    }

    /// Add one constituent of a flow intersection proved for `path` in this scope.
    pub(crate) fn narrow_intersection(&self, path: NarrowPath, ty: Ty) {
        let mut flow = self.flow.borrow_mut();
        let types = flow.intersections.entry(path).or_default();
        if !types.contains(&ty) {
            types.push(ty);
        }
    }

    /// Intersection constituents proved in THIS scope. The checker owns the ancestry walk because
    /// it must stop at the binding/receiver boundary for the path, like ordinary path narrowing.
    pub(crate) fn intersection_narrowing(&self, path: &NarrowPath) -> Vec<Ty> {
        self.flow
            .borrow()
            .intersections
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// Add one negative flow fact for `path` in this lexical frame.
    pub(crate) fn exclude(&self, path: NarrowPath, exclusion: FlowExclusion) {
        let mut flow = self.flow.borrow_mut();
        let exclusions = flow.exclusions.entry(path).or_default();
        if !exclusions.contains(&exclusion) {
            exclusions.push(exclusion);
        }
    }

    /// Negative facts proved in THIS scope. The checker owns the ancestry walk so it can stop at
    /// the binding or receiver boundary that gives the path its identity.
    pub(crate) fn exclusions(&self, path: &NarrowPath) -> Vec<FlowExclusion> {
        self.flow
            .borrow()
            .exclusions
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// The proof THIS scope holds for `path`, if any. Callers walk `ancestors` themselves because
    /// the walk must stop at the rung owning the path's root.
    pub(crate) fn path_narrowing(&self, path: &NarrowPath) -> Option<Ty> {
        self.flow
            .borrow()
            .paths
            .get(path)
            .map(|narrowing| narrowing.ty)
    }

    /// Drop every path proof rooted at `root`. A NEW binding under that name invalidates them —
    /// the proof was about the old value.
    pub(crate) fn invalidate_paths_rooted_at(&self, root: &str) {
        let mut flow = self.flow.borrow_mut();
        flow.paths.retain(|path, _| path.root != root);
        flow.intersections.retain(|path, _| path.root != root);
        flow.exclusions.retain(|path, _| path.root != root);
    }

    /// Record the straight-line read type of `name` for the rest of THIS scope, or with `None`
    /// drop it.
    ///
    /// Proving is scope-local, but INVALIDATION is not: an assignment inside a nested scope
    /// disproves a narrowing an enclosing scope established (`var x: Int? = 10; if (c) { x = null }`
    /// — the outer proof is dead once the branch can run), so `None` clears the whole chain.
    pub(crate) fn narrow_local(&self, name: &str, ty: Option<Ty>) {
        // An assignment replaces every type fact about this runtime value, including an `A & B`
        // proof established by an enclosing condition. Stop at the declaring rung so a same-named
        // outer binding keeps its unrelated facts.
        for rung in self.ancestors() {
            rung.flow
                .borrow_mut()
                .intersections
                .retain(|path, _| path.root != name);
            rung.flow
                .borrow_mut()
                .exclusions
                .retain(|path, _| path.root != name);
            if rung.declared_here(name, Ns::Value) {
                break;
            }
        }
        match ty {
            Some(ty) => {
                self.flow.borrow_mut().locals.insert(name.to_string(), ty);
                // A nested write invalidates an enclosing straight-line fact for the same binding.
                if self.declared_here(name, Ns::Value) {
                    return;
                }
                for rung in self.ancestors().skip(1) {
                    rung.flow.borrow_mut().locals.remove(name);
                    if rung.declared_here(name, Ns::Value) {
                        return;
                    }
                }
            }
            None => {
                // Stop at the rung DECLARING `name`: an assignment to a shadowing variable of the
                // same name says nothing about the binding it shadows.
                for rung in self.ancestors() {
                    rung.flow.borrow_mut().locals.remove(name);
                    if rung.declared_here(name, Ns::Value) {
                        return;
                    }
                }
            }
        }
    }

    /// The nearest visible straight-line read type proven for `name`. A nested block inherits facts
    /// from its entry edge, but a same-named declaration cuts the walk exactly like value lookup.
    pub(crate) fn local_narrowing(&self, name: &str) -> Option<Ty> {
        for rung in self.ancestors() {
            if let Some(ty) = rung.flow.borrow().locals.get(name).copied() {
                return Some(ty);
            }
            if rung.declared_here(name, Ns::Value) {
                return None;
            }
        }
        None
    }

    /// Record that a condition guarding THIS scope tried to smart-cast `name` to `ty` but the
    /// proof was declined — the variable is a local `var` a capturing closure mutates.
    pub(crate) fn decline_cast(&self, name: &str, ty: Ty) {
        self.flow.borrow_mut().declined.push((name.to_string(), ty));
    }

    /// Record a declined cast at the rung that owns the active binding.
    pub(crate) fn decline_cast_at_binding(&self, name: &str, ty: Ty) {
        for rung in self.ancestors() {
            if rung.declared_here(name, Ns::Value) {
                rung.flow.borrow_mut().declined.push((name.to_string(), ty));
                return;
            }
        }
    }

    /// The target a condition tried to smart-cast `name` to in THIS or an enclosing scope, if
    /// any. Walks outward like a read of the variable and stops at the rung declaring it: a
    /// shadowing binding is not the variable the condition spoke about.
    pub(crate) fn declined_cast(&self, name: &str) -> Option<Ty> {
        for rung in self.ancestors() {
            if let Some((_, ty)) = rung
                .flow
                .borrow()
                .declined
                .iter()
                .rev()
                .find(|(n, _)| n == name)
            {
                return Some(*ty);
            }
            if rung.declared_here(name, Ns::Value) {
                return None;
            }
        }
        None
    }

    /// Replace the payload of the innermost `name` binding in `ns` wherever it lives in the
    /// chain, returning how many rungs up it was found (0 = THIS rung). Unlike [`Self::rebind`]
    /// (this rung only) this follows the binding: a smart-cast shadow of a `var` must track a
    /// reassignment even when the write sits in a nested block under the shadow's rung. The
    /// closure receives the current payload and the rung depth.
    pub(crate) fn rebind_nearest(
        &self,
        name: &str,
        ns: Ns,
        f: impl FnOnce(&B, usize) -> B,
    ) -> Option<usize> {
        for (depth, rung) in self.ancestors().enumerate() {
            let mut bindings = rung.bindings.borrow_mut();
            if let Some(slot) = bindings
                .iter_mut()
                .rev()
                .find(|b| b.ns == ns && b.name == name)
            {
                slot.payload = f(&slot.payload, depth);
                return Some(depth);
            }
        }
        None
    }

    /// Every visible straight-line narrowing, innermost fact first and cut by shadowing bindings.
    pub(crate) fn local_narrowings(&self) -> HashMap<String, Ty> {
        let mut visible = HashMap::new();
        let mut shadowed = std::collections::HashSet::new();
        for rung in self.ancestors() {
            for (name, &ty) in rung.flow.borrow().locals.iter() {
                if !shadowed.contains(name.as_str()) {
                    visible.entry(name.clone()).or_insert(ty);
                }
            }
            for binding in rung.bindings.borrow().iter().rev() {
                if binding.ns == Ns::Value {
                    shadowed.insert(binding.name.clone());
                }
            }
        }
        visible
    }

    /// Implicit receivers, innermost first: extension receivers and `this@Class` rungs. Stops at
    /// the first class scope that does not carry its outer instance.
    pub(crate) fn implicit_receivers(&self) -> Vec<Ty> {
        self.implicit_receiver_values()
            .into_iter()
            .map(|receiver| receiver.ty)
            .collect()
    }

    /// Implicit receivers paired with the extension declaration, if any, that introduced their
    /// runtime value. Ordering is identical to [`Self::implicit_receivers`].
    pub(crate) fn implicit_receivers_with_declarations(
        &self,
    ) -> Vec<(Ty, Option<Span>, (usize, usize), bool)> {
        self.implicit_receiver_values()
            .into_iter()
            .map(|receiver| {
                (
                    receiver.ty,
                    receiver.extension_declaration,
                    receiver.identity,
                    receiver.class_receiver,
                )
            })
            .collect()
    }

    pub(crate) fn implicit_receiver_context_name(
        &self,
        identity: (usize, usize),
    ) -> Option<String> {
        self.implicit_receiver_values()
            .into_iter()
            .find(|receiver| receiver.identity == identity)
            .and_then(|receiver| receiver.context_name)
    }

    pub(crate) fn implicit_receiver_has_context_label(
        &self,
        identity: (usize, usize),
        label: &str,
    ) -> bool {
        self.ancestors().any(|scope| {
            let scope_identity = scope as *const Self as usize;
            scope
                .context_receivers
                .borrow()
                .iter()
                .rev()
                .enumerate()
                .any(|(index, receiver)| {
                    (scope_identity, index + 1) == identity
                        && receiver.label.as_deref() == Some(label)
                })
        })
    }

    pub(crate) fn implicit_receiver_context_label(
        &self,
        identity: (usize, usize),
    ) -> Option<String> {
        self.ancestors().find_map(|scope| {
            let scope_identity = scope as *const Self as usize;
            scope
                .context_receivers
                .borrow()
                .iter()
                .rev()
                .enumerate()
                .find_map(|(index, receiver)| {
                    ((scope_identity, index + 1) == identity)
                        .then(|| receiver.label.clone())
                        .flatten()
                })
        })
    }

    pub(crate) fn implicit_receiver_has_extension_label(
        &self,
        identity: (usize, usize),
        label: &str,
    ) -> bool {
        self.ancestors().any(|scope| {
            let scope_identity = scope as *const Self as usize;
            (scope_identity, 0) == identity
                && matches!(scope.kind, ScopeKind::Function { receiver: Some(_) })
                && scope.extension_receiver_label.as_deref() == Some(label)
        })
    }

    /// Identity of the innermost class receiver rung, if this lexical chain has one.
    ///
    /// Receiver types are not identities: a context parameter, extension receiver, and class
    /// receiver may all have the same applied type while denoting different runtime values.
    pub(crate) fn innermost_class_receiver_identity(&self) -> Option<(usize, usize)> {
        self.ancestors().find_map(|scope| {
            matches!(scope.kind, ScopeKind::Class { .. })
                .then_some((scope as *const Self as usize, 0))
        })
    }

    fn implicit_receiver_values(&self) -> Vec<ScopedReceiver> {
        let mut out = Vec::new();
        let mut same_name_depths = HashMap::<String, usize>::new();
        let push = |out: &mut Vec<ScopedReceiver>,
                    same_name_depths: &mut HashMap<String, usize>,
                    ty,
                    identity,
                    context_name: Option<String>,
                    extension_declaration: Option<Span>| {
            let context_shadow_depth = context_name
                .as_ref()
                .map(|name| {
                    let depth = *same_name_depths.get(name).unwrap_or(&0);
                    *same_name_depths.entry(name.clone()).or_default() += 1;
                    depth
                })
                .unwrap_or(0);
            out.push(ScopedReceiver {
                ty,
                identity,
                extension_declaration,
                class_receiver: false,
                context_name,
                context_shadow_depth,
            });
        };
        for scope in self.ancestors() {
            let scope_identity = scope as *const Self as usize;
            let cuts_outer = matches!(
                scope.kind,
                ScopeKind::Class {
                    carries_outer: false,
                    ..
                }
            );
            let mut receiver_binding_counts = scope.parent_receiver_aliases.clone();
            if let Some(name) = scope.current_receiver_name.as_ref() {
                *receiver_binding_counts.entry(name.clone()).or_default() += 1;
            }
            for receiver in scope.context_receivers.borrow().iter() {
                if receiver.name != "_" {
                    *receiver_binding_counts
                        .entry(receiver.name.clone())
                        .or_default() += 1;
                }
            }
            for binding in scope.bindings.borrow().iter().rev() {
                if binding.ns != Ns::Value {
                    continue;
                }
                if receiver_binding_counts
                    .get_mut(&binding.name)
                    .is_some_and(|remaining| {
                        if *remaining == 0 {
                            false
                        } else {
                            *remaining -= 1;
                            true
                        }
                    })
                {
                    continue;
                }
                *same_name_depths.entry(binding.name.clone()).or_default() += 1;
            }
            match scope.kind {
                ScopeKind::Function {
                    receiver: Some(receiver),
                } => {
                    push(
                        &mut out,
                        &mut same_name_depths,
                        receiver,
                        (scope_identity, 0),
                        scope.current_receiver_name.clone(),
                        scope.extension_receiver_declaration,
                    );
                }
                ScopeKind::Class { ty, .. } => {
                    out.push(ScopedReceiver::plain(ty, (scope_identity, 0)));
                }
                _ => {}
            }
            if !scope.context_receivers.borrow().is_empty() {
                for (index, receiver) in scope.context_receivers.borrow().iter().rev().enumerate() {
                    let context_name = (receiver.name != "_").then(|| receiver.name.clone());
                    push(
                        &mut out,
                        &mut same_name_depths,
                        receiver.ty,
                        (scope_identity, index + 1),
                        context_name,
                        None,
                    );
                }
            }
            if cuts_outer {
                break;
            }
        }
        out
    }

    /// Implicit receivers encountered before the first lexical binding of `name` in `ns`.
    ///
    /// Unqualified lookup is a rung-by-rung tower, not "all lexical bindings, then all receivers":
    /// a binding on the current rung wins there, while a nearer extension/receiver-lambda receiver
    /// wins over a binding on an outer rung. This projection lets the checker query receiver members
    /// before committing the ordinary chain-wide binding lookup without copying scope traversal.
    pub(crate) fn implicit_receiver_identities_before_binding(
        &self,
        name: &str,
        ns: Ns,
    ) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for scope in self.ancestors() {
            let scope_identity = scope as *const Self as usize;
            let cuts_outer = matches!(
                scope.kind,
                ScopeKind::Class {
                    carries_outer: false,
                    ..
                }
            );
            if scope.declared_here(name, ns) {
                break;
            }
            match scope.kind {
                ScopeKind::Function {
                    receiver: Some(receiver),
                } => {
                    let _ = receiver;
                    out.push((scope_identity, 0));
                }
                ScopeKind::Class { ty, .. } => {
                    let _ = ty;
                    out.push((scope_identity, 0));
                }
                _ => {}
            }
            if !scope.context_receivers.borrow().is_empty() {
                out.extend(
                    scope
                        .context_receivers
                        .borrow()
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(index, _)| (scope_identity, index + 1)),
                );
            }
            if cuts_outer {
                break;
            }
        }
        out
    }

    #[cfg(test)]
    pub(crate) fn implicit_receivers_before_binding(&self, name: &str, ns: Ns) -> Vec<Ty> {
        let identities = self.implicit_receiver_identities_before_binding(name, ns);
        self.implicit_receiver_values()
            .into_iter()
            .filter(|receiver| identities.contains(&receiver.identity))
            .map(|receiver| receiver.ty)
            .collect()
    }

    /// The type a bare `this` denotes.
    pub(crate) fn this_ty(&self) -> Option<Ty> {
        self.implicit_receivers().into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    fn class(name: &str, carries_outer: bool) -> ScopeKind {
        ScopeKind::Class {
            ty: Ty::Obj(type_name(name), &[]),
            carries_outer,
        }
    }

    fn obj(name: &str) -> Ty {
        Ty::Obj(type_name(name), &[])
    }

    #[test]
    fn context_receiver_coordinate_counts_same_named_shadows() {
        let root: Scope<'_, u32> = Scope::root();
        let outer = root.function_child(Some(Ty::String), Some("value".to_string()), &[]);
        let inner = outer.function_child(Some(Ty::Int), Some("value".to_string()), &[]);
        inner.rebind("value", Ns::Value, 1);
        let body = inner.child(ScopeKind::Block);
        body.rebind("value", Ns::Value, 2);

        let selected = body.find_context_value(|ty| ty == Ty::String, |_| false);
        assert!(matches!(
            selected,
            Some(ContextValue::ImplicitReceiver {
                ty: Ty::String,
                context_name: Some(name),
                context_shadow_depth: 2,
                ..
            }) if name == "value"
        ));
    }

    #[test]
    fn named_context_parameter_binding_aliases_its_parent_receiver() {
        let root: Scope<'_, u32> = Scope::root();
        let receiver = ContextReceiver::new(Ty::String, "value", None);
        let function = root.function_child(None, None, std::slice::from_ref(&receiver));
        let parameters = function.parameter_child(std::slice::from_ref(&receiver));
        parameters.rebind("value", Ns::Value, 1);

        let selected = parameters.find_context_value(|ty| ty == Ty::String, |_| false);
        assert!(matches!(
            selected,
            Some(ContextValue::ImplicitReceiver {
                ty: Ty::String,
                context_name: Some(name),
                context_shadow_depth: 0,
                ..
            }) if name == "value"
        ));
    }

    #[test]
    fn shadowing_is_legal_and_scoped_to_the_scope_that_declares_it() {
        let root: Scope<'_, u32> = Scope::root();
        let outer = root.child(ScopeKind::Function { receiver: None });
        outer.rebind("x", Ns::Value, 1);
        assert!(outer.declared_here("x", Ns::Value));

        let inner = outer.child(ScopeKind::Block);
        assert!(
            !inner.declared_here("x", Ns::Value),
            "`declared_here` must not walk parents — shadowing an outer scope is legal in Kotlin, \
             a redeclaration in the SAME scope is not"
        );
        inner.rebind("x", Ns::Value, 2);
        assert_eq!(inner.own_binding("x", Ns::Value), Some(2));
        assert_eq!(
            outer.own_binding("x", Ns::Value),
            Some(1),
            "the inner binding must not disturb the one it shadows"
        );
    }

    #[test]
    fn a_parameter_is_an_outer_scope_to_the_body() {
        // `fun f(x: Int) { val x = 1 }` is a warning in Kotlin, not "conflicting declarations",
        // which holds only if params and body are DIFFERENT scopes (unlike Java).
        let root: Scope<'_, u32> = Scope::root();
        let params = root.child(ScopeKind::Function { receiver: None });
        params.rebind("x", Ns::Value, 1);
        let body = params.child(ScopeKind::Block);
        assert!(!body.declared_here("x", Ns::Value));
    }

    #[test]
    fn nested_class_cuts_the_receiver_chain_but_inner_does_not() {
        let root: Scope<'_, u32> = Scope::root();
        let outer = root.child(class("A", false));

        let nested = outer.child(class("A$C", false));
        assert_eq!(
            nested.implicit_receivers(),
            vec![obj("A$C")],
            "a plain nested class has no outer instance: `this@A` is unreachable"
        );

        let inner = outer.child(class("A$B", true));
        assert_eq!(
            inner.implicit_receivers(),
            vec![obj("A$B"), obj("A")],
            "an `inner class` keeps the enclosing instance"
        );
    }

    #[test]
    fn extension_receiver_is_a_rung_between_class_rungs() {
        let root: Scope<'_, u32> = Scope::root();
        let class_scope = root.child(class("A", false));
        let ext = class_scope.child(ScopeKind::Function {
            receiver: Some(obj("kotlin/String")),
        });
        assert_eq!(
            ext.implicit_receivers(),
            vec![obj("kotlin/String"), obj("A")],
            "the extension receiver is nearer than the enclosing class"
        );
        assert_eq!(ext.this_ty(), Some(obj("kotlin/String")));
    }

    #[test]
    fn class_receiver_identity_is_not_a_same_typed_context_receiver() {
        let root: Scope<'_, u32> = Scope::root();
        let class_scope = root.child(class("A", false));
        let function = class_scope.function_child(
            None,
            None,
            &[ContextReceiver::new(obj("A"), "other", None)],
        );

        let receivers = function.implicit_receivers_with_declarations();
        let class_identity = function
            .innermost_class_receiver_identity()
            .expect("class receiver identity");

        assert_eq!(receivers.len(), 2);
        assert_eq!(receivers[0].0, obj("A"));
        assert_eq!(receivers[1].0, obj("A"));
        assert_ne!(receivers[0].2, class_identity);
        assert_eq!(receivers[1].2, class_identity);
    }

    #[test]
    fn extension_receiver_precedes_an_outer_constructor_binding() {
        let root: Scope<'_, u32> = Scope::root();
        let class_scope = root.child(class("Container", false));
        let constructor_body = class_scope.child(ScopeKind::Block);
        constructor_body.rebind("value", Ns::Value, 1);
        let accessor = constructor_body.child(ScopeKind::Function {
            receiver: Some(obj("Token")),
        });

        assert_eq!(
            accessor.implicit_receivers_before_binding("value", Ns::Value),
            vec![obj("Token")]
        );
        assert!(accessor
            .implicit_receivers_before_binding("local", Ns::Value)
            .contains(&obj("Container")));
    }

    #[test]
    fn a_local_class_inside_a_member_function_sees_both_receivers() {
        // class A { fun f() { class B { fun g() { <here> } } } }
        let root: Scope<'_, u32> = Scope::root();
        let a = root.child(class("A", false));
        let f = a.child(ScopeKind::Function { receiver: None });
        let b = f.child(class("B", true));
        let g = b.child(ScopeKind::Function { receiver: None });
        assert_eq!(g.implicit_receivers(), vec![obj("B"), obj("A")]);
    }

    /// The innermost classifier binding for `name`, as the checker's type-parameter lookup does it.
    fn classifier(scope: &Scope<'_, u32>, name: &str) -> Option<u32> {
        scope
            .classifier_rungs()
            .find_map(|rung| rung.own_binding(name, Ns::Classifier))
    }

    #[test]
    fn a_plain_nested_class_cuts_the_type_parameter_walk_but_a_local_class_does_not() {
        // kotlinc 2.4.10: `class A<T> { class B { fun g(): T } }` is "unresolved reference 'T'",
        // while a local class or an anonymous object inside a member of `A` still sees `T`.
        let root: Scope<'_, u32> = Scope::root();
        let a = root.child(class("A", false));
        a.rebind("T", Ns::Classifier, 1);

        let nested = a.child(class("A$B", false));
        assert_eq!(
            classifier(&nested, "T"),
            None,
            "a plain nested class cannot reach the outer class's type parameters"
        );

        let member = a.child(ScopeKind::Function { receiver: None });
        let local = member.child(class("L", true));
        assert_eq!(
            classifier(&local, "T"),
            Some(1),
            "a local class carries the outer instance, so `T` stays reachable"
        );
    }

    #[test]
    fn a_type_parameter_retires_with_the_rung_that_declared_it() {
        // A declaration's type parameters (and their `reified` marks) must not stay visible to the
        // next declaration checked from the same enclosing scope.
        let root: Scope<'_, u32> = Scope::root();
        {
            let first = root.child(ScopeKind::Block);
            first.rebind("T", Ns::Classifier, 1);
            assert_eq!(classifier(&first, "T"), Some(1));
        }
        let second = root.child(ScopeKind::Block);
        assert_eq!(
            classifier(&second, "T"),
            None,
            "a sibling declaration must not inherit the previous one's type parameters"
        );
    }

    #[test]
    fn a_function_rung_does_not_cut_the_type_parameter_walk() {
        // `class C<T> { fun <U> m() { … } }`: the method's parameters sit on ITS rung and the
        // class's on the class rung — one namespace, different declaring rung — and the body sees
        // both. Only a class rung that drops its outer instance ends the walk.
        let root: Scope<'_, u32> = Scope::root();
        let class_scope = root.child(class("C", false));
        class_scope.rebind("T", Ns::Classifier, 1);
        let method = class_scope.child(ScopeKind::Function { receiver: None });
        method.rebind("U", Ns::Classifier, 2);
        let body = method.child(ScopeKind::Block);
        assert_eq!(classifier(&body, "U"), Some(2));
        assert_eq!(classifier(&body, "T"), Some(1));
        assert_eq!(
            classifier(&class_scope, "U"),
            None,
            "the method's parameters retire with the method's rung"
        );
    }

    #[test]
    fn an_inner_type_parameter_shadows_an_enclosing_one_of_the_same_name() {
        let root: Scope<'_, u32> = Scope::root();
        let class_scope = root.child(class("A", false));
        class_scope.rebind("T", Ns::Classifier, 1);
        let method = class_scope.child(ScopeKind::Block);
        method.rebind("T", Ns::Classifier, 2);
        assert_eq!(classifier(&method, "T"), Some(2));
        assert_eq!(classifier(&class_scope, "T"), Some(1));
    }

    #[test]
    fn the_file_rung_survives_the_classifier_cut() {
        // A plain nested class ends the classifier walk, but the FILE rung is the root of every
        // chain: nothing that severs the receiver chain can put a declaration outside its own file.
        let root: Scope<'_, u32> = Scope::root();
        root.rebind("FileWide", Ns::Classifier, 7);
        let a = root.child(class("A", false));
        a.rebind("T", Ns::Classifier, 1);
        let nested = a.child(class("A$B", false));

        assert_eq!(
            classifier(&nested, "T"),
            None,
            "the outer class's type parameters stop at the cut"
        );
        assert_eq!(
            classifier(&nested, "FileWide"),
            Some(7),
            "the file rung is consulted past the cut"
        );
    }

    #[test]
    fn an_enclosing_flow_fact_holds_on_nested_scope_entry_and_a_write_invalidates_it() {
        let root: Scope<'_, u32> = Scope::root();
        let function = root.child(ScopeKind::Function { receiver: None });
        function.rebind("x", Ns::Value, 1);
        function.narrow_local("x", Some(Ty::Int));

        let branch = function.child(ScopeKind::Block);
        assert_eq!(
            branch.local_narrowing("x"),
            Some(Ty::Int),
            "a nested scope starts with the enclosing execution-edge fact"
        );
        branch.narrow_local("x", Some(Ty::Long));
        drop(branch);
        assert_eq!(
            function.local_narrowing("x"),
            None,
            "a branch write invalidates the enclosing fact"
        );
    }

    #[test]
    fn ancestors_runs_innermost_first_and_terminates_at_the_root() {
        let root: Scope<'_, u32> = Scope::root();
        let a = root.child(class("A", false));
        let f = a.child(ScopeKind::Function { receiver: None });
        let b = f.child(ScopeKind::Block);
        let kinds: Vec<_> = b
            .ancestors()
            .map(|s| std::mem::discriminant(&s.kind()))
            .collect();
        assert_eq!(kinds.len(), 4);
        assert_eq!(kinds[0], std::mem::discriminant(&ScopeKind::Block));
        assert_eq!(kinds[3], std::mem::discriminant(&ScopeKind::File));
    }
}
