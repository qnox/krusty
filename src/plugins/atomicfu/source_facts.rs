//! What the atomicfu transform needs from one checked file, recorded at the handoff.
//!
//! An atomic is a property whose type is one of `kotlinx.atomicfu`'s atomic classes. Each one the
//! transform lowers is recorded with the store that initializes it and every operation the file
//! applies to it, together with the field-updater declarations those operations become. A file
//! that uses an atomic in any form this implementation does not lower is rejected here, with the
//! construct's position, so it is never compiled to a different program than kotlinc's.

use crate::diag::Span;
use crate::fx_hash::HashMap;
use crate::ir::{for_each_child, Callee, ClassId, ExprId, IrCheckedOperation, IrExpr, IrFile};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginRuntime, RuntimeDeclaration, RuntimeFunction,
};
use crate::plugins::{PluginRejection, PluginSourceView};
use crate::types::{type_name, Ty, TypeName};

const PACKAGE: &str = "kotlinx/atomicfu";
const FIELD_UPDATERS: &str = "java/util/concurrent/atomic";

/// The atomicfu facts of one file.
pub struct AtomicfuFileFacts {
    pub(super) updaters: Vec<FieldUpdater>,
    pub(super) operations: Vec<UpdaterOperation>,
}

/// An atomic property of a class, which becomes a volatile field and a static field updater.
pub(super) struct FieldUpdater {
    pub(super) class: ClassId,
    /// Its index among the class's properties.
    pub(super) property: usize,
    pub(super) value: AtomicValue,
    /// The constructor's store of the property's initializer, and the value the `atomic(…)` call
    /// it stores wraps.
    pub(super) initializer: (ExprId, ExprId),
    /// `newUpdater(Class, String)` of the updater class.
    pub(super) new_updater: BoundRuntimeFunction,
}

/// One operation on an atomic property, which becomes a call of its field updater.
pub(super) struct UpdaterOperation {
    /// The operation, which the transform replaces in place.
    pub(super) expression: ExprId,
    /// Its atomic, an index into [`AtomicfuFileFacts::updaters`].
    pub(super) updater: usize,
    /// The operation's read of the atomic, which becomes the read of its updater.
    pub(super) read: ExprId,
    /// The instance whose atomic the operation applies to.
    pub(super) instance: ExprId,
    /// The operation's own arguments.
    pub(super) arguments: Vec<ExprId>,
    /// The updater function the operation calls.
    pub(super) function: BoundRuntimeFunction,
}

/// The value an atomic holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtomicValue {
    Int,
    Long,
}

impl AtomicValue {
    pub(super) fn ty(self) -> Ty {
        match self {
            Self::Int => Ty::Int,
            Self::Long => Ty::Long,
        }
    }

    /// The `java.util.concurrent.atomic` field updater for a field holding the value.
    pub(super) fn updater(self) -> TypeName {
        type_name(&format!(
            "{FIELD_UPDATERS}/{}",
            match self {
                Self::Int => "AtomicIntegerFieldUpdater",
                Self::Long => "AtomicLongFieldUpdater",
            }
        ))
    }
}

/// How the transform treats a type of `kotlinx.atomicfu`.
enum AtomicType {
    Lowered(AtomicValue),
    /// An atomic this implementation does not lower yet.
    Unsupported(&'static str),
}

fn atomic_type(ty: Ty) -> Option<AtomicType> {
    let classifier = ty.obj_internal()?;
    let atomic = |name: &str| classifier == type_name(&format!("{PACKAGE}/{name}"));
    Some(if atomic("AtomicInt") {
        AtomicType::Lowered(AtomicValue::Int)
    } else if atomic("AtomicLong") {
        AtomicType::Lowered(AtomicValue::Long)
    } else if atomic("AtomicBoolean") {
        AtomicType::Unsupported("an AtomicBoolean")
    } else if atomic("AtomicRef") {
        AtomicType::Unsupported("an AtomicRef")
    } else if [
        "AtomicIntArray",
        "AtomicLongArray",
        "AtomicBooleanArray",
        "AtomicArray",
    ]
    .into_iter()
    .any(atomic)
    {
        AtomicType::Unsupported("an atomic array")
    } else {
        return None;
    })
}

/// An operation of an atomic class, by the updater function it becomes and the parameters that
/// function takes after the updated instance. `value` is the atomic's property: a read with no
/// arguments, a write with one.
fn updater_function(
    name: &str,
    arguments: usize,
    value: Ty,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match name {
        "value" if arguments == 0 => ("get", vec![], value),
        "value" => ("set", vec![value], Ty::Unit),
        "lazySet" => ("set", vec![value], Ty::Unit),
        "compareAndSet" => ("compareAndSet", vec![value, value], Ty::Boolean),
        "getAndSet" => ("getAndSet", vec![value], value),
        "getAndIncrement" => ("getAndIncrement", vec![], value),
        "getAndDecrement" => ("getAndDecrement", vec![], value),
        "incrementAndGet" => ("incrementAndGet", vec![], value),
        "decrementAndGet" => ("decrementAndGet", vec![], value),
        "getAndAdd" => ("getAndAdd", vec![value], value),
        "addAndGet" => ("addAndGet", vec![value], value),
        _ => return None,
    })
}

