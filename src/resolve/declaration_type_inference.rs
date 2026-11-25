use super::*;

/// Source identity for lightweight signature inference. The AST and its module file index are one
/// semantic context: alias/import edges are file-scoped, so passing either independently invites a
/// mismatched lookup and needlessly widens every inference helper's argument list.
#[derive(Clone, Copy)]
pub(super) struct InferenceSource<'a>(&'a File, u32, Option<InferenceReceiver>);

#[derive(Clone, Copy)]
enum InferenceReceiver {
    Singleton(TypeName),
    Extension { ty: Ty, declaration: Span },
}

impl<'a> InferenceSource<'a> {
    pub(super) fn file(file: &'a File, file_index: u32) -> Self {
        Self(file, file_index, None)
    }

    pub(super) fn with_implicit_classifier(self, classifier: Option<TypeName>) -> Self {
        Self(self.0, self.1, classifier.map(InferenceReceiver::Singleton))
    }

    pub(super) fn with_extension_receiver(self, ty: Ty, declaration: Span) -> Self {
        Self(
            self.0,
            self.1,
            Some(InferenceReceiver::Extension { ty, declaration }),
        )
    }
}

/// A property whose type comes from its initializer or expression getter and which the signature
/// walk could not determine, kept with the context that walk gave it.
///
/// Top-level and class-body properties share one queue and one engine on purpose. They ask the same
/// question, and keeping them in separate passes reintroduces exactly the fault being removed: one
/// pass has to run before the other, so a member reading a module property and a module property
/// reading a member cannot both be answered.
pub(super) struct DeferredProperty {
    pub(super) key: DeclKey,
    pub(super) file_index: u32,
    pub(super) name: String,
    pub(super) span: Span,
    /// The initializer, or the expression getter body for a computed property. `None` for a
    /// declaration with nothing typeable to read — a BLOCK getter, which needs an annotation — so
    /// that it declines through the one decline point and is diagnosed with the rest.
    pub(super) expression: Option<ExprId>,
    /// Set when `expression` is a DELEGATE (`val x by option(...)`). Its type is not the
    /// declaration's: the declaration's is what the delegate's `getValue` returns, resolved once
    /// the delegate itself is typed.
    pub(super) delegate_of: Option<Ty>,
    /// Set when `expression` types the property's explicit BACKING FIELD rather than the property
    /// itself (`val items: List<String> field = mutableListOf()`). The property's own type is
    /// declared; only the field's is inferred, and it is published into the storage slot.
    pub(super) backing_field: bool,
    /// The value scope the walk resolved this declaration in: context parameters for a top-level
    /// property, the enclosing class's members and lexical values for a member. Entries still
    /// `Ty::Error` are declarations that have not been typed YET and are re-asked through the
    /// engine, which is what the old passes approximated by refreshing the scope between rounds.
    pub(super) scope: Vec<(String, Ty, bool)>,
    pub(super) kind: DeferredKind,
}

/// What a deferred property is, in the two respects that change how it resolves and where its
/// answer is published.
pub(super) enum DeferredKind {
    TopLevel {
        /// Source position within its file, for the initialization-order rule.
        position: u32,
        /// Whether the type comes from an EAGER initializer rather than an expression getter. The
        /// two differ in what they may read: a getter is an executable body and may name a
        /// declaration written later, while an initializer runs in declaration order and may not —
        /// kotlinc rejects `val eager = later; val later = 1` with "variable 'later' must be
        /// initialized" while accepting the same pair split across two files.
        is_eager: bool,
        /// A context property is not reachable by its bare name — signature collection files it
        /// under `context_prop_names` rather than `props` — so it must not answer a demand for that
        /// spelling either.
        has_context_params: bool,
        /// An EXTENSION property. It is top-level in the sense that no class owns it, but it is
        /// never reachable by its bare name and it lives in `ext_props` rather than in `props` —
        /// so it neither answers a bare-name demand nor is published (or declined) through the
        /// module property tables, which belong to a plain property of the same spelling.
        has_receiver: bool,
    },
    /// The exact semantic owner, rather than the source/display name used as one symbol-table
    /// index: two files may legally declare the same simple class name in different packages, and
    /// an answer must never be published onto the other one.
    Member(TypeName),
}

impl DeferredProperty {
    fn owner(&self) -> Option<TypeName> {
        match &self.kind {
            DeferredKind::TopLevel { .. } => None,
            DeferredKind::Member(owner) => Some(*owner),
        }
    }

    /// Whether the declaration's own lexical scope binds `name`. A bound name is a class member or a
    /// context parameter, which SHADOWS any module property of the same spelling; the walk already
    /// recorded that, and the entry being a placeholder does not undo it.
    fn scope_binds(&self, name: &str) -> bool {
        self.scope.iter().any(|(bound, _, _)| bound == name)
    }
}

/// Which top-level property spellings a declaration at a given position may NOT read, because they
/// are declared later in its own file.
///
/// Only same-FILE order is a restriction: kotlinc accepts the identical forward reference when the
/// two declarations live in different files (measured on 2.4.10), so this cannot be a module-wide
/// position comparison.
#[derive(Default)]
struct SourceOrderIndex(HashMap<(u32, String), u32>);
/// The one wording for an unannotated property whose type could not be inferred. Emitted by the
/// checker (not the signature passes) so an initializer or getter that diagnoses itself
/// (`val LOG = logger<Foo>()`) does not also draw this error on top of the unresolved reference.
pub(super) fn cannot_infer_property_message(name: &str) -> String {
    format!("krusty: cannot infer the type of property '{name}'; add an explicit type")
}

pub(super) const RECURSIVE_INFERENCE_MESSAGE: &str = "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.";
pub(super) const EXPLICIT_PROPERTY_TYPE_MESSAGE: &str =
    "this property must have an explicit type, be initialized, or be delegated.";

impl SourceOrderIndex {
    fn of(files: &[File]) -> Self {
        let mut positions = HashMap::default();
        for (file_index, file) in files.iter().enumerate() {
            for &declaration in &file.decls {
                let Decl::Property(property) = file.decl(declaration) else {
                    continue;
                };
                if property.receiver.is_some() {
                    continue;
                }
                positions
                    .entry((file_index as u32, property.name.clone()))
                    .or_insert(property.span.lo);
            }
        }
        Self(positions)
    }

    fn declared_later(&self, file: u32, position: u32, name: &str) -> bool {
        self.0
            .get(&(file, name.to_string()))
            .is_some_and(|declared| *declared > position)
    }
}

/// Resolves deferred property types on demand, memoised, with cycles declining.
///
/// Borrows the symbol table, so it is built where it is used rather than kept alive across the
/// walk; the memo it consults ([`TypeEngine`]) borrows nothing and therefore does outlive it.
struct DeferredInferenceDriver<'a> {
    files: &'a [File],
    table: &'a SymbolTable,
    engine: &'a TypeEngine,
    deferred: &'a HashMap<DeclKey, &'a DeferredProperty>,
    /// Module-wide index from a top-level property's source spelling to its declaration. A name
    /// declared by more than one deferred property is absent: two declarations of one name are
    /// already a conflict diagnostic, and guessing between them would type a reference from
    /// whichever happened to be indexed last.
    ///
    /// The index is by SIMPLE NAME across the module, matching `SymbolTable::props`, which is what
    /// this hook stands in for while a declaration's own type is still being determined. Import
    /// scoping for module properties is a property of that table, not of this index.
    by_name: &'a HashMap<String, DeclKey>,
    /// The same index for class members, keyed by the owner's semantic identity so that two classes
    /// sharing a simple name in different packages cannot answer for each other.
    by_member: &'a HashMap<(TypeName, String), DeclKey>,
    /// Top-level functions whose return is not determined, by source spelling. A call to one of
    /// them cannot be answered from the table, so the function is resolved on demand. An OVERLOADED
    /// spelling is absent — a name alone cannot choose between overloads.
    functions: &'a HashMap<String, (u32, DeclId)>,
    /// Every one of them, overloads included, by spelling. A call site knows its argument types and
    /// can choose; this is the index it chooses from.
    functions_by_name: &'a HashMap<String, Vec<(u32, DeclId)>>,
    /// The same for class members: every undetermined method by declaring class and spelling,
    /// overloads included.
    methods_by_name: &'a HashMap<(TypeName, String), Vec<UndeterminedMethod>>,
    /// Member FUNCTIONS whose expression body the walk could not type, keyed by the class that
    /// declares them. A declaration reading `C().gen()` needs the return resolved the same way a
    /// property read does.
    methods: &'a HashMap<(TypeName, String), (u32, DeclId, u32)>,
    source_order: &'a SourceOrderIndex,
    rejected_reads: &'a std::cell::RefCell<HashMap<DeclKey, Vec<(Span, String)>>>,
    recursive_dependents: &'a std::cell::RefCell<crate::fx_hash::HashSet<DeclKey>>,
}

