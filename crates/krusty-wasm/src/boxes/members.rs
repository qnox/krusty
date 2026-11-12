//! Library class members a box implements.
//!
//! A primitive's classifier overrides members of the library classes above it: `Int.toLong`
//! implements `Number.toLong`. A call of such a member on a value of the library class goes through
//! a stub of the program's dispatch (`super::super::interface_dispatch`), and each box implements
//! the member with a function that reads the value it holds and performs the primitive's override,
//! which checked FIR froze from the member hierarchy (`krusty::backend::PrimitiveImplementation`).
//!
//! The boxes are every implementation such a stub has: a class this generator declares with a
//! superclass another file declares declines (`super::super::codegen::classes`), so the only
//! instances of a library class's subclasses a module holds are boxes. An interface's member
//! (`Comparable.compareTo`) also has other implementations (strings, enums, the module's own
//! classes) and is not one of these.
//!
//! A member is recorded the first time a call goes through it, and a box the first time a value of
//! its classifier is boxed; whichever comes second writes the box's implementation.

use krusty::backend::{BackendCompilerIntrinsic, KlibDependencyCallable, PrimitiveImplementation};
use krusty::fx_hash::HashMap;
use krusty::types::{Ty, TypeName};

use super::super::encode::{Code, Module, ValType};
use super::super::interface_dispatch::MemberRef;
use super::super::numeric_conversions;
use super::super::runtime::Runtime;
use super::{define, BoxForm};

/// A function's wasm parameters (the receiver first) and results.
type Signature = (Vec<ValType>, Vec<ValType>);

/// The library class members called on a value that may be a box.
#[derive(Default)]
pub(crate) struct BoxMembers {
    members: Vec<Member>,
    /// Each member's index in `members`, by its declaration's KLIB identity.
    by_identity: HashMap<KlibDependencyCallable, usize>,
}

struct Member {
    member: MemberRef,
    signature: Signature,
    /// What the box of each primitive whose classifier is below the member's runs, by that
    /// classifier's identity, or why this generator cannot write it.
    implementations: Vec<(TypeName, Result<Operation, String>)>,
}

/// What a box's implementation of a member does with the value it holds.
#[derive(Clone, Copy)]
enum Operation {
    /// Convert it to this numeric type (`Int.toLong()`).
    Convert(Ty),
}

impl Runtime {
    /// The member of a library class published as `identity`, which the boxes of the primitives
    /// below it implement with `implementations`, carried as `signature` (the receiver first).
    /// Recording it declines when a box this module declared runs something this generator
    /// cannot write.
    pub(crate) fn box_member(
        &mut self,
        module: &mut Module,
        identity: &KlibDependencyCallable,
        signature: Signature,
        implementations: &[PrimitiveImplementation],
    ) -> Result<MemberRef, String> {
        if let Some(&index) = self.boxes.members.by_identity.get(identity) {
            let known = &self.boxes.members.members[index];
            if known.signature != signature {
                return Err("a library class member carried two ways".to_string());
            }
            return Ok(known.member);
        }
        let member = self
            .objects
            .interfaces
            .unnumbered(module, signature.clone());
        let implementations = implementations
            .iter()
            .filter_map(|implementation| {
                let identity = implementation.receiver.obj_internal()?;
                Some((identity, operation(implementation, &signature)))
            })
            .collect();
        let index = self.boxes.members.members.len();
        self.boxes.members.members.push(Member {
            member,
            signature,
            implementations,
        });
        self.boxes
            .members
            .by_identity
            .insert(identity.clone(), index);
        // In declaration order, so the functions are written the same way on every run.
        let mut forms = self.boxes.forms.values().copied().collect::<Vec<_>>();
        forms.sort_unstable_by_key(|form| form.structure);
        for form in forms {
            self.implement_box_member(module, index, form)?;
        }
        Ok(member)
    }

    /// Write the implementation of every recorded member by the new box `form`.
    pub(super) fn implement_box_members(
        &mut self,
        module: &mut Module,
        form: BoxForm,
    ) -> Result<(), String> {
        for index in 0..self.boxes.members.members.len() {
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
        let recorded = &self.boxes.members.members[index];
        let member = recorded.member;
        let identity = form.boxed.obj_internal();
        let Some((_, operation)) = recorded
            .implementations
            .iter()
            .find(|(known, _)| Some(*known) == identity)
        else {
            // The boxed classifier is not below the member's: no value of it reaches it.
            return Ok(());
        };
        let operation = operation.clone()?;
        let mut code = Code::default();
        code.local_get(0);
        form.emit_unbox(&mut code);
        match operation {
            Operation::Convert(to) => numeric_conversions::convert(&mut code, form.boxed, to)?,
        }
        let function = define(
            module,
            self.objects.interfaces.ty(member),
            (Vec::new(), code),
        );
        self.objects
            .interfaces
            .implement(form.class, member, function)
    }
}

/// What the box of `implementation`'s receiver runs for a member carried as `signature`, from its
/// override.
fn operation(
    implementation: &PrimitiveImplementation,
    (params, results): &Signature,
) -> Result<Operation, String> {
    let receiver = implementation.receiver;
    let Some(declaration) = &implementation.declaration else {
        return Err(format!(
            "a `{receiver:?}` box inheriting a library class member's implementation"
        ));
    };
    match declaration.operation {
        Some(BackendCompilerIntrinsic::NumericConversion)
            if declaration.params.is_empty()
                && params.len() == 1
                && *results
                    == Vec::from_iter(numeric_conversions::numeric_carrier(declaration.ret)) =>
        {
            numeric_conversions::convert(&mut Code::default(), receiver, declaration.ret)?;
            Ok(Operation::Convert(declaration.ret))
        }
        operation => Err(format!(
            "a `{receiver:?}` box implementing a library class member as {operation:?}"
        )),
    }
}
