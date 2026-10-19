//! Field order of a local or anonymous class's captures across repeated checks of its body.
//!
//! Postponed generic-lambda checking may revisit the same construction while one receiver type is
//! temporarily `Pending`. A provisional revisit must neither replace the exact symbolic type
//! recorded by the earlier check nor renumber an established capture field: descendant
//! constructions can already carry one of these ordinals as their resolved `ClassStorage` source.
//! The final check takes the order it selected (kotlinc's first-use order) and returns the field
//! permutation, so direct descendants update their exact storage coordinates instead of the class
//! body being checked again. This table crosses the retained-inline boundary and no pending
//! semantic type may reach checked FIR.
//!
//! Field ordinals are identities, not the physical layout. Once a local class's body has been
//! checked, its lexical-value captures also get kotlinc's first-use order, computed from the
//! storage identities that body recorded. The order is a recorded fact the JVM backend applies;
//! every earlier phase keeps field order.

use super::context_capture::context_argument_sites;
use super::scope::PathRoot;
use super::CheckerScope;
use super::{
    AnonymousObjectCapture, AnonymousObjectCaptureSource, ExprLowering, Local,
    ReceiverFnValueOrigin, ResolvedCall, SelectedContextSources, Ty,
};
use crate::ast::{
    ClassDecl, ClassInit, CtorDelegation, Decl, DeclId, Expr, ExprId, FunBody, FunDecl, Stmt,
    StmtId,
};
use crate::diag::Span;
use crate::fx_hash::{HashMap, HashSet};

/// Reconcile `selected` with the fields a previous check `established`. Returns the capture list
/// and, for each established field, its new ordinal (`None` when the capture was dropped).
pub(super) fn reconcile(
    established: Option<&[AnonymousObjectCapture]>,
    mut selected: Vec<AnonymousObjectCapture>,
    provisional: bool,
) -> (Vec<AnonymousObjectCapture>, Vec<Option<u32>>) {
    let Some(established) = established else {
        return (selected, Vec::new());
    };
    let mut field_remap = vec![None; established.len()];
    if !provisional {
        for (field, exact) in established.iter().enumerate() {
            if let Some(position) = selected
                .iter()
                .position(|capture| same_capture(capture, exact))
            {
                keep_established_facts(&mut selected[position], exact);
                field_remap[field] = u32::try_from(position).ok();
            }
        }
        return (selected, field_remap);
    }
    let mut pending = selected;
    let mut selected = Vec::with_capacity(established.len().max(pending.len()));
    for (field, exact) in established.iter().enumerate() {
        let Some(position) = pending
            .iter()
            .position(|capture| same_capture(capture, exact))
        else {
            field_remap[field] = u32::try_from(selected.len()).ok();
            selected.push(exact.clone());
            continue;
        };
        let mut capture = pending.remove(position);
        keep_established_facts(&mut capture, exact);
        field_remap[field] = u32::try_from(selected.len()).ok();
        selected.push(capture);
    }
    selected.extend(pending);
    (selected, field_remap)
}

fn same_capture(capture: &AnonymousObjectCapture, exact: &AnonymousObjectCapture) -> bool {
    capture.name == exact.name && capture.source == exact.source
}

/// A capture keeps its established cell sharing, and its established type while the revisit only
/// knows a pending one.
fn keep_established_facts(capture: &mut AnonymousObjectCapture, exact: &AnonymousObjectCapture) {
    capture.shared_cell |= exact.shared_cell;
    if (capture.ty.mentions_pending()
        || capture
            .storage_ty
            .is_some_and(|storage| storage.mentions_pending()))
        && !exact.ty.mentions_pending()
        && exact
            .storage_ty
            .is_none_or(|storage| !storage.mentions_pending())
    {
        capture.ty = exact.ty;
        capture.storage_ty = exact.storage_ty;
    }
}

