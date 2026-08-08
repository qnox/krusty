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
        ty: TypeName,
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
#[derive(Clone, Copy)]
pub(crate) struct PathNarrowing {
    pub(crate) ty: Ty,
}

/// Flow facts proven within one scope. Both kinds live HERE rather than on the binding they talk
/// about, which is what makes them die with the scope that proved them — in both directions, with
/// no reset step. A fact about a binding declared further out is still the inner scope's fact.
#[derive(Default)]
pub(crate) struct Flow {
    /// Property paths (`a.b`) proven non-null or `is T` by an enclosing condition.
    paths: HashMap<NarrowPath, PathNarrowing>,
    /// Straight-line READ types for a `var` assigned a non-null value (`var x: Int?; x = 10`
    /// reads as `Int`). Sound only along one statement sequence in one scope, so lookups read the
    /// CURRENT scope's frame and never walk outward.
    locals: HashMap<String, Ty>,
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

struct Binding<B> {
    name: String,
    ns: Ns,
    payload: B,
}

pub(crate) struct Scope<'p, B> {
    parent: Option<&'p Scope<'p, B>>,
    kind: ScopeKind,
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
    pub(crate) fn classifier_rungs<'s>(&'s self) -> impl Iterator<Item = &'s Scope<'p, B>> {
        let mut cut = false;
        self.ancestors().take_while(move |rung| {
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

    /// Whether THIS scope already binds `name` in `ns` — Kotlin's "conflicting declarations".
    /// Shadowing an OUTER scope is legal, so this deliberately does not walk parents.
    pub(crate) fn declared_here(&self, name: &str, ns: Ns) -> bool {
        self.bindings
            .borrow()
            .iter()
            .any(|b| b.ns == ns && b.name == name)
    }

    /// Remove and return the innermost binding of `name`, wherever in the chain it lives. Used to
    /// HIDE a binding for the duration of a nested resolution (a class property that would
    /// otherwise shadow an extension receiver); the caller re-binds it afterwards.
    pub(crate) fn take_binding(&self, name: &str, ns: Ns) -> Option<B> {
        for scope in self.ancestors() {
            let mut bindings = scope.bindings.borrow_mut();
            if let Some(at) = bindings.iter().rposition(|b| b.ns == ns && b.name == name) {
                return Some(bindings.remove(at).payload);
            }
        }
        None
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

    /// The innermost binding satisfying `pred`, with its name. Innermost-first matches Kotlin's
    /// preference for the nearest binding when resolving a context parameter by TYPE.
    pub(crate) fn find_binding(
        &self,
        ns: Ns,
        mut pred: impl FnMut(&B) -> bool,
    ) -> Option<(String, B)>
    where
        B: Clone,
    {
        for scope in self.ancestors() {
            for binding in scope.bindings.borrow().iter().rev() {
                if binding.ns == ns && pred(&binding.payload) {
                    return Some((binding.name.clone(), binding.payload.clone()));
                }
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
        self.flow
            .borrow_mut()
            .paths
            .retain(|path, _| path.root != root);
    }

    /// Record the straight-line read type of `name` for the rest of THIS scope, or with `None`
    /// drop it.
    ///
    /// Proving is scope-local, but INVALIDATION is not: an assignment inside a nested scope
    /// disproves a narrowing an enclosing scope established (`var x: Int? = 10; if (c) { x = null }`
    /// — the outer proof is dead once the branch can run), so `None` clears the whole chain.
    pub(crate) fn narrow_local(&self, name: &str, ty: Option<Ty>) {
        match ty {
            Some(ty) => {
                self.flow.borrow_mut().locals.insert(name.to_string(), ty);
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

    /// The straight-line read type proven for `name` in THIS scope. Deliberately does not walk
    /// outward: a narrowing proven before a branch does not hold inside it.
    pub(crate) fn local_narrowing(&self, name: &str) -> Option<Ty> {
        self.flow.borrow().locals.get(name).copied()
    }

    /// Every straight-line narrowing THIS scope holds, by name.
    pub(crate) fn local_narrowings(&self) -> HashMap<String, Ty> {
        self.flow.borrow().locals.clone()
    }

    /// Implicit receivers, innermost first: extension receivers and `this@Class` rungs. Stops at
    /// the first class scope that does not carry its outer instance.
    pub(crate) fn implicit_receivers(&self) -> Vec<Ty> {
        let mut out = Vec::new();
        for scope in self.ancestors() {
            match scope.kind {
                ScopeKind::Function {
                    receiver: Some(receiver),
                } => out.push(receiver),
                ScopeKind::Class {
                    ty, carries_outer, ..
                } => {
                    out.push(Ty::Obj(ty, &[]));
                    if !carries_outer {
                        break;
                    }
                }
                _ => {}
            }
        }
        out
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
            ty: type_name(name),
            carries_outer,
        }
    }

    fn obj(name: &str) -> Ty {
        Ty::Obj(type_name(name), &[])
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
    fn a_flow_fact_belongs_to_the_scope_that_proved_it() {
        let root: Scope<'_, u32> = Scope::root();
        let function = root.child(ScopeKind::Function { receiver: None });
        function.narrow_local("x", Some(Ty::Int));

        let branch = function.child(ScopeKind::Block);
        assert_eq!(
            branch.local_narrowing("x"),
            None,
            "a straight-line narrowing does not hold inside a nested scope"
        );
        branch.narrow_local("x", Some(Ty::Long));
        drop(branch);
        assert_eq!(
            function.local_narrowing("x"),
            Some(Ty::Int),
            "and a narrowing proven inside a branch does not escape it"
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
