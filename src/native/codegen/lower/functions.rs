//! Function values: lambdas, the objects that carry their captures, and calling one.
//!
//! A lambda is an object, and that is not an analogy — it is allocated by the same collector,
//! described by the same `KType`, and dispatched through the same vtable as any class here. Common
//! lowering has already done the hard half: the body is a synthesized top-level function
//! ([`IrExpr::Lambda::impl_fn`]) whose LEADING parameters are the captured values and whose
//! trailing ones are the lambda's own. So a function value is a small object holding the captures,
//! and calling it is a vtable dispatch to a thunk that unpacks them and calls that function.
//!
//! **The thunk's signature is Kotlin's own.** On the JVM a lambda is a `FunctionN` whose `invoke`
//! takes and returns `Object`; the IR says so where it describes [`IrExpr::InvokeFunction`]
//! ("arguments are boxed to `Object`; the `Object` result is cast/unboxed to `ret`"). The same
//! convention is what a native call site needs, and for the same reason: the site knows the
//! function's arity but not which lambda it holds, so every function value of a given arity must
//! be callable one way. The thunk converts at the boundary — unbox each argument to what the body
//! declares, box the result — and the body itself is compiled with its real types.
//!
//! **A captured `var` is a holder.** Common lowering boxes a mutable local a closure captures into
//! a [`IrExpr::RefNew`] holder and rewrites reads and writes as [`IrExpr::RefGet`]/[`IrExpr::RefSet`],
//! so that the closure and the enclosing frame share one cell rather than a copy. That is a
//! one-field object here, with the field traced when it holds a reference.

use super::objects::trusted;
use super::*;

/// The vtable slot a function value's body occupies: right after `kotlin.Any`'s three, so a
/// function value answers `equals`/`hashCode`/`toString` like any other object.
const INVOKE_SLOT: u32 = 3;

/// A site that creates a function value. A lambda and a callable reference differ in how they are
/// written and in nothing else that matters here: each names a function holding the body and a list
/// of values to carry, and `obj::method`'s bound receiver is simply the first of those. Common
/// lowering has already synthesized the adapter that calls the referenced function, so a reference
/// is a lambda whose body someone else wrote.
struct Site {
    impl_fn: u32,
    captures: Vec<u32>,
    /// The `fun interface` this lambda was converted to, when it was. A SAM conversion is still a
    /// function value carrying captures; what differs is the TYPE it wears, and therefore which
    /// vtable a caller dispatches through.
    sam: Option<crate::ir::IrSamTarget>,
    /// Set for a CALLABLE REFERENCE: the declaration it names, which is what equality compares.
    /// A lambda leaves it unset and keeps identity equality, which is what Kotlin gives one.
    identity: Option<String>,
    /// Which capture holds the value EQUALITY reads — a reference's bound receiver, or the
    /// function a SAM delegate wraps — when one does. The descriptor records where it lands so
    /// the runtime does not have to guess it is the first reference-typed field: a reference to a
    /// local function carries that function's ordinary captures as reference fields too.
    receiver_capture: Option<usize>,
}

/// The site an expression creates, or `None` when it creates none.
fn closure_site(expr: &IrExpr) -> Option<Result<Site, Unsupported>> {
    match expr {
        // The declared arity is deliberately not read here: the implementation function's own
        // parameters are the source of truth, and a suspend lambda's body carries a continuation
        // the arity does not count.
        IrExpr::Lambda {
            impl_fn,
            captures,
            sam,
            ..
        } => Some(Ok(Site {
            impl_fn: *impl_fn,
            captures: captures.clone(),
            sam: sam.clone(),
            // A SAM delegate's identity depends on WHAT IT WRAPS, which needs the arena to see —
            // so it and the flag below are decided in `declare_lambdas` rather than here.
            identity: None,
            receiver_capture: None,
        })),
        IrExpr::CallableReference(reference) => Some(if reference.declaration_suspend {
            Err("a suspend callable reference".to_string())
        } else {
            Ok(Site {
                impl_fn: reference.adapter,
                // The adapter's frame is the referenced declaration's captures, then the bound
                // receiver, then the parameters the caller supplies — see
                // `fir_lower::local_callables`, which counts `own_start` in exactly that order.
                // A reference with no captures of its own is the only shape where the two orders
                // coincide, and it used to be the only shape emitted with this pair.
                captures: reference
                    .captures
                    .iter()
                    .copied()
                    .chain(reference.bound_receiver)
                    .collect(),
                // A callable reference converted to a `fun interface` arrives as a lambda with a
                // SAM target, not as a reference, so there is none to carry here.
                sam: None,
                identity: Some(reference_identity(reference)),
                receiver_capture: reference
                    .bound_receiver
                    .is_some()
                    .then(|| reference.captures.len()),
            })
        }),
        _ => None,
    }
}