/// Put a local class's used receiver captures in the order its body first used them, as kotlinc
/// does, keeping every other capture in its field. `used` pairs each receiver's first-use position
/// with its capture source.
pub(super) fn order_receivers_by_first_use(
    captures: &mut [AnonymousObjectCapture],
    bindings: &mut [Option<u32>],
    mut used: Vec<(usize, AnonymousObjectCaptureSource)>,
) {
    used.sort_by_key(|(first_use, _)| *first_use);
    let fields = captures
        .iter()
        .enumerate()
        .filter(|(_, capture)| used.iter().any(|(_, source)| *source == capture.source))
        .map(|(field, _)| field)
        .collect::<Vec<_>>();
    let mut ordered = fields
        .iter()
        .map(|&field| (captures[field].clone(), bindings[field]))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(capture, _)| {
        used.iter()
            .position(|(_, source)| *source == capture.source)
    });
    for (field, (capture, binding)) in fields.into_iter().zip(ordered) {
        captures[field] = capture;
        bindings[field] = binding;
    }
}

/// kotlinc's first-use order of a local class's lexical-value captures.
///
/// kotlinc's closure analysis lists the values a closure reads itself, in source order, and then
/// the values of the closures it includes, in the order it meets them. A class's own code is its
/// property initializers, `init` blocks and delegation values; it includes its primary constructor
/// (parameter defaults, then super-constructor arguments, then a local superclass) and then its
/// members, in source order. A lambda or an anonymous object is included where it is written; a
/// local function where it is called or referenced, and a local or inner class where it is
/// constructed. One that is never used is never included by kotlinc and captures nothing there.
/// krusty's capture set still holds what such a declaration reads, so the walk includes it after
/// everything else its closure includes.
///
/// Every use is a resolved identity the checked body recorded: a read or write bound to the
/// class's capture storage, a read or write of a captured delegated local (which keeps its
/// lexical binding), or a capture of an included declaration. One declared inside the class names
/// this class's storage directly; one declared outside it names the lexical binding the class
/// captured for it. Nothing is matched by spelling, so a same-named member (`fun b() = b()`) is
/// not a use of a captured `b`.
///
/// Field identities do not change: `order[position]` is the capture field kotlinc puts at that
/// position of the constructor prefix and the field table. Receivers keep their positions. A
/// capture without a use means the class body was not completely checked in this visit; such a
/// visit publishes no order, and checked FIR rejects a local class whose captures never received
/// one.
impl super::Checker<'_> {
    pub(super) fn publish_local_class_capture_layout(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        declaration: DeclId,
        class: &ClassDecl,
    ) {
        let Some(captures) = self.discovered_local_class_captures.get(&declaration) else {
            return;
        };
        let bindings = self
            .discovered_local_class_capture_bindings
            .get(&declaration)
            .map_or(&[][..], Vec::as_slice);
        // A captured delegated local is not class storage inside the class: its reads and writes
        // keep the lexical binding, identified here by the identity the capture recorded.
        let delegated = captures
            .iter()
            .zip(bindings)
            .enumerate()
            .filter(|(_, (capture, _))| capture.storage_ty.is_some())
            .filter_map(|(field, (capture, binding))| {
                let (local, _) =
                    self.lexical_binding_by_identity(scope, &capture.name, (*binding)?)?;
                Some((local.flow_identity, u32::try_from(field).ok()?))
            })
            .collect();
        let mut uses = FirstUses {
            checker: self,
            statement,
            owner: Owner::Class,
            span: class.span,
            captures,
            bindings,
            delegated,
            fields: Vec::new(),
            included: HashSet::default(),
            context_uses: HashMap::default(),
            unordered: None,
        };
        uses.collect(Closure::Class(class, Some(declaration)));
        if let Some(unordered) = uses.unordered.take() {
            let message = format!("internal error: local class '{}': {unordered}", class.name);
            self.diags.error(class.span, message);
            return;
        }
        // A revisit that does not check the whole body (one for signature defaults) keeps the
        // order of the complete check: established capture fields keep their identities.
        if let Some(order) = first_use_order(captures, &uses.fields) {
            self.class_capture_uses.orders.insert(declaration, order);
        }
    }

    /// Record the lexical identities of a local function's captured values in kotlinc's first-use
    /// order, which a class that includes the function takes them in. The function's own capture
    /// vector, which its lifted parameters and the capture dependencies follow, is left as it is.
    ///
    /// The walk is the class walk's: the function reads its values in source order (parameter
    /// defaults, then the body) and then includes the lambdas, local functions and classifiers it
    /// uses. Every use is the flow identity a read or write of the captured binding recorded, or
    /// the lexical identity an included declaration captured, never a spelling. A capture with no
    /// recorded use or no lexical identity has no first-use position; that is reported, and no
    /// order is published, rather than placing it by name or declaration. `locals` is the exact
    /// binding of each capture in `captured`.
    pub(super) fn record_local_function_first_use(
        &mut self,
        statement: StmtId,
        function: &FunDecl,
        captured: &[(String, Ty, bool)],
        locals: &[Option<Local>],
    ) {
        let bindings = locals
            .iter()
            .map(|local| {
                local
                    .as_ref()
                    .and_then(|local| local.lexical_capture_identity)
            })
            .collect::<Vec<_>>();
        let delegated = locals
            .iter()
            .enumerate()
            .filter_map(|(capture, local)| {
                Some((local.as_ref()?.flow_identity, u32::try_from(capture).ok()?))
            })
            .collect::<Vec<_>>();
        let span = self.file.stmt_spans[statement.0 as usize];
        let context_uses = self.context_argument_captures(function, &delegated);
        let mut uses = FirstUses {
            checker: self,
            statement,
            owner: Owner::Function,
            span,
            captures: &[],
            bindings: &bindings,
            delegated,
            fields: Vec::new(),
            included: std::iter::once(Inclusion::LocalFunction(statement)).collect(),
            context_uses,
            unordered: None,
        };
        uses.collect(Closure::Callable(callable_roots(
            &function.params,
            &function.body,
        )));
        let (first_uses, included_unordered) = (uses.fields, uses.unordered);
        let unordered = (0..captured.len()).find(|&capture| {
            bindings[capture].is_none() || !first_uses.contains(&(capture as u32))
        });
        let order = match (included_unordered, unordered) {
            (Some(unordered), _) => Err(unordered),
            (None, Some(capture)) => Err(UnorderedCapture {
                function: function.name.clone(),
                capture: Some(captured[capture].0.clone()),
            }),
            (None, None) => Ok(first_uses
                .iter()
                .filter_map(|&capture| bindings[capture as usize])
                .collect()),
        };
        self.class_capture_uses
            .function_orders
            .insert(statement, order);
    }

    /// The captures of `function` that each expression in it passes as an implicit context
    /// argument or receiver. `delegated` maps a capture's binding (flow identity) to the capture;
    /// each passed value is the flow identity the checker recorded when it selected the value.
    fn context_argument_captures(
        &self,
        function: &FunDecl,
        delegated: &[(u32, u32)],
    ) -> HashMap<ExprId, Vec<u32>> {
        let roots = callable_roots(&function.params, &function.body);
        let sources = SelectedContextSources {
            expr_types: &self.expr_types,
            implicit_receiver_selections: &self.implicit_receiver_selections,
            context_args: &self.context_args,
            resolved_calls: &self.resolved_calls,
            resolved_super_calls: &self.resolved_super_calls,
            stmt_lowers: &self.stmt_lowers,
        };
        let mut uses = HashMap::<ExprId, Vec<u32>>::default();
        for (site, identity) in context_argument_sites(self.file, sources, &roots) {
            if let Some(&(_, capture)) = delegated.iter().find(|(binding, _)| *binding == identity)
            {
                uses.entry(site).or_default().push(capture);
            }
        }
        uses
    }

    /// Record an assignment to capture storage or to a local's binding.
    pub(super) fn record_class_storage_write(&mut self, statement: StmtId, target: Option<Local>) {
        let target = match target.map(|local| (local.origin, local.flow_identity)) {
            Some((
                ReceiverFnValueOrigin::ClassStorage(field)
                | ReceiverFnValueOrigin::EnumEntryPropertyStorage { field, .. },
                _,
            )) => StorageUse::Field(field),
            Some((ReceiverFnValueOrigin::Local, identity)) => StorageUse::Binding(identity),
            _ => return,
        };
        self.class_capture_uses.writes.insert(statement, target);
    }
}

