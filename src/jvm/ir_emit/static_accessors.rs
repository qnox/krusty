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
//! site routes itself through [`routes_through_accessor`], the same rule the plan applies. Owners
//! are [`StaticOwner`] identities throughout; an interface owner's accessors are not `final` and
//! are named by `InterfaceMethodref`s.

use super::access_bridges::ProtectedMemberAccessBridge;
use super::*;
use crate::jvm::private_static_access::{bridged_storage, StaticOwner};
use std::cell::RefCell;
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
    /// `access$<name>(<receiver>, …)`, calling a protected member of a class in another package:
    /// an index into the plan's protected accessors.
    Protected(u32),
}

/// Every static owner's accessors, in first-use order.
#[derive(Default)]
pub(super) struct StaticAccessorPlan {
    by_owner: HashMap<StaticOwner, Vec<StaticAccessor>>,
    protected: Vec<ProtectedMemberAccessBridge>,
}

impl StaticAccessorPlan {
    fn accessors_of(&self, owner: StaticOwner) -> &[StaticAccessor] {
        self.by_owner.get(&owner).map_or(&[], Vec::as_slice)
    }
}

/// A JVM class whose code this file emits, with the IR roots of that code.
pub(super) struct EmissionContext {
    pub(super) owner: StaticOwner,
    /// The class's index in the file's classes; `None` for the facade.
    class: Option<usize>,
    pub(super) roots: Vec<crate::ir::ExprId>,
}

/// The code of each class the file emits, the facade's first. `class_member_fids` are the
/// functions some class lists as its methods; every other receiverless body is the facade's.
pub(super) fn emission_contexts(
    ir: &IrFile,
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
        owner: StaticOwner::Facade,
        class: None,
        roots: facade_roots,
    }];
    for (index, class) in ir.classes.iter().enumerate() {
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
                .filter(|property| property.owner == Some(class.fq_name))
                .map(|property| property.init),
        );
        contexts.push(EmissionContext {
            owner: StaticOwner::Class(class.fq_name),
            class: Some(index),
            roots,
        });
    }
    contexts
}

/// Whether code reaches a private static `function` through its owner's accessor: exactly when
/// the code is emitted into another class than the owner.
pub(super) fn routes_through_accessor(ir: &IrFile, emitted_by_owner: bool, function: u32) -> bool {
    !emitted_by_owner && ir.method_visibility(function).is_private()
}

/// The constant naming static method `name` of `owner`: an interface's through an
/// `InterfaceMethodref`, as `invokestatic` requires.
pub(super) fn static_methodref(
    cw: &mut ClassWriter,
    ir: &IrFile,
    facade: &str,
    owner: StaticOwner,
    name: &str,
    descriptor: &str,
) -> u16 {
    let internal = owner.internal_name(facade);
    if owner.is_interface(ir) {
        cw.interface_methodref(&internal, name, descriptor)
    } else {
        cw.methodref(&internal, name, descriptor)
    }
}

/// Find every accessor the file's code needs, ordered by the source position of its first use,
/// the order kotlinc's lowering meets them in. A reference carrier's uses count where the
/// reference is written, since that is where kotlinc lowers its body.
pub(super) fn plan(
    ir: &IrFile,
    env: &EmitEnv,
    contexts: &[EmissionContext],
    class_member_fids: &HashSet<u32>,
) -> StaticAccessorPlan {
    let protected_calls = env.run.protected_member_access_bridges.borrow();
    let walk = Walk {
        ir,
        class_member_fids,
        protected_calls: &protected_calls,
        protected: RefCell::default(),
    };
    let mut carriers: HashMap<TypeName, Vec<Use>> = HashMap::new();
    for context in contexts {
        let Some(class) = context
            .class
            .map(|index| &ir.classes[index])
            .filter(|class| class.func_ref.is_some() || class.prop_ref.is_some())
        else {
            continue;
        };
        let mut uses = Vec::new();
        for &root in &context.roots {
            walk.collect(context.owner, root, 0, &HashMap::new(), &mut uses);
        }
        uses.extend(synthesized_carrier_uses(&walk, env, class));
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
        let owner = context.class.map(|index| ir.classes[index].fq_name);
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
                    walk.collect(context.owner, root, 0, &carriers, &mut uses);
                }
            }
        }
    }
    // A stable sort keeps the traversal order among uses on one line.
    uses.sort_by_key(|found| found.line);
    let mut plan = StaticAccessorPlan {
        protected: walk.protected.into_inner(),
        ..StaticAccessorPlan::default()
    };
    let mut seen = HashSet::new();
    for Use {
        owner, accessor, ..
    } in uses
    {
        if seen.insert((owner, accessor)) {
            plan.by_owner.entry(owner).or_default().push(accessor);
        }
    }
    plan
}

