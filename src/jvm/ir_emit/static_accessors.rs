//! kotlinc's synthetic accessors for PRIVATE static declarations used from another JVM class.
//!
//! A private top-level function or property is a static member of its file facade, and a private
//! `companion { … }` block member one of the class that declared the block: its static placement.
//! Kotlin lets any code of the file use it, while a JVM class may not reach another class's private
//! member. kotlinc's `SyntheticAccessorLowering` therefore gives the static owner one `public static
//! final synthetic` forwarder per target — `access$<name>` for a function, `access$get<X>$p` and
//! `access$set<X>$p` for a property's field — and every use from another class calls it instead.
//! A nested class, a callable-reference carrier and a lambda class are all such other classes.
//! A private member property's field is reached the same way. A class declares
//! `access$get<X>$p(<owner>)` and `access$set<X>$p(<owner>, value)` for the uses that need them.
//! A named object's backing field is itself static, so its bridge is `access$get<X>$p()` /
//! `access$set<X>$p(value)` and reads or writes that field with `getstatic` / `putstatic`. A
//! `field` use inside a declared accessor is that same field access when it is lowered into
//! another class: the accessor reads or writes the field and does not call the accessor that
//! contains the use. A declared accessor stays an instance method, and its bridge still takes
//! the object.
//!
//! The owner appends its accessors after every declared and lifted member, ahead of `<clinit>`, in
//! the order the file first uses them. [`plan`] finds those uses once per emission pass; each use
//! site routes itself through [`routes_through_accessor`], the same rule the plan applies. Owners
//! are [`StaticOwner`] identities throughout; an interface owner's accessors are not `final` and
//! are named by `InterfaceMethodref`s.

use super::access_bridges::ProtectedMemberAccessBridge;
use super::*;
use crate::jvm::private_static_access::{
    bridged_getter, bridged_setter, declared_backing_getter, declared_backing_setter,
    member_property_accessor_name, StaticOwner,
};
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
    /// `access$get<X>(<owner>)`, calling the private source-declared getter of member property
    /// `property` of class `class`.
    MemberGetter { class: u32, property: u32 },
    /// `access$set<X>(<owner>, value)`, calling the private source-declared setter of member
    /// property `property` of `class`.
    MemberSetter { class: u32, property: u32 },
    /// `access$get<X>$p(<owner>)` that reads the backing field, including when the property
    /// declares a getter. A [`Self::MemberGetter`] of that property calls the getter instead.
    FieldGetter { class: u32, property: u32 },
    /// `access$set<X>$p(<owner>, value)` that writes the backing field, including when the
    /// property declares a setter.
    FieldSetter { class: u32, property: u32 },
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
                .filter_map(|property| property.init),
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
                .filter_map(|property| property.init),
        );
        contexts.push(EmissionContext {
            owner: StaticOwner::Class(class.fq_name),
            class: Some(index),
            roots,
        });
    }
    contexts
}

