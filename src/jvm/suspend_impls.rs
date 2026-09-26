//! An overridable suspend member's body moves to `<name>$suspendImpl`.
//!
//! kotlinc (`AddContinuationLowering.createStaticSuspendImpl`) compiles a `suspend fun` with a body
//! that a subclass may override into two methods: the member itself, which is a trampoline, and a
//! static `<name>$suspendImpl` taking the receiver as its first parameter and carrying the body.
//!
//! ```text
//! public Object work(String, Continuation)                  aload_0; aload_1; aload_2;
//!                                                           invokestatic work$suspendImpl; areturn
//! static Object work$suspendImpl(Base, String, Continuation) ← the state machine
//! ```
//!
//! The split exists so that re-entering the machine from its continuation does not dispatch
//! virtually to an override, and so that an override runs the super body by calling the static.
//! Every member of an interface with a body is split (its static is `public`); a class member is
//! split when it is overridable (`open`, `abstract` or a non-`final` `override` in a class that can
//! be subclassed), and its static is package-private.
//!
//! This runs as an IR rewrite rather than at emission so the ordinary method emitter produces both
//! methods, with the debug tables, generic signature and nullability annotations it already derives.
//! Writing the trampoline at emission would mean hand-copying all of that.
//!
//! It runs AFTER `lower_suspend`: the body it moves is the finished state machine, or the body
//! the coroutine transformer takes once the class is written.

use std::collections::{HashMap, HashSet};

use crate::ir::{Callee, IrClass, IrExpr, IrFile, IrFunction};
use crate::types::Ty;

fn move_fact<T>(facts: &mut HashMap<u32, T>, implementation: u32, declaration: u32) {
    if let Some(fact) = facts.remove(&implementation) {
        facts.insert(declaration, fact);
    }
}

fn copy_fact<T: Clone>(facts: &mut HashMap<u32, T>, implementation: u32, declaration: u32) {
    if let Some(fact) = facts.get(&implementation).cloned() {
        facts.insert(declaration, fact);
    }
}

fn move_marker(markers: &mut HashSet<u32>, implementation: u32, declaration: u32) {
    if markers.remove(&implementation) {
        markers.insert(declaration);
    }
}

fn move_list_marker(markers: &mut [u32], implementation: u32, declaration: u32) {
    for marker in markers
        .iter_mut()
        .filter(|marker| **marker == implementation)
    {
        *marker = declaration;
    }
}

/// Transfer facts owned by the Kotlin declaration to its new trampoline identity.
///
/// The original id now names only the backend's synthetic body carrier. Signature shapes are the
/// one deliberate exception: both physical methods publish a generic JVM `Signature`, so those are
/// copied below rather than moved.
fn move_declaration_facts(ir: &mut IrFile, implementation: u32, declaration: u32) {
    copy_fact(&mut ir.function_annotations, implementation, declaration);
    move_fact(
        &mut ir.fn_param_declared_nullable,
        implementation,
        declaration,
    );
    move_fact(&mut ir.fn_declared_spellings, implementation, declaration);
    // Both surfaces project parameter/debug names from the declaration spelling: the trampoline
    // is the metadata declaration, while the static body carrier still owns the implementation's
    // LocalVariableTable. Neither may derive it from the `$suspendImpl` physical name.
    copy_fact(&mut ir.fn_source_names, implementation, declaration);
    move_fact(&mut ir.fn_context_counts, implementation, declaration);
    move_fact(&mut ir.fn_param_no_infer, implementation, declaration);
    move_fact(
        &mut ir.fn_return_value_statuses,
        implementation,
        declaration,
    );
    move_fact(
        &mut ir.default_stub_boxed_params,
        implementation,
        declaration,
    );
    move_fact(&mut ir.fn_varargs, implementation, declaration);
    move_fact(&mut ir.fn_sig_lines, implementation, declaration);

    move_marker(&mut ir.extension_receiver_fns, implementation, declaration);
    move_marker(&mut ir.inline_fns, implementation, declaration);
    move_marker(&mut ir.operator_fns, implementation, declaration);
    move_marker(&mut ir.infix_fns, implementation, declaration);
    move_marker(&mut ir.open_methods, implementation, declaration);
    move_marker(&mut ir.deprecated_methods, implementation, declaration);
    move_marker(&mut ir.public_inline_functions, implementation, declaration);
    move_marker(&mut ir.internal_methods, implementation, declaration);
    move_list_marker(&mut ir.fresh_method_decls, implementation, declaration);
}