impl DeferredInferenceDriver<'_> {
    fn resolve(&self, key: DeclKey) -> Resolution {
        let Some(entry) = self.deferred.get(&key).copied() else {
            return Resolution::Declined(DeclineReason::Untypeable);
        };
        self.engine.resolve(key, || {
            let expression = entry.expression?;
            let file_index = entry.file_index as usize;
            let file = &self.files[file_index];
            let demand = |name: &str| self.demand(entry, name);
            let demand_call = |name: &str, arg_tys: &[Ty]| self.demand_call_overload(name, arg_tys);
            let demand_member = |receiver: Ty, name: &str, arg_tys: &[Ty]| {
                self.demand_on_receiver(Some(entry), receiver, name, arg_tys)
            };
            let rejects = |name: &str, span: Span| {
                let rejected = !self.readable_from(entry, name);
                if rejected {
                    let read = (span, name.to_string());
                    let mut rejected_reads = self.rejected_reads.borrow_mut();
                    let reads = rejected_reads.entry(entry.key).or_default();
                    if !reads.contains(&read) {
                        reads.push(read);
                    }
                }
                rejected
            };
            let inferred = match entry.kind {
                DeferredKind::TopLevel { .. } => infer_declaration_ty(
                    file,
                    entry.file_index,
                    DeclId(entry.key.decl),
                    expression,
                    self.table,
                    EngineSeams {
                        demand: &demand,
                        demand_call: &demand_call,
                        demand_member: &demand_member,
                        rejects: &rejects,
                    },
                ),
                DeferredKind::Member(_) if file.is_local_declaration(DeclId(entry.key.decl)) => {
                    infer_local_class_member_ty(
                        file,
                        entry.file_index,
                        entry.span,
                        expression,
                        self.table,
                    )
                }
                DeferredKind::Member(_) => infer_class_member_ty(
                    file,
                    entry.file_index,
                    DeclId(entry.key.decl),
                    expression,
                    &entry.scope,
                    self.table,
                    EngineSeams {
                        demand: &demand,
                        demand_call: &demand_call,
                        demand_member: &demand_member,
                        rejects: &rejects,
                    },
                ),
            };
            // A delegate's own type is not the declaration's: the declaration reads through
            // `getValue`, so resolve that once the delegate itself is typed.
            let inferred = match entry.delegate_of {
                Some(this_ref) if inferred != Ty::Error => {
                    // `provideDelegate` may itself have an inferred return, and its result is the
                    // type `getValue` is then looked up on — so it has to be resolved BEFORE the
                    // chain is followed, not patched afterwards.
                    let delegate = self
                        .demand_on_receiver(None, inferred, "provideDelegate", &[])
                        .filter(|provided| !provided.mentions_pending())
                        .map(|provided| {
                            let bindings = inferred
                                .kotlin_class_internal()
                                .and_then(|owner| self.table.class_by_type_name(owner))
                                .map(|class| class.type_parameter_bindings(inferred))
                                .unwrap_or_default()
                                .into_iter()
                                .collect();
                            crate::symbol_resolver::ty_subst_keep_unbound(provided, &bindings)
                        })
                        .unwrap_or(inferred);
                    delegated_getvalue_ret_for_signature(
                        file,
                        self.table,
                        self.table.libraries.as_ref(),
                        delegate,
                        this_ref,
                    )
                    // `getValue`'s own return may still be being determined — `val O by W("OK")` on
                    // `class W<T>(val value: T) { operator fun getValue(…) = value }`. Ask the engine for
                    // that declaration and substitute it in, the same way a call or a reference does;
                    // publishing the marker would make "not resolved" the property's type.
                    .map(|ret| match ret.mentions_pending() {
                        true => self
                            .demand_on_receiver(None, delegate, "getValue", &[])
                            .filter(|answered| !answered.mentions_pending())
                            // The answer is `getValue`'s DECLARED return — `T` for
                            // `class W<T> { operator fun getValue(…) = value }` — so the delegate's own
                            // type arguments are what make it this property's type.
                            .map(|answered| {
                                let bindings = delegate
                                    .kotlin_class_internal()
                                    .and_then(|owner| self.table.class_by_type_name(owner))
                                    .map(|class| class.type_parameter_bindings(delegate))
                                    .unwrap_or_default()
                                    .into_iter()
                                    .collect();
                                crate::symbol_resolver::ty_subst_keep_unbound(answered, &bindings)
                            })
                            .map_or(ret, |answered| {
                                crate::types::ty_replace_pending(ret, answered)
                            }),
                        false => ret,
                    })
                    .unwrap_or(Ty::Error)
                }
                Some(_) => Ty::Error,
                None => inferred,
            };
            let inferred = inferred_declaration_ty(inferred);
            (inferred != Ty::Error).then_some(inferred)
        })
    }

    /// The type of another declaration this one names, resolving it first if needed.
    ///
    /// A name the declaration's own scope BINDS is answered only from that scope's declarations. It
    /// shadows any module property of the same spelling, and falling through to the module when the
    /// owner cannot answer would type the reference from an unrelated declaration — for
    /// `open class Base { val a = … }` / `class Derived : Base() { val b = a }` alongside a
    /// top-level `val a`, that types `Derived.b` from the top-level one, which is a wrong field
    /// descriptor rather than a diagnostic. An inherited member is not in `by_member`, which is
    /// keyed by the DECLARING owner, so this declines exactly where the source needs the supertype
    /// walk that signature collection has not done yet.
    fn demand(&self, from: &DeferredProperty, name: &str) -> Option<Ty> {
        self.demand_declared(from, name)
            .or_else(|| self.module_property(from, name))
    }

    /// A module property that already HAS a type. Answered through this hook rather than by copying
    /// the module's property table into every declaration's value scope: that copy is per
    /// declaration and the table grows with the module, so it is quadratic in the number of
    /// declarations being resolved — 2400 forward-referencing properties spent 25s almost entirely
    /// cloning scope entries. Name resolution here is first-match over the declaration's OWN scope
    /// and then this, which is the order the copy produced.
    fn module_property(&self, from: &DeferredProperty, name: &str) -> Option<Ty> {
        if !self.readable_from(from, name) {
            return None;
        }
        self.table
            .props
            .get(name)
            .map(|(ty, _, _)| *ty)
            .filter(|ty| *ty != Ty::Pending && *ty != Ty::Error)
    }

    /// The declaration `name` refers to, resolved now if it has not been typed yet.
    fn demand_declared(&self, from: &DeferredProperty, name: &str) -> Option<Ty> {
        if let Some(owner) = from.owner() {
            if let Some(ty) = self.demand_member(Some(from), owner, name) {
                return Some(ty);
            }
            // The class — or something it inherits from — declares this spelling, so it SHADOWS any
            // module property of the same name. Falling through would type the reference from an
            // unrelated declaration: for `open class Base { val a = … }` and
            // `class Derived : Base() { val b = a }` alongside a top-level `val a`, that types
            // `Derived.b` from the top-level one, which is a wrong field descriptor rather than a
            // diagnostic. Declining is the recoverable answer.
            if self.class_declares(owner, name) {
                return None;
            }
        }
        if from.scope_binds(name) {
            return None;
        }
        self.by_name
            .get(name)
            .and_then(|key| self.resolution_ty_for(Some(from), self.resolve(*key)))
            .or_else(|| self.demand_function(name))
    }

    fn resolution_ty_for(
        &self,
        from: Option<&DeferredProperty>,
        resolution: Resolution,
    ) -> Option<Ty> {
        if resolution.declined() == Some(DeclineReason::Recursive) {
            if let Some(from) = from {
                self.recursive_dependents.borrow_mut().insert(from.key);
            }
        }
        resolution.ty()
    }

    /// The return of a top-level function whose own body has not been typed yet.
    ///
    /// A property initialized by a call to such a function cannot be answered from the table: the
    /// walk records the return as undetermined, so the function is resolved on demand exactly as a
    /// property is, and memoised the same way.
    fn demand_function(&self, name: &str) -> Option<Ty> {
        let (file_index, declaration) = *self.functions.get(name)?;
        self.engine
            .resolve(DeclKey::declaration(file_index, declaration.0), || {
                infer_function_return_ty(
                    &self.files[file_index as usize],
                    file_index,
                    declaration,
                    self.table,
                    EngineSeams {
                        demand: &|name: &str| self.demand_name_only(name),
                        demand_call: &|name: &str, arg_tys: &[Ty]| {
                            self.demand_call_overload(name, arg_tys)
                        },
                        demand_member: &|receiver: Ty, name: &str, arg_tys: &[Ty]| {
                            self.demand_on_receiver(None, receiver, name, arg_tys)
                        },
                        rejects: &|_, _| false,
                    },
                )
            })
            .ty()
    }

    /// A module function by spelling AND the call's argument types.
    ///
    /// The by-spelling index drops an overloaded name because a name alone cannot choose between
    /// overloads. A CALL can: its arguments are already typed. Applicability is decided the way the
    /// table decides it — parameter count, then each argument against the declared parameter — and
    /// an answer is given only when exactly one candidate survives, so a genuinely ambiguous call
    /// still declines rather than picking one.
    fn demand_call_overload(&self, name: &str, arg_tys: &[Ty]) -> Option<Ty> {
        if let Some(ty) = self.demand_name_only(name) {
            return Some(ty);
        }
        let candidates = self.functions_by_name.get(name)?;
        if candidates.len() < 2 {
            return None;
        }
        let applicable = candidates
            .iter()
            .filter(|(file_index, declaration)| {
                !self
                    .engine
                    .is_computing(DeclKey::declaration(*file_index, declaration.0))
            })
            .filter(|(file_index, declaration)| {
                let Some(Decl::Fun(function)) = self
                    .files
                    .get(*file_index as usize)
                    .map(|file| file.decl(*declaration))
                else {
                    return false;
                };
                function.receiver.is_none()
                    && function.params.len() == arg_tys.len()
                    && self
                        .signature_of(*file_index, *declaration)
                        .is_some_and(|params| {
                            params.iter().zip(arg_tys).all(|(parameter, argument)| {
                                *argument == Ty::Error
                                    || erased_type_key(*parameter) == erased_type_key(*argument)
                            })
                        })
            })
            .collect::<Vec<_>>();
        let [(file_index, declaration)] = applicable.as_slice() else {
            return None;
        };
        self.engine
            .resolve(DeclKey::declaration(*file_index, declaration.0), || {
                infer_function_return_ty(
                    &self.files[*file_index as usize],
                    *file_index,
                    *declaration,
                    self.table,
                    EngineSeams {
                        demand: &|name: &str| self.demand_name_only(name),
                        demand_call: &|name: &str, arg_tys: &[Ty]| {
                            self.demand_call_overload(name, arg_tys)
                        },
                        demand_member: &|receiver: Ty, name: &str, arg_tys: &[Ty]| {
                            self.demand_on_receiver(None, receiver, name, arg_tys)
                        },
                        rejects: &|_, _| false,
                    },
                )
            })
            .ty()
    }

    /// The collected parameter types of the function declared at `(file, declaration)`.
    fn signature_of(&self, file_index: u32, declaration: DeclId) -> Option<Vec<Ty>> {
        self.table
            .funs
            .values()
            .flatten()
            .find(|signature| {
                signature.source_file == Some(file_index)
                    && signature.source_decl == Some(declaration)
            })
            .map(|signature| signature.params.clone())
    }

    /// A module declaration by spelling, without a requesting declaration's ordering rules.
    fn demand_name_only(&self, name: &str) -> Option<Ty> {
        self.by_name
            .get(name)
            .and_then(|key| self.resolve(*key).ty())
            .or_else(|| self.demand_function(name))
    }

    /// The type of the member `owner` sees under `name`, resolving that declaration first if needed.
    ///
    /// This is the seam for every read that goes THROUGH a receiver — `other.a`, `this.a`, an
    /// implicit companion receiver. Those do not reach the bare-name hook: they resolve against the
    /// symbol table's member records, which still hold the placeholder while the member's own type
    /// is being determined, and a placeholder read as an answer declines a program kotlinc accepts.
    ///
    /// The index is keyed by the DECLARING owner, so an inherited member is found by walking the
    /// supertypes rather than by a second index that could disagree with the class graph.
    fn demand_member(
        &self,
        from: Option<&DeferredProperty>,
        owner: TypeName,
        name: &str,
    ) -> Option<Ty> {
        let key = *self
            .owner_chain(owner)
            .into_iter()
            .find_map(|declaring| self.by_member.get(&(declaring, name.to_string())))?;
        self.resolution_ty_for(from, self.resolve(key))
    }

    /// Whether `owner` or any of its supertypes declares a property spelled `name`, whatever its
    /// type is so far.
    fn class_declares(&self, owner: TypeName, name: &str) -> bool {
        self.owner_chain(owner).into_iter().any(|declaring| {
            self.table
                .class_by_type_name(declaring)
                .is_some_and(|signature| signature.declared_props.contains_key(name))
        })
    }

    /// Every classifier whose members `owner`'s body can read by bare name, nearest first: itself,
    /// its source superclasses, the COMPANION of each, and the same again for each lexically
    /// enclosing class.
    ///
    /// A companion's members are readable unqualified from the class body and from a nested class
    /// (`companion object { val token = … }` read as `token`), so a chain of superclasses alone
    /// misses them. Interfaces are not walked: a property they declare is abstract and has no
    /// initializer to resolve. Bounded by the number of classes so a malformed cyclic hierarchy
    /// cannot spin here.
    fn owner_chain(&self, owner: TypeName) -> Vec<TypeName> {
        let mut chain: Vec<TypeName> = Vec::new();
        let mut lexical = Some(owner);
        let bound = self.table.classes.len() + 1;
        for _ in 0..bound {
            let Some(start) = lexical else { break };
            let mut current = Some(start);
            for _ in 0..bound {
                let Some(class) = current else { break };
                if chain.contains(&class) {
                    break;
                }
                chain.push(class);
                let signature = self.table.class_by_type_name(class);
                if let Some(companion) =
                    signature.and_then(|signature| signature.companion_internal)
                {
                    if !chain.contains(&companion) {
                        chain.push(companion);
                    }
                }
                current = signature.and_then(|signature| signature.super_internal);
            }
            lexical = self
                .table
                .class_by_type_name(start)
                .and_then(|signature| signature.inner_of);
        }
        chain
    }

    /// [`Self::demand_member`] addressed by the receiver's semantic type rather than its identity,
    /// which is the form an expression read has to hand.
    fn demand_on_receiver(
        &self,
        from: Option<&DeferredProperty>,
        receiver: Ty,
        name: &str,
        arg_tys: &[Ty],
    ) -> Option<Ty> {
        let owner = receiver.kotlin_class_internal()?;
        self.demand_member(from, owner, name)
            .or_else(|| self.demand_method(owner, name))
            .or_else(|| self.demand_method_overload(owner, name, arg_tys))
            .or_else(|| self.demand_module_extension(receiver, name))
    }

    /// A member FUNCTION by spelling AND the call's argument types.
    ///
    /// [`Self::demand_method`] is keyed by spelling, which cannot name one of several overloads, so
    /// an overloaded member is absent from that index — `private fun foo() = foo(1)` beside
    /// `private fun foo(i: Int) = "O"` left BOTH undetermined and settling to `Unit`. A call knows
    /// its argument types; applicability is decided on them, and an answer is given only when
    /// exactly one candidate survives.
    fn demand_method_overload(&self, owner: TypeName, name: &str, arg_tys: &[Ty]) -> Option<Ty> {
        let applicable = self
            .owner_chain(owner)
            .into_iter()
            .flat_map(|declaring| {
                self.methods_by_name
                    .get(&(declaring, name.to_string()))
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            })
            .filter(|method| {
                !self.engine.is_computing(DeclKey::method(
                    method.file_index,
                    method.declaration.0,
                    method.method_index,
                ))
            })
            .filter(|method| {
                self.table
                    .class_by_type_name(method.owner)
                    .and_then(|class| class.methods.get(&method.name))
                    .and_then(|overloads| overloads.get(method.overload_index))
                    .is_some_and(|signature| {
                        signature.params.len() == arg_tys.len()
                            && signature.params.iter().zip(arg_tys).all(|(p, a)| {
                                *a == Ty::Error || erased_type_key(*p) == erased_type_key(*a)
                            })
                    })
            })
            .collect::<Vec<_>>();
        let [method] = applicable.as_slice() else {
            return None;
        };
        self.engine
            .resolve(
                DeclKey::method(method.file_index, method.declaration.0, method.method_index),
                || {
                    Some(infer_method_return_ty(
                        &self.files[method.file_index as usize],
                        method.file_index,
                        method.declaration,
                        method.method_index,
                        self.table,
                        EngineSeams {
                            demand: &|name: &str| self.demand_name_only(name),
                            demand_call: &|name: &str, arg_tys: &[Ty]| {
                                self.demand_call_overload(name, arg_tys)
                            },
                            demand_member: &|receiver: Ty, name: &str, arg_tys: &[Ty]| {
                                self.demand_on_receiver(None, receiver, name, arg_tys)
                            },
                            rejects: &|_, _| false,
                        },
                    ))
                },
            )
            .ty()
    }

    /// A module-level EXTENSION reached through a receiver — `operator fun Int.getValue(…) = …`
    /// answering `val x by …`.
    ///
    /// The read goes through a receiver, so it arrives here, but the declaration is top-level: no
    /// owner chain contains it. The RECEIVER still has to match: answering on spelling alone would
    /// let `fun String.tag() = 1.5` type a `tag()` call on an `Int` — a wrong type published with
    /// no diagnostic anywhere. The extension table is keyed by erased receiver exactly as ordinary
    /// extension lookup keys it, so the same key decides this.
    fn demand_module_extension(&self, receiver: Ty, name: &str) -> Option<Ty> {
        let (file_index, declaration) = *self.functions.get(name)?;
        let declared_here = |signature: &&Signature| {
            signature.source_file == Some(file_index) && signature.source_decl == Some(declaration)
        };
        let families = self.table.ext_funs.get(name)?;
        receiver
            .erased_recv_candidates()
            .into_iter()
            .filter_map(|key| families.get(&key))
            .flatten()
            .find(declared_here)?;
        self.demand_function(name)
    }

    /// A member FUNCTION's inferred return, resolved on demand and memoised like every other
    /// declaration. This is what the return pre-inference passes did by sweeping the module to a
    /// fixpoint; asking for the one declaration that is needed replaces the sweep.
    fn demand_method(&self, owner: TypeName, name: &str) -> Option<Ty> {
        // A Java getter read as a Kotlin SYNTHETIC PROPERTY asks for `entries` while the declaration
        // is spelled `getEntries` — the same mapping every other reader of these members applies. A
        // demand that only tried the written spelling left the read undetermined, and the function
        // that owned it settled to `Unit`.
        let accessor = {
            let mut characters = name.chars();
            characters
                .next()
                .map(|first| format!("get{}{}", first.to_uppercase(), characters.as_str()))
        };
        let spellings = [Some(name.to_string()), accessor].into_iter().flatten();
        let (file_index, declaration, method_index) = spellings
            .flat_map(|spelling| {
                self.owner_chain(owner)
                    .into_iter()
                    .filter_map(move |declaring| self.methods.get(&(declaring, spelling.clone())))
                    .collect::<Vec<_>>()
            })
            .next()
            .copied()?;
        self.engine
            .resolve(
                DeclKey::method(file_index, declaration.0, method_index),
                || {
                    Some(infer_method_return_ty(
                        &self.files[file_index as usize],
                        file_index,
                        declaration,
                        method_index,
                        self.table,
                        EngineSeams {
                            demand: &|name: &str| self.demand_name_only(name),
                            demand_call: &|name: &str, arg_tys: &[Ty]| {
                                self.demand_call_overload(name, arg_tys)
                            },
                            demand_member: &|receiver: Ty, name: &str, arg_tys: &[Ty]| {
                                self.demand_on_receiver(None, receiver, name, arg_tys)
                            },
                            rejects: &|_, _| false,
                        },
                    ))
                },
            )
            .ty()
    }

    /// Whether `from`'s body may read the module property spelled `name` at all. An eager
    /// initializer runs in declaration order, so a declaration written LATER IN ITS OWN FILE has no
    /// value yet and must not type it; an expression getter is executable and may.
    fn readable_from(&self, from: &DeferredProperty, name: &str) -> bool {
        match from.kind {
            DeferredKind::TopLevel {
                position,
                is_eager: true,
                ..
            } => !self
                .source_order
                .declared_later(from.file_index, position, name),
            _ => true,
        }
    }
}