/// Whether code reaches a private static `function` through its owner's accessor: another class
/// needs access, and an inline declaration's delegated helper exports the same boundary to copies.
pub(super) fn routes_through_accessor(
    ir: &IrFile,
    helper_access: &crate::jvm::local_delegate_accessors::HelperAccess,
    emitted_by_owner: bool,
    function: u32,
) -> bool {
    (!emitted_by_owner || helper_access.requires_accessor(function))
        && ir.method_visibility(function).is_private()
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
        property_realizations: env.property_realizations,
        protected_calls: &protected_calls,
        protected: RefCell::default(),
        helper_access: env.local_delegate_access,
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
    // An inline declaration exports its private helper even when this module has no caller in
    // another class: compiled clients must have the same public synthetic access boundary.
    uses.extend(
        env.local_delegate_access
            .exported()
            .map(|(function, owner)| Use {
                line: ir.fn_decl_lines.get(&function).copied().unwrap_or(0),
                owner: StaticOwner::of(owner),
                accessor: StaticAccessor::Function(function),
            }),
    );
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
        if let Some(target) = function_reference_target(ir, reference).filter(|target| {
            routes_through_accessor(ir, env.local_delegate_access, context == owner, *target)
        }) {
            uses.push(Use {
                line: 0,
                owner,
                accessor: StaticAccessor::Function(target),
            });
        }
    }
    let realization = class.prop_ref.as_ref().and_then(|reference| {
        env.property_reference_realizations
            .get(class.fq_name)
            .map(|realization| (reference.mutable, realization))
    });
    if let Some((mutable, realization)) = realization {
        if let Some(storage) = realization.bridged_getter {
            uses.push(Use {
                line: 0,
                owner: storage.owner,
                accessor: StaticAccessor::Getter(storage.index),
            });
        }
        if let Some(storage) = realization.bridged_setter.filter(|_| mutable) {
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
        // A private member property's carrier reads, and a mutable one writes, through the
        // owner's field accessors; a property without a field has no such accessor.
        if let Some(crate::ir::IrLocalPropertyLayout::Member {
            class: declaring,
            property,
            ..
        }) = realization
            .member_access_bridge
            .and_then(|target| ir.local_property_layouts.get(&target))
            .filter(|layout| {
                matches!(
                    layout,
                    crate::ir::IrLocalPropertyLayout::Member {
                        backing_field: Some(_),
                        ..
                    }
                )
            })
        {
            let owner = StaticOwner::Class(ir.classes[*declaring as usize].fq_name);
            let (declaring, property) = (*declaring, *property);
            let declared = &ir.classes[declaring as usize].properties[property as usize];
            uses.push(Use {
                line: 0,
                owner,
                accessor: if declared.getter.is_some() {
                    StaticAccessor::MemberGetter {
                        class: declaring,
                        property,
                    }
                } else {
                    StaticAccessor::FieldGetter {
                        class: declaring,
                        property,
                    }
                },
            });
            if class
                .prop_ref
                .as_ref()
                .is_some_and(|reference| reference.mutable)
            {
                uses.push(Use {
                    line: 0,
                    owner,
                    accessor: if declared.setter.is_some() {
                        StaticAccessor::MemberSetter {
                            class: declaring,
                            property,
                        }
                    } else {
                        StaticAccessor::FieldSetter {
                            class: declaring,
                            property,
                        }
                    },
                });
            }
        }
    }
    uses
}