/// A callable reference's identity for EQUALITY: which declaration it names, and whether a
/// receiver is bound to it.
///
/// Two `Foo::bar` written in two places are different objects with different descriptors, and
/// Kotlin says they are equal — so this is the thing they must share. `foo::bar` gets a different
/// one from `Foo::bar` for the opposite reason: those must not be equal. Derived from the IR's
/// `target`, which is the declaration's own identity rather than anything about the site.
fn reference_identity(reference: &crate::ir::IrCallableReference) -> String {
    use crate::ir::IrCallableReferenceTarget as Target;
    let named = match &reference.target {
        Target::Module(callable) => format!("m{}", model::c_identifier(&format!("{callable:?}"))),
        Target::Constructor { classifier } => {
            format!("c{}", model::c_identifier(&classifier.render()))
        }
        Target::Local { owner, name } => format!(
            "l{}_{}",
            model::c_identifier(&owner.map(|o| o.render()).unwrap_or_default()),
            model::c_identifier(name)
        ),
        // The provider's own identity for the declaration, which is what two `Boolean::not`
        // written in two files share — and the only thing about it this side is entitled to read.
        Target::External { declaration } => format!("e{}", declaration.raw()),
    };
    let bound = if reference.bound_receiver.is_some() {
        "b"
    } else {
        "u"
    };
    format!("kt_refid_{bound}_{named}")
}

/// The emitted pieces of one lambda site.
pub(super) struct LambdaItems {
    descriptor: DataId,
    instance_size: u32,
    /// Byte offset of each capture, parallel to the site's `captures`.
    capture_offsets: Vec<u32>,
    /// A lambda that captures nothing is ONE object, not one per evaluation: `{}` has the same
    /// `hashCode` every time it is written, which a program can see. With no fields to hold there
    /// is nothing to allocate, so the instance is static storage — the collector never sees it,
    /// and a call site costs an address rather than a collection.
    singleton: Option<DataId>,
}

