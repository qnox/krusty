//! Library classifier members that builtin classes implement.
//!
//! A builtin class overrides members of the library classifiers above it: `String.length`
//! implements `CharSequence.length`, `Int.toLong` implements `Number.toLong`. A call of such a
//! member goes through the stub of its library member (`super::interface_dispatch`), and each
//! runtime representation of a builtin implements it with a function performing the class's
//! override, which checked FIR froze from the member hierarchy
//! (`krusty::backend::BuiltinImplementation`): a string's own, tested for before any interface table
//! is read, and a box's, which reads the value the box holds first. A box is matched to its builtin
//! class by the classifier's identity.
//!
//! A member's builtin implementations are recorded the first time a call goes through it, and a
//! box the first time a value of its classifier is boxed; whichever comes second writes the box's
//! implementation. The call declines when a builtin class runs something this generator cannot
//! write, so no stub ever reaches a builtin value it has no implementation for.

use krusty::backend::{BackendCompilerIntrinsic, BuiltinImplementation};
use krusty::types::{Ty, TypeName};

use super::boxes::BoxForm;
use super::builtin_order;
use super::encode::{Code, Function, HeapType, Module, ValType};
use super::interface_dispatch::MemberRef;
use super::numeric_conversions;
use super::runtime::Runtime;

/// A function's wasm parameters (the receiver first) and results.
type Signature = (Vec<ValType>, Vec<ValType>);

/// The library members a call has gone through whose builtin implementations are recorded.
#[derive(Default)]
pub(super) struct BuiltinMembers {
    members: Vec<Member>,
}

struct Member {
    member: MemberRef,
    /// What the box of each builtin classifier runs for the member, by the classifier's identity.
    boxes: Vec<(TypeName, Operation)>,
}

/// The runtime representation a builtin class's values are carried in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Representation {
    String,
    /// The box of this type's classifier.
    Box(Ty),
}

impl Representation {
    /// The builtin values carried in this representation, for a diagnostic.
    fn describe(self) -> String {
        match self {
            Self::String => "a `String`".to_string(),
            Self::Box(boxed) => format!("a boxed `{boxed:?}`"),
        }
    }
}

/// What a builtin representation's implementation of a member does with its receiver.
#[derive(Clone, Copy)]
enum Operation {
    /// Convert the value a box holds to this numeric type (`Int.toLong()`).
    Convert(Ty),
    /// A string's length in UTF-16 code units.
    StringLength,
    /// A string's code unit at an index.
    StringGet,
    /// The order of a string and another (`String.compareTo`).
    StringCompare,
    /// The order of the value a box holds and the value another box of its class holds
    /// (`Int.compareTo(Int)`).
    Compare,
}

impl Runtime {
    /// Record the builtin implementations of `member`, a library member carried as `signature`
    /// (the receiver first), which a call goes through: a string's now, and each box's now or when
    /// the box is declared. Declines when a builtin class runs something this generator cannot
    /// write for it.
    pub(super) fn builtin_member(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        signature: &Signature,
        implementations: &[BuiltinImplementation],
    ) -> Result<(), String> {
        if self
            .builtin_members
            .members
            .iter()
            .any(|known| known.member == member)
        {
            return Ok(());
        }
        let mut boxes = Vec::new();
        let mut string = None;
        for implementation in implementations {
            let receiver = implementation.receiver;
            let representation = if receiver == Ty::String {
                Representation::String
            } else {
                Representation::Box(receiver)
            };
            let operation = operation(representation, implementation, signature)?;
            match representation {
                Representation::String => string = Some(operation),
                Representation::Box(boxed) => {
                    let identity = boxed
                        .obj_internal()
                        .ok_or_else(|| format!("a library member implemented by `{boxed:?}`"))?;
                    boxes.push((identity, operation));
                }
            }
        }
        if let Some(operation) = string {
            let function =
                self.define_implementation(module, member, Receiver::String, operation)?;
            self.objects
                .interfaces
                .implement_representation(member, self.string, function)?;
        }
        let index = self.builtin_members.members.len();
        self.builtin_members.members.push(Member { member, boxes });
        for form in self.box_forms() {
            self.implement_box_member(module, index, form)?;
        }
        Ok(())
    }

    /// Write the implementation of every recorded member by the new box `form`.
    pub(super) fn implement_box_members(
        &mut self,
        module: &mut Module,
        form: BoxForm,
    ) -> Result<(), String> {
        for index in 0..self.builtin_members.members.len() {
            self.implement_box_member(module, index, form)?;
        }
        Ok(())
    }

    /// Write the box `form`'s implementation of the member at `index`, if it has one.
    fn implement_box_member(
        &mut self,
        module: &mut Module,
        index: usize,
        form: BoxForm,
    ) -> Result<(), String> {
        let recorded = &self.builtin_members.members[index];
        let member = recorded.member;
        let identity = form.boxed.obj_internal();
        let Some(&(_, operation)) = recorded
            .boxes
            .iter()
            .find(|(known, _)| Some(*known) == identity)
        else {
            // The boxed classifier is not below the member's: no value of it reaches it.
            return Ok(());
        };
        let function =
            self.define_implementation(module, member, Receiver::Box(form), operation)?;
        self.objects
            .interfaces
            .implement(form.class, member, function)
    }