/// Type the properties the signature walk left undetermined, then publish and diagnose.
///
/// The walk visits files in ARGUMENT order, so a property initialized from a declaration in a later
/// file could not be typed while the walk ran; a getter, being an executable body, may legally read
/// a later declaration even within one file; and a class member may be waiting on either. All of
/// these are the same question — what is this declaration's type — and all are answered here by the
/// engine, which resolves each declaration when it is first asked for and remembers the answer.
/// That is what makes `krusty A.kt B.kt` and `krusty B.kt A.kt` agree, and what makes a dependency
/// loop decline rather than recurse.
fn declaration_is_body_local(file: &File, declaration: DeclId) -> bool {
    file.is_local_declaration(declaration)
        || file
            .anonymous_object_classes
            .values()
            .any(|candidate| *candidate == declaration)
}

pub(super) fn resolve_deferred_properties(
    files: &[File],
    table: &mut SymbolTable,
    deferred: &[DeferredProperty],
    resolve_local_declarations: bool,
    diags: &mut DiagSink,
) {
    // No early return on an empty deferred queue: this is also where every undetermined RETURN and
    // member-extension getter is resolved, and a module can have those with no deferred property at
    // all.
    let by_key = deferred
        .iter()
        .filter(|entry| {
            resolve_local_declarations
                || !declaration_is_body_local(
                    &files[entry.file_index as usize],
                    DeclId(entry.key.decl),
                )
        })
        .map(|entry| (entry.key, entry))
        .collect::<HashMap<_, _>>();
    let mut by_name: HashMap<String, DeclKey> = HashMap::default();
    let mut ambiguous: crate::fx_hash::HashSet<String> = crate::fx_hash::HashSet::default();
    let mut by_member: HashMap<(TypeName, String), DeclKey> = HashMap::default();
    let mut ambiguous_members: crate::fx_hash::HashSet<(TypeName, String)> =
        crate::fx_hash::HashSet::default();
    for entry in deferred {
        if !resolve_local_declarations
            && declaration_is_body_local(&files[entry.file_index as usize], DeclId(entry.key.decl))
        {
            continue;
        }
        match &entry.kind {
            DeferredKind::TopLevel {
                has_context_params: true,
                ..
            }
            | DeferredKind::TopLevel {
                has_receiver: true, ..
            } => {}
            DeferredKind::TopLevel { .. } => {
                if by_name
                    .insert(entry.name.clone(), entry.key)
                    .is_some_and(|previous| previous != entry.key)
                {
                    ambiguous.insert(entry.name.clone());
                }
            }
            DeferredKind::Member(owner) => {
                // Two entries for the SAME declaration are not two declarations: a `var` with an
                // inferred type queues its property and its backing field under one key. Only a
                // genuine second declaration of the spelling makes the name unanswerable.
                if by_member
                    .insert((*owner, entry.name.clone()), entry.key)
                    .is_some_and(|previous| previous != entry.key)
                {
                    ambiguous_members.insert((*owner, entry.name.clone()));
                }
            }
        }
    }
    // A spelling declared twice is already a conflict diagnostic; answering a demand for it from
    // whichever declaration was indexed last would type the reference from an arbitrary one.
    for name in &ambiguous {
        by_name.remove(name);
    }
    for key in &ambiguous_members {
        by_member.remove(key);
    }
    // Functions whose expression body the walk could not type. Their return reads as undetermined,
    // and a property initialized by a call to one needs it resolved first.
    let mut undetermined_functions: HashMap<String, (u32, DeclId)> = HashMap::default();
    let mut undetermined_function_decls: Vec<(u32, DeclId)> = Vec::new();
    let mut undetermined_functions_by_name: HashMap<String, Vec<(u32, DeclId)>> =
        HashMap::default();
    let mut ambiguous_functions: crate::fx_hash::HashSet<String> =
        crate::fx_hash::HashSet::default();
    for (file_index, file) in files.iter().enumerate() {
        for &declaration in &file.decls {
            let Decl::Fun(function) = file.decl(declaration) else {
                continue;
            };
            // Extensions are indexed too: their spelling is the key, and a spelling declared more
            // than once is dropped below exactly as a plain function's is.
            if !function_needs_return_preinfer(function) {
                continue;
            }
            // As for members: the DECLARATION list carries every one, overloads included, because
            // publishing an answer never has to choose between them.
            undetermined_function_decls.push((file_index as u32, declaration));
            undetermined_functions_by_name
                .entry(function.name.clone())
                .or_default()
                .push((file_index as u32, declaration));
            if undetermined_functions
                .insert(function.name.clone(), (file_index as u32, declaration))
                .is_some()
            {
                ambiguous_functions.insert(function.name.clone());
            }
        }
    }
    // An overloaded spelling cannot be answered by name alone.
    for name in &ambiguous_functions {
        undetermined_functions.remove(name);
    }
    // The same for MEMBER functions, keyed by the declaring class. The key comes from the table's
    // own record of which declaration produced each classifier, so a nested or same-named class
    // cannot be confused with another file's.
    let mut undetermined_methods: HashMap<(TypeName, String), (u32, DeclId, u32)> =
        HashMap::default();
    let mut undetermined_method_decls: Vec<UndeterminedMethod> = Vec::new();
    let mut undetermined_methods_by_name: HashMap<(TypeName, String), Vec<UndeterminedMethod>> =
        HashMap::default();
    let mut ambiguous_methods: crate::fx_hash::HashSet<(TypeName, String)> =
        crate::fx_hash::HashSet::default();
    for (internal, signature) in &table.classes {
        let Some(declaration) = signature.source_decl else {
            continue;
        };
        let file_index = signature.source_file;
        let Some(file) = files.get(file_index as usize) else {
            continue;
        };
        let Decl::Class(class) = file.decl(declaration) else {
            continue;
        };
        if !resolve_local_declarations && declaration_is_body_local(file, declaration) {
            continue;
        }
        for (method_index, method) in class.methods.iter().enumerate() {
            if !function_needs_return_preinfer(method) {
                continue;
            }
            // The DECLARATION list carries every one, overloads included: publishing does not go
            // through a spelling, it knows which declaration it asked about. Its position among the
            // same-spelled overloads of the same kind is what selects the signature to write, the
            // way the lowerer selects one.
            let overload_index = class.methods[..method_index]
                .iter()
                .filter(|previous| {
                    previous.name == method.name
                        && previous.receiver.is_some() == method.receiver.is_some()
                })
                .count();
            undetermined_methods_by_name
                .entry((*internal, method.name.clone()))
                .or_default()
                .push(UndeterminedMethod {
                    owner: *internal,
                    name: method.name.clone(),
                    is_extension: method.receiver.is_some(),
                    overload_index,
                    file_index,
                    declaration,
                    method_index: method_index as u32,
                });
            undetermined_method_decls.push(UndeterminedMethod {
                owner: *internal,
                name: method.name.clone(),
                is_extension: method.receiver.is_some(),
                overload_index,
                file_index,
                declaration,
                method_index: method_index as u32,
            });
            let key = (*internal, method.name.clone());
            if undetermined_methods
                .insert(key.clone(), (file_index, declaration, method_index as u32))
                .is_some()
            {
                ambiguous_methods.insert(key);
            }
        }
    }
    // The by-SPELLING index drops an overloaded name: a demand that arrives with a name alone
    // cannot choose between them. The declaration list above keeps them.
    for key in &ambiguous_methods {
        undetermined_methods.remove(key);
    }
    // Member-extension PROPERTIES, keyed by the declaring class and the property's position among
    // the extensions of that spelling — the same index the publish target uses.
    let mut undetermined_member_ext_props: Vec<(TypeName, String, usize, u32, DeclId, u32)> =
        Vec::new();
    for (internal, signature) in &table.classes {
        let Some(declaration) = signature.source_decl else {
            continue;
        };
        let file_index = signature.source_file;
        let Some(file) = files.get(file_index as usize) else {
            continue;
        };
        let Decl::Class(class) = file.decl(declaration) else {
            continue;
        };
        if !resolve_local_declarations && declaration_is_body_local(file, declaration) {
            continue;
        }
        for (property_index, property) in class.body_props.iter().enumerate() {
            if member_extension_property_preinfer_body(property).is_none() {
                continue;
            }
            let extension_index = class.body_props[..property_index]
                .iter()
                .filter(|previous| previous.receiver.is_some() && previous.name == property.name)
                .count();
            undetermined_member_ext_props.push((
                *internal,
                property.name.clone(),
                extension_index,
                file_index,
                declaration,
                property_index as u32,
            ));
        }
    }
    let engine = TypeEngine::new();
    let source_order = SourceOrderIndex::of(files);
    let rejected_reads = std::cell::RefCell::new(HashMap::default());
    let recursive_dependents = std::cell::RefCell::new(crate::fx_hash::HashSet::default());
    let resolutions = {
        let driver = DeferredInferenceDriver {
            files,
            table,
            engine: &engine,
            deferred: &by_key,
            by_name: &by_name,
            by_member: &by_member,
            functions: &undetermined_functions,
            functions_by_name: &undetermined_functions_by_name,
            methods_by_name: &undetermined_methods_by_name,
            methods: &undetermined_methods,
            source_order: &source_order,
            rejected_reads: &rejected_reads,
            recursive_dependents: &recursive_dependents,
        };
        deferred
            .iter()
            .filter(|entry| {
                resolve_local_declarations
                    || !declaration_is_body_local(
                        &files[entry.file_index as usize],
                        DeclId(entry.key.decl),
                    )
            })
            .map(|entry| (entry, driver.resolve(entry.key)))
            .collect::<Vec<_>>()
    };
    let (engine_function_returns, engine_returns, engine_member_ext_props) = {
        let driver = DeferredInferenceDriver {
            files,
            table,
            engine: &engine,
            deferred: &by_key,
            by_name: &by_name,
            by_member: &by_member,
            functions: &undetermined_functions,
            functions_by_name: &undetermined_functions_by_name,
            methods_by_name: &undetermined_methods_by_name,
            methods: &undetermined_methods,
            source_order: &source_order,
            rejected_reads: &rejected_reads,
            recursive_dependents: &recursive_dependents,
        };
        // Asked per DECLARATION, so an overloaded spelling is answered too. Asking by name drops
        // both overloads — a name alone cannot choose between them — and they then settled to
        // `Unit`, which is how `fun f(n: Int) = n` and `fun f(s: String) = s` ended up sharing one
        // return.
        let function_returns = undetermined_function_decls
            .iter()
            .filter_map(|&(file_index, declaration)| {
                let ty = engine
                    .resolve(DeclKey::declaration(file_index, declaration.0), || {
                        infer_function_return_ty(
                            &files[file_index as usize],
                            file_index,
                            declaration,
                            table,
                            EngineSeams {
                                demand: &|name: &str| driver.demand_name_only(name),
                                demand_call: &|name: &str, arg_tys: &[Ty]| {
                                    driver.demand_call_overload(name, arg_tys)
                                },
                                demand_member: &|receiver: Ty, name: &str, arg_tys: &[Ty]| {
                                    driver.demand_on_receiver(None, receiver, name, arg_tys)
                                },
                                rejects: &|_, _| false,
                            },
                        )
                    })
                    .ty()?;
                Some((file_index, declaration, ty))
            })
            .collect::<Vec<_>>();
        // Every inferred-return member, not only the ones still marked pending: the sweep can
        // publish a WRONG answer rather than no answer — `Unit`, computed from a call it could not
        // yet resolve — and a wrong answer is exactly what the engine exists to replace. Asked per
        // DECLARATION, so an overloaded spelling is answered too; asking by name would drop both
        // overloads and settle them to `Unit`, which made the answer depend on the order the files
        // were passed in.
        let method_returns = undetermined_method_decls
            .iter()
            .filter_map(|method| {
                let ty = engine
                    .resolve(
                        DeclKey::method(
                            method.file_index,
                            method.declaration.0,
                            method.method_index,
                        ),
                        || {
                            Some(infer_method_return_ty(
                                &files[method.file_index as usize],
                                method.file_index,
                                method.declaration,
                                method.method_index,
                                table,
                                EngineSeams {
                                    demand: &|name: &str| driver.demand_name_only(name),
                                    demand_call: &|name: &str, arg_tys: &[Ty]| {
                                        driver.demand_call_overload(name, arg_tys)
                                    },
                                    demand_member: &|receiver: Ty, name: &str, arg_tys: &[Ty]| {
                                        driver.demand_on_receiver(None, receiver, name, arg_tys)
                                    },
                                    rejects: &|_, _| false,
                                },
                            ))
                        },
                    )
                    .ty()?;
                Some((method, ty))
            })
            .collect::<Vec<_>>();
        let member_ext_property_types = undetermined_member_ext_props
            .iter()
            .filter_map(
                |(owner, name, extension_index, file_index, declaration, property_index)| {
                    let ty = engine
                        .resolve(
                            DeclKey::member(*file_index, declaration.0, *property_index),
                            || {
                                Some(infer_member_ext_property_ty(
                                    &files[*file_index as usize],
                                    *file_index,
                                    *declaration,
                                    *property_index,
                                    table,
                                    EngineSeams {
                                        demand: &|name: &str| driver.demand_name_only(name),
                                        demand_call: &|name: &str, arg_tys: &[Ty]| {
                                            driver.demand_call_overload(name, arg_tys)
                                        },
                                        demand_member:
                                            &|receiver: Ty, name: &str, arg_tys: &[Ty]| {
                                                driver.demand_on_receiver(
                                                    None, receiver, name, arg_tys,
                                                )
                                            },
                                        rejects: &|_, _| false,
                                    },
                                ))
                            },
                        )
                        .ty()?;
                    Some((*owner, name.clone(), *extension_index, ty))
                },
            )
            .collect::<Vec<_>>();
        (function_returns, method_returns, member_ext_property_types)
    };
    for (owner, name, extension_index, ret) in engine_member_ext_props {
        if ret == Ty::Error {
            continue;
        }
        if let Some(signature) = table
            .class_by_type_name_mut(owner)
            .and_then(|class| class.member_ext_props.get_mut(&name))
            .and_then(|properties| properties.get_mut(extension_index))
        {
            signature.ret = inferred_declaration_ty(ret);
        }
    }
    for (file_index, declaration, ret) in engine_function_returns {
        if ret == Ty::Error {
            continue;
        }
        let declared_here = |signature: &&mut Signature| {
            signature.source_file == Some(file_index) && signature.source_decl == Some(declaration)
        };
        if let Some(signature) = table
            .funs
            .values_mut()
            .find_map(|signatures| signatures.iter_mut().find(declared_here))
        {
            signature.set_inferred_return(ret);
            continue;
        }
        // An EXTENSION lives in its own table, keyed by spelling and then by receiver.
        if let Some(signature) = table.ext_funs.values_mut().find_map(|receivers| {
            receivers
                .values_mut()
                .find_map(|signatures| signatures.iter_mut().find(declared_here))
        }) {
            signature.set_inferred_return(ret);
        }
    }
    for (method, ret) in engine_returns {
        // `Error` is the engine saying "I could not determine this", not a type. Publishing it
        // would overwrite whatever collection did determine (an anonymous object's member keeps
        // the walk's answer, for one) with a placeholder that reads downstream as a real shape —
        // an `Object` descriptor where the override needs `void`. Leave the existing answer alone;
        // an undetermined member still carries the marker and settles with the rest.
        if ret == Ty::Error {
            continue;
        }
        let Some(class) = table.class_by_type_name_mut(method.owner) else {
            continue;
        };
        class.set_inferred_source_method_return(
            crate::libraries::SourceMember::Class {
                file: method.file_index,
                owner: method.declaration.0,
                method: method.method_index,
            },
            ret,
        );
        // The signature written is the one this DECLARATION produced — its position among the
        // same-spelled overloads of the same kind. Choosing by spelling alone wrote an inferred
        // return onto an unrelated overload, including one with a DECLARED return.
        if method.is_extension {
            let Some((receiver, params)) = class
                .member_ext_funs
                .get(&method.name)
                .and_then(|overloads| overloads.get(method.overload_index))
                .map(|function| (function.receiver_ty, function.signature.params.clone()))
            else {
                continue;
            };
            class.set_inferred_member_extension_return(&method.name, receiver, &params, ret);
            continue;
        }
        let Some(params) = class
            .methods
            .get(&method.name)
            .and_then(|overloads| overloads.get(method.overload_index))
            .map(|signature| signature.params.clone())
        else {
            continue;
        };
        class.set_inferred_method_return(&method.name, &params, ret);
    }
    for (entry, resolution) in resolutions {
        // An answer that carries the error placeholder is a resolution FAILURE wearing a type's
        // clothes: publishing it puts `<error>` into `@Metadata` and `java/lang/Object` into every
        // descriptor derived from it, with no diagnostic anywhere. It takes the decline path with
        // the undetermined ones, which reports it.
        let Some(ty) = resolution.ty().filter(|ty| !ty.mentions_error()) else {
            // Undetermined is not a type. Replace the marker with the ordinary error placeholder so
            // nothing downstream — emission, `@Metadata`, a later read — can mistake "not resolved"
            // for a shape. The decline is DIAGNOSED by the authoritative check of the declaration,
            // not here: only that run knows whether the body already reported its own error (an
            // unresolved reference inside the initializer), in which case kotlinc shows no second
            // inference error.
            let failure = if resolution.declined() == Some(DeclineReason::Recursive)
                || recursive_dependents.borrow().contains(&entry.key)
            {
                let span = entry
                    .expression
                    .and_then(|expression| files[entry.file_index as usize].expr_span(expression))
                    .unwrap_or(entry.span);
                PropertyInferenceFailure::Recursive { span }
            } else if let Some(reads) = rejected_reads.borrow().get(&entry.key).cloned() {
                PropertyInferenceFailure::UninitializedReads(reads)
            } else {
                PropertyInferenceFailure::Untypeable
            };
            table.property_inference_failures.insert(entry.key, failure);
            decline_pending_property(table, entry);
            continue;
        };
        match &entry.kind {
            DeferredKind::TopLevel { .. } if entry.backing_field => {
                publish_top_level_backing_field_type(table, entry, ty)
            }
            DeferredKind::TopLevel { .. } => {
                publish_top_level_property_type(files, table, entry, ty)
            }
            DeferredKind::Member(owner) if entry.backing_field => {
                publish_member_backing_field_type(table, *owner, &entry.name, ty);
            }
            DeferredKind::Member(owner) => {
                publish_member_property_type(table, *owner, entry, ty);
                if let Decl::Class(class) =
                    files[entry.file_index as usize].decl(DeclId(entry.key.decl))
                {
                    let body_property_index = entry
                        .key
                        .member
                        .checked_sub(
                            u32::try_from(class.props.len())
                                .expect("too many constructor properties"),
                        )
                        .and_then(|index| usize::try_from(index).ok());
                    if let Some(property) = class
                        .body_props
                        .get(body_property_index.unwrap_or(usize::MAX))
                        .filter(|property| property.name == entry.name)
                    {
                        publish_member_constant(
                            &files[entry.file_index as usize],
                            table,
                            *owner,
                            property,
                            ty,
                        );
                    }
                }
            }
        }
    }

    // Property cycles are carried to the property checker because it owns suppression of cascaded
    // body diagnostics. Function cycles have no analogous declaration-failure record, so report
    // every stable declaration on the engine's cycle stack here, before Pass 2 begins.
    for key in engine.cycle_members() {
        let Some(file) = files.get(key.file as usize) else {
            continue;
        };
        let declaration = DeclId(key.decl);
        let function = match key.list {
            crate::type_engine::MemberList::Own => match file.decl(declaration) {
                Decl::Fun(function) => Some(function),
                Decl::Class(_) | Decl::Property(_) => None,
            },
            crate::type_engine::MemberList::Method => match file.decl(declaration) {
                Decl::Class(class) => class.methods.get(key.member as usize),
                Decl::Fun(_) | Decl::Property(_) => None,
            },
            crate::type_engine::MemberList::Property => None,
        };
        let Some(function) = function else {
            continue;
        };
        let span = match &function.body {
            FunBody::Expr(expression) => file.expr_span(*expression).unwrap_or(function.span),
            FunBody::Block(_) | FunBody::None => function.span,
        };
        diags.set_file(key.file);
        diags.error(span, RECURSIVE_INFERENCE_MESSAGE);
    }
}

