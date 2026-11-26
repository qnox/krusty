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
//! implementation. Frozen declarations for other boxes stay as evidence until their representation
//! is materialized. An actual representation declines when its override cannot be emitted, so no
//! stub reaches a builtin value without its implementation.

use krusty::backend::{BackendClassifierSource, BackendCompilerIntrinsic, BuiltinImplementation};
use krusty::libraries::RuntimeExceptionKind;
use krusty::types::{Ty, TypeName};

use super::boxes::BoxForm;
use super::builtin_order;
use super::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use super::interface_dispatch::MemberRef;
use super::numeric_conversions;
use super::runtime::Runtime;

/// `i32.ge_u`.
const I32_GE_U: u8 = 0x4f;

/// A function's wasm parameters (the receiver first) and results.
type Signature = (Vec<ValType>, Vec<ValType>);

/// The library member a call goes through, as its checked declaration gives it: its name and the
/// builtin classes implementing it.
#[derive(Clone, Copy)]
pub(super) struct LibraryMemberDeclaration<'a> {
    pub(super) name: &'a str,
    pub(super) implementations: &'a [BuiltinImplementation],
}

/// The library members a call has gone through whose builtin implementations are recorded.
#[derive(Default)]
pub(super) struct BuiltinMembers {
    members: Vec<Member>,
    builders: BuilderMembers,
}

/// What the runtime's `StringBuilder` (`super::string_builders`) does for the library members a
/// call goes through. A member it cannot answer declines the program only once a builder is
/// constructed: before that, no builder value reaches its stub.
#[derive(Default)]
struct BuilderMembers {
    constructed: bool,
    unanswered: Vec<(MemberRef, String)>,
}

struct Member {
    member: MemberRef,
    /// What the box of each builtin classifier runs for the member, by the classifier's identity.
    boxes: BoxImplementations,
}

/// Frozen declarations are evidence; only an actual box requests an executable implementation.
struct BoxImplementations {
    signature: Signature,
    declarations: Vec<(TypeName, BuiltinImplementation)>,
}

impl BoxImplementations {
    fn operation(&self, boxed: Ty) -> Result<Option<Operation>, String> {
        let identity = boxed.obj_internal();
        self.declarations
            .iter()
            .find(|(known, _)| Some(*known) == identity)
            .map(|(_, declaration)| {
                operation(Representation::Box(boxed), declaration, &self.signature)
            })
            .transpose()
    }
}

/// The runtime representation a builtin class's values are carried in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Representation {
    String,
    /// The box of this type's classifier.
    Box(Ty),
    /// The runtime's `StringBuilder`.
    StringBuilder,
}