/// Copy the semantic signature shapes needed by both the declaration and its body carrier.
///
/// The backend signature boundary combines the static's unmodified source shape with the recorded
/// interface owner to produce its receiver-first physical signature. The declaration consumes the
/// same source shape directly.
fn split_signature_facts(ir: &mut IrFile, implementation: u32, declaration: u32) {
    copy_fact(&mut ir.vc_declared_sigs, implementation, declaration);
    copy_fact(&mut ir.signatures, implementation, declaration);
    copy_fact(&mut ir.member_semantic_sigs, implementation, declaration);
    copy_fact(&mut ir.suspend_declared_sigs, implementation, declaration);
}

/// Whether member `fid` of `class` keeps only a trampoline, its body moving to `$suspendImpl`.
fn splits(ir: &IrFile, class: &IrClass, fid: u32) -> bool {
    let function = &ir.functions[fid as usize];
    let splits_in_class = || {
        // kotlinc's `isOverridable`: not private, not final, in a class that is not final.
        ir.open_methods.contains(&fid)
            && (class.is_open || class.is_abstract || class.is_sealed)
            && !class.is_value
    };
    !function.is_static
        && function.body.is_some()
        && ir.suspend_funs.contains(&fid)
        && !ir.private_methods.contains(&fid)
        && (class.is_interface || splits_in_class())
}

/// Whether suspend member `fid` keeps only a trampoline and its body moves to the static
/// `<name>$suspendImpl`: its continuation re-enters that static, never the member.
pub(crate) fn moves_to_suspend_impl(ir: &IrFile, fid: u32) -> bool {
    ir.functions[fid as usize]
        .dispatch_receiver
        .and_then(|owner| ir.classes.iter().find(|class| class.fq_name_id() == owner))
        .is_some_and(|class| splits(ir, class, fid))
}

/// Whose `$default` stub follows method `fid` in its class: its own, or, after the `$suspendImpl` a
/// member's body moved to, that member's, since kotlinc adds the static right after the member and
/// ahead of the stub. `None` for such a member itself.
pub(crate) fn default_stub_after(ir: &IrFile, fid: u32) -> Option<u32> {
    match ir.jvm_suspend_impl_bodies.get(&fid) {
        Some(&(_, declaration)) => Some(declaration),
        None if ir
            .jvm_suspend_impl_bodies
            .values()
            .any(|&(_, declaration)| declaration == fid) =>
        {
            None
        }
        None => Some(fid),
    }
}

/// The JVM name of the static a member's body moves to.
pub(crate) fn suspend_impl_name(member: &str) -> String {
    format!("{member}$suspendImpl")
}