/// The engine's member-demand hook: what `owner`'s `name` denotes, refined by a CALL's argument
/// types (empty for a plain read, where there is nothing to choose between overloads with).
pub(super) type DemandMember<'a> = &'a dyn Fn(Ty, &str, &[Ty]) -> Option<Ty>;

/// The engine's bare-name demand: what a module declaration spelled `name` denotes.
pub(super) type DemandName<'a> = &'a dyn Fn(&str) -> Option<Ty>;

/// The same for a spelling that may be OVERLOADED, chosen by a call's argument types.
pub(super) type DemandCall<'a> = &'a dyn Fn(&str, &[Ty]) -> Option<Ty>;

/// One class member whose return the walk could not determine, addressed by DECLARATION.
///
/// The by-spelling index cannot carry an overloaded name; this can, because publishing an answer
/// never has to choose — it already knows which declaration was asked about.
struct UndeterminedMethod {
    owner: TypeName,
    pub(super) name: String,
    /// A member EXTENSION keeps its own signature table, so which one to write is decided here.
    is_extension: bool,
    /// Position among the same-spelled overloads of the same kind, as the lowerer counts them.
    overload_index: usize,
    pub(super) file_index: u32,
    declaration: DeclId,
    method_index: u32,
}

/// Turn a declaration the engine could not determine into the ordinary error placeholder.
fn decline_pending_property(table: &mut SymbolTable, entry: &DeferredProperty) {
    // An extension property the engine could not determine is REMOVED from the extension table
    // rather than left carrying a placeholder: an entry whose type is `Error` is registered — it
    // resolves at every use site and reaches emission and `@Metadata` with `<error>` in its
    // descriptor. Not registering it is what collection did with the same answer, and the use site
    // reports the ordinary unresolved-reference diagnostic.
    //
    // The module property tables below are NOT touched for one: they are keyed by bare spelling,
    // which an extension property does not own — writing there would place the placeholder on an
    // unrelated `val` of the same name. `publish_top_level_property_type` returns early for the
    // same reason.
    if matches!(
        entry.kind,
        DeferredKind::TopLevel {
            has_receiver: true,
            ..
        }
    ) {
        // In the streamed path this record is also the stable receiver/parameter header for the
        // compact signature graph. The legacy engine declining its result approximation must not
        // erase that header before the authoritative solver runs. `fail_undetermined_returns`
        // still retires the private `Pending` marker, and finalization projects the graph result
        // back into this exact stable declaration before Pass 2.
        let retained_for_signature_graph = table.ext_props.values().flatten().any(|signature| {
            signature.source == (entry.file_index, entry.key.decl)
                && signature.ty == Ty::Pending
                && signature.stable_declaration.is_some()
        });
        if retained_for_signature_graph {
            return;
        }
        for overloads in table.ext_props.values_mut() {
            overloads.retain(|signature| {
                signature.source != (entry.file_index, entry.key.decl)
                    || signature.ty != Ty::Pending
            });
        }
        return;
    }
    if entry.backing_field && entry.owner().is_none() {
        if let Some(signature) = table
            .source_props
            .get_mut(&(entry.file_index, entry.key.decl))
            .filter(|signature| signature.storage_ty == Some(Ty::Pending))
        {
            signature.storage_ty = Some(Ty::Error);
        }
        return;
    }
    if let Some(signature) = table
        .source_props
        .get_mut(&(entry.file_index, entry.key.decl))
        .filter(|signature| signature.ty == Ty::Pending)
    {
        signature.ty = Ty::Error;
    }
    if let Some(module_property) = table
        .props
        .get_mut(&entry.name)
        .filter(|property| property.0 == Ty::Pending)
    {
        module_property.0 = Ty::Error;
    }
    if let Some(owner) = entry.owner() {
        if let Some(signature) = table.class_by_type_name_mut(owner) {
            if let Some(property) = signature
                .declared_props
                .get_mut(&entry.name)
                .filter(|property| property.ty == Ty::Pending)
            {
                property.ty = Ty::Error;
            }
            for backing in signature.props.iter_mut() {
                if backing.0 == entry.name && backing.1 == Ty::Pending {
                    backing.1 = Ty::Error;
                }
            }
        }
    }
}