impl Representation {
    /// The builtin values carried in this representation, for a diagnostic.
    fn describe(self) -> String {
        match self {
            Self::String => "a `String`".to_string(),
            Self::Box(boxed) => format!("a boxed `{boxed:?}`"),
            Self::StringBuilder => "a `StringBuilder`".to_string(),
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
    /// The characters of a string between two indices (`String.subSequence`).
    StringSubSequence,
    /// The builder's entry for a member it does not realize while no builder exists: nothing can
    /// call it, and a program constructing a builder declines a call through the member.
    Unreachable,
    /// The order of the value a box holds and the value another box of its class holds
    /// (`Int.compareTo(Int)`).
    Compare,
    /// A builder member the runtime realizes (`StringBuilder.append(Char)`).
    Builder(BuilderMember),
}

/// A `StringBuilder` override the runtime realizes, as a direct call of it does
/// (`super::codegen` realizes those at the builder's receiver).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuilderMember {
    /// `append(CharSequence?)`, as the target declares it (`crate::char_sequence_appends`).
    AppendCharSequence,
    /// `append(Char)`.
    AppendChar,
    /// `length`.
    Length,
    /// `get(Int)`, which throws the runtime `IndexOutOfBoundsException` whose struct is
    /// `index_out_of_bounds` for an index outside the units in use.
    Get { index_out_of_bounds: u32 },
    /// `subSequence(Int, Int)`, which copies the range of the units in use out as a string.
    SubSequence,
}

impl Runtime {
    /// Record the builtin implementations of `member`, the library member `declared` carried as
    /// `signature` (the receiver first), which a call goes through: a string's and the runtime
    /// builder's and list's now, and each box's now or when the box is declared. Declines when a builtin class
    /// runs something this generator cannot write for it.
    pub(super) fn builtin_member(
        &mut self,
        module: &mut Module,
        classifiers: &dyn BackendClassifierSource,
        member: MemberRef,
        declared: LibraryMemberDeclaration<'_>,
        signature: &Signature,
    ) -> Result<(), String> {
        let LibraryMemberDeclaration {
            name,
            implementations,
            ..
        } = declared;
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
        let mut builder = None;
        for implementation in implementations {
            let receiver = implementation.receiver;
            let representation = if receiver == Ty::String {
                Representation::String
            } else if receiver
                .obj_internal()
                .is_some_and(super::string_builders::lays_out)
            {
                Representation::StringBuilder
            } else {
                Representation::Box(receiver)
            };
            match representation {
                Representation::String => {
                    string = Some(operation(representation, implementation, signature)?);
                }
                Representation::StringBuilder => {
                    builder = Some(builder_operation(
                        name,
                        implementation,
                        signature,
                        &mut || {
                            self.exception_class(
                                module,
                                classifiers,
                                RuntimeExceptionKind::IndexOutOfBounds.classifier(),
                            )
                        },
                    ));
                }
                Representation::Box(boxed) => {
                    let identity = boxed
                        .obj_internal()
                        .ok_or_else(|| format!("a library member implemented by `{boxed:?}`"))?;
                    boxes.push((identity, implementation.clone()));
                }
            }
        }
        if let Some(operation) = string {
            if matches!(operation, Operation::StringSubSequence) {
                self.string_sub_sequence(module, classifiers)?;
            }
            let function =
                self.define_implementation(module, member, Receiver::String, operation)?;
            self.objects
                .interfaces
                .implement_representation(member, self.string, function)?;
        }
        match builder {
            Some(Ok(operation)) => {
                if matches!(operation, Operation::Builder(BuilderMember::SubSequence)) {
                    self.units_sub_sequence(module, classifiers)?;
                }
                let function =
                    self.define_implementation(module, member, Receiver::Runtime, operation)?;
                self.objects
                    .interfaces
                    .implement(self.string_builders.class, member, function)?;
            }
            Some(Err(reason)) => {
                let function = self.define_implementation(
                    module,
                    member,
                    Receiver::Runtime,
                    Operation::Unreachable,
                )?;
                self.objects
                    .interfaces
                    .implement(self.string_builders.class, member, function)?;
                self.unanswered_builder_member(member, reason);
            }
            None => {}
        }
        let index = self.builtin_members.members.len();
        self.builtin_members.members.push(Member {
            member,
            boxes: BoxImplementations {
                signature: signature.clone(),
                declarations: boxes,
            },
        });
        for form in self.box_forms() {
            self.implement_box_member(module, index, form)?;
        }
        Ok(())
    }

    /// Record that the program constructs a runtime `StringBuilder`, so a member it cannot answer
    /// declines a call through that member.
    pub(super) fn string_builder_constructed(&mut self) {
        let builders = &mut self.builtin_members.builders;
        if builders.constructed {
            return;
        }
        builders.constructed = true;
        for (member, reason) in std::mem::take(&mut builders.unanswered) {
            self.objects.interfaces.unimplementable(member, reason);
        }
    }