/// The types a function's parameters are CARRIED as, which are not always the types it declares.
///
/// A `var` that a closure captures is replaced by a holder, and what is passed to the lambda's body
/// is that cell — but the parameter still says `Int`, because `Int` is what the programmer wrote.
/// Believing the declaration instead truncates a pointer into a 32-bit parameter, which is a
/// miscompile with no symptom where it happens.
///
/// Common lowering RECORDS which parameters carry a holder, in `shared_capture_parameters`, and
/// that record is the answer. The body scan below is a second, weaker source for the same fact —
/// a body that reaches a holder through [`IrExpr::RefGet`]/[`IrExpr::RefSet`] is holding one. It
/// cannot see a parameter the body only PASSES ON: a lambda that does nothing with the cell but
/// hand it to an object it constructs dereferences it nowhere, so the scan alone typed that
/// parameter `Int` and the thunk loaded a pointer as an `i32`. Both are consulted because the
/// record is authoritative and the scan costs nothing; neither can wrongly claim a parameter is a
/// holder, only miss one.
pub(super) fn carried_parameters(ir: &IrFile, id: crate::ir::FunId) -> Vec<Ty> {
    let function = &ir.functions[id as usize];
    let Some(body) = function.body else {
        return function.params.clone();
    };
    // `this` occupies slot 0 when there is one, so a parameter's slot is offset by it.
    let first = usize::from(function.dispatch_receiver.is_some());
    let mut holders = vec![false; function.params.len() + first];
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let IrExpr::RefGet { holder, .. } | IrExpr::RefSet { holder, .. } = ir.expr(id) {
            if let IrExpr::GetValue(slot) = ir.expr(*holder) {
                if let Some(flag) = holders.get_mut(*slot as usize) {
                    *flag = true;
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, id, &mut |child| pending.push(child));
    }
    function
        .params
        .iter()
        .enumerate()
        .map(|(index, declared)| {
            let ordinal = u32::try_from(index).expect("too many parameters");
            if holders[index + first] || ir.shared_capture_parameters.contains_key(&(id, ordinal)) {
                any()
            } else {
                *declared
            }
        })
        .collect()
}

/// Lay out values after the object header, each aligned to its own size — the same rule the class
/// model applies to fields, for the same reason: the collector is told where the references are
/// and must be told the truth.
fn layout(types: &[Ty]) -> (Vec<u32>, u32, Vec<u32>) {
    let mut offsets = Vec::with_capacity(types.len());
    let mut references = Vec::new();
    let mut end = model::HEADER_SIZE;
    for ty in types {
        let size = carrier(*ty).clif().map_or(8, |clif| clif.bytes());
        let offset = end.next_multiple_of(size);
        if carrier(*ty) == Carrier::Ref {
            references.push(offset);
        }
        offsets.push(offset);
        end = offset + size;
    }
    (offsets, end.next_multiple_of(8), references)
}

impl<'a> FileLowering<'a> {
    /// The marker two callable references to the same declaration share, defined once per file.
    ///
    /// Its ADDRESS is the identity; nothing ever reads its contents. One byte, because a symbol
    /// needs storage to have an address at all.
    fn reference_identity_marker(&mut self, name: &str) -> Result<DataId, Unsupported> {
        if let Some(existing) = self.reference_identities.get(name) {
            return Ok(*existing);
        }
        let id = self.declare_local_data(name, false)?;
        let mut description = DataDescription::new();
        description.define(vec![0u8; 1].into_boxed_slice());
        description.set_align(1);
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining `{name}` ({error})"))?;
        self.reference_identities.insert(name.to_string(), id);
        Ok(id)
    }

    /// Declare a type and a thunk for every lambda in the file, before any body is compiled.
    pub(super) fn declare_lambdas(&mut self) -> Result<(), Unsupported> {
        for index in 0..self.ir.exprs.len() {
            let Some(site) = closure_site(&self.ir.exprs[index]) else {
                continue;
            };
            let Site {
                impl_fn,
                captures,
                sam,
                identity,
                mut receiver_capture,
            } = site?;
            let body = self
                .ir
                .functions
                .get(impl_fn as usize)
                .ok_or_else(|| "a function value with no body function".to_string())?;
            // A lambda the checked lowering spliced into an `inline` caller leaves its expression
            // node behind in the arena, orphaned, with the implementation it named already
            // cleared. Declaring a thunk for it would emit a call to a function nobody defines.
            // Nothing reaches the node, so nothing needs it.
            if body.body.is_none() {
                continue;
            }
            // What the body takes beyond the captures is what a caller supplies. A SUSPEND lambda's
            // body takes a continuation nobody here can pass, and that is what this catches.
            let Some(arity) = body.params.len().checked_sub(captures.len()) else {
                return Err(format!(
                    "a function value whose body takes {} parameters for {} captures",
                    body.params.len(),
                    captures.len()
                ));
            };
            if let IrExpr::Lambda {
                arity: declared, ..
            } = &self.ir.exprs[index]
            {
                if usize::from(*declared) != arity {
                    return Err(format!(
                        "a lambda of arity {declared} whose body takes {} parameters for {} \
                         captures",
                        body.params.len(),
                        captures.len()
                    ));
                }
            }
            // A SAM conversion is a delegate that CAPTURES the function value it wraps (see
            // `fir_lower::sam_conversions`). Two of them are equal exactly when the same interface
            // wraps equal functions, and "equal functions" is the question the captured object
            // already answers for itself — so the identity is keyed on the interface, which is the
            // one thing two `id(::f)` written in two places share: neither the delegate nor its
            // thunk is.
            //
            // Only over a REFERENCE. Kotlin says SAMs over lambdas are never equal, and they would
            // be here: `id { }` at one site captures one non-capturing lambda, which is a
            // singleton, so two conversions of it would wrap the same object. `simpleLambdas.kt`
            // is that case, and it is why this asks what the delegate wraps rather than trusting
            // that a SAM delegate always wraps something with an identity of its own.
            let sam_identity = {
                let wrapped = captures.first().copied();
                sam.as_ref().zip(wrapped).and_then(|(target, wrapped)| {
                    matches!(self.ir.expr(wrapped), IrExpr::CallableReference(_)).then(|| {
                        format!(
                            "kt_refid_sam_{}",
                            model::c_identifier(&target.classifier.render())
                        )
                    })
                })
            };
            if identity.is_none() && sam_identity.is_some() {
                // What the delegate WRAPS is the value its equality compares, and it is capture 0.
                receiver_capture = Some(0);
            }
            let identity = identity.or(sam_identity);
            let capture_types: Vec<Ty> =
                carried_parameters(self.ir, impl_fn)[..captures.len()].to_vec();
            let (capture_offsets, instance_size, references) = layout(&capture_types);
            // Where the value equality reads lands, or 0 for a reference that binds nothing. No
            // field can sit at offset 0 — the header is there — so 0 says "none" unambiguously.
            let receiver_offset = receiver_capture
                .and_then(|capture| capture_offsets.get(capture).copied())
                .unwrap_or(0);

            let base = format!("kt_fn_{index}");
            let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
            let any_type = self.import_data("kt_type_any")?;
            // A conversion to a functional interface the RUNTIME knows changes nothing about the
            // object: nothing but its single member is ever asked of it, and both the runtime and a
            // call site that can see the type reach that member through the one invoke slot every
            // function value declares. So it is built as the plain function value it already is,
            // with no interface table and no program-wide member number.
            let sam = sam.filter(|target| {
                super::super::super::intrinsics::runtime_functional_interface(target.classifier)
                    .is_none()
            });
            match &sam {
                // Converted to a `fun interface`: the object wears that interface's type, so its
                // table has to be one a caller can dispatch through — full length, with the
                // interface's own member pointing at this lambda's body.
                Some(target) => {
                    let (mut vtable, interfaces, kotlin_name) =
                        self.sam_table(target, &base, impl_fn, &capture_offsets)?;
                    let marker = match &identity {
                        Some(name) => {
                            let marker = self.reference_identity_marker(name)?;
                            vtable[0] = self.runtime_member_import("kt_reference_equals")?;
                            vtable[1] = self.runtime_member_import("kt_reference_hash_code")?;
                            Some((marker, receiver_offset))
                        }
                        None => None,
                    };
                    self.define_type_descriptor(
                        descriptor,
                        &base,
                        &kotlin_name,
                        instance_size,
                        &references,
                        &vtable,
                        any_type,
                        &interfaces,
                        marker,
                        super::objects::WalkMembers::default(),
                    )?;
                }
                None => {
                    let thunk = self.declare_local_function(
                        &format!("{base}_invoke"),
                        &vec![any(); arity + 1],
                        any(),
                    )?;
                    let mut vtable = self.any_vtable()?;
                    // A callable reference answers `equals`/`hashCode` by WHAT IT REFERS TO, not
                    // by identity — `Foo::bar == Foo::bar` though the two are different objects.
                    // The pair goes in the object's own table so that a comparison reaching it
                    // through `Any`, which is how two `Any` parameters are compared, gets the same
                    // answer as one through the reference's own type.
                    let marker = match &identity {
                        Some(name) => {
                            let marker = self.reference_identity_marker(name)?;
                            // `any_vtable` builds the three `kotlin.Any` slots in order, so these
                            // two are `equals` and `hashCode` (`KT_SLOT_*` in `krusty_rt.h`).
                            vtable[0] = self.runtime_member_import("kt_reference_equals")?;
                            vtable[1] = self.runtime_member_import("kt_reference_hash_code")?;
                            Some((marker, receiver_offset))
                        }
                        None => None,
                    };
                    vtable.push(thunk);
                    // A REFERENCE answers `KCallable.name` too, and a program reaches it through
                    // a variable rather than through the written reference — so the answer is a
                    // member of the object rather than a constant folded at the site. It takes
                    // the same slot a property reference's does (`references::NAME`), which is
                    // what lets one read through `KCallable`, the type both wear, dispatch
                    // without knowing which of the two it has. The slot between is a property's
                    // `set`, which no type a function reference wears declares; it is filled
                    // rather than left short so the table has one shape.
                    if let Some(declared) = self.reference_declaration_name(index as u32) {
                        let name =
                            self.declare_local_function(&format!("{base}_name"), &[any()], any())?;
                        vtable.push(thunk);
                        vtable.push(name);
                        self.define_reference_name(name, &base, &declared)?;
                    }
                    // `toString` on a function value prints this name, as `Function1` would on the
                    // JVM.
                    let kotlin_name = format!("kotlin.Function{arity}");
                    // What an `is` against a function type asks about. This object's own type is
                    // one of a kind — there is a descriptor per lambda — so the type written at a
                    // check site is never this one, and the markers are what the two have in
                    // common. Both the arity's and the bare `Function` are named, because
                    // `KType.interfaces` is flattened and an interface's own bases are not walked.
                    // 22 is Kotlin's largest function arity, and the runtime declares exactly
                    // those; a wider one names nothing rather than a symbol that does not exist.
                    let mut markers = Vec::new();
                    if arity <= 22 {
                        markers.push(self.import_data(&format!("kt_type_function{arity}"))?);
                        markers.push(self.import_data("kt_type_function")?);
                    }
                    self.define_type_descriptor(
                        descriptor,
                        &base,
                        &kotlin_name,
                        instance_size,
                        &references,
                        &vtable,
                        any_type,
                        &markers,
                        marker,
                        super::objects::WalkMembers::default(),
                    )?;
                    self.define_thunk(thunk, &base, impl_fn, &capture_offsets, arity)?;
                }
            }
            let singleton = if captures.is_empty() {
                let instance = self.declare_local_data(&format!("{base}_instance"), false)?;
                let mut description = DataDescription::new();
                description.define(vec![0; model::HEADER_SIZE as usize].into_boxed_slice());
                description.set_align(8);
                let global = self
                    .module
                    .declare_data_in_data(descriptor, &mut description);
                description.write_data_addr(0, global, 0);
                self.module
                    .define_data(instance, &description)
                    .map_err(|error| format!("defining `{base}_instance` ({error})"))?;
                Some(instance)
            } else {
                None
            };
            self.lambdas.insert(
                index as u32,
                LambdaItems {
                    descriptor,
                    instance_size,
                    capture_offsets,
                    singleton,
                },
            );
        }
        Ok(())
    }

    /// The table, interface list and name of the object a lambda becomes when it is converted to a
    /// `fun interface`.
    ///
    /// A SAM conversion changes the TYPE a function value wears, and a type is a table here. The
    /// object holds its captures exactly as a lambda's does; what differs is that a caller reaches
    /// it through the interface's own member number rather than through the single invoke slot, so
    /// the table must be as long as every class's — the interface region included — with that one
    /// member filled. The rest is the abstract trap, and unreachable for the same reason it is in
    /// a class: nothing can name a member through a type this object does not have.
    fn sam_table(
        &mut self,
        target: &crate::ir::IrSamTarget,
        base: &str,
        impl_fn: u32,
        capture_offsets: &[u32],
    ) -> Result<(Vec<FuncId>, Vec<DataId>, String), Unsupported> {
        if target.suspend {
            return Err("a suspend functional interface".to_string());
        }
        let Some(interface) = self.ir.class_id_by_name(target.classifier) else {
            return Err(format!(
                "a functional interface declared outside this file (`{}`)",
                target.classifier.render()
            ));
        };
        let declaration = &self.ir.classes[interface as usize];
        let method = declaration
            .methods
            .iter()
            .copied()
            .find(|&fid| self.ir.functions[fid as usize].name == target.method)
            .ok_or_else(|| {
                format!(
                    "a functional interface without its own `{}` (`{}`)",
                    target.method,
                    target.classifier.render()
                )
            })?;
        let key = model::function_key(self.ir, interface, method);
        let slot = self.model.slot(interface, &key).ok_or_else(|| {
            format!(
                "a functional interface member with no slot (`{}.{}`)",
                target.classifier.render(),
                target.method
            )
        })? as usize;

        // The thunk wears the interface member's own signature, because that is what the call site
        // dispatches with — not the boxed one an ordinary function value's invoke slot uses.
        let parameters = self.ir.functions[method as usize].params.clone();
        let result = self.ir.functions[method as usize].ret;
        let mut signature = vec![any()];
        signature.extend(parameters.iter().copied());
        let thunk = self.declare_local_function(&format!("{base}_invoke"), &signature, result)?;
        self.define_sam_thunk(thunk, base, impl_fn, capture_offsets, &parameters, result)?;

        // Start from the table the interface itself defines — its default methods, and any
        // `kotlin.Any` member it overrides — so a SAM object answers `result()` or `toString()`
        // the way an ordinary implementor would, and only then fill in the member the lambda is.
        let template = self.model.layout(interface).vtable.clone();
        let mut vtable = Vec::with_capacity(template.len());
        for entry in &template {
            vtable.push(match entry {
                model::Slot::Runtime(symbol) => self.runtime_member_import(symbol)?,
                model::Slot::Function(fid) => match self.functions[*fid as usize] {
                    Some(id) => id,
                    None => self.import("kt_abstract_method_called", &[], Ty::Unit)?,
                },
                model::Slot::Abstract => self.import("kt_abstract_method_called", &[], Ty::Unit)?,
                model::Slot::FieldGetter { .. }
                | model::Slot::FieldSetter { .. }
                | model::Slot::ValueMember { .. }
                | model::Slot::AnnotationMember { .. }
                | model::Slot::Bridge { .. }
                | model::Slot::AccessorBridge { .. } => {
                    return Err("a functional interface with a synthesized member".to_string())
                }
            });
        }
        if slot >= vtable.len() {
            return Err("a functional interface member outside the vtable".to_string());
        }
        vtable[slot] = thunk;

        let interfaces: Vec<DataId> = std::iter::once(interface)
            .chain(self.model.interfaces[interface as usize].iter().copied())
            .map(|id| self.classes[id as usize].descriptor)
            .collect();
        let kotlin_name = declaration.fq_name().replace(['/', '$'], ".");
        Ok((vtable, interfaces, kotlin_name))
    }

    /// Why a lambda's body function was not emitted, for a thunk that needs to call it.
    ///
    /// Three shapes reach `declare_functions`'s skip, and they are not one question: a body the
    /// checked lowering CLEARED after splicing it into an inline caller is an orphan nothing should
    /// have reached, while a body kept out because it returns NON-LOCALLY is live and deliberately
    /// unreachable by name. Naming which one it is turns a single unfollowable decline into a
    /// report that says what to look at.
    fn missing_body_reason(&self, impl_fn: u32) -> String {
        if self.ir.functions[impl_fn as usize].body.is_none() {
            return "a lambda whose body was cleared after splicing".to_string();
        }
        if self
            .ir
            .inline_only_fns
            .contains(&(impl_fn as crate::ir::FunId))
        {
            return "a lambda that returns non-locally, which only its caller can run".to_string();
        }
        "a lambda whose body has no code".to_string()
    }

    /// A SAM object's entry point: the captures it carries, then the interface method's own
    /// arguments converted to what the lambda body declares.
    fn define_sam_thunk(
        &mut self,
        thunk: FuncId,
        base: &str,
        impl_fn: u32,
        capture_offsets: &[u32],
        parameters: &[Ty],
        result: Ty,
    ) -> Result<(), Unsupported> {
        let body = &self.ir.functions[impl_fn as usize];
        let declared = carried_parameters(self.ir, impl_fn);
        let produced = body.ret;
        let target =
            self.functions[impl_fn as usize].ok_or_else(|| self.missing_body_reason(impl_fn))?;
        if declared.len() != capture_offsets.len() + parameters.len() {
            return Err("a functional interface method of a different arity".to_string());
        }
        let mut signature = vec![any()];
        signature.extend(parameters.iter().copied());
        let signature = self.signature_of(&signature, result)?;
        let capture_offsets = capture_offsets.to_vec();
        let incoming = parameters.to_vec();
        let name = format!("{base}_invoke");
        self.emit_function(thunk, signature, result, &name, &mut |body, params| {
            let mut arguments = Vec::with_capacity(declared.len());
            for (offset, ty) in capture_offsets.iter().zip(&declared) {
                let clif = carrier(*ty).clif().expect("a capture is never `Unit`");
                arguments.push(
                    body.builder
                        .ins()
                        .load(clif, trusted(), params[0], *offset as i32),
                );
            }
            for (index, ty) in declared[capture_offsets.len()..].iter().enumerate() {
                let Some(value) = body.convert(params[index + 1], Some(incoming[index]), *ty)?
                else {
                    return Err("a `Unit` argument to a functional interface".to_string());
                };
                arguments.push(value);
            }
            let func_ref = body.func_ref(target);
            let call = body.emit_call(func_ref, &arguments)?;
            let returned = body.builder.inst_results(call).first().copied();
            match (returned, carrier(result)) {
                (_, Carrier::Void) => {}
                (Some(value), _) => {
                    let value = body
                        .convert(value, Some(produced), result)?
                        .expect("a non-void carrier");
                    body.builder.ins().return_(&[value]);
                    body.terminate();
                }
                // A `Unit` body answering an interface that declares a value: the runtime's
                // singleton is that value.
                (None, _) => {
                    let unit = body
                        .runtime_call("kt_unit", &[], any(), &[])?
                        .expect("`kt_unit` returns the singleton");
                    body.builder.ins().return_(&[unit]);
                    body.terminate();
                }
            }
            Ok(())
        })
    }

    /// The uniform entry point: unpack the captures this object carries, convert each argument
    /// from the reference the caller passed, call the body, and hand the result back as a
    /// reference.
    fn define_thunk(
        &mut self,
        thunk: FuncId,
        base: &str,
        impl_fn: u32,
        capture_offsets: &[u32],
        arity: usize,
    ) -> Result<(), Unsupported> {
        let body = &self.ir.functions[impl_fn as usize];
        let parameters = carried_parameters(self.ir, impl_fn);
        let ret = body.ret;
        let target =
            self.functions[impl_fn as usize].ok_or_else(|| self.missing_body_reason(impl_fn))?;
        let signature = self.signature_of(&vec![any(); arity + 1], any())?;
        let capture_offsets = capture_offsets.to_vec();
        let name = format!("{base}_invoke");

        self.emit_function(thunk, signature, any(), &name, &mut |body, params| {
            let mut arguments = Vec::with_capacity(parameters.len());
            for (offset, ty) in capture_offsets.iter().zip(&parameters) {
                let clif = carrier(*ty).clif().expect("a capture is never `Unit`");
                arguments.push(
                    body.builder
                        .ins()
                        .load(clif, trusted(), params[0], *offset as i32),
                );
            }
            for (index, ty) in parameters[capture_offsets.len()..].iter().enumerate() {
                let Some(value) = body.convert(params[index + 1], Some(any()), *ty)? else {
                    return Err("a `Unit` lambda parameter".to_string());
                };
                arguments.push(value);
            }
            let func_ref = body.func_ref(target);
            let call = body.emit_call(func_ref, &arguments)?;
            let result = body.builder.inst_results(call).first().copied();
            // A `Unit`-returning lambda still answers with a reference: the runtime's `Unit`.
            let result = match result {
                Some(value) => body
                    .convert(value, Some(ret), any())?
                    .expect("a reference carrier"),
                None => body
                    .runtime_call("kt_unit", &[], any(), &[])?
                    .expect("`kt_unit` returns the singleton"),
            };
            body.builder.ins().return_(&[result]);
            body.terminate();
            Ok(())
        })
    }

    /// The holder type for a captured `var` of the given carrier, declared on first use.
    pub(super) fn holder_type(&mut self, elem: Ty) -> Result<DataId, Unsupported> {
        let clif = carrier(elem)
            .clif()
            .ok_or_else(|| "a captured `Unit` variable".to_string())?;
        let key = clif.to_string();
        if let Some(id) = self.holders.get(&key) {
            return Ok(*id);
        }
        let base = format!("kt_ref_{key}");
        let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
        let (_, instance_size, references) = layout(&[elem]);
        let vtable = self.any_vtable()?;
        let any_type = self.import_data("kt_type_any")?;
        self.define_type_descriptor(
            descriptor,
            &base,
            "kotlin.jvm.internal.Ref",
            instance_size,
            &references,
            &vtable,
            any_type,
            &[],
            None,
            super::objects::WalkMembers::default(),
        )?;
        self.holders.insert(key, descriptor);
        Ok(descriptor)
    }
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// Allocate a function value and fill in what it captured.
    pub(super) fn lambda(&mut self, site: u32) -> Result<Option<Value>, Unsupported> {
        let Site {
            impl_fn,
            captures,
            sam,
            ..
        } = closure_site(self.file.ir.expr(site)).expect("only a closure site reaches here")?;
        // Converting a NULLABLE function value to a `fun interface` yields null when the value is
        // null — `isNull(nullableFun(true))` must answer true, not call `invoke` on a wrapper
        // around nothing. The checked lowering expresses such a conversion as a lambda whose one
        // capture is the value being converted, so the null test is on that capture.
        if sam.is_some() {
            if let [operand] = captures.as_slice() {
                let converts = self.file.ir.functions[impl_fn as usize]
                    .name
                    .starts_with("$fir_sam_delegate_");
                let nullable = self.type_of(*operand).is_some_and(|ty| ty.is_nullable());
                if converts && nullable {
                    return self.nullable_sam(site, *operand);
                }
            }
        }
        self.wrap_closure(site)
    }

    /// Build the object a closure site makes: its captures, then the allocation that holds them.
    fn wrap_closure(&mut self, site: u32) -> Result<Option<Value>, Unsupported> {
        let Site {
            impl_fn, captures, ..
        } = closure_site(self.file.ir.expr(site)).expect("only a closure site reaches here")?;
        let items = self
            .file
            .lambdas
            .get(&site)
            .ok_or_else(|| "an undeclared lambda".to_string())?;
        if let Some(instance) = items.singleton {
            return Ok(Some(self.data_address(instance)));
        }
        let (descriptor, instance_size, offsets) = (
            items.descriptor,
            items.instance_size,
            items.capture_offsets.clone(),
        );
        let parameters = carried_parameters(self.file.ir, impl_fn);

        // Captures first, then the allocation: a capture that allocates must not leave a
        // half-built object for a collection to find.
        let mut values = Vec::with_capacity(captures.len());
        for (capture, ty) in captures.iter().zip(&parameters) {
            let Some(value) = self.coerce(*capture, *ty)? else {
                return Err("a `Unit` capture".to_string());
            };
            if self.terminated {
                return Ok(None);
            }
            values.push(value);
        }
        let object = self.allocate(descriptor, instance_size)?;
        for (value, offset) in values.into_iter().zip(&offsets) {
            self.builder
                .ins()
                .store(trusted(), value, object, *offset as i32);
        }
        Ok(Some(object))
    }

    /// Call a function value: box each argument, dispatch through the invoke slot, convert the
    /// result back. Kotlin's own calling convention for a `FunctionN`.
    pub(super) fn invoke_function(
        &mut self,
        func: u32,
        args: &[u32],
        params: &[Ty],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(function) = self.receiver(func)? else {
            return Ok(None);
        };
        let mut arguments = Vec::with_capacity(args.len());
        for (argument, ty) in args.iter().zip(params) {
            let Some(value) = self.coerce(*argument, *ty)? else {
                return Err("a `Unit` argument to a function value".to_string());
            };
            if self.terminated {
                return Ok(None);
            }
            let Some(value) = self.convert(value, Some(*ty), any())? else {
                return Err("a `Unit` argument to a function value".to_string());
            };
            arguments.push(value);
        }
        let result = self.dispatch(
            function,
            INVOKE_SLOT,
            &vec![any(); arguments.len()],
            any(),
            &arguments,
        )?;
        match result {
            Some(value) if carrier(ret) != Carrier::Void => self.convert(value, Some(any()), ret),
            _ => Ok(None),
        }
    }

    /// A SAM conversion whose operand may be null: null in, null out; anything else takes the
    /// ordinary path and is wrapped.
    fn nullable_sam(&mut self, site: u32, operand: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(operand)?;
        if self.terminated {
            return Ok(None);
        }
        let zero = self.builder.ins().iconst(types::I64, 0);
        let is_null = self.builder.ins().icmp(IntCC::Equal, value, zero);
        let merge = self.builder.create_block();
        self.builder.append_block_param(merge, types::I64);
        let wrap = self.builder.create_block();
        let empty = self.builder.create_block();
        self.builder.ins().brif(is_null, empty, &[], wrap, &[]);

        self.continue_in(empty);
        self.builder.seal_block(empty);
        let null = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().jump(merge, &[BlockArg::Value(null)]);

        self.continue_in(wrap);
        self.builder.seal_block(wrap);
        let Some(object) = self.wrap_closure(site)? else {
            return Ok(None);
        };
        self.builder.ins().jump(merge, &[BlockArg::Value(object)]);

        self.continue_in(merge);
        self.builder.seal_block(merge);
        Ok(Some(self.builder.block_params(merge)[0]))
    }

    /// Call a function value that is already a `Value`, with arguments that already are too:
    /// `invoke_function`'s other half, for a caller holding values rather than expressions.
    pub(super) fn invoke_value(
        &mut self,
        function: Value,
        arguments: &[Value],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let result = self.dispatch(
            function,
            INVOKE_SLOT,
            &vec![any(); arguments.len()],
            any(),
            arguments,
        )?;
        match result {
            Some(value) if carrier(ret) != Carrier::Void => self.convert(value, Some(any()), ret),
            _ => Ok(None),
        }
    }

    /// The cell a captured `var` lives in, so the closure and the frame that made it share one.
    pub(super) fn ref_new(&mut self, elem: Ty, init: u32) -> Result<Option<Value>, Unsupported> {
        let descriptor = self.file.holder_type(elem)?;
        let (offsets, instance_size, _) = super::functions::layout(&[elem]);
        let Some(value) = self.coerce(init, elem)? else {
            return Err("a captured `Unit` variable".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        let holder = self.allocate(descriptor, instance_size)?;
        self.builder
            .ins()
            .store(trusted(), value, holder, offsets[0] as i32);
        Ok(Some(holder))
    }

    pub(super) fn ref_get(&mut self, holder: u32, elem: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(holder) = self.receiver(holder)? else {
            return Ok(None);
        };
        let (offsets, _, _) = super::functions::layout(&[elem]);
        let clif = carrier(elem)
            .clif()
            .ok_or_else(|| "a captured `Unit` variable".to_string())?;
        Ok(Some(self.builder.ins().load(
            clif,
            trusted(),
            holder,
            offsets[0] as i32,
        )))
    }

    /// A write to a captured `var`, which evaluates to the value written.
    pub(super) fn ref_set(
        &mut self,
        holder: u32,
        elem: Ty,
        value: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(holder) = self.receiver(holder)? else {
            return Ok(None);
        };
        let value = self.coerce(value, elem)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(value) = value else {
            return Err("a `Unit` value assigned to a captured variable".to_string());
        };
        let (offsets, _, _) = super::functions::layout(&[elem]);
        self.builder
            .ins()
            .store(trusted(), value, holder, offsets[0] as i32);
        Ok(Some(value))
    }
}