/// Write a resolved top-level property type into the module signature.
fn publish_top_level_property_type(
    files: &[File],
    table: &mut SymbolTable,
    entry: &DeferredProperty,
    ty: Ty,
) {
    let Decl::Property(property) = files[entry.file_index as usize].decl(DeclId(entry.key.decl))
    else {
        return;
    };
    // The compile-time value depends on the type the engine settled on: `source_literal_constant`
    // reads the literal THROUGH it (a `UIntLit` is a long constant under `ULong` and an int
    // constant otherwise), so it cannot have been computed while the type was still a placeholder.
    let compile_time_constant = (!property.is_var)
        .then(|| {
            property.init.and_then(|init| {
                source_literal_constant(&files[entry.file_index as usize], init, ty)
            })
        })
        .flatten();
    // An EXTENSION property lives in its own table, keyed by erased receiver and spelling; it has
    // no `source_props` entry to update.
    if property.receiver.is_some() {
        if let Some(signature) = table
            .ext_props
            .values_mut()
            .flatten()
            .find(|signature| signature.source == (entry.file_index, entry.key.decl))
            .filter(|signature| signature.ty == Ty::Pending)
        {
            signature.ty = ty;
        }
        return;
    }
    crate::trace_compiler!(
        "signature_inference",
        "finalize top-level property name={} source=({}, {}) type={ty:?}",
        property.name,
        entry.file_index,
        entry.key.decl,
    );
    let published = table
        .source_props
        .get_mut(&(entry.file_index, entry.key.decl))
        .filter(|signature| signature.ty == Ty::Pending)
        .map(|signature| {
            signature.ty = ty;
            signature.compile_time_constant = compile_time_constant.clone();
            signature.context_params.is_empty()
        });
    if published == Some(true) {
        if let Some(module_property) = table.props.get_mut(&property.name) {
            module_property.0 = ty;
        }
    }
}

/// Publish a top-level explicit backing field without changing the property's declared signature.
fn publish_top_level_backing_field_type(table: &mut SymbolTable, entry: &DeferredProperty, ty: Ty) {
    if let Some(signature) = table
        .source_props
        .get_mut(&(entry.file_index, entry.key.decl))
        .filter(|signature| signature.storage_ty == Some(Ty::Pending))
    {
        signature.storage_ty = Some(ty);
    }
}

fn member_property_by_source_mut(
    class: &mut ClassSig,
    source: crate::libraries::SourceMember,
) -> Option<&mut DeclaredPropertySig> {
    class
        .declared_props
        .values_mut()
        .chain(class.contextual_props.values_mut().flatten())
        .find(|property| property.source_member == Some(source))
}

/// Write a resolved member property type into its exact declaration signature and, for an ordinary
/// stored property, its backing-field entry. A contextual property has no field and may share its
/// spelling with other overloads, so it is selected by source identity rather than by name.
fn publish_member_property_type(
    table: &mut SymbolTable,
    owner: TypeName,
    entry: &DeferredProperty,
    ty: Ty,
) {
    let Some(signature) = table.class_by_type_name_mut(owner) else {
        return;
    };
    let source = crate::libraries::SourceMember::ClassProperty {
        file: entry.file_index,
        owner: entry.key.decl,
        property: entry.key.member,
    };
    if let Some(property) = member_property_by_source_mut(signature, source) {
        property.ty = ty;
    }
    if let Some(backing) = signature
        .props
        .iter_mut()
        .find(|(property_name, _, _)| property_name == &entry.name)
    {
        backing.1 = ty;
    }
}

/// Write a resolved explicit backing-field type into its owner's signature.
///
/// The PROPERTY's type is declared here; only the field's is inferred, so this touches the storage
/// slot and the backing entry, never `declared_props`.
fn publish_member_backing_field_type(table: &mut SymbolTable, owner: TypeName, name: &str, ty: Ty) {
    let Some(signature) = table.class_by_type_name_mut(owner) else {
        return;
    };
    if let Some(property) = signature.declared_props.get_mut(name) {
        property.storage_ty = Some(ty);
    }
    if let Some(backing) = signature
        .props
        .iter_mut()
        .find(|(property_name, _, _)| property_name == name)
    {
        backing.1 = ty;
    }
}

/// Record a `const val` member's compile-time value once its type is known.
///
/// The value is read THROUGH the type — a `UIntLit` is a long constant under `ULong` and an int
/// constant otherwise — so collection, which saw only the placeholder, could not compute it.
pub(super) fn publish_member_constant(
    file: &File,
    table: &mut SymbolTable,
    owner: TypeName,
    property: &PropDecl,
    ty: Ty,
) {
    if !property.is_const {
        return;
    }
    let Some(init) = property.init else {
        return;
    };
    let Some(value) = source_literal_constant(file, init, ty) else {
        return;
    };
    let is_singleton = table
        .class_by_type_name(owner)
        .is_some_and(ClassSig::is_object);
    if !is_singleton {
        return;
    }
    if let Some(class) = table.class_by_type_name_mut(owner) {
        class.constants.insert(property.name.clone(), value);
    }
}

/// Infer a declaration initializer's type with a fresh cycle-guard, using `table` to resolve
/// module-local class properties — the common entry used by signature collection.
///
/// Reads only what the table already holds. A reference to a module declaration whose own type is
/// still being determined resolves through [`infer_lit_ty_scoped_on_demand`] instead.
pub(super) fn infer_lit_ty_scoped(
    source: InferenceSource<'_>,
    e: ExprId,
    class_names: &ClassNames,
    props: &[(String, Ty, bool)],
    src: &dyn SemanticPlatform,
    table: &SymbolTable,
) -> Ty {
    let _ = (class_names, src);
    infer_lit_ty_scoped_on_demand(source, e, props, table, &|_| None, &|_, _, _| None)
}

/// Whether an argument of type `actual` could be passed for a parameter declared `parameter`.
///
/// Answers YES wherever it cannot tell — a type variable, an erased top, an unresolved type, or a
/// function type (whose shape this does not compare) all fit — and otherwise asks the ordinary
/// subtyping relation, so a `Number` parameter accepts an `Int` argument and a `CharSequence` one a
/// `String`. Comparing classifier IDENTITY instead reads every supertype parameter as a non-fit,
/// which is exactly the case where the call belongs to the other constructor.
fn constructor_param_may_take(
    oracle: &dyn crate::assignable::TypeOracle,
    actual: Ty,
    parameter: Ty,
) -> bool {
    if parameter == Ty::Error || actual == Ty::Error {
        return true;
    }
    if parameter.is_ty_param() || actual.is_ty_param() {
        return true;
    }
    if parameter.is_fun() || actual.is_fun() {
        return true;
    }
    if parameter.non_null() == Ty::obj("kotlin/Any") {
        return true;
    }
    crate::assignable::is_subtype(&crate::assignable::TyCtx::new(), oracle, actual, parameter)
}

/// Whether a declaration with these parameters could be CALLED with `arity` arguments.
///
/// A declared parameter count is not an arity: every default makes a shorter call reachable and a
/// vararg makes longer ones reachable.
fn constructor_at_arity(declared: usize, optional: usize, vararg: bool, arity: usize) -> bool {
    let least = declared.saturating_sub(optional);
    if vararg {
        arity + 1 >= least
    } else {
        arity >= least && arity <= declared
    }
}

fn constructor_params_may_take(
    oracle: &dyn crate::assignable::TypeOracle,
    parameters: &[Ty],
    actuals: &[Option<Ty>],
) -> bool {
    parameters
        .iter()
        .zip(actuals)
        .all(|(parameter, actual)| match actual {
            Some(actual) => constructor_param_may_take(oracle, *actual, *parameter),
            None => true,
        })
}

/// A construction whose call a SECONDARY constructor could own must be published unapplied.
///
/// The applied type comes from binding the class's parameters against the PRIMARY constructor.
/// `class Cell<T>(val value: T, val flag: Boolean) { constructor(n: Int, v: T) }` written
/// `Cell(5, "tag")` resolves to the secondary one, so binding `T` from the primary's `(T, Boolean)`
/// gives `Cell<Int>` — a `checkcast` to `Integer` on a `String`, emitted with no diagnostic and
/// throwing on first read where kotlinc runs. Keeping the classifier raw rejects the program
/// instead of miscompiling it.
fn withheld_construction_ty(
    file: &File,
    expression: ExprId,
    expr_types: &[Ty],
    table: &SymbolTable,
    ty: Ty,
) -> Ty {
    let TyKind::Obj(internal, args) = ty.kind() else {
        return ty;
    };
    if args.is_empty() {
        return ty;
    }
    let Expr::Call {
        args: call_args, ..
    } = file.expr(expression)
    else {
        return ty;
    };
    let Some(class) = table.class_by_type_name(internal) else {
        return ty;
    };
    if class.secondary_ctors.is_empty() {
        return ty;
    }
    let actuals = call_args
        .iter()
        .map(|argument| {
            expr_types
                .get(argument.0 as usize)
                .copied()
                .filter(|ty| *ty != Ty::Error)
        })
        .collect::<Vec<_>>();
    let arity = call_args.len();
    let oracle = table.source_constructor_matcher_with(table.libraries.as_ref());
    let primary = &class.ctor_params;
    // Every default makes a SHORTER call reach the primary, so a declared parameter count is not an
    // arity. Ignoring defaults reads an ordinary `C(x)` on `class C<T>(x: T, y: Int = 0)` as a call
    // the primary cannot take, and withholds a type argument the construction genuinely fixed.
    let primary_optional = class
        .ctor_param_names
        .iter()
        .filter(|(_, has_default)| *has_default)
        .count();
    let primary_fits = constructor_at_arity(
        primary.len(),
        primary_optional,
        class.ctor_vararg.is_some(),
        arity,
    ) && constructor_params_may_take(&oracle, primary, &actuals);
    // A secondary owns the call only where it takes the arity AND its parameters accept the
    // arguments. Where the primary also fits, the call is the primary's: an applicable primary is
    // what the applied type was bound from.
    let secondary_owns = !primary_fits
        && class.secondary_ctors.iter().any(|parameters| {
            parameters.len() == arity && constructor_params_may_take(&oracle, parameters, &actuals)
        });
    let primary_owns_it = primary_fits
        || !class
            .secondary_ctors
            .iter()
            .any(|parameters| parameters.len() == arity);
    if primary_owns_it && !secondary_owns {
        return ty;
    }
    Ty::obj_name(internal)
}

/// Whether this expression has no publishable declaration type because every value-producing path
/// diverges. Ordinary least-upper-bound computation is semantic and therefore needs no separate
/// declaration-only correction here.
fn declaration_body_is_unpublishable(file: &File, expression: ExprId, expr_types: &[Ty]) -> bool {
    // A body that never yields a value gives the declaration no type. `val a = throw E()` and
    // `val a = return` would otherwise infer `Nothing` and emit a `Ljava/lang/Void;` field, where
    // kotlinc rejects the property outright ("property type 'Nothing' needs to be specified
    // explicitly"). An elvis whose RIGHT side throws is unaffected: its type is the left side's.
    if expr_types
        .get(expression.0 as usize)
        .is_some_and(|ty| *ty == Ty::Nothing)
    {
        return true;
    }
    let Expr::Elvis { lhs, rhs } = file.expr(expression) else {
        return false;
    };
    let side = |e: &ExprId| {
        expr_types
            .get(e.0 as usize)
            .copied()
            .unwrap_or(Ty::Error)
            .non_null()
    };
    let (left, right) = (side(lhs), side(rhs));
    // `null ?: throw …` yields the LEFT side, and a bare `null` left becomes `Nothing?` — a
    // publishable type — so the rule above never sees it. Neither side ever produces a value, so
    // the declaration has no type; kotlinc rejects it.
    right == Ty::Nothing && matches!(left, Ty::Null | Ty::Nothing)
}