fn unsupported(what: &str) -> String {
    format!(
        "krusty's atomicfu compiler plugin does not support {what} yet; compile this module with \
         kotlinc"
    )
}

pub(super) fn record(
    ir: &mut IrFile,
    source: &PluginSourceView<'_>,
    runtime: &mut dyn PluginRuntime,
    rejections: &mut Vec<PluginRejection>,
) {
    let mut recorder = Recorder {
        ir,
        source,
        runtime,
        rejections,
        updaters: Vec::new(),
        by_property: HashMap::default(),
        bound: HashMap::default(),
    };
    recorder.declarations();
    let operations = recorder.operations();
    let updaters = std::mem::take(&mut recorder.updaters);
    if updaters.is_empty() {
        return;
    }
    ir.atomicfu = Some(Box::new(AtomicfuFileFacts {
        updaters,
        operations,
    }));
}

struct Recorder<'a, 'b> {
    ir: &'a mut IrFile,
    source: &'a PluginSourceView<'b>,
    runtime: &'a mut dyn PluginRuntime,
    rejections: &'a mut Vec<PluginRejection>,
    updaters: Vec<FieldUpdater>,
    /// Each lowered atomic by its checked identity.
    by_property: HashMap<crate::ir::PropertyId, usize>,
    /// Each updater function selected so far.
    bound: HashMap<(TypeName, &'static str), BoundRuntimeFunction>,
}

impl Recorder<'_, '_> {
    fn reject(&mut self, span: Option<Span>, what: &str) {
        self.rejections.push(PluginRejection {
            span: span.unwrap_or(Span::new(0, 0)),
            message: unsupported(what),
        });
    }

    /// Classify every atomic property the file declares.
    fn declarations(&mut self) {
        let mut checked = self
            .ir
            .checked_properties
            .iter()
            .map(|(&id, property)| (id, property.class, property.source_order))
            .collect::<Vec<_>>();
        checked.sort_by_key(|&(_, _, source_order)| source_order);
        for (id, class, source_order) in checked {
            let property = &self.ir.checked_properties[&id];
            let (span, ty, delegated) = (
                property.diagnostic_span,
                property.ty,
                property.delegate.is_some(),
            );
            let Some(atomic) = atomic_type(ty) else {
                continue;
            };
            let value = match atomic {
                AtomicType::Lowered(value) => value,
                AtomicType::Unsupported(what) => {
                    self.reject(span, what);
                    continue;
                }
            };
            if delegated {
                self.reject(span, "a delegated atomic property");
                continue;
            }
            let Some(class) = class else {
                self.reject(span, "a top-level atomic property");
                continue;
            };
            let declaring = &self.ir.classes[class as usize];
            if declaring.is_object || declaring.is_interface {
                self.reject(span, "an atomic property of an object");
                continue;
            }
            let Some(index) = declaring
                .properties
                .iter()
                .position(|declared| declared.source_order == source_order)
            else {
                continue;
            };
            // kotlinc's checker refuses a `var` atomic: only the value inside it may change.
            if declaring.properties[index].is_var {
                self.reject(span, "a mutable atomic property");
                continue;
            }
            let Some(field) = declaring.properties[index].backing_field else {
                self.reject(span, "an atomic property without a backing field");
                continue;
            };
            let Some(initializer) = self.initializer(class, field) else {
                self.reject(span, "an atomic property not initialized by atomic(value)");
                continue;
            };
            let updater = value.updater();
            let Some(new_updater) = self.bind(
                RuntimeDeclaration::Static(updater),
                "newUpdater",
                vec![
                    Ty::platform_nullable(Ty::obj("java/lang/Class")),
                    Ty::platform_nullable(Ty::String),
                ],
                Ty::platform_nullable(Ty::obj_name(updater)),
                span,
            ) else {
                continue;
            };
            self.by_property.insert(id, self.updaters.len());
            self.updaters.push(FieldUpdater {
                class,
                property: index,
                value,
                initializer,
                new_updater,
            });
        }
    }