/// One use of another class's private static declaration: the accessor it needs and its line.
#[derive(Clone)]
struct Use {
    line: u32,
    owner: StaticOwner,
    accessor: StaticAccessor,
}

/// The uses a carrier's synthesized body makes: a function reference whose `invoke` is not
/// lowered calls its target itself, and a property reference reads and writes bridged storage or
/// calls protected accessors.
fn synthesized_carrier_uses(walk: &Walk, env: &EmitEnv, class: &IrClass) -> Vec<Use> {
    let ir = walk.ir;
    let context = StaticOwner::Class(class.fq_name);
    let mut uses = Vec::new();
    if let Some(reference) = class.func_ref.as_ref().filter(|reference| {
        reference.invoke.is_none()
            && matches!(
                reference.dispatch,
                crate::ir::FrDispatch::Static | crate::ir::FrDispatch::StaticBound
            )
    }) {
        let owner = StaticOwner::of(reference.call_owner);
        if let Some(target) = function_reference_target(ir, reference)
            .filter(|target| routes_through_accessor(ir, context == owner, *target))
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
        uses.push(Use {
            line: 0,
            owner: storage.owner,
            accessor: StaticAccessor::Getter(storage.index),
        });
        if mutable {
            uses.push(Use {
                line: 0,
                owner: storage.owner,
                accessor: StaticAccessor::Setter(storage.index),
            });
        }
    }
    if let Some(realization) = class
        .prop_ref
        .as_ref()
        .and_then(|_| env.property_reference_realizations.get(class.fq_name))
    {
        for bridge in [
            &realization.protected_getter_bridge,
            &realization.protected_setter_bridge,
        ]
        .into_iter()
        .flatten()
        {
            uses.push(walk.protected_use(0, ProtectedMemberAccessBridge::of_reference(bridge)));
        }
    }
    uses
}

struct Walk<'a> {
    ir: &'a IrFile,
    class_member_fids: &'a HashSet<u32>,
    /// The protected member calls emitted outside the class that may make them, by call.
    protected_calls: &'a HashMap<crate::ir::ExprId, ProtectedMemberAccessBridge>,
    /// Each protected-member accessor found, once per owner and signature.
    protected: RefCell<Vec<ProtectedMemberAccessBridge>>,
}

impl Walk<'_> {
    /// The use of protected-member accessor `bridge`, which its owner declares once whichever
    /// call or reference carrier needs it.
    fn protected_use(&self, line: u32, bridge: ProtectedMemberAccessBridge) -> Use {
        let owner = StaticOwner::Class(bridge.owner);
        let signature = bridge.signature();
        let mut known = self.protected.borrow_mut();
        let index = known
            .iter()
            .position(|found| found.owner == bridge.owner && found.signature() == signature)
            .unwrap_or_else(|| {
                known.push(bridge);
                known.len() - 1
            });
        Use {
            line,
            owner,
            accessor: StaticAccessor::Protected(index as u32),
        }
    }
}