/// Resolver-only storage-use facts that order local-class captures.
#[derive(Default)]
pub(super) struct ClassCaptureUses {
    /// Assignment statements and what each one writes.
    writes: HashMap<StmtId, StorageUse>,
    /// Published first-use orders, keyed by local classifier.
    pub(super) orders: HashMap<DeclId, Box<[u32]>>,
    /// Each local function's captured lexical identities in first-use order, which a class that
    /// includes the function takes them in, or the capture no recorded use orders.
    function_orders: HashMap<StmtId, Result<Box<[u32]>, UnorderedCapture>>,
}

/// A local function's captured value that no recorded use places. Every capture is a read, write,
/// implicit context argument or included declaration the checker recorded, so this is an internal
/// error; only a class that includes the function needs the order, and it reports this rather than
/// placing the value by name.
#[derive(Clone)]
struct UnorderedCapture {
    function: String,
    /// `None` when the function's order was never recorded at all.
    capture: Option<String>,
}

impl std::fmt::Display for UnorderedCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.capture {
            Some(capture) => write!(
                f,
                "no recorded use orders captured value '{capture}' of local function '{}'",
                self.function
            ),
            None => write!(
                f,
                "local function '{}' has no recorded capture order",
                self.function
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum StorageUse {
    /// Capture storage of the innermost classifier.
    Field(u32),
    /// A local's lexical binding, by flow identity.
    Binding(u32),
}

fn first_use_order(captures: &[AnonymousObjectCapture], uses: &[u32]) -> Option<Box<[u32]>> {
    let values = captures
        .iter()
        .enumerate()
        .filter(|(_, capture)| is_value_capture(capture))
        .map(|(field, _)| field)
        .collect::<Vec<_>>();
    let mut ranked = Vec::with_capacity(values.len());
    for &field in &values {
        let first_use = uses.iter().position(|used| *used as usize == field)?;
        ranked.push((first_use, field));
    }
    ranked.sort_unstable();
    let mut order = (0..captures.len())
        .map(|field| u32::try_from(field).expect("too many local-class captures"))
        .collect::<Box<[u32]>>();
    for (&position, (_, field)) in values.iter().zip(ranked) {
        order[position] = u32::try_from(field).expect("too many local-class captures");
    }
    Some(order)
}

fn is_value_capture(capture: &AnonymousObjectCapture) -> bool {
    matches!(
        capture.source,
        AnonymousObjectCaptureSource::LexicalValue
            | AnonymousObjectCaptureSource::ClassStorage { .. }
    )
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum Inclusion {
    LocalFunction(StmtId),
    Classifier(DeclId),
}

enum Closure<'w> {
    /// A class whose own code is walked: the local class (with its declaration) or an inner class.
    Class(&'w ClassDecl, Option<DeclId>),
    /// A body: parameter defaults, then the body.
    Callable(Vec<ExprId>),
    /// A local function or classifier, included where it is used.
    Included(Inclusion),
}

/// Whose captures a walk orders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// A local class: its captures are its storage fields.
    Class,
    /// A local function: its captures are lexical bindings. Class storage it reaches belongs to
    /// an enclosing class, not to the function.
    Function,
}

/// The captures (local class fields or local function captures) in the order kotlinc's closure
/// analysis first meets them.
struct FirstUses<'w, 'c> {
    checker: &'w super::Checker<'c>,
    /// The local class's or function's statement, which owns a class's inner classes.
    statement: StmtId,
    owner: Owner,
    /// The declaration's source range: a declaration inside it reads the captures directly.
    span: Span,
    captures: &'w [AnonymousObjectCapture],
    /// The lexical binding identity of each capture, parallel to `captures`.
    bindings: &'w [Option<u32>],
    /// Captured delegated locals: binding flow identity and capture field.
    delegated: Vec<(u32, u32)>,
    fields: Vec<u32>,
    included: HashSet<Inclusion>,
    /// The captures each expression passes as an implicit context argument or receiver.
    context_uses: HashMap<ExprId, Vec<u32>>,
    /// An included local function whose captures have no complete first-use order.
    unordered: Option<UnorderedCapture>,
}

/// One closure: what it reads itself, then what it includes, by source position, then the local
/// declarations it holds that nothing included.
struct ClosureUses<'w> {
    direct: Vec<u32>,
    nested: Vec<(u32, Closure<'w>)>,
    declared: Vec<Inclusion>,
}

fn callable_roots(params: &[crate::ast::Param], body: &FunBody) -> Vec<ExprId> {
    params
        .iter()
        .filter_map(|parameter| parameter.default)
        .chain(body_expression(body))
        .collect()
}

fn body_expression(body: &FunBody) -> Option<ExprId> {
    match body {
        FunBody::Expr(body) | FunBody::Block(body) => Some(*body),
        FunBody::None => None,
    }
}

impl<'w> FirstUses<'w, '_> {
    fn collect(&mut self, closure: Closure<'w>) {
        let mut closure_uses = ClosureUses {
            direct: Vec::new(),
            nested: Vec::new(),
            declared: Vec::new(),
        };
        match closure {
            Closure::Class(class, declaration) => self.class(class, declaration, &mut closure_uses),
            Closure::Callable(roots) => {
                for root in roots {
                    self.expression(root, &mut closure_uses);
                }
            }
            Closure::Included(inclusion) => {
                if !self.included.insert(inclusion) {
                    return;
                }
                match inclusion {
                    Inclusion::LocalFunction(statement) => {
                        self.local_function(statement, &mut closure_uses);
                    }
                    Inclusion::Classifier(declaration) => {
                        self.classifier(declaration, &mut closure_uses);
                    }
                }
            }
        }
        closure_uses.nested.sort_by_key(|(position, _)| *position);
        for field in closure_uses.direct {
            if !self.fields.contains(&field) {
                self.fields.push(field);
            }
        }
        for (_, nested) in closure_uses.nested {
            self.collect(nested);
        }
        for declared in closure_uses.declared {
            self.collect(Closure::Included(declared));
        }
    }

    fn position(&self, expression: ExprId) -> u32 {
        self.checker
            .file
            .expr_span(expression)
            .map_or(0, |span| span.lo)
    }

    fn inside_class(&self, span: Span) -> bool {
        self.span.lo <= span.lo && span.hi <= self.span.hi
    }

    /// The field of the capture whose lexical binding is `identity`.
    fn captured_binding(&self, identity: u32) -> Option<u32> {
        let field = self
            .bindings
            .iter()
            .position(|binding| *binding == Some(identity))?;
        u32::try_from(field).ok()
    }

    /// The field of the capture an outside declaration's capture `capture` (with lexical binding
    /// `binding`) is carried by.
    fn carried_capture(
        &self,
        capture: &AnonymousObjectCapture,
        binding: Option<u32>,
    ) -> Option<u32> {
        if let Some(identity) = binding {
            return self.captured_binding(identity);
        }
        let field = self.captures.iter().position(|carried| {
            is_value_capture(carried)
                && carried.source != AnonymousObjectCaptureSource::LexicalValue
                && carried.source == capture.source
        })?;
        u32::try_from(field).ok()
    }

    fn class(
        &mut self,
        class: &'w ClassDecl,
        declaration: Option<DeclId>,
        uses: &mut ClosureUses<'w>,
    ) {
        for step in &class.init_order {
            match step {
                ClassInit::PropInit(index) => {
                    let property = &class.body_props[*index];
                    for expression in property.init.into_iter().chain(property.delegate) {
                        self.expression(expression, uses);
                    }
                }
                ClassInit::Block(body) => self.expression(*body, uses),
            }
        }
        for delegation in &class.interface_delegations {
            self.expression(delegation.value, uses);
        }
        // The primary constructor is the class's first declaration. Its superclass constructor
        // call includes a local superclass.
        let primary = class
            .props
            .iter()
            .filter_map(|property| property.default)
            .chain(class.base_args.iter().copied())
            .collect();
        uses.nested.push((0, Closure::Callable(primary)));
        if let Some(superclass) = declaration.and_then(|declaration| {
            self.checker
                .local_superclass_declaration(declaration, class)
        }) {
            uses.nested
                .push((0, Closure::Included(Inclusion::Classifier(superclass))));
        }
        for method in &class.methods {
            let roots = callable_roots(&method.params, &method.body);
            uses.nested.push((method.span.lo, Closure::Callable(roots)));
        }
        for property in &class.body_props {
            if let Some(body) = property.getter.as_ref().and_then(body_expression) {
                let position = self.position(body);
                uses.nested.push((position, Closure::Callable(vec![body])));
            }
            if let Some(setter) = &property.setter {
                if let Some(body) = setter.body.as_ref().and_then(body_expression) {
                    uses.nested
                        .push((setter.span.lo, Closure::Callable(vec![body])));
                }
            }
        }
        for constructor in &class.secondary_ctors {
            let delegation = match &constructor.delegation {
                CtorDelegation::None => &[][..],
                CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
            };
            let roots = constructor
                .params
                .iter()
                .filter_map(|parameter| parameter.default)
                .chain(delegation.iter().copied())
                .chain(constructor.body)
                .collect();
            uses.nested
                .push((constructor.span.lo, Closure::Callable(roots)));
        }
        // An inner class is a local declaration of the class, included where it is constructed.
        let file = self.checker.file;
        for &declaration in file
            .local_class_nested
            .get(&self.statement)
            .into_iter()
            .flatten()
        {
            if matches!(file.decl(declaration), Decl::Class(inner) if inner.inner_of.is_some()) {
                uses.declared.push(Inclusion::Classifier(declaration));
            }
        }
    }

    /// An included local function. One declared inside the class reads its storage directly; one
    /// declared outside it uses the values the class captured for it.
    fn local_function(&mut self, statement: StmtId, uses: &mut ClosureUses<'w>) {
        let checker = self.checker;
        let Stmt::LocalFun(function) = checker.file.stmt(statement) else {
            return;
        };
        // A function nested in the walked function has its own recorded order, which includes
        // the values it passes implicitly; one inside a class reads the class's storage.
        if matches!(self.owner, Owner::Class)
            && self.inside_class(checker.file.stmt_spans[statement.0 as usize])
        {
            for root in callable_roots(&function.params, &function.body) {
                self.expression(root, uses);
            }
            return;
        }
        match checker.class_capture_uses.function_orders.get(&statement) {
            Some(Ok(order)) => {
                for identity in order {
                    uses.direct.extend(self.captured_binding(*identity));
                }
            }
            Some(Err(unordered)) => {
                self.unordered.get_or_insert_with(|| unordered.clone());
            }
            // Not yet checked: nothing records its order, so it is reported like a missing use.
            None => {
                self.unordered.get_or_insert_with(|| UnorderedCapture {
                    function: function.name.clone(),
                    capture: None,
                });
            }
        }
    }

    /// An included classifier: an inner class's code reads this class's storage directly; any
    /// other classifier contributes its captures, in its first-use order.
    fn classifier(&mut self, declaration: DeclId, uses: &mut ClosureUses<'w>) {
        let checker = self.checker;
        let file = checker.file;
        let Decl::Class(class) = file.decl(declaration) else {
            return;
        };
        let captures = checker
            .discovered_local_class_captures
            .get(&declaration)
            .or_else(|| checker.discovered_anonymous_captures.get(&declaration));
        let Some(captures) = captures else {
            let inner = file
                .local_class_nested
                .get(&self.statement)
                .is_some_and(|nested| nested.contains(&declaration));
            if inner {
                self.class(class, None, uses);
            }
            return;
        };
        let bindings = checker
            .discovered_local_class_capture_bindings
            .get(&declaration)
            .map_or(&[][..], Vec::as_slice);
        let mut physical = captures.iter().enumerate().collect::<Vec<_>>();
        if let Some(order) = checker.class_capture_uses.orders.get(&declaration) {
            physical.sort_by_key(|(field, _)| {
                order.iter().position(|ordered| *ordered as usize == *field)
            });
        }
        let inside = file
            .anonymous_object_classes
            .values()
            .any(|object| *object == declaration)
            || self.inside_class(class.span);
        for (field, capture) in physical {
            let carried = if inside {
                match capture.source {
                    AnonymousObjectCaptureSource::ClassStorage { field } => {
                        (self.owner == Owner::Class).then_some(field)
                    }
                    // A value the class captured, which the nested classifier captures again.
                    _ => bindings
                        .get(field)
                        .copied()
                        .flatten()
                        .and_then(|identity| self.captured_binding(identity)),
                }
            } else {
                self.carried_capture(capture, bindings.get(field).copied().flatten())
            };
            uses.direct.extend(carried);
        }
    }

    /// The capture field a recorded use names, if it is one of this class's captures.
    fn storage_use(&self, used: StorageUse) -> Option<u32> {
        match used {
            StorageUse::Field(field) => (self.owner == Owner::Class).then_some(field),
            StorageUse::Binding(identity) => self
                .delegated
                .iter()
                .find(|(binding, _)| *binding == identity)
                .map(|(_, field)| *field),
        }
    }

    fn expression(&mut self, expression: ExprId, uses: &mut ClosureUses<'w>) {
        let checker = self.checker;
        let file = checker.file;
        let position = self.position(expression);
        if let Some(&object) = file.anonymous_object_classes.get(&expression) {
            // A forwarded super-constructor argument runs at the construction site.
            for &argument in checker
                .discovered_anonymous_super_forwards
                .get(&object)
                .into_iter()
                .flatten()
            {
                self.expression(argument, uses);
            }
            let object = Closure::Included(Inclusion::Classifier(object));
            uses.nested.push((position, object));
            return;
        }
        let read = match checker.expr_lowers.get(&expression) {
            Some(ExprLowering::ClassStorageRead { field }) => Some(StorageUse::Field(*field)),
            _ => match checker.read_flow_roots.get(&expression) {
                Some(PathRoot::Value(identity)) => Some(StorageUse::Binding(*identity)),
                _ => None,
            },
        };
        uses.direct
            .extend(read.and_then(|read| self.storage_use(read)));
        // An implicit context argument is a use of the value it passes, where the call is.
        if let Some(captures) = self.context_uses.get(&expression) {
            uses.direct.extend(captures.iter().copied());
        }
        let function = match checker.expr_lowers.get(&expression) {
            Some(
                ExprLowering::LocalFunction { stmt_id, .. }
                | ExprLowering::AdaptedLocalFunctionRef { stmt_id, .. },
            ) => Some(*stmt_id),
            _ => match checker.resolved_calls.get(&expression) {
                Some(ResolvedCall::LocalFunction(call)) => Some(call.stmt_id),
                _ => None,
            },
        };
        if let Some(function) = function {
            let function = Closure::Included(Inclusion::LocalFunction(function));
            uses.nested.push((position, function));
        }
        if let Some(classifier) = checker
            .resolved_constructors
            .get(&expression)
            .and_then(|constructor| checker.constructed_local_classifier(constructor))
        {
            let classifier = Closure::Included(Inclusion::Classifier(classifier));
            uses.nested.push((position, classifier));
        }
        if let Expr::Lambda { body, .. } = file.expr(expression) {
            uses.nested.push((position, Closure::Callable(vec![*body])));
            return;
        }
        // The child walk takes two closures at once, so they share the list through a `RefCell`.
        let children = std::cell::RefCell::new(Vec::new());
        file.any_child_expr(
            expression,
            &mut |child| {
                children.borrow_mut().push(Ok(child));
                false
            },
            &mut |statement| {
                children.borrow_mut().push(Err(statement));
                false
            },
        );
        for child in children.into_inner() {
            match child {
                Ok(child) => self.expression(child, uses),
                Err(statement) => self.statement(statement, uses),
            }
        }
    }

    fn statement(&mut self, statement: StmtId, uses: &mut ClosureUses<'w>) {
        let checker = self.checker;
        // A local function or class is included where it is used, not where it is declared.
        match checker.file.stmt(statement) {
            Stmt::LocalFun(_) => {
                uses.declared.push(Inclusion::LocalFunction(statement));
                return;
            }
            Stmt::LocalClass(_) => {
                if let Some(&declaration) = checker.file.local_class_decls.get(&statement) {
                    uses.declared.push(Inclusion::Classifier(declaration));
                }
                return;
            }
            _ => {}
        }
        if let Some(&written) = checker.class_capture_uses.writes.get(&statement) {
            uses.direct.extend(self.storage_use(written));
        }
        let mut children = Vec::new();
        checker.file.any_child_stmt(statement, &mut |child| {
            children.push(child);
            false
        });
        for child in children {
            self.expression(child, uses);
        }
    }
}