/// Rewrite every suspend member whose body moves to `$suspendImpl`.
///
/// The ORIGINAL function keeps its id and its body and becomes the static: every fact the suspend
/// pass recorded against that id — its continuation class, its spill metadata — stays attached to
/// the code it describes. The trampoline is the new function, and it takes the original's name.
///
/// Prepending the receiver parameter does not move a value index: an instance method's `this` is
/// already index 0 and its parameters follow, which is exactly the static's layout.
pub(crate) fn lower_suspend_impls(ir: &mut IrFile) {
    for class_index in 0..ir.classes.len() {
        let owner = ir.classes[class_index].fq_name_id();
        let members = ir.classes[class_index].methods.clone();
        let mut rewritten: Vec<(usize, u32, u32)> = Vec::new();
        for (position, &fid) in members.iter().enumerate() {
            if !splits(ir, &ir.classes[class_index], fid) {
                continue;
            }
            let function = &ir.functions[fid as usize];
            let trampoline_name = function.name.clone();
            let params = function.params.clone();
            let ret = function.ret;
            let arguments = (0..=params.len() as u32)
                .map(|value| ir.add_expr(IrExpr::GetValue(value)))
                .collect::<Vec<_>>();
            let call = ir.add_expr(IrExpr::Call {
                callee: Callee::ClassStatic {
                    owner,
                    function: fid,
                },
                dispatch_receiver: None,
                args: arguments,
            });
            let ret_expr = ir.add_expr(IrExpr::Return(Some(call)));
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![ret_expr],
                value: None,
            });
            let trampoline = ir.add_fun(IrFunction {
                name: trampoline_name,
                params: params.clone(),
                ret,
                body: Some(body),
                is_static: false,
                dispatch_receiver: Some(owner),
                param_checks: vec![None; params.len()],
            });
            // The trampoline owns the declaration parameters and its defaults. The static keeps
            // only physical debug identities: retaining defaults there would publish the bogus
            // `<name>$suspendImpl$default` in addition to the declaration's legitimate stub.
            if let Some(mut implementation_info) = ir.fn_params.remove(&fid) {
                let declaration_info = implementation_info.clone();
                implementation_info.defaults = None;
                implementation_info.stub_only = false;
                implementation_info.prepend_generated(crate::ir::IrParameterIdentity::generated(
                    crate::ir::IrGeneratedParameterRole::HolderReceiver,
                    None,
                ));
                ir.fn_params.insert(fid, implementation_info);
                ir.fn_params.insert(trampoline, declaration_info);
            }
            if let Some(declaration_annotations) = ir.fn_param_annotations.get(&fid).cloned() {
                ir.fn_param_annotations
                    .insert(trampoline, declaration_annotations.clone());
                let mut implementation_annotations =
                    vec![crate::ir::DeclarationAnnotations::default()];
                implementation_annotations.extend(declaration_annotations);
                ir.fn_param_annotations
                    .insert(fid, implementation_annotations);
            }
            // NOT the declaration line: kotlinc gives the trampoline a LocalVariableTable and no
            // LineNumberTable at all. It is a forwarder with no source statement of its own.
            // It DOES record locals — kotlinc gives it `$this`, the declared parameters and
            // `$completion` — which is a separate fact from having a line table.
            ir.fn_debug_locals.insert(trampoline);
            move_declaration_facts(ir, fid, trampoline);
            split_signature_facts(ir, fid, trampoline);
            // The trampoline now IS the source declaration; the original id is only its JVM body
            // carrier. Retarget the stable checked-callable edge at the same identity split so
            // metadata and later exact call/reference consumers never infer declaration ownership
            // from either generated method spelling.
            let mut source_declaration_retargeted = false;
            for realization in ir.checked_callable_functions.values_mut() {
                if *realization == fid {
                    *realization = trampoline;
                    source_declaration_retargeted = true;
                }
            }
            assert!(
                source_declaration_retargeted,
                "a suspend member declaration retains its checked callable identity"
            );
            ir.jvm_suspend_impl_bodies.insert(fid, (owner, trampoline));
            // The original becomes the static, named `<name>$suspendImpl`, with the receiver as
            // its first parameter.
            {
                let function = &mut ir.functions[fid as usize];
                function.name = suspend_impl_name(&function.name);
                function.is_static = true;
                function.dispatch_receiver = None;
                function.params.insert(0, Ty::obj_name(owner));
                function.param_checks.insert(0, None);
            }
            // kotlinc emits it `static synthetic`: public on an interface (0x1009), package-private
            // on a class (0x1008, from the recorded split below). The synthetic mark is also what
            // keeps it out of `@Metadata`: it is a JVM implementation detail of the declaration,
            // not a second Kotlin function, and the declaration is described by the trampoline.
            ir.synthetic_methods.insert(fid);
            // The emitter orders interface members by recorded source order, and a function without
            // one trails at the end. The trampoline stands where the member was declared, so it
            // takes the original's position; the sort is stable, and the trampoline is inserted
            // ahead of the static, so the two stay adjacent in kotlinc's order.
            if let Some(order) = ir.fn_source_order.get(&fid).copied() {
                ir.fn_source_order.insert(trampoline, order);
            }
            rewritten.push((position, trampoline, fid));
        }
        // Replace the source member in place so every checked `MethodCall { class, index }` keeps
        // naming that declaration and later member indices stay stable. The body carrier is new and
        // therefore appends safely; its copied source-order key sorts it directly after the
        // trampoline during emission.
        for (position, trampoline, implementation) in rewritten {
            ir.classes[class_index].methods[position] = trampoline;
            ir.classes[class_index].methods.push(implementation);
        }
    }
}