impl Walk<'_> {
    /// Record, in evaluation order, each use under `root` that code of `context` makes of another
    /// class's private static declaration. A use without a source line takes its parent's; the
    /// construction of a carrier in `carriers` stands for that carrier's own uses.
    fn collect(
        &self,
        context: StaticOwner,
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
            if let Some(bridge) = self.protected_calls.get(&expression) {
                uses.push(self.protected_use(line, bridge.clone()));
                continue;
            }
            let Some((owner, accessor)) = self.target(expression) else {
                continue;
            };
            let needed = match accessor {
                StaticAccessor::Function(function) => {
                    routes_through_accessor(ir, context == owner, function)
                }
                StaticAccessor::Getter(_) | StaticAccessor::Setter(_) => context != owner,
                StaticAccessor::Protected(_) => true,
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
    fn target(&self, expression: crate::ir::ExprId) -> Option<(StaticOwner, StaticAccessor)> {
        match self.ir.expr(expression) {
            IrExpr::Call {
                callee: Callee::Local(function),
                ..
            } if !self.class_member_fids.contains(function) => {
                Some((StaticOwner::Facade, StaticAccessor::Function(*function)))
            }
            IrExpr::Call {
                callee: Callee::ClassStatic { owner, function },
                ..
            } => Some((
                StaticOwner::Class(*owner),
                StaticAccessor::Function(*function),
            )),
            IrExpr::GetStatic(index) => bridged_storage(self.ir, *index)
                .map(|storage| (storage.owner, StaticAccessor::Getter(*index))),
            IrExpr::SetStatic { index, .. } => bridged_storage(self.ir, *index)
                .map(|storage| (storage.owner, StaticAccessor::Setter(*index))),
            _ => None,
        }
    }
}

/// Append `owner`'s planned accessors to its class, with kotlinc's debug tables: each maps its
/// body to the line the owner is declared on (a facade's is the file's first) and names the
/// parameters it forwards. An interface's accessors are `public static synthetic`, every other
/// owner's `public static final synthetic`.
pub(super) fn emit(
    ir: &IrFile,
    plan: &StaticAccessorPlan,
    owner: StaticOwner,
    facade: &str,
    declaration_line: u32,
    cw: &mut ClassWriter,
) {
    let accessor = Accessor {
        ir,
        owner,
        facade,
        declaration_line,
        flags: if owner.is_interface(ir) {
            0x1009
        } else {
            0x1019
        },
    };
    for planned in plan.accessors_of(owner) {
        match *planned {
            StaticAccessor::Function(function) => accessor.function(function, cw),
            StaticAccessor::Getter(index) => accessor.getter(index, cw),
            StaticAccessor::Setter(index) => accessor.setter(index, cw),
            StaticAccessor::Protected(index) => {
                let bridge = &plan.protected[index as usize];
                let owner = bridge.owner.render();
                super::access_bridges::emit_protected_member_access_bridge(
                    bridge,
                    &owner,
                    cw,
                    declaration_line,
                );
            }
        }
    }
}

/// The accessors one static owner declares.
struct Accessor<'a> {
    ir: &'a IrFile,
    owner: StaticOwner,
    facade: &'a str,
    declaration_line: u32,
    flags: u16,
}

impl Accessor<'_> {
    fn getter(&self, index: u32, cw: &mut ClassWriter) {
        let property = &self.ir.statics[index as usize];
        let ty = jvm_declared_ty(&property.ty);
        let descriptor = format!("(){}", type_descriptor(ty));
        let name = format!("access${}$p", property_getter_name(&property.name));
        let mut code = CodeBuilder::new(0);
        code.mark_line(self.declaration_line);
        let field = self.field(index, ty, cw);
        code.getstatic(field, slot_words(ty) as i32);
        emit_return(ty, &mut code);
        code.ensure_locals(0);
        code.link();
        cw.add_method(self.flags, &name, &descriptor, &code);
    }

    fn setter(&self, index: u32, cw: &mut ClassWriter) {
        let property = &self.ir.statics[index as usize];
        let ty = jvm_declared_ty(&property.ty);
        let descriptor = format!("({})V", type_descriptor(ty));
        let name = format!("access${}$p", property_setter_name(&property.name));
        let words = slot_words(ty);
        let mut code = CodeBuilder::new(words);
        code.mark_line(self.declaration_line);
        load(ty, 0, &mut code);
        let field = self.field(index, ty, cw);
        code.putstatic(field, words as i32);
        code.ret_void();
        code.ensure_locals(words);
        code.link();
        cw.add_method(self.flags, &name, &descriptor, &code);
        cw.set_method_debug(
            &name,
            &descriptor,
            None,
            &[("<set-?>".to_string(), type_descriptor(ty), 0)],
        );
    }

    fn field(&self, index: u32, ty: Ty, cw: &mut ClassWriter) -> u16 {
        cw.fieldref(
            &self.owner.internal_name(self.facade),
            self.ir.static_field_jvm_name(index),
            &type_descriptor(ty),
        )
    }

    fn function(&self, function: u32, cw: &mut ClassWriter) {
        let ir = self.ir;
        let target = &ir.functions[function as usize];
        let parameters = jvm_function_params(ir, function);
        let result = jvm_declared_ty(&target.ret);
        let descriptor = method_descriptor(&parameters, result);
        let name = format!("access${}", target.name);
        // One accessor per target: a function whose emission-built state machine re-enters it
        // through `access$<name>` already has it.
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
        code.mark_line(self.declaration_line);
        let method = static_methodref(cw, ir, self.facade, self.owner, &target.name, &descriptor);
        code.invokestatic(method, words as i32, slot_words(result) as i32);
        emit_return(result, &mut code);
        code.ensure_locals(words);
        code.link();
        cw.add_method(self.flags, &name, &descriptor, &code);
        cw.set_method_debug(&name, &descriptor, None, &locals);
    }
}
