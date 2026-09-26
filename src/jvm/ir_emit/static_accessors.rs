//! kotlinc's synthetic accessors for PRIVATE static declarations used from another JVM class.
//!
//! A private top-level function or property is a static member of its file facade, and a private
//! `companion { … }` block member one of the class that declared the block: its static placement.
//! Kotlin lets any code of the file use it, while a JVM class may not reach another class's private
//! member. kotlinc's `SyntheticAccessorLowering` therefore gives the static owner one `public static
//! final synthetic` forwarder per target — `access$<name>` for a function, `access$get<X>$p` and
//! `access$set<X>$p` for a property's field — and every use from another class calls it instead.
//! A nested class, a callable-reference carrier and a lambda class are all such other classes.
//!
//! The owner appends its accessors after every declared and lifted member, ahead of `<clinit>`, in
//! the order the file first uses them. [`plan`] finds those uses once per emission pass; each use
//! site routes itself through [`routes_through_accessor`], the same rule the plan applies.

use super::*;
use std::collections::{HashMap, HashSet};

/// One synthetic accessor a static owner declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum StaticAccessor {
    /// `access$<name>`, forwarding to a private static function.
    Function(u32),
    /// `access$get<X>$p`, reading static storage `index`.
    Getter(u32),
    /// `access$set<X>$p`, writing static storage `index`.
    Setter(u32),
}

/// Every static owner's accessors, in first-use order.
#[derive(Default)]
pub(super) struct StaticAccessorPlan {
    by_owner: HashMap<String, Vec<StaticAccessor>>,
}

impl StaticAccessorPlan {
    fn accessors_of(&self, owner: &str) -> &[StaticAccessor] {
        self.by_owner.get(owner).map_or(&[], Vec::as_slice)
    }
}

/// A JVM class whose code this file emits, with the IR roots of that code.
pub(super) struct EmissionContext {
    pub(super) owner: String,
    pub(super) roots: Vec<crate::ir::ExprId>,
}

/// The code of each class the file emits, the facade's first. `class_member_fids` are the
/// functions some class lists as its methods; every other receiverless body is the facade's.
pub(super) fn emission_contexts(
    ir: &IrFile,
    facade: &str,
    class_member_fids: &HashSet<u32>,
) -> Vec<EmissionContext> {
    let facade_roots = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(fid, function)| {
            !class_member_fids.contains(&(*fid as u32)) && function.dispatch_receiver.is_none()
        })
        .filter_map(|(_, function)| function.body)
        .chain(
            ir.statics
                .iter()
                .filter(|property| property.owner.is_none())
                .map(|property| property.init),
        )
        .collect();
    let mut contexts = vec![EmissionContext {
        owner: facade.to_string(),
        roots: facade_roots,
    }];
    for class in &ir.classes {
        let owner = class.fq_name();
        let mut roots = class
            .methods
            .iter()
            .filter_map(|fid| {
                ir.functions
                    .get(*fid as usize)
                    .and_then(|function| function.body)
            })
            .collect::<Vec<_>>();
        for fid in &class.methods {
            if let Some(defaults) = ir
                .fn_params
                .get(fid)
                .and_then(|parameters| parameters.defaults.as_ref())
            {
                roots.extend(defaults.iter().flatten().copied());
            }
        }
        roots.extend(class.init_body);
        roots.extend(class.super_arg_prelude.iter().copied());
        roots.extend(class.super_args.iter().copied());
        roots.extend(
            class
                .properties
                .iter()
                .filter_map(|property| property.initializer),
        );
        for constructor in &class.secondary_ctors {
            roots.extend(constructor.body);
            roots.extend(constructor.defaults.iter().flatten().copied());
            roots.extend(constructor.delegate_prelude.iter().copied());
            roots.extend(constructor.delegate_args.iter().copied());
        }
        for entry in &class.enum_entries {
            roots.extend(entry.args.iter().copied());
        }
        roots.extend(
            ir.statics
                .iter()
                .filter(|property| property.owner_matches(&owner))
                .map(|property| property.init),
        );
        contexts.push(EmissionContext { owner, roots });
    }
    contexts
}