struct Walk<'a> {
    ir: &'a IrFile,
    class_member_fids: &'a HashSet<u32>,
    property_realizations: &'a crate::jvm::property_realizations::PropertyRealizations,
    /// The protected member calls emitted outside the class that may make them, by call.
    protected_calls: &'a HashMap<crate::ir::ExprId, ProtectedMemberAccessBridge>,
    /// Each protected-member accessor found, once per owner and signature.
    protected: RefCell<Vec<ProtectedMemberAccessBridge>>,
    helper_access: &'a crate::jvm::local_delegate_accessors::HelperAccess,
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
                    routes_through_accessor(ir, self.helper_access, context == owner, function)
                }
                StaticAccessor::Getter(_)
                | StaticAccessor::Setter(_)
                | StaticAccessor::MemberGetter { .. }
                | StaticAccessor::MemberSetter { .. }
                | StaticAccessor::FieldGetter { .. }
                | StaticAccessor::FieldSetter { .. } => context != owner,
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
            } if !self.class_member_fids.contains(function)
                && self.helper_access.foreign_owner(*function).is_none() =>
            {
                Some((StaticOwner::Facade, StaticAccessor::Function(*function)))
            }
            IrExpr::Call {
                callee: Callee::ClassStatic { owner, function },
                ..
            } => Some((
                StaticOwner::Class(*owner),
                StaticAccessor::Function(*function),
            )),
            IrExpr::GetStatic(index) => bridged_getter(self.ir, *index)
                .or_else(|| declared_backing_getter(self.ir, *index))
                .map(|storage| (storage.owner, StaticAccessor::Getter(*index))),
            IrExpr::SetStatic { index, .. } => bridged_setter(self.ir, *index)
                .or_else(|| declared_backing_setter(self.ir, *index))
                .map(|storage| (storage.owner, StaticAccessor::Setter(*index))),
            IrExpr::GetField { class, index, .. } => {
                self.backing_field_accessor(*class, *index, false)
            }
            IrExpr::SetField { class, index, .. } => {
                self.backing_field_accessor(*class, *index, true)
            }
            IrExpr::PropertyRead { .. } => self.private_member_property(expression, true),
            IrExpr::PropertyWrite { .. } => self.private_member_property(expression, false),
            _ => None,
        }
    }

    /// The field accessor a read or write of a private member property uses, when common lowering
    /// marked the property as reached from another class. A read of a property that declares its
    /// getter calls that getter instead.
    fn private_member_property(
        &self,
        expression: crate::ir::ExprId,
        read: bool,
    ) -> Option<(StaticOwner, StaticAccessor)> {
        let crate::jvm::property_realizations::PropertyRealization::Local(target) =
            self.property_realizations.get(expression)?
        else {
            return None;
        };
        let crate::ir::IrLocalPropertyLayout::Member {
            class,
            property,
            private: true,
            ..
        } = *self.ir.local_property_layouts.get(target)?
        else {
            return None;
        };
        let owner = &self.ir.classes[class as usize];
        let declared = owner.properties.get(property as usize)?;
        if !declared.needs_access_bridge || declared.backing_field.is_none() {
            return None;
        }
        let accessor = match (read, declared.getter.is_some(), declared.setter.is_some()) {
            (true, true, _) => return None,
            (true, false, _) => StaticAccessor::FieldGetter { class, property },
            (false, _, true) => StaticAccessor::MemberSetter { class, property },
            (false, _, false) => StaticAccessor::FieldSetter { class, property },
        };
        Some((StaticOwner::Class(owner.fq_name), accessor))
    }

    /// The field accessor a `field` read or write uses when the backing field is private and the
    /// use is lowered as a direct field operation. The accessor always touches the field: a
    /// declared getter or setter is what contains the use, so calling it would re-enter it.
    fn backing_field_accessor(
        &self,
        class: u32,
        field: u32,
        write: bool,
    ) -> Option<(StaticOwner, StaticAccessor)> {
        let class_decl = self.ir.classes.get(class as usize)?;
        let field_decl = class_decl.fields.get(field as usize)?;
        if !field_decl.is_private() || super::static_storage(self.ir, class_decl) {
            return None;
        }
        let property = class_decl
            .properties
            .iter()
            .position(|property| property.backing_field == Some(field))?
            as u32;
        let accessor = if write {
            StaticAccessor::FieldSetter { class, property }
        } else {
            StaticAccessor::FieldGetter { class, property }
        };
        Some((StaticOwner::Class(class_decl.fq_name), accessor))
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
            StaticAccessor::MemberGetter { class, property } => {
                accessor.member_getter(class, property, cw)
            }
            StaticAccessor::MemberSetter { class, property } => {
                accessor.member_setter(class, property, cw)
            }
            StaticAccessor::FieldGetter { class, property } => {
                accessor.field_getter(class, property, cw)
            }
            StaticAccessor::FieldSetter { class, property } => {
                accessor.field_setter(class, property, cw)
            }
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

/// JVM shape of `access$get<X>$p` / `access$set<X>$p` that reads or writes a backing field
/// directly. The call site and the accessor body share it, so the descriptor cannot drift.
struct FieldAccessorShape {
    name: String,
    descriptor: String,
    internal: String,
    field_name: String,
    field_descriptor: String,
    field_ty: Ty,
    words: u16,
}

fn field_accessor_shape(
    ir: &IrFile,
    class: u32,
    property: u32,
    write: bool,
) -> Option<FieldAccessorShape> {
    let owner = ir.classes.get(class as usize)?;
    let property = owner.properties.get(property as usize)?;
    let field = owner.fields.get(property.backing_field? as usize)?;
    let internal = owner.fq_name.render();
    let field_ty = jvm_declared_ty(&field.ty);
    let field_descriptor = type_descriptor(field_ty);
    let name = if write {
        member_property_accessor_name(&property_setter_name(&property.name), false)
    } else {
        member_property_accessor_name(&property_getter_name(&property.name), false)
    };
    let descriptor = if write {
        format!("(L{internal};{field_descriptor})V")
    } else {
        format!("(L{internal};){field_descriptor}")
    };
    Some(FieldAccessorShape {
        name,
        descriptor,
        internal,
        field_name: instance_field_jvm_name(ir, owner, field),
        field_descriptor,
        field_ty,
        words: slot_words(field_ty),
    })
}

/// The `invokestatic` of a private backing field's accessor, when `reader` is not the field's
/// class. `None` keeps the direct `getfield` / `putfield`.
pub(super) fn cross_class_backing_field_method(
    cw: &mut ClassWriter,
    ir: &IrFile,
    facade: &str,
    reader: Option<StaticOwner>,
    class: u32,
    field: u32,
    write: bool,
) -> Option<u16> {
    let class_decl = ir.classes.get(class as usize)?;
    if reader == Some(StaticOwner::Class(class_decl.fq_name)) {
        return None;
    }
    let field_decl = class_decl.fields.get(field as usize)?;
    if !field_decl.is_private() || super::static_storage(ir, class_decl) {
        return None;
    }
    let property = class_decl
        .properties
        .iter()
        .position(|property| property.backing_field == Some(field))? as u32;
    let access = field_accessor_shape(ir, class, property, write)?;
    Some(static_methodref(
        cw,
        ir,
        facade,
        StaticOwner::Class(class_decl.fq_name),
        &access.name,
        &access.descriptor,
    ))
}

/// The accessors one static owner declares.
struct Accessor<'a> {
    ir: &'a IrFile,
    owner: StaticOwner,
    facade: &'a str,
    declaration_line: u32,
    flags: u16,
}