    /// The constructor's store of `field`, and the value of the `atomic(value)` call it stores.
    fn initializer(&self, class: ClassId, field: u32) -> Option<(ExprId, ExprId)> {
        let body = self.ir.classes[class as usize].init_body?;
        let IrExpr::Block { stmts, .. } = &self.ir.exprs[body as usize] else {
            return None;
        };
        stmts.iter().find_map(|&statement| {
            let IrExpr::SetField {
                class: stored,
                index,
                value,
                ..
            } = &self.ir.exprs[statement as usize]
            else {
                return None;
            };
            if (*stored, *index) != (class, field) {
                return None;
            }
            let IrExpr::Call { callee, args, .. } = &self.ir.exprs[*value as usize] else {
                return None;
            };
            let Callee::External { target, params, .. } = &**callee else {
                return None;
            };
            let declaration = self.source.checked.dependency_callable(*target)?;
            (declaration.owner == type_name(PACKAGE)
                && declaration.name == "atomic"
                && params.len() == 1
                && args.len() == 1)
                .then(|| (statement, args[0]))
        })
    }

    fn bind(
        &mut self,
        declaration: RuntimeDeclaration,
        name: &'static str,
        params: Vec<Ty>,
        ret: Ty,
        span: Option<Span>,
    ) -> Option<BoundRuntimeFunction> {
        let owner = match declaration {
            RuntimeDeclaration::Package(owner)
            | RuntimeDeclaration::Member(owner)
            | RuntimeDeclaration::Static(owner) => owner,
        };
        if let Some(bound) = self.bound.get(&(owner, name)) {
            if bound.params == params {
                return Some(bound.clone());
            }
        }
        let function = RuntimeFunction {
            declaration,
            name,
            params,
            ret,
            appended: Vec::new(),
        };
        match self.runtime.function(&function) {
            Ok(bound) => {
                self.bound.insert((owner, name), bound.clone());
                Some(bound)
            }
            Err(error) => {
                self.rejections.push(PluginRejection {
                    span: span.unwrap_or(Span::new(0, 0)),
                    message: format!("atomicfu cannot call `{}`: {error}", function.described()),
                });
                None
            }
        }
    }