/// The static owner of storage `index` when its field is reached only through `access$…$p`
/// accessors: a PRIVATE property of a facade or of a companion block, with neither a constant
/// value, an accessor of its own nor a public field.
pub(super) fn bridged_storage_owner(ir: &IrFile, facade: &str, index: u32) -> Option<String> {
    let property = &ir.statics[index as usize];
    if property.is_const
        || property.custom_accessor
        || ir.is_jvm_field_static(index)
        || !property.visibility.is_private()
    {
        return None;
    }
    match property.owner {
        None => Some(facade.to_string()),
        Some(owner) if ir.companion_blocks.is_storage(index) => Some(owner.render()),
        Some(_) => None,
    }
}

/// Whether code emitted into `context` reaches a private static `function` of `owner` through
/// the owner's accessor: exactly when the two are different classes.
pub(super) fn routes_through_accessor(
    ir: &IrFile,
    context: &str,
    owner: &str,
    function: u32,
) -> bool {
    context != owner && ir.private_methods.contains(&function)
}

/// Find every accessor the file's code needs, ordered by the source position of its first use,
/// the order kotlinc's lowering meets them in. A reference carrier's uses count where the
/// reference is written, since that is where kotlinc lowers its body.
pub(super) fn plan(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    contexts: &[EmissionContext],
    class_member_fids: &HashSet<u32>,
) -> StaticAccessorPlan {
    let walk = Walk {
        ir,
        facade,
        class_member_fids,
    };
    let mut carriers: HashMap<TypeName, Vec<Use>> = HashMap::new();
    for context in contexts {
        let Some(class) = ir
            .classes
            .iter()
            .find(|class| class.fq_name() == context.owner)
            .filter(|class| class.func_ref.is_some() || class.prop_ref.is_some())
        else {
            continue;
        };
        let mut uses = Vec::new();
        for &root in &context.roots {
            walk.collect(&context.owner, root, 0, &HashMap::new(), &mut uses);
        }
        uses.extend(synthesized_carrier_uses(ir, facade, env, class));
        carriers.insert(class.fq_name, uses);
    }
    let reference_lines = ir
        .callable_reference_names
        .iter()
        .filter_map(|(expression, class)| {
            ir.expr_source_lines
                .get(expression)
                .map(|line| (*class, *line))
        })
        .collect::<HashMap<_, _>>();
    let mut uses = Vec::new();
    for context in contexts {
        let owner = ir
            .classes
            .iter()
            .find(|class| class.fq_name() == context.owner)
            .map(|class| class.fq_name);
        match owner.and_then(|owner| carriers.get(&owner).map(|uses| (owner, uses))) {
            // A carrier constructed nowhere this walk sees counts at its reference's line.
            Some((owner, carrier_uses)) => {
                let line = reference_lines.get(&owner).copied().unwrap_or(0);
                uses.extend(carrier_uses.iter().map(|found| Use {
                    line,
                    ..found.clone()
                }));
            }
            None => {
                for &root in &context.roots {
                    walk.collect(&context.owner, root, 0, &carriers, &mut uses);
                }
            }
        }
    }
    // A stable sort keeps the traversal order among uses on one line.
    uses.sort_by_key(|found| found.line);
    let mut plan = StaticAccessorPlan::default();
    let mut seen = HashSet::new();
    for Use {
        owner, accessor, ..
    } in uses
    {
        if seen.insert((owner.clone(), accessor)) {
            plan.by_owner.entry(owner).or_default().push(accessor);
        }
    }
    plan
}

/// One use of another class's private static declaration: the accessor it needs and its line.
#[derive(Clone)]
struct Use {
    line: u32,
    owner: String,
    accessor: StaticAccessor,
}