/// The bridge another class uses for private member property `property`. A source-declared
/// accessor is `access$getX` / `access$setX` and carries that accessor's JVM type; a plain field
/// is `access$getX$p` / `access$setX$p`. A named object's plain backing field is static, so its
/// bridge takes no instance. A declared accessor, and every instance field, still receives the owner.
pub(super) fn member_property_access_bridge(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    owner: TypeName,
    property: &crate::ir::IrProperty,
    value: &str,
    read: bool,
) -> crate::jvm::inline::PropertyAccess {
    let owner_spelling = owner.render();
    let declared_accessor = if read {
        property.getter.is_some()
    } else {
        property.setter.is_some()
    };
    let static_field = !declared_accessor && super::static_storage(ir, class);
    let exposed = property
        .backing_field
        .and_then(|index| class.fields.get(index as usize))
        .map(|field| type_descriptor(declared_property_accessor_jvm(ir, property, field)))
        .unwrap_or_else(|| value.to_string());
    let carried = if declared_accessor {
        exposed.as_str()
    } else {
        value
    };
    let descriptor = if read {
        if static_field {
            format!("(){exposed}")
        } else {
            format!("(L{owner_spelling};){carried}")
        }
    } else if static_field {
        format!("({exposed})V")
    } else {
        format!("(L{owner_spelling};{carried})V")
    };
    let accessor = if read {
        property_getter_name(&property.name)
    } else {
        property_setter_name(&property.name)
    };
    let inline_uninitialized_guard = (read && !declared_accessor)
        .then_some(property.backing_field)
        .flatten()
        .and_then(|field| class.fields.get(field as usize))
        .filter(|field| field.is_lateinit())
        .map(|_| property.name.clone());
    crate::jvm::inline::PropertyAccess::AccessBridge {
        owner,
        name: member_property_accessor_name(&accessor, declared_accessor),
        descriptor,
        takes_receiver: !static_field,
        inline_uninitialized_guard,
    }
}