    /// Every operation on a lowered atomic. An atomic read anywhere but as the receiver of an
    /// operation in a body of its own class is rejected, as is any operation this implementation
    /// does not lower.
    fn operations(&mut self) -> Vec<UpdaterOperation> {
        let ir = &*self.ir;
        let mut owner_of = vec![None; ir.exprs.len()];
        for (class, declaration) in ir.classes.iter().enumerate() {
            let roots = declaration
                .methods
                .iter()
                .filter_map(|&function| ir.functions[function as usize].body)
                .chain(declaration.init_body);
            for root in roots {
                let mut pending = vec![root];
                while let Some(expression) = pending.pop() {
                    owner_of[expression as usize] = Some(class as ClassId);
                    for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
                }
            }
        }
        let mut operations = Vec::new();
        let mut receivers = Vec::new();
        for expression in 0..self.ir.exprs.len() as ExprId {
            let Some((name, receiver, arguments)) = self.atomic_member(expression) else {
                continue;
            };
            let span = self.source.expression_span(self.ir, expression);
            let Some((updater, instance)) = self.atomic_read(receiver) else {
                // An atomic property this implementation does not lower is rejected where it is
                // declared.
                if !self.atomic_read_of_any(receiver) {
                    self.reject(
                        span,
                        "an operation on an atomic that is not a property read",
                    );
                }
                continue;
            };
            receivers.push(receiver);
            let class = self.updaters[updater].class;
            if owner_of[expression as usize] != Some(class) {
                self.reject(
                    span,
                    "an atomic operation outside a member function of the atomic's class",
                );
                continue;
            }
            let value = self.updaters[updater].value;
            let Some((function, params, ret)) =
                updater_function(&name, arguments.len(), value.ty())
            else {
                self.reject(span, &format!("the atomic operation `{name}`"));
                continue;
            };
            if params.len() != arguments.len() {
                self.reject(span, &format!("the atomic operation `{name}`"));
                continue;
            }
            let updater_class = value.updater();
            let Some(bound) = self.bind(
                RuntimeDeclaration::Member(updater_class),
                function,
                std::iter::once(Ty::platform_nullable(Ty::obj("kotlin/Any")))
                    .chain(params)
                    .collect(),
                ret,
                span,
            ) else {
                continue;
            };
            operations.push(UpdaterOperation {
                expression,
                read: receiver,
                updater,
                instance,
                arguments,
                function: bound,
            });
        }
        // An atomicfu function an atomic is passed to (`loop`, `update`, …) has no lowering yet.
        for expression in 0..self.ir.exprs.len() as ExprId {
            let IrExpr::Call {
                callee,
                dispatch_receiver,
                args,
            } = &self.ir.exprs[expression as usize]
            else {
                continue;
            };
            let Callee::External { target, .. } = &**callee else {
                continue;
            };
            let Some(declaration) = self.source.checked.dependency_callable(*target) else {
                continue;
            };
            let atomics = dispatch_receiver
                .iter()
                .chain(args)
                .copied()
                .filter(|&argument| self.atomic_read(argument).is_some())
                .collect::<Vec<_>>();
            if declaration.owner != type_name(PACKAGE) || atomics.is_empty() {
                continue;
            }
            let what = format!("the atomic operation `{}`", declaration.name);
            receivers.extend(atomics);
            let span = self.source.expression_span(self.ir, expression);
            self.reject(span, &what);
        }
        // Every other read of a lowered atomic would observe the wrapper the transform removes.
        for expression in 0..self.ir.exprs.len() as ExprId {
            if receivers.contains(&expression) {
                continue;
            }
            if self.atomic_read(expression).is_some() {
                let span = self.source.expression_span(self.ir, expression);
                self.reject(span, "an atomic property used as a value");
            }
        }
        operations
    }

    /// A call of an atomic class's member, or an access of its `value`: the member's name, its
    /// receiver and its arguments.
    fn atomic_member(&self, expression: ExprId) -> Option<(String, ExprId, Vec<ExprId>)> {
        let checked = self.source.checked;
        let (declaration, receiver, arguments) = match &self.ir.exprs[expression as usize] {
            IrExpr::Call {
                callee,
                dispatch_receiver: Some(receiver),
                args,
            } => {
                let Callee::External { target, .. } = &**callee else {
                    return None;
                };
                (
                    checked.dependency_callable(*target)?,
                    *receiver,
                    args.clone(),
                )
            }
            IrExpr::Checked(operation) => match &**operation {
                IrCheckedOperation::ExternalPropertyRead {
                    target,
                    receiver: Some(receiver),
                    ..
                } => (checked.dependency_property(*target)?, *receiver, Vec::new()),
                IrCheckedOperation::ExternalPropertyWrite {
                    target,
                    receiver: Some(receiver),
                    arguments,
                    ..
                } => (
                    checked.dependency_property(*target)?,
                    *receiver,
                    arguments.clone(),
                ),
                _ => return None,
            },
            _ => return None,
        };
        let owner = declaration.owner;
        (owner.parent() == Some(type_name(PACKAGE)) && atomic_type(Ty::obj_name(owner)).is_some())
            .then(|| (declaration.name.to_string(), receiver, arguments))
    }

    /// The lowered atomic `expression` reads, and the instance it reads it from.
    fn atomic_read(&self, expression: ExprId) -> Option<(usize, ExprId)> {
        let IrExpr::Checked(operation) = &self.ir.exprs[expression as usize] else {
            return None;
        };
        let IrCheckedOperation::PropertyRead {
            target,
            dispatch_receiver: Some(instance),
            ..
        } = &**operation
        else {
            return None;
        };
        Some((*self.by_property.get(target)?, *instance))
    }

    /// Whether `expression` reads any property of an atomic type, lowered or not.
    fn atomic_read_of_any(&self, expression: ExprId) -> bool {
        let IrExpr::Checked(operation) = &self.ir.exprs[expression as usize] else {
            return false;
        };
        let IrCheckedOperation::PropertyRead { target, .. } = &**operation else {
            return false;
        };
        self.ir
            .checked_properties
            .get(target)
            .is_some_and(|property| atomic_type(property.ty).is_some())
    }
}