/// The engine's seams into one checker run.
///
/// Grouped rather than passed one by one: they are three views of a single question — what does the
/// engine say about a name this declaration reads — and a run either has all three or, for the
/// authoritative check, none.
#[derive(Clone, Copy)]
struct EngineSeams<'a> {
    /// A name whose declaration has not been typed yet.
    demand: DemandName<'a>,
    /// A spelling that may be OVERLOADED, chosen by the call's argument types.
    demand_call: DemandCall<'a>,
    /// The same, for a read through a receiver. The argument types are the CALL's, empty for a
    /// plain read: a name alone cannot choose between overloads, and they can.
    demand_member: DemandMember<'a>,
    /// A name this declaration may not read at all, by initialization order.
    rejects: &'a dyn Fn(&str, Span) -> bool,
}

/// Retire the collection-only pending marker after demand-driven inference. A failed declaration
/// carries `Error` only on the diagnostic path; inventing `Unit` here would publish a false ABI.
pub(super) fn fail_undetermined_returns(table: &mut SymbolTable) {
    for signatures in table.funs.values_mut() {
        for signature in signatures.iter_mut() {
            if signature.ret == Ty::Pending {
                signature.set_inferred_return(Ty::Error);
            }
        }
    }
    for overloads in table.ext_funs.values_mut() {
        for receivers in overloads.values_mut() {
            for signature in receivers.iter_mut() {
                if signature.ret == Ty::Pending {
                    signature.set_inferred_return(Ty::Error);
                }
            }
        }
    }
    // Top-level PROPERTIES and top-level EXTENSION properties keep their own tables. An extension
    // property with an inferred type (`val A.z get() = this.x`) settles here; leaving the marker in
    // place trips the finalization assertion and aborts the compiler.
    for (ty, _, _) in table.props.values_mut() {
        if *ty == Ty::Pending {
            *ty = Ty::Error;
        }
    }
    for property in table.source_props.values_mut() {
        if property.ty == Ty::Pending {
            property.ty = Ty::Error;
        }
    }
    for properties in table.ext_props.values_mut() {
        for property in properties.iter_mut() {
            if property.receiver == Ty::Pending {
                property.receiver = Ty::Error;
            }
            if property.ty == Ty::Pending {
                property.ty = Ty::Error;
            }
            for context in property.context_params.iter_mut() {
                if *context == Ty::Pending {
                    *context = Ty::Error;
                }
            }
        }
    }
    // Class members too. The marker means "ask the engine for this declaration"; where nothing
    // answered — an overloaded spelling the index cannot key, a member of a classifier with no
    // source declaration — it must not survive into checking or emission, so it settles to the same
    // `Unit` the walk used to guess. The difference is that the guess is now the LAST resort rather
    // than the first answer.
    for class in table.classes.values_mut() {
        for (_, ty, _) in &mut class.props {
            if *ty == Ty::Pending {
                *ty = Ty::Error;
            }
        }
        for property in class.declared_props.values_mut() {
            if property.ty == Ty::Pending {
                property.ty = Ty::Error;
            }
            if property.storage_ty == Some(Ty::Pending) {
                property.storage_ty = Some(Ty::Error);
            }
        }
        for property in class.contextual_props.values_mut().flatten() {
            if property.ty == Ty::Pending {
                property.ty = Ty::Error;
            }
            debug_assert!(
                property.storage_ty.is_none(),
                "a context property cannot own backing storage"
            );
        }
        for overloads in class.methods.values_mut() {
            for signature in overloads.iter_mut() {
                if signature.ret == Ty::Pending {
                    signature.set_inferred_return(Ty::Error);
                }
            }
        }
        for plan in &mut class.source_methods {
            if plan.signature.ret == Ty::Pending {
                plan.signature.set_inferred_return(Ty::Error);
            }
        }
        for overloads in class.generic_methods.values_mut() {
            for method in overloads.iter_mut() {
                if method.signature.ret == Ty::Pending {
                    method.signature.ret = Ty::Error;
                }
            }
        }
        // A member EXTENSION keeps its own table, and a marker left there reaches emission exactly
        // as an ordinary member's would: `operator fun String.invoke() = Unit` is a real method with
        // a real descriptor.
        for overloads in class.member_ext_funs.values_mut() {
            for function in overloads.iter_mut() {
                if function.signature.ret == Ty::Pending {
                    function.signature.set_inferred_return(Ty::Error);
                }
            }
        }
        // Member-extension PROPERTIES keep a third table, and their getter descriptor is emitted
        // from it. A marker left here reaches `type_encoder`, which rejects it.
        for properties in class.member_ext_props.values_mut() {
            for property in properties.iter_mut() {
                if property.ret == Ty::Pending {
                    property.ret = Ty::Error;
                }
            }
        }
    }
}

pub(super) fn module_signatures_mention_pending(table: &SymbolTable) -> bool {
    let generic_mentions_pending = |generic: &GenericSig| {
        generic.receiver.is_some_and(Ty::mentions_pending)
            || generic.params.iter().copied().any(Ty::mentions_pending)
            || generic.ret.mentions_pending()
            || generic
                .formal_bounds
                .iter()
                .flatten()
                .copied()
                .any(Ty::mentions_pending)
    };
    let signature_mentions_pending = |signature: &Signature| {
        signature.params.iter().copied().any(Ty::mentions_pending)
            || signature.ret.mentions_pending()
            || signature.source_receiver.is_some_and(Ty::mentions_pending)
            || signature
                .generic_sig
                .as_ref()
                .is_some_and(generic_mentions_pending)
    };
    if table
        .funs
        .values()
        .flatten()
        .any(&signature_mentions_pending)
        || table
            .ext_funs
            .values()
            .flat_map(HashMap::values)
            .flatten()
            .any(&signature_mentions_pending)
        || table
            .source_props
            .values()
            .any(|property| property.ty.mentions_pending())
        || table.props.values().any(|(ty, _, _)| ty.mentions_pending())
        || table.ext_props.values().flatten().any(|property| {
            property.receiver.mentions_pending()
                || property.ty.mentions_pending()
                || property
                    .context_params
                    .iter()
                    .copied()
                    .any(Ty::mentions_pending)
        })
    {
        return true;
    }
    table.classes.values().any(|class| {
        class.ctor_params.iter().copied().any(Ty::mentions_pending)
            || class
                .ctor_param_shapes
                .iter()
                .any(|(ty, _)| ty.mentions_pending())
            || class
                .secondary_ctors
                .iter()
                .flatten()
                .copied()
                .any(Ty::mentions_pending)
            || class.props.iter().any(|(_, ty, _)| ty.mentions_pending())
            || class.declared_props.values().any(|property| {
                property.ty.mentions_pending()
                    || property.storage_ty.is_some_and(Ty::mentions_pending)
                    || property
                        .context_params
                        .iter()
                        .copied()
                        .any(Ty::mentions_pending)
            })
            || class
                .methods
                .values()
                .flatten()
                .any(&signature_mentions_pending)
            || class
                .source_methods
                .iter()
                .any(|plan| signature_mentions_pending(&plan.signature))
            || class.member_ext_funs.values().flatten().any(|function| {
                function.receiver_ty.mentions_pending()
                    || signature_mentions_pending(&function.signature)
            })
            || class.member_ext_props.values().flatten().any(|property| {
                property.receiver.mentions_pending()
                    || property.ret.mentions_pending()
                    || property
                        .context_params
                        .iter()
                        .copied()
                        .any(Ty::mentions_pending)
                    || property
                        .type_param_bounds
                        .iter()
                        .copied()
                        .any(Ty::mentions_pending)
            })
            || class.generic_methods.values().flatten().any(|method| {
                method.signature.ret.mentions_pending()
                    || method.params.iter().copied().any(Ty::mentions_pending)
                    || method
                        .signature
                        .params
                        .iter()
                        .copied()
                        .any(Ty::mentions_pending)
                    || method
                        .signature
                        .formal_bounds
                        .iter()
                        .flatten()
                        .copied()
                        .any(Ty::mentions_pending)
            })
            || class
                .type_parameters
                .type_param_bounds
                .iter()
                .copied()
                .any(Ty::mentions_pending)
            || class
                .type_parameter_extra_bounds
                .iter()
                .flatten()
                .copied()
                .any(Ty::mentions_pending)
            || class
                .captured_type_parameters
                .type_param_bounds
                .iter()
                .copied()
                .any(Ty::mentions_pending)
    })
}

/// Type ONE top-level function's expression body, to answer a call whose return is not determined.
///
/// The declaration is run the same way its own inference would run it, so the answer is the one the
/// function itself yields rather than a guess made at the call site.
fn infer_function_return_ty(
    file: &File,
    file_index: u32,
    declaration: DeclId,
    table: &SymbolTable,
    engine: EngineSeams<'_>,
) -> Option<Ty> {
    let Decl::Fun(function) = file.decl(declaration) else {
        return None;
    };
    if !function_needs_return_preinfer(function) {
        return None;
    }
    let mut scratch = DiagSink::new();
    let mut checker = make_checker(file, file_index, None, table, &mut scratch);
    checker.demand_name = Some(engine.demand);
    checker.demand_call = Some(engine.demand_call);
    checker.demand_member = Some(engine.demand_member);
    checker.inference_only = true;
    checker.anonymous_lexical_scope = anonymous_lexical_class_scope(file);
    checker.set_anonymous_lexical_class_context(declaration);
    let root = CheckerScope::root();
    let resolve = class_internal_resolver(table);
    let declaration_scope = root.child(ScopeKind::Function { receiver: None });
    let scope = &declaration_scope;
    scope.declare_tparams(
        &function.type_params,
        &TParams::symbolic_from_decl_with(
            &function.type_params,
            &function.type_param_bounds,
            &resolve,
        ),
        |name| function.reified_type_params.contains(name),
    );
    let outer = std::mem::replace(&mut checker.symbolic_signature_inference, true);
    checker.check_fun(scope, function, Some(declaration));
    checker.symbolic_signature_inference = outer;
    let incomplete = checker.inference_incomplete;
    // An EXTENSION records its return in its own table, keyed by spelling as well — the module's
    // extensions live in a table keyed by receiver, so the walk cannot key them the way it keys a
    // plain function. Which table the answer landed in is a property of the declaration, not of the
    // question, so both are read.
    let rets = std::mem::take(&mut checker.inferred_fun_rets);
    let ext_rets = std::mem::take(&mut checker.inferred_ext_fun_rets);
    drop(checker);
    if incomplete {
        return None;
    }
    rets.get(&(file_index, declaration.0))
        .copied()
        .or_else(|| {
            ext_rets
                .iter()
                .find(|((file, decl, _), _)| *file == file_index && *decl == declaration.0)
                .map(|(_, ret)| *ret)
        })
        .filter(|ty| *ty != Ty::Error && *ty != Ty::Pending)
}

/// Type a LOCAL class's member by checking the function that declares the class.
///
/// A local class's member initializer may read the enclosing function's locals — `fun f() { val s =
/// "x"; class A(val p: String) { val s2 = s + p } }` — and no signature-level scope can hold them:
/// the signature walk never enters function bodies, so typing the member in its own class declines.
///
/// The checker answers this when it walks the function: it types `s + p` with `s` in scope. The
/// declaration's type is READ from that run rather than recomputed — the same jump-and-resolve the
/// engine does everywhere else, with the enclosing function as the unit resolved. That function is
/// the innermost one whose span contains the member. The run is a full check, not an inference-only
/// one, because inference mode does not descend into a local class body; its diagnostics go to a
/// scratch sink and are discarded, since this run exists only to read one type.
fn infer_local_class_member_ty(
    file: &File,
    file_index: u32,
    member_span: Span,
    expression: ExprId,
    table: &SymbolTable,
) -> Ty {
    let Some(declaration) = file
        .decls
        .iter()
        .copied()
        .filter(|&declaration| {
            matches!(file.decl(declaration), Decl::Fun(function)
                if function.span.lo <= member_span.lo && member_span.lo <= function.span.hi)
        })
        .min_by_key(|&declaration| match file.decl(declaration) {
            Decl::Fun(function) => function.span.hi.saturating_sub(function.span.lo),
            _ => u32::MAX,
        })
    else {
        return Ty::Error;
    };
    let Decl::Fun(function) = file.decl(declaration) else {
        return Ty::Error;
    };
    let mut scratch = DiagSink::new();
    let mut checker = make_checker(file, file_index, None, table, &mut scratch);
    // No demand hooks: the enclosing function's OWN body typically reads the member being computed
    // (`return A().s2`), and answering that read through the engine re-enters the declaration that
    // is already being computed — a cycle, which declines the very resolution this run exists to
    // produce. The initializer needs the function's locals, which this run has, and nothing else.
    checker.anonymous_lexical_scope = anonymous_lexical_class_scope(file);
    checker.set_anonymous_lexical_class_context(declaration);
    let root = CheckerScope::root();
    let resolve = class_internal_resolver(table);
    let declaration_scope = root.child(ScopeKind::Function { receiver: None });
    let scope = &declaration_scope;
    scope.declare_tparams(
        &function.type_params,
        &TParams::symbolic_from_decl_with(
            &function.type_params,
            &function.type_param_bounds,
            &resolve,
        ),
        |name| function.reified_type_params.contains(name),
    );
    checker.check_fun(scope, function, Some(declaration));
    let types = std::mem::take(&mut checker.expr_types);
    drop(checker);
    types
        .get(expression.0 as usize)
        .copied()
        .unwrap_or(Ty::Error)
}