impl Accessor<'_> {
    /// `access$get<X>`: call the private property's source-declared getter.
    fn member_getter(&self, class: u32, property: u32, cw: &mut ClassWriter) {
        let ir = self.ir;
        let owner = &ir.classes[class as usize];
        let property = &owner.properties[property as usize];
        let field = &owner.fields[property
            .backing_field
            .expect("a bridged member property has a backing field")
            as usize];
        let internal = owner.fq_name.render();
        let ty = declared_property_accessor_jvm(ir, property, field);
        let name = member_property_accessor_name(&property_getter_name(&property.name), true);
        let descriptor = format!("(L{internal};){}", type_descriptor(ty));
        let mut code = CodeBuilder::new(1);
        code.mark_line(self.declaration_line);
        code.aload(0);
        let getter = &ir.functions[property
            .getter
            .expect("a member getter bridge calls a declared getter")
            as usize];
        let method = cw.methodref(&internal, &getter.name, &ir_method_desc(&[], &getter.ret));
        code.invokevirtual(method, 0, slot_words(ty) as i32);
        emit_return(ty, &mut code);
        code.ensure_locals(1);
        code.link();
        cw.add_method(self.flags, &name, &descriptor, &code);
        cw.set_method_debug(
            &name,
            &descriptor,
            None,
            &[("$this".to_string(), format!("L{internal};"), 0)],
        );
    }

    /// `access$set<X>`: call the private property's source-declared setter.
    fn member_setter(&self, class: u32, property: u32, cw: &mut ClassWriter) {
        let ir = self.ir;
        let owner = &ir.classes[class as usize];
        let property = &owner.properties[property as usize];
        let field = &owner.fields[property
            .backing_field
            .expect("a bridged member property has a backing field")
            as usize];
        let internal = owner.fq_name.render();
        let ty = declared_property_accessor_jvm(ir, property, field);
        let name = member_property_accessor_name(&property_setter_name(&property.name), true);
        let descriptor = format!("(L{internal};{})V", type_descriptor(ty));
        let words = slot_words(ty);
        let mut code = CodeBuilder::new(1 + words);
        code.mark_line(self.declaration_line);
        code.aload(0);
        load(ty, 1, &mut code);
        let setter = &ir.functions[property
            .setter
            .expect("a member setter bridge calls a declared setter")
            as usize];
        let setter_descriptor = method_descriptor(&[jvm_declared_ty(&setter.params[0])], Ty::Unit);
        let method = cw.methodref(&internal, &setter.name, &setter_descriptor);
        code.invokevirtual(method, words as i32, 0);
        code.ret_void();
        code.ensure_locals(1 + words);
        code.link();
        cw.add_method(self.flags, &name, &descriptor, &code);
        cw.set_method_debug(
            &name,
            &descriptor,
            None,
            &[
                ("$this".to_string(), format!("L{internal};"), 0),
                ("<set-?>".to_string(), type_descriptor(ty), 1),
            ],
        );
    }

    /// `access$get<X>$p`: read the backing field. The property's getter is what contains a nested
    /// `field` use, so this accessor must not call it. A named object's plain field is static:
    /// its bridge takes no receiver and uses `getstatic`.
    fn field_getter(&self, class: u32, property: u32, cw: &mut ClassWriter) {
        if self.emit_static_plain_field(class, property, false, cw) {
            return;
        }
        let access = field_accessor_shape(self.ir, class, property, false)
            .expect("a planned field getter must retain its backing field");
        let mut code = CodeBuilder::new(1);
        code.mark_line(self.declaration_line);
        code.aload(0);
        let field_ref = cw.fieldref(
            &access.internal,
            &access.field_name,
            &access.field_descriptor,
        );
        code.getfield(field_ref, access.words as i32);
        emit_return(access.field_ty, &mut code);
        code.ensure_locals(1);
        code.link();
        cw.add_method(self.flags, &access.name, &access.descriptor, &code);
        cw.set_method_debug(
            &access.name,
            &access.descriptor,
            None,
            &[("$this".to_string(), format!("L{};", access.internal), 0)],
        );
    }

    /// `access$set<X>$p`: write the backing field, without calling a declared setter. A named
    /// object's plain field is static: its bridge takes no receiver and uses `putstatic`.
    fn field_setter(&self, class: u32, property: u32, cw: &mut ClassWriter) {
        if self.emit_static_plain_field(class, property, true, cw) {
            return;
        }
        let access = field_accessor_shape(self.ir, class, property, true)
            .expect("a planned field setter must retain its backing field");
        let mut code = CodeBuilder::new(1 + access.words);
        code.mark_line(self.declaration_line);
        code.aload(0);
        load(access.field_ty, 1, &mut code);
        let field_ref = cw.fieldref(
            &access.internal,
            &access.field_name,
            &access.field_descriptor,
        );
        code.putfield(field_ref, access.words as i32);
        code.ret_void();
        code.ensure_locals(1 + access.words);
        code.link();
        cw.add_method(self.flags, &access.name, &access.descriptor, &code);
        cw.set_method_debug(
            &access.name,
            &access.descriptor,
            None,
            &[
                ("$this".to_string(), format!("L{};", access.internal), 0),
                ("<set-?>".to_string(), access.field_descriptor, 1),
            ],
        );
    }

    /// Receiverless `getstatic` / `putstatic` bridge for a named object's private backing field.
    /// A declared accessor is an instance method and stays on [`Self::member_getter`] /
    /// [`Self::member_setter`]. Returns whether this property took the static path.
    fn emit_static_plain_field(
        &self,
        class: u32,
        property: u32,
        write: bool,
        cw: &mut ClassWriter,
    ) -> bool {
        let ir = self.ir;
        let owner = &ir.classes[class as usize];
        let property = &owner.properties[property as usize];
        let declared = if write {
            property.setter.is_some()
        } else {
            property.getter.is_some()
        };
        if declared || !super::static_storage(ir, owner) {
            return false;
        }
        let field = &owner.fields[property
            .backing_field
            .expect("a bridged member property has a backing field")
            as usize];
        let internal = owner.fq_name.render();
        let field_ty = jvm_declared_ty(&field.ty);
        let field_descriptor = type_descriptor(field_ty);
        let ty = declared_property_accessor_jvm(ir, property, field);
        let physical = instance_field_jvm_name(ir, owner, field);
        if write {
            let name = format!("access${}$p", property_setter_name(&property.name));
            let descriptor = format!("({})V", type_descriptor(ty));
            let words = slot_words(ty);
            let mut code = CodeBuilder::new(words);
            code.mark_line(self.declaration_line);
            load(ty, 0, &mut code);
            emit_backing_field_write_adaptation(ir, cw, &mut code, property, ty, field_ty);
            let field_ref = cw.fieldref(&internal, &physical, &field_descriptor);
            code.putstatic(field_ref, slot_words(field_ty) as i32);
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
        } else {
            let name = format!("access${}$p", property_getter_name(&property.name));
            let descriptor = format!("(){}", type_descriptor(ty));
            let mut code = CodeBuilder::new(0);
            code.mark_line(self.declaration_line);
            let field_ref = cw.fieldref(&internal, &physical, &field_descriptor);
            code.getstatic(field_ref, slot_words(field_ty) as i32);
            emit_backing_field_read_adaptation(ir, cw, &mut code, property, field_ty, ty);
            emit_return(ty, &mut code);
            code.ensure_locals(0);
            code.link();
            cw.add_method(self.flags, &name, &descriptor, &code);
        }
        true
    }

    fn getter(&self, index: u32, cw: &mut ClassWriter) {
        let property = &self.ir.statics[index as usize];
        let ty = jvm_declared_ty(&property.ty);
        let descriptor = format!("(){}", type_descriptor(ty));
        let name = format!("access${}$p", property_getter_name(&property.name));
        reserve_signature(cw, &name, &descriptor);
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
        reserve_signature(cw, &name, &descriptor);
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
        reserve_signature(cw, &name, &descriptor);
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

/// Intern an accessor's name and descriptor before its body, as kotlinc visits a method.
fn reserve_signature(cw: &mut ClassWriter, name: &str, descriptor: &str) {
    cw.reserve_method_name(name);
    cw.reserve_descriptor(descriptor);
}