    fn unanswered_builder_member(&mut self, member: MemberRef, reason: String) {
        let builders = &mut self.builtin_members.builders;
        if builders.constructed {
            self.objects.interfaces.unimplementable(member, reason);
        } else {
            builders.unanswered.push((member, reason));
        }
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
        let Some(operation) = recorded.boxes.operation(form.boxed)? else {
            // No value of this boxed classifier reaches the member.
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
                let key = builtin_order::order_key(form.boxed)
                    .ok_or_else(|| format!("an order of boxed `{:?}`", form.boxed))?;
                // The receiver's and the argument's keys, each read out of its box.
                let (left, right) = (2, 3);
                form.emit_unbox(&mut code);
                builtin_order::store_order_key(&mut code, &self.hashing, form.boxed, left);
                code.local_get(1);
                form.emit_unbox(&mut code);
                builtin_order::store_order_key(&mut code, &self.hashing, form.boxed, right);
                locals.extend([key, key]);
                builtin_order::emit_key_order(&mut code, key, left, right);
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
            (Receiver::String, Operation::StringSubSequence) => {
                let slice = self
                    .defined_string_sub_sequence()
                    .ok_or("a string slice implemented before its function")?;
                code.ref_cast(false, HeapType::Concrete(self.string))
                    .local_get(1)
                    .local_get(2)
                    .call(slice);
            }
            (Receiver::Runtime, Operation::Unreachable) => {
                code.unreachable();
            }
            (Receiver::Runtime, Operation::Builder(builder)) => {
                let builders = &self.string_builders;
                match builder {
                    BuilderMember::AppendCharSequence => {
                        let append = self.append_char_sequence(module)?;
                        code.local_get(1).call(append);
                    }
                    BuilderMember::AppendChar => {
                        code.local_get(1).call(builders.append_char);
                    }
                    BuilderMember::Length => {
                        code.call(builders.length);
                    }
                    BuilderMember::SubSequence => {
                        let slice = self
                            .defined_units_sub_sequence()
                            .ok_or("a builder slice implemented before its function")?;
                        code.call(builders.units)
                            .local_get(0)
                            .call(builders.length)
                            .local_get(1)
                            .local_get(2)
                            .call(slice);
                    }
                    BuilderMember::Get {
                        index_out_of_bounds,
                    } => {
                        // `StringBuilder.get` throws for an index outside the units in use, as a
                        // direct call of it does. Unsigned, a negative index is outside too.
                        code.local_get(1)
                            .local_get(0)
                            .call(builders.length)
                            .op(I32_GE_U)
                            .if_(BlockType::Empty);
                        self.exceptions.construct_class(
                            &mut code,
                            index_out_of_bounds,
                            None,
                            self.string,
                        )?;
                        code.throw(self.exceptions.tag).end();
                        code.local_get(1).call(builders.get);
                    }
                }
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
    /// An instance of a class the runtime lays out, which its functions take as a reference.
    Runtime,
    /// The box of a builtin classifier.
    Box(BoxForm),
}

/// What the runtime builder runs for the member `name` carried as `signature`, from
/// `StringBuilder`'s override of it. An override keeps the member's name; its declared parameters
/// and result say which overload it is, as a direct call's selected declaration does.
fn builder_operation(
    name: &str,
    implementation: &BuiltinImplementation,
    (params, results): &Signature,
    index_out_of_bounds: &mut dyn FnMut() -> Result<u32, String>,
) -> Result<Operation, String> {
    let Some(declaration) = &implementation.declaration else {
        return Err(format!(
            "a library member `{name}` a `StringBuilder` inherits"
        ));
    };
    let reference = [crate::objects::REFERENCE];
    let member = match (name, &*declaration.params) {
        ("append", [param])
            if *param == Ty::nullable(Ty::obj_name(krusty::types::wk::char_sequence())) =>
        {
            Some(BuilderMember::AppendCharSequence)
        }
        ("append", [Ty::Char]) if params[1..] == [ValType::I32] => Some(BuilderMember::AppendChar),
        ("length", []) if declaration.ret == Ty::Int && *results == [ValType::I32] => {
            Some(BuilderMember::Length)
        }
        ("get", [Ty::Int]) if declaration.ret == Ty::Char && params[1..] == [ValType::I32] => {
            Some(BuilderMember::Get {
                index_out_of_bounds: index_out_of_bounds()?,
            })
        }
        ("subSequence", [Ty::Int, Ty::Int])
            if declaration.ret == Ty::obj_name(krusty::types::wk::char_sequence())
                && params[1..] == [ValType::I32, ValType::I32]
                && *results == reference =>
        {
            Some(BuilderMember::SubSequence)
        }
        _ => None,
    };
    let appends = matches!(
        member,
        Some(BuilderMember::AppendCharSequence | BuilderMember::AppendChar)
    );
    match member {
        Some(member) if !appends || *results == reference => Ok(Operation::Builder(member)),
        _ => Err(format!(
            "the `StringBuilder` override of `{name}`, which the runtime does not realize"
        )),
    }
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
                && builtin_order::order_key(boxed).is_some()
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
        (Representation::String, Some(BackendCompilerIntrinsic::StringSubSequence))
            if *declaration.params == [Ty::Int, Ty::Int]
                && params[1..] == [ValType::I32, ValType::I32]
                && *results == [crate::objects::REFERENCE] =>
        {
            Ok(Operation::StringSubSequence)
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

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::backend::BuiltinOverrideDeclaration;

    #[test]
    fn only_an_actual_box_requires_its_frozen_override_to_be_executable() {
        let integer = Ty::Int;
        let unsigned = Ty::UByte;
        let implementations = BoxImplementations {
            signature: (vec![crate::objects::REFERENCE; 2], vec![ValType::I32]),
            declarations: vec![
                (
                    integer.obj_internal().expect("boxed Int identity"),
                    BuiltinImplementation {
                        receiver: integer,
                        declaration: Some(BuiltinOverrideDeclaration {
                            operation: Some(BackendCompilerIntrinsic::PrimitiveCompare),
                            params: vec![integer].into_boxed_slice(),
                            ret: Ty::Int,
                        }),
                    },
                ),
                (
                    unsigned.obj_internal().expect("boxed UByte identity"),
                    BuiltinImplementation {
                        receiver: unsigned,
                        declaration: Some(BuiltinOverrideDeclaration {
                            operation: None,
                            params: vec![unsigned].into_boxed_slice(),
                            ret: Ty::Int,
                        }),
                    },
                ),
            ],
        };
        assert!(matches!(
            implementations.operation(integer),
            Ok(Some(Operation::Compare))
        ));
        assert!(matches!(implementations.operation(Ty::Long), Ok(None)));
        assert_eq!(
            implementations.operation(unsigned).map(|_| ()),
            Err(format!(
                "a library member a boxed `{unsigned:?}` overrides with a body"
            )),
        );
    }
}