/// The uses a carrier's synthesized body makes: a function reference whose `invoke` is not
/// lowered calls its target itself, and a property reference reads and writes bridged storage.
fn synthesized_carrier_uses(ir: &IrFile, facade: &str, env: &EmitEnv, class: &IrClass) -> Vec<Use> {
    let context = class.fq_name();
    let mut uses = Vec::new();
    if let Some(reference) = class.func_ref.as_ref().filter(|reference| {
        reference.invoke.is_none()
            && matches!(
                reference.dispatch,
                crate::ir::FrDispatch::Static | crate::ir::FrDispatch::StaticBound
            )
    }) {
        let owner = reference.call_owner_or_facade(facade);
        if let Some(target) = function_reference_target(ir, reference)
            .filter(|target| routes_through_accessor(ir, &context, &owner, *target))
        {
            uses.push(Use {
                line: 0,
                owner,
                accessor: StaticAccessor::Function(target),
            });
        }
    }
    let storage = class.prop_ref.as_ref().and_then(|reference| {
        env.property_reference_realizations
            .get(class.fq_name)
            .and_then(|realization| realization.bridged_storage)
            .map(|storage| (reference.mutable, storage))
    });
    if let Some((mutable, storage)) = storage {
        if let Some(owner) = bridged_storage_owner(ir, facade, storage) {
            uses.push(Use {
                line: 0,
                owner: owner.clone(),
                accessor: StaticAccessor::Getter(storage),
            });
            if mutable {
                uses.push(Use {
                    line: 0,
                    owner,
                    accessor: StaticAccessor::Setter(storage),
                });
            }
        }
    }
    uses
}

struct Walk<'a> {
    ir: &'a IrFile,
    facade: &'a str,
    class_member_fids: &'a HashSet<u32>,
}

impl Walk<'_> {
    /// Record, in evaluation order, each use under `root` that code of `context` makes of another
    /// class's private static declaration. A use without a source line takes its parent's; the
    /// construction of a carrier in `carriers` stands for that carrier's own uses.
    fn collect(
        &self,
        context: &str,
        root: crate::ir::ExprId,
        line: u32,
        carriers: &HashMap<TypeName, Vec<Use>>,
        uses: &mut Vec<Use>,
    ) {
        let ir = self.ir;
        // Post-order: an operand's use precedes the use of the call or store consuming it.
        let mut stack = vec![(root, line, false)];
        while let Some((expression, inherited, expanded)) = stack.pop() {
            let line = ir
                .expr_source_lines
                .get(&expression)
                .copied()
                .unwrap_or(inherited);
            if !expanded {
                stack.push((expression, inherited, true));
                let mut children = Vec::new();
                crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
                stack.extend(children.into_iter().rev().map(|child| (child, line, false)));
                continue;
            }
            let carrier = match ir.expr(expression) {
                IrExpr::StaticInstance { owner, .. } => Some(ir.classes[*owner as usize].fq_name),
                IrExpr::New { internal, .. } => Some(*internal),
                _ => None,
            };
            if let Some(carrier_uses) = carrier.and_then(|carrier| carriers.get(&carrier)) {
                uses.extend(carrier_uses.iter().map(|found| Use {
                    line,
                    ..found.clone()
                }));
                continue;
            }
            let Some((owner, accessor)) = self.target(expression) else {
                continue;
            };
            let needed = match accessor {
                StaticAccessor::Function(function) => {
                    routes_through_accessor(ir, context, &owner, function)
                }
                StaticAccessor::Getter(_) | StaticAccessor::Setter(_) => context != owner,
            };
            if needed {
                uses.push(Use {
                    line,
                    owner,
                    accessor,
                });
            }
        }
    }

    /// The private static declaration `expression` uses, with its static owner, if any.
    fn target(&self, expression: crate::ir::ExprId) -> Option<(String, StaticAccessor)> {
        match self.ir.expr(expression) {
            IrExpr::Call {
                callee: Callee::Local(function),
                ..
            } if !self.class_member_fids.contains(function) => {
                Some((self.facade.to_string(), StaticAccessor::Function(*function)))
            }
            IrExpr::Call {
                callee: Callee::ClassStatic { owner, function },
                ..
            } => Some((owner.render(), StaticAccessor::Function(*function))),
            IrExpr::GetStatic(index) => bridged_storage_owner(self.ir, self.facade, *index)
                .map(|owner| (owner, StaticAccessor::Getter(*index))),
            IrExpr::SetStatic { index, .. } => bridged_storage_owner(self.ir, self.facade, *index)
                .map(|owner| (owner, StaticAccessor::Setter(*index))),
            _ => None,
        }
    }
}