/// Type ONE class-body declaration in its class's checking context.
///
/// `check_class` validates members against the types the symbol table already holds; it does not
/// recompute them, so running it while those types are placeholders leaves every initializer
/// `Ty::Error` with no diagnostic to show for it. What a member's body actually needs is the
/// context its class establishes — the class's type parameters, the dispatch receiver and `this`
/// labels, and its sibling members as scoped properties — and then ordinary expression resolution,
/// which is what the enclosing declaration would give it.
fn infer_class_member_ty(
    file: &File,
    file_index: u32,
    owner: DeclId,
    expression: ExprId,
    scope_values: &[(String, Ty, bool)],
    table: &SymbolTable,
    engine: EngineSeams<'_>,
) -> Ty {
    let EngineSeams {
        demand,
        demand_call,
        demand_member,
        rejects,
    } = engine;
    let Decl::Class(class) = file.decl(owner) else {
        return Ty::Error;
    };
    let mut scratch = DiagSink::new();
    let mut checker = make_checker(file, file_index, None, table, &mut scratch);
    checker.demand_name = Some(demand);
    checker.demand_call = Some(demand_call);
    checker.demand_member = Some(demand_member);
    checker.demand_rejects = Some(rejects);
    checker.inference_only = true;
    checker.symbolic_signature_inference = true;
    checker.anonymous_lexical_scope = anonymous_lexical_class_scope(file);
    checker.set_anonymous_lexical_class_context(owner);
    let Some(internal) = checker
        .same_package_class(&class.name)
        .map(ClassSig::internal_name)
    else {
        return Ty::Error;
    };
    // The dispatch rung carries the APPLIED type (`C<T>`, not the raw `C`), which is what member
    // bodies resolve against.
    let class_tparams = TParams::symbolic_from_decl_with(
        &class.type_params,
        &class.type_param_bounds,
        &class_internal_resolver(table),
    )
    .alpha_renamed_declaration(
        &class.type_params,
        checker.compilation_id,
        file_index,
        class.span.lo,
    );
    let dispatch = Ty::obj_args_name(
        internal,
        &class
            .type_params
            .iter()
            .map(|name| class_tparams.bound(name))
            .collect::<Vec<_>>(),
    );
    let root = CheckerScope::root();
    let dispatch_scope = root.dispatch_receiver_child(dispatch);
    let scope = &dispatch_scope;
    scope.declare_tparams(&class.type_params, &class_tparams, |_| false);
    checker
        .this_labels
        .extend(class_receiver_labels(class, file, table, Some(dispatch)));
    let mut properties = checker.scoped_properties(scope, internal);
    let entry_properties = checker.enclosing_enum_entry_properties(scope, class, &properties);
    properties.extend(entry_properties);
    checker.declare_enclosing_enum_entry_methods(scope, class);
    for property in &properties {
        checker.declare_scoped_property(scope, property, true);
    }
    // The declaration's OWN value bindings — its context parameters, and the receiver an extension
    // property binds — are part of its body's scope and are not members of the class, so
    // `scoped_properties` does not carry them. `context(s: String) val v get() = s` reads `s` from
    // here or not at all.
    for (name, ty, is_var) in scope_values {
        checker.declare(scope, name, *ty, *is_var);
    }
    let inferred =
        checker.with_ret_allowed(Ty::Unit, false, |checker| checker.expr(scope, expression));
    let declines = checker.inference_incomplete
        || declaration_body_is_unpublishable(file, expression, &checker.expr_types);
    let types = std::mem::take(&mut checker.expr_types);
    drop(checker);
    // Diagnostics are NOT the signal here. Resolving one member reaches siblings whose own types
    // are still placeholders, so this run reports errors the authoritative check will not; the type
    // the expression itself got is what decides.
    if declines {
        return Ty::Error;
    }
    if inferred == Ty::Pending {
        // The body's own type is still the marker: something it read was undetermined and stayed
        // that way. Publishing it would make "not resolved" look like a type — and, for a cycle
        // between two members, would report nothing at all.
        return Ty::Error;
    }
    withheld_construction_ty(file, expression, &types, table, inferred)
}

/// Type ONE member-extension property's getter, in its class's context.
///
/// `class C { val Foo.name get() = this.length }` needs four things bound before the getter means
/// anything: the class's dispatch receiver and type parameters, the property's OWN type parameters
/// on their own rung so they retire with it, the extension receiver as `this`, and the property's
/// context parameters. The sweep built exactly this; the engine builds it for one declaration, when
/// that declaration is asked for.
fn infer_member_ext_property_ty(
    file: &File,
    file_index: u32,
    owner: DeclId,
    property_index: u32,
    table: &SymbolTable,
    engine: EngineSeams<'_>,
) -> Ty {
    let Decl::Class(class) = file.decl(owner) else {
        return Ty::Error;
    };
    let Some(property) = class.body_props.get(property_index as usize) else {
        return Ty::Error;
    };
    let Some((receiver, getter)) = member_extension_property_preinfer_body(property) else {
        return Ty::Error;
    };
    let mut scratch = DiagSink::new();
    let mut checker = make_checker(file, file_index, None, table, &mut scratch);
    checker.demand_name = Some(engine.demand);
    checker.demand_call = Some(engine.demand_call);
    checker.demand_member = Some(engine.demand_member);
    checker.demand_rejects = Some(engine.rejects);
    checker.inference_only = true;
    checker.anonymous_lexical_scope = anonymous_lexical_class_scope(file);
    checker.set_anonymous_lexical_class_context(owner);
    let Some(internal) = checker
        .same_package_class(&class.name)
        .map(ClassSig::internal_name)
    else {
        return Ty::Error;
    };
    let resolve = class_internal_resolver(table);
    // The class's formals carry the declaration-owned identity here too: without it the getter's
    // `T` is a different variable from the class's own and `Wrapper<T>` comes back `Wrapper<Any>`.
    let class_tparams =
        TParams::symbolic_from_decl_with(&class.type_params, &class.type_param_bounds, &resolve)
            .alpha_renamed_declaration(
                &class.type_params,
                checker.compilation_id,
                file_index,
                class.span.lo,
            );
    let dispatch = Ty::obj_args_name(
        internal,
        &class
            .type_params
            .iter()
            .map(|name| class_tparams.bound(name))
            .collect::<Vec<_>>(),
    );
    let root = CheckerScope::root();
    let dispatch_scope = root.dispatch_receiver_child(dispatch);
    let scope = &dispatch_scope;
    scope.declare_tparams(&class.type_params, &class_tparams, |_| false);
    checker
        .this_labels
        .extend(class_receiver_labels(class, file, table, Some(dispatch)));
    for declared in &checker.scoped_properties(scope, internal) {
        checker.declare_scoped_property(scope, declared, true);
    }
    let property_scope = scope.child(ScopeKind::Function { receiver: None });
    let scope = &property_scope;
    scope.declare_tparams(
        &property.type_params,
        &class_tparams
            .symbolic_extended_with(&property.type_params, &property.type_param_bounds, &resolve)
            .alpha_renamed_declaration(
                &property.type_params,
                checker.compilation_id,
                file_index,
                property.span.lo,
            ),
        |_| false,
    );
    let receiver_ty = checker.type_ref_ty(scope, receiver);
    checker.this_labels.push((
        property.name.clone(),
        receiver_ty,
        false,
        Some(ReceiverDeclarationRole::Extension(receiver.span)),
    ));
    let outer = std::mem::replace(&mut checker.symbolic_signature_inference, true);
    let inferred = {
        let getter_scope = scope.declaration_function_child_with_context(
            Some(receiver_ty),
            Some(lexical_receiver_declaration(file, receiver)),
            &[],
        );
        let scope = &getter_scope;
        for parameter in &property.context_params {
            if parameter.name == "_" {
                continue;
            }
            let parameter_type = checker.type_ref_ty(scope, &parameter.ty);
            checker.declare_context_parameter(scope, &parameter.name, parameter_type);
        }
        checker.expr(scope, getter)
    };
    checker.symbolic_signature_inference = outer;
    let incomplete = checker.inference_incomplete;
    drop(checker);
    match incomplete || inferred == Ty::Error || inferred.mentions_pending() {
        true => Ty::Error,
        false => inferred,
    }
}

/// Type a member FUNCTION's inferred return, in its class's checking context.
///
/// The same jump the engine makes for a member property: the class's formals are symbolic, `this`
/// is bound to the applied dispatch type, and the class's own properties are in scope, so a body
/// reading `value` or a companion constant answers from the declaration rather than from whatever
/// the walk had recorded when it passed by.
fn infer_method_return_ty(
    file: &File,
    file_index: u32,
    owner: DeclId,
    method_index: u32,
    table: &SymbolTable,
    engine: EngineSeams<'_>,
) -> Ty {
    let Decl::Class(class) = file.decl(owner) else {
        return Ty::Error;
    };
    let Some(method) = class.methods.get(method_index as usize) else {
        return Ty::Error;
    };
    let mut scratch = DiagSink::new();
    let mut checker = make_checker(file, file_index, None, table, &mut scratch);
    checker.demand_name = Some(engine.demand);
    checker.demand_call = Some(engine.demand_call);
    checker.demand_member = Some(engine.demand_member);
    checker.demand_rejects = Some(engine.rejects);
    checker.inference_only = true;
    checker.anonymous_lexical_scope = anonymous_lexical_class_scope(file);
    checker.set_anonymous_lexical_class_context(owner);
    let Some(internal) = checker
        .same_package_class(&class.name)
        .map(ClassSig::internal_name)
    else {
        return Ty::Error;
    };
    let resolve = class_internal_resolver(table);
    // The class's formals carry the DECLARATION-owned identity, as they do when a member property is
    // typed. Without it the body's `T` is a different variable from the class's own, and every read
    // through it erases to the bound.
    // Symbolic formals, minted with the DECLARATION-owned identity. A plain formal erases to its
    // bound (`fun get() = value` comes back `Any`), and a symbolic one under its source name is not
    // what the call substitutes against: the receiver's bindings are keyed by that same declaration
    // identity, so `T` under any other key passes through unsubstituted.
    let class_tparams =
        TParams::symbolic_from_decl_with(&class.type_params, &class.type_param_bounds, &resolve)
            .alpha_renamed_declaration(
                &class.type_params,
                checker.compilation_id,
                file_index,
                class.span.lo,
            );
    let dispatch = Ty::obj_args_name(
        internal,
        &class
            .type_params
            .iter()
            .map(|name| class_tparams.bound(name))
            .collect::<Vec<_>>(),
    );
    let root = CheckerScope::root();
    let dispatch_scope = root.dispatch_receiver_child(dispatch);
    let scope = &dispatch_scope;
    scope.declare_tparams(&class.type_params, &class_tparams, |_| false);
    checker
        .this_labels
        .extend(class_receiver_labels(class, file, table, Some(dispatch)));
    let mut properties = checker.scoped_properties(scope, internal);
    let entry_properties = checker.enclosing_enum_entry_properties(scope, class, &properties);
    properties.extend(entry_properties);
    checker.declare_enclosing_enum_entry_methods(scope, class);
    for property in &properties {
        checker.declare_scoped_property(scope, property, true);
    }
    // The method's OWN type parameters go on their own rung first: an extension receiver may mention
    // them (`fun <T> T.first()`), and resolving the receiver before they are declared erases it to
    // its bound. Only then is the receiver typed and bound as `this`, with the class's dispatch
    // receiver behind it — which is what lets `fun Foo.name() = this.length` inside a class answer
    // from the declaration instead of declining.
    let rung = scope.child(ScopeKind::Function { receiver: None });
    let rung = &rung;
    rung.declare_tparams(
        &method.type_params,
        &class_tparams
            .symbolic_extended_with(&method.type_params, &method.type_param_bounds, &resolve)
            .alpha_renamed_declaration(
                &method.type_params,
                checker.compilation_id,
                file_index,
                method.signature_span.lo,
            ),
        |name| method.reified_type_params.contains(name),
    );
    let extension_receiver = method
        .receiver
        .as_ref()
        .map(|receiver| checker.type_ref_ty(rung, receiver));
    if let Some(receiver) = extension_receiver {
        checker.this_labels.push((
            method.name.clone(),
            receiver,
            false,
            Some(ReceiverDeclarationRole::Extension(
                method
                    .receiver
                    .as_ref()
                    .expect("extension receiver declared")
                    .span,
            )),
        ));
    }
    // The scope `check_method` is handed carries the DISPATCH receiver, not the extension one: it
    // pushes the extension receiver itself, and it reads `scope.this_ty()` BEFORE doing so to decide
    // which table the inferred return belongs in. Handing it a scope whose `this` is already the
    // extension receiver made that read the receiver — a type PARAMETER for
    // `fun <T : Any> T.self()` — so the return was recorded in neither table and the declaration
    // settled to `Unit`.
    let method_scope = rung.dispatch_receiver_child(dispatch);
    let method_scope = &method_scope;
    let outer = std::mem::replace(&mut checker.symbolic_signature_inference, true);
    checker.check_method(method_scope, method, &properties, None, None);
    checker.symbolic_signature_inference = outer;
    let incomplete = checker.inference_incomplete;
    // Keyed by the method that was ASKED for. `check_method` records every return it computed while
    // walking, so taking whichever entry happened to be first answered a different declaration.
    //
    // A member EXTENSION records into its own table — the ordinary methods are keyed by name and
    // parameters, an extension by its receiver as well — so both are read here. `class.methods`
    // holds either kind, and which table the answer landed in is a property of the declaration,
    // not of the question being asked.
    let inferred = checker
        .inferred_method_rets
        .iter()
        .find(|((declaring, name, _), _)| *declaring == internal && *name == method.name)
        .map(|(_, ret)| *ret)
        .or_else(|| {
            // A member EXTENSION records into its own table, keyed by its RECEIVER rather than by
            // the class that declares it, so the owner is not the discriminator here — the receiver
            // is, and it is what tells two same-spelled extensions apart.
            //
            // The recorded receiver is the one `check_method` resolved in its own scope, which for
            // a GENERIC receiver (`fun <T : Any> T.self()`) is a type parameter under a different
            // identity than the one minted here. Exact identity is preferred; failing that, the
            // ERASURE decides, which is what the extension tables themselves are keyed by.
            let receiver = extension_receiver?;
            let recorded_for = |matches: &dyn Fn(Ty) -> bool| {
                checker
                    .inferred_member_ext_fun_rets
                    .iter()
                    .find(|((_, name, recorded, _), _)| *name == method.name && matches(*recorded))
                    .map(|(_, ret)| *ret)
            };
            recorded_for(&|recorded| recorded == receiver).or_else(|| {
                recorded_for(&|recorded| erased_type_key(recorded) == erased_type_key(receiver))
            })
        })
        .unwrap_or(Ty::Error);
    drop(checker);
    match incomplete || inferred == Ty::Error || inferred.mentions_pending() {
        true => Ty::Error,
        false => inferred,
    }
}