    /// A function carried as `member` is that reads its receiver as `receiver` holds it and
    /// performs `operation` on it and the member's other parameters.
    fn define_implementation(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        receiver: Receiver,
        operation: Operation,
    ) -> Result<u32, String> {
        let mut code = Code::default();
        let mut locals = Vec::new();
        code.local_get(0);
        match (receiver, operation) {
            (Receiver::Box(form), Operation::Compare) => {
                let carrier = builtin_order::ordered_carrier(form.boxed)
                    .ok_or_else(|| format!("an order of boxed `{:?}`", form.boxed))?;
                // The receiver and the argument, each read out of its box, then the scratch.
                let (left, right) = (2, 3);
                form.emit_unbox(&mut code);
                code.local_set(left).local_get(1);
                form.emit_unbox(&mut code);
                code.local_set(right);
                locals.extend([carrier, carrier]);
                locals.extend(builtin_order::order_scratch(carrier));
                builtin_order::emit_order(&mut code, carrier, left, right, 4);
            }
            (Receiver::String, Operation::StringCompare) => {
                let compare = self.string_compare(module);
                code.ref_cast(false, HeapType::Concrete(self.string))
                    .local_get(1)
                    .ref_cast(false, HeapType::Concrete(self.string))
                    .call(compare);
            }
            (Receiver::Box(form), Operation::Convert(to)) => {
                form.emit_unbox(&mut code);
                numeric_conversions::convert(&mut code, form.boxed, to)?;
            }
            (Receiver::String, Operation::StringLength) => {
                code.ref_cast(false, HeapType::Concrete(self.string))
                    .struct_get(self.string, 0)
                    .array_len();
            }
            (Receiver::String, Operation::StringGet) => {
                code.ref_cast(false, HeapType::Concrete(self.string))
                    .struct_get(self.string, 0)
                    .local_get(1)
                    .array_get_u(self.chars);
            }
            _ => return Err("a builtin implementation of another representation".to_string()),
        }
        let function = module.declare();
        module.define(
            function,
            Function {
                ty: self.objects.interfaces.ty(member),
                locals,
                code,
            },
        );
        Ok(function)
    }
}

/// How an implementation reads its receiver.
#[derive(Clone, Copy)]
enum Receiver {
    String,
    /// The box of a builtin classifier.
    Box(BoxForm),
}

/// What `representation` runs for a member carried as `signature`, from its class's override.
fn operation(
    representation: Representation,
    implementation: &BuiltinImplementation,
    (params, results): &Signature,
) -> Result<Operation, String> {
    let Some(declaration) = &implementation.declaration else {
        return Err(format!(
            "a library member {} inherits",
            representation.describe()
        ));
    };
    match (representation, declaration.operation) {
        (Representation::Box(boxed), Some(BackendCompilerIntrinsic::NumericConversion))
            if declaration.params.is_empty()
                && params.len() == 1
                && *results
                    == Vec::from_iter(numeric_conversions::numeric_carrier(declaration.ret)) =>
        {
            numeric_conversions::convert(&mut Code::default(), boxed, declaration.ret)?;
            Ok(Operation::Convert(declaration.ret))
        }
        (Representation::String, Some(BackendCompilerIntrinsic::StringLength))
            if declaration.params.is_empty() && params.len() == 1 && *results == [ValType::I32] =>
        {
            Ok(Operation::StringLength)
        }
        (Representation::String, Some(BackendCompilerIntrinsic::StringGet))
            if *declaration.params == [Ty::Int]
                && declaration.ret == Ty::Char
                && params[1..] == [ValType::I32]
                && *results == [ValType::I32] =>
        {
            Ok(Operation::StringGet)
        }
        (Representation::Box(boxed), Some(BackendCompilerIntrinsic::PrimitiveCompare))
            if *declaration.params == [boxed]
                && declaration.ret == Ty::Int
                && builtin_order::ordered_carrier(boxed).is_some()
                && params.len() == 2
                && *results == [ValType::I32] =>
        {
            Ok(Operation::Compare)
        }
        (Representation::String, Some(BackendCompilerIntrinsic::StringCompare))
            if *declaration.params == [Ty::String]
                && declaration.ret == Ty::Int
                && params.len() == 2
                && *results == [ValType::I32] =>
        {
            Ok(Operation::StringCompare)
        }
        (_, Some(operation)) => Err(format!(
            "a library member {} overrides as {operation:?}",
            representation.describe()
        )),
        (_, None) => Err(format!(
            "a library member {} overrides with a body",
            representation.describe()
        )),
    }
}