/// Append `owner`'s planned accessors to its class, with kotlinc's debug tables: each maps its
/// body to the line the owner is declared on (a facade's is the file's first) and names the
/// parameters it forwards.
pub(super) fn emit(
    ir: &IrFile,
    plan: &StaticAccessorPlan,
    owner: &str,
    declaration_line: u32,
    cw: &mut ClassWriter,
) {
    for accessor in plan.accessors_of(owner) {
        match *accessor {
            StaticAccessor::Function(function) => {
                emit_function_accessor(ir, function, owner, declaration_line, cw);
            }
            StaticAccessor::Getter(index) => {
                let property = &ir.statics[index as usize];
                let ty = jvm_declared_ty(&property.ty);
                let descriptor = format!("(){}", type_descriptor(ty));
                let name = format!("access${}$p", property_getter_name(&property.name));
                let mut code = CodeBuilder::new(0);
                code.mark_line(declaration_line);
                let field =
                    cw.fieldref(owner, ir.static_field_jvm_name(index), &type_descriptor(ty));
                code.getstatic(field, slot_words(ty) as i32);
                emit_return(ty, &mut code);
                code.ensure_locals(0);
                code.link();
                cw.add_method(0x1019, &name, &descriptor, &code);
            }
            StaticAccessor::Setter(index) => {
                let property = &ir.statics[index as usize];
                let ty = jvm_declared_ty(&property.ty);
                let descriptor = format!("({})V", type_descriptor(ty));
                let name = format!("access${}$p", property_setter_name(&property.name));
                let words = slot_words(ty);
                let mut code = CodeBuilder::new(words);
                code.mark_line(declaration_line);
                load(ty, 0, &mut code);
                let field =
                    cw.fieldref(owner, ir.static_field_jvm_name(index), &type_descriptor(ty));
                code.putstatic(field, words as i32);
                code.ret_void();
                code.ensure_locals(words);
                code.link();
                cw.add_method(0x1019, &name, &descriptor, &code);
                cw.set_method_debug(
                    &name,
                    &descriptor,
                    None,
                    &[("<set-?>".to_string(), type_descriptor(ty), 0)],
                );
            }
        }
    }
}

fn emit_function_accessor(
    ir: &IrFile,
    function: u32,
    owner: &str,
    declaration_line: u32,
    cw: &mut ClassWriter,
) {
    let target = &ir.functions[function as usize];
    let parameters = jvm_function_params(ir, function);
    let result = jvm_declared_ty(&target.ret);
    let descriptor = method_descriptor(&parameters, result);
    let name = format!("access${}", target.name);
    // One accessor per target: a function whose emission-built state machine re-enters it through
    // `access$<name>` already has it.
    if cw.declares_method(&name, &descriptor) {
        return;
    }
    let words: u16 = parameters.iter().map(|ty| slot_words(*ty)).sum();
    let mut code = CodeBuilder::new(words);
    let mut slot = 0u16;
    let mut locals = Vec::new();
    let names = crate::jvm::parameter_names::function_locals(ir, function, &parameters);
    for (ordinal, &ty) in parameters.iter().enumerate() {
        load(ty, slot, &mut code);
        if let Some(local) = names
            .as_ref()
            .and_then(|names| names.get(ordinal).cloned().flatten())
        {
            locals.push((local, type_descriptor(ty), slot));
        }
        slot += slot_words(ty);
    }
    // kotlinc maps the forwarding call, after its operands are loaded.
    code.mark_line(declaration_line);
    let method = cw.methodref(owner, &target.name, &descriptor);
    code.invokestatic(method, words as i32, slot_words(result) as i32);
    emit_return(result, &mut code);
    code.ensure_locals(words);
    code.link();
    cw.add_method(0x1019, &name, &descriptor, &code);
    cw.set_method_debug(&name, &descriptor, None, &locals);
}