/// Type a top-level declaration by running the checker over the DECLARATION, not over a scope
/// rebuilt by hand.
///
/// `check_property` is what establishes a property's checking context — its type parameters, its
/// context parameters, the backing-field binding, and the postponed-inference frame a lambda
/// argument resolves against. A scope assembled from a list of `(name, type)` bindings has none of
/// that, which is why `val derived = base.map { it + 1 }` could not be typed that way: the lambda
/// had no frame to be postponed into. Running the declaration and then reading the type the checker
/// recorded for its body is the same thing the reference compiler does when it jumps to an
/// implicitly-typed declaration.
fn infer_declaration_ty(
    file: &File,
    file_index: u32,
    declaration: DeclId,
    expression: ExprId,
    table: &SymbolTable,
    engine: EngineSeams<'_>,
) -> Ty {
    let EngineSeams {
        demand,
        demand_call,
        demand_member,
        rejects,
    } = engine;
    let Decl::Property(property) = file.decl(declaration) else {
        return Ty::Error;
    };
    let mut scratch = DiagSink::new();
    let mut checker = make_checker(file, file_index, None, table, &mut scratch);
    checker.demand_name = Some(&demand);
    checker.demand_call = Some(&demand_call);
    checker.demand_member = Some(&demand_member);
    checker.demand_rejects = Some(&rejects);
    checker.inference_only = true;
    checker.symbolic_signature_inference = true;
    checker.anonymous_lexical_scope = anonymous_lexical_class_scope(file);
    checker.set_anonymous_lexical_class_context(declaration);
    let root = CheckerScope::root();
    checker.check_property(&root, property, declaration);
    let inferred = checker
        .expr_types
        .get(expression.0 as usize)
        .copied()
        .unwrap_or(Ty::Error);
    // A CALLABLE REFERENCE records its type beside the expression table rather than in it, so the
    // expression itself is still the marker here. Reading only `expr_types` published that marker as
    // the declaration's type, and a marker must never reach `@Metadata` — `val f = C::foo` aborted
    // emission outright. The reference's own recorded type is the answer, and it is the same one the
    // walk publishes for this shape anywhere else.
    let inferred = match inferred == Ty::Pending {
        true => checker
            .callable_reference_types
            .get(&expression)
            .copied()
            .unwrap_or(inferred),
        false => inferred,
    };
    // A CALL of a member function whose own return is still being determined types as the marker:
    // the call read the member's recorded return rather than asking for it. The member READ path
    // asks the engine in exactly this situation; a call has to ask too. The receiver's type is what
    // names the owner, and the callee spells the member.
    let inferred = match (inferred == Ty::Pending, file.expr(expression)) {
        (true, Expr::Call { callee, .. }) => match file.expr(*callee) {
            Expr::Member { receiver, name } => checker
                .expr_types
                .get(receiver.0 as usize)
                .copied()
                .filter(|receiver| *receiver != Ty::Error && !receiver.mentions_pending())
                .and_then(|owner| (engine.demand_member)(owner, name, &[]))
                .filter(|answered| !answered.mentions_pending())
                .unwrap_or(inferred),
            _ => inferred,
        },
        _ => inferred,
    };
    // The reference's recorded type can itself still carry the marker: `C::bar` is
    // `KProperty1<C, <not determined>>` while `bar` is being determined, because the reference read
    // the member's record rather than asking for it. The member READ path asks the engine in exactly
    // this situation; asking here covers the reference without teaching every reference site to. The
    // reference's own classifier names the owner, and the expression names the member.
    let inferred = match (inferred.mentions_pending(), file.expr(expression)) {
        (true, Expr::CallableRef { receiver, name }) => receiver
            // A BOUND reference (`C()::bar`) names its owner with the receiver EXPRESSION, and its
            // classifier carries only the member's own type — `KProperty0<T>` — so the type
            // arguments cannot supply the owner the way an unbound `C::bar` does.
            .and_then(|receiver| checker.expr_types.get(receiver.0 as usize).copied())
            .filter(|receiver| *receiver != Ty::Error && !receiver.mentions_pending())
            // An UNBOUND reference spells a classifier, which is not a value: its receiver
            // expression has no type, and the owner comes from the classifier instead.
            .or_else(|| inferred.type_args().first().copied())
            // A FUNCTION reference is a `Ty::Fun`, which carries no type arguments at all, so
            // neither route above names an owner. The receiver still SPELLS the classifier —
            // `C::foo` — and that spelling is the owner.
            .or_else(|| {
                let Some(Expr::Name(spelling)) = receiver.map(|receiver| file.expr(receiver))
                else {
                    return None;
                };
                checker
                    .same_package_class(spelling)
                    .map(ClassSig::internal_name)
                    .map(Ty::obj_name)
            })
            .and_then(|owner| (engine.demand_member)(owner, name, &[]))
            .filter(|answered| !answered.mentions_pending())
            .map_or(inferred, |answered| {
                crate::types::ty_replace_pending(inferred, answered)
            }),
        _ => inferred,
    };
    // A declaration whose body has no value-producing path still cannot publish a type. Ordinary
    // expression joins already compute their semantic least upper bound and need no second policy.
    if declaration_body_is_unpublishable(file, expression, &checker.expr_types) {
        return Ty::Error;
    }
    // `checker.inference_incomplete` is deliberately NOT a decline on its own. It is FILE-global —
    // it records that this run met a classifier collection had not reached yet — while the question
    // here is about ONE declaration, which need not mention that classifier at all. The evidence
    // that this declaration was answered is its own type: a complete one was determined, and one
    // still carrying the marker was not.
    if inferred.mentions_pending() {
        return Ty::Error;
    }
    let inferred = withheld_construction_ty(file, expression, &checker.expr_types, table, inferred);
    // The declaration's own body is what decides. Diagnostics from this run are discarded: the
    // authoritative check reports on the declaration later, and reporting here would duplicate
    // every message or, worse, report one the demand hook was about to make moot.
    inferred
}

/// Type one declaration's body with the REAL checker.
///
/// This is the "jump" the resolution engine performs: an implicitly-typed declaration gets its type
/// by running ordinary body resolution for it, exactly as the reference compiler does
/// (`ReturnTypeCalculatorWithJump`). There is no second, reduced expression grammar — that is what
/// made the signature pass and the checker disagree, an elvis or a lambda's result type being typed
/// by one and not the other.
///
/// `demand` and `demand_member` are the engine's seams: a name, or a member read, whose declaration
/// has not been typed yet resolves through them instead of reading the placeholder the symbol table
/// still holds. Diagnostics go to a scratch sink — this run exists to produce a type, and the
/// authoritative check reports on the declaration later.
fn infer_lit_ty_scoped_on_demand(
    source: InferenceSource<'_>,
    e: ExprId,
    props: &[(String, Ty, bool)],
    table: &SymbolTable,
    demand: &dyn Fn(&str) -> Option<Ty>,
    demand_member: DemandMember<'_>,
) -> Ty {
    let InferenceSource(file, file_index, implicit_receiver) = source;
    let mut scratch = DiagSink::new();
    let mut checker = make_checker(file, file_index, None, table, &mut scratch);
    checker.demand_name = Some(&demand);
    checker.demand_member = Some(&demand_member);
    checker.inference_only = true;
    checker.anonymous_lexical_scope = anonymous_lexical_class_scope(file);
    // Signature inference wants a declaration's type in terms of its own type parameters, not their
    // erasures: a member of `Holder<T>` initialized from `T` is `T`, and erasing it here would write
    // the bound into the field descriptor.
    checker.symbolic_signature_inference = false;

    let root = CheckerScope::root();
    let declaration = match implicit_receiver {
        Some(InferenceReceiver::Singleton(classifier)) => {
            root.singleton_receiver_child(Some(Ty::obj_name(classifier)))
        }
        Some(InferenceReceiver::Extension { ty, declaration }) => root
            .declaration_function_child_with_context(
                Some(ty),
                Some((declaration, String::new())),
                &[],
            ),
        None => root.child(ScopeKind::Block),
    };
    let scope = if implicit_receiver.is_some() {
        &declaration
    } else {
        &root
    };
    if let Some(receiver) = implicit_receiver {
        let (ty, role) = match receiver {
            InferenceReceiver::Singleton(classifier) => (Ty::obj_name(classifier), None),
            InferenceReceiver::Extension { ty, declaration } => {
                (ty, Some(ReceiverDeclarationRole::Extension(declaration)))
            }
        };
        checker.this_labels.push((String::new(), ty, false, role));
        if let InferenceReceiver::Extension { declaration, .. } = receiver {
            checker
                .extension_receiver_labels
                .push((checker.this_labels.len() - 1, declaration));
        }
    }
    for (name, ty, is_var) in props {
        checker.declare(scope, name, *ty, *is_var);
    }
    // An initializer is not a function body: a `return` in it does not belong to any function, and
    // giving it one would type `val a = return` instead of rejecting it.
    let inferred = checker.with_ret_allowed(Ty::Unit, false, |checker| checker.expr(scope, e));
    let declines = checker.inference_incomplete
        || declaration_body_is_unpublishable(file, e, &checker.expr_types);
    drop(checker);
    if scratch.has_errors() || declines {
        return Ty::Error;
    }
    inferred
}

pub(super) fn unify_ty_common(
    shape: Ty,
    actual: Ty,
    tparams: &[crate::types::TyParamName],
    binds: &mut crate::types::TyParamBindings,
    join: &dyn Fn(Ty, Ty) -> Ty,
) {
    match (shape.kind(), actual.kind()) {
        (TyKind::TyParam(name, _) | TyKind::Var(name, _), _) if tparams.contains(&name) => {
            binds
                .entry(crate::types::TyParamName::new(&name))
                .and_modify(|bound| {
                    if *bound != actual {
                        *bound = join(*bound, actual);
                    }
                })
                .or_insert(actual);
        }
        (TyKind::Fun(shape), TyKind::Fun(actual)) => {
            for (parameter, argument) in shape.params.iter().zip(&actual.params) {
                unify_ty_common(*parameter, *argument, tparams, binds, join);
            }
            unify_ty_common(shape.ret, actual.ret, tparams, binds, join);
        }
        (TyKind::Obj(_, parameters), TyKind::Obj(_, arguments)) => {
            for (parameter, argument) in parameters.iter().zip(arguments) {
                unify_ty_common(*parameter, *argument, tparams, binds, join);
            }
        }
        (TyKind::Nullable(shape), TyKind::Nullable(actual)) => {
            unify_ty_common(shape, actual, tparams, binds, join);
        }
        _ => {}
    }
}

pub(super) fn is_function_property_shape(ty: Ty) -> bool {
    ty.is_fun() || matches!(ty.kind(), TyKind::Nullable(inner) if inner.is_fun())
}
