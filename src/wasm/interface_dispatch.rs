//! Calls through an interface, `is` against one, and the interface tables both read: what only the
//! whole program decides, written when the module is finished, as the link does on Native
//! (`crate::native::interface_calls`).
//!
//! An interface member is numbered by the module that declares its interface
//! (`crate::backend::class_tables::module_interfaces`), so it is identified here by that module and
//! its number; a dependent module reaches it by the identity the declaring module published for it
//! (`crate::backend::interface_identity`). A call through a member is `call $stub`, one function
//! per member that no file defines. Every class with instances records, for each member it
//! implements, the function standing there, and every interface it is an instance of. When the
//! module is finished each stub is written from those records: a direct call when one function
//! implements the member in the whole program, one `ref.test` and two direct calls when one struct
//! type separates the classes of the two functions that do, and otherwise a lookup in the
//! receiver's interface table. The members looked up are packed so that no class's table holds two
//! at one index; a class's table is an immutable `(array funcref)` its dispatch table points to,
//! null when it holds none. An `is` function per interface tests every implementing struct.

use crate::fx_hash::HashMap;
use crate::types::TypeName;

use super::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use super::objects::{ObjectRoot, ITABLE_FIELD, REFERENCE};
use super::runtime::OR32;

/// One interface member of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct MemberRef(u32);

/// One class with instances of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct ClassRef(u32);

/// The program's interface members, their implementations and the interfaces' instances.
#[derive(Default)]
pub(super) struct InterfaceDispatch {
    members: Vec<Member>,
    /// Each member by the module that numbered it and its number there.
    numbered: HashMap<(String, u32), MemberRef>,
    /// Each member a source module published an identity for, by that identity.
    identities: HashMap<Vec<u8>, MemberRef>,
    classes: Vec<ClassRecord>,
    /// The `is` function of each interface something tests, in the order they were asked for.
    tests: Vec<(TypeName, u32)>,
}

struct Member {
    params: Vec<ValType>,
    results: Vec<ValType>,
    ty: u32,
    /// Declared once a call goes through the member.
    stub: Option<u32>,
    implementations: Vec<(ClassRef, u32)>,
}

struct ClassRecord {
    /// The class's struct, then each struct it is declared a subtype of, nearest first.
    ancestors: Vec<u32>,
    /// The global its dispatch table reads its interface table from.
    itable: u32,
    interfaces: Vec<TypeName>,
}

/// How a stub reaches the implementations of its member.
enum Shape {
    /// Nothing implements it, so nothing reaches it.
    Unreachable,
    Direct(u32),
    /// `then` for an instance of `structure`, `otherwise` for every other implementation.
    Tested {
        structure: u32,
        then: u32,
        otherwise: u32,
    },
    /// Through the receiver's interface table, at this index.
    Table(u32),
}

impl InterfaceDispatch {
    /// The member numbered `number` by `module_name`, carried as `params` (the receiver first) and
    /// `results`. Every file naming the member must carry it alike.
    pub(super) fn numbered(
        &mut self,
        module: &mut Module,
        module_name: &str,
        number: u32,
        (params, results): (Vec<ValType>, Vec<ValType>),
    ) -> Result<MemberRef, String> {
        if let Some(&member) = self.numbered.get(&(module_name.to_owned(), number)) {
            let known = &self.members[member.0 as usize];
            if known.params != params || known.results != results {
                return Err("an interface member carried two ways".to_string());
            }
            return Ok(member);
        }
        let member = MemberRef(self.members.len() as u32);
        let ty = module.func_type(params.clone(), results.clone());
        self.members.push(Member {
            params,
            results,
            ty,
            stub: None,
            implementations: Vec::new(),
        });
        self.numbered
            .insert((module_name.to_owned(), number), member);
        Ok(member)
    }

    /// Publish `identity` as `member`'s, for a dependent module to name it by.
    pub(super) fn publish(&mut self, identity: Vec<u8>, member: MemberRef) -> Result<(), String> {
        match self.identities.insert(identity, member) {
            Some(previous) if previous != member => {
                Err("two interface members published one identity".to_string())
            }
            _ => Ok(()),
        }
    }

    /// The member a source module published `identity` for; `None` for a member of a library
    /// interface, which no stub calls.
    pub(super) fn identified(&self, identity: &[u8]) -> Option<MemberRef> {
        self.identities.get(identity).copied()
    }

    /// The function type `member` is carried as.
    pub(super) fn ty(&self, member: MemberRef) -> u32 {
        self.members[member.0 as usize].ty
    }

    /// The stub a call through `member` goes to.
    pub(super) fn stub(&mut self, module: &mut Module, member: MemberRef) -> u32 {
        *self.members[member.0 as usize]
            .stub
            .get_or_insert_with(|| module.declare())
    }

    /// Record a class with instances whose struct is `ancestors[0]`, declared a subtype of the
    /// rest; the global its dispatch table reads its interface table from comes with it.
    pub(super) fn class(
        &mut self,
        module: &mut Module,
        itable_type: u32,
        ancestors: Vec<u32>,
    ) -> (ClassRef, u32) {
        let mut init = Code::default();
        init.ref_null(HeapType::Concrete(itable_type));
        let itable = module.constant_global(ValType::nullable(itable_type), init);
        let class = ClassRef(self.classes.len() as u32);
        self.classes.push(ClassRecord {
            ancestors,
            itable,
            interfaces: Vec::new(),
        });
        (class, itable)
    }

    /// Whether `class` already records an implementation of `member`.
    pub(super) fn implements(&self, class: ClassRef, member: MemberRef) -> bool {
        self.members[member.0 as usize]
            .implementations
            .iter()
            .any(|(known, _)| *known == class)
    }

    /// Record that `function`, carried as `member` is, implements `member` for `class`.
    pub(super) fn implement(
        &mut self,
        class: ClassRef,
        member: MemberRef,
        function: u32,
    ) -> Result<(), String> {
        let implementations = &mut self.members[member.0 as usize].implementations;
        match implementations.iter().find(|(known, _)| *known == class) {
            Some((_, known)) if *known != function => {
                Err("a class implementing one interface member twice".to_string())
            }
            Some(_) => Ok(()),
            None => {
                implementations.push((class, function));
                Ok(())
            }
        }
    }

    /// Record that `class`'s instances are instances of `interface`.
    pub(super) fn instance_of(&mut self, class: ClassRef, interface: TypeName) {
        let interfaces = &mut self.classes[class.0 as usize].interfaces;
        if !interfaces.contains(&interface) {
            interfaces.push(interface);
        }
    }

    /// `(ref null eq) -> i32`: whether a value is an instance of `interface`.
    pub(super) fn test(&mut self, module: &mut Module, interface: TypeName) -> u32 {
        if let Some((_, function)) = self.tests.iter().find(|(known, _)| *known == interface) {
            return *function;
        }
        let function = module.declare();
        self.tests.push((interface, function));
        function
    }

    /// Write every stub, interface table and `is` function from what the program recorded.
    pub(super) fn finalize(self, module: &mut Module, objects: &ObjectRoot) {
        let mut tables: Vec<Vec<Option<u32>>> = vec![Vec::new(); self.classes.len()];
        let mut stubs = Vec::new();
        for member in &self.members {
            let Some(stub) = member.stub else {
                continue;
            };
            let shape = self.shape(member).unwrap_or_else(|| {
                let index = (0..)
                    .find(|index| {
                        member.implementations.iter().all(|(class, _)| {
                            tables[class.0 as usize]
                                .get(*index as usize)
                                .is_none_or(Option::is_none)
                        })
                    })
                    .expect("some index is free in every table");
                for (class, function) in &member.implementations {
                    let table = &mut tables[class.0 as usize];
                    if table.len() <= index as usize {
                        table.resize(index as usize + 1, None);
                    }
                    table[index as usize] = Some(*function);
                }
                Shape::Table(index)
            });
            stubs.push((stub, member, shape));
        }
        for (stub, member, shape) in stubs {
            let code = stub_body(member, &shape, objects);
            module.define(
                stub,
                Function {
                    ty: member.ty,
                    locals: Vec::new(),
                    code,
                },
            );
        }
        for (class, entries) in self.classes.iter().zip(tables) {
            if entries.is_empty() {
                continue;
            }
            let mut init = Code::default();
            for entry in &entries {
                match entry {
                    Some(function) => {
                        module.reference_function(*function);
                        init.ref_func(*function);
                    }
                    None => {
                        init.ref_null(HeapType::Func);
                    }
                }
            }
            init.array_new_fixed(objects.itable, entries.len() as u32);
            module.set_constant_init(class.itable, init);
        }
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        for (interface, function) in &self.tests {
            let code = self.test_body(*interface);
            module.define(
                *function,
                Function {
                    ty,
                    locals: Vec::new(),
                    code,
                },
            );
        }
    }

    /// The stub shape that needs no interface table, if `member` has one.
    fn shape(&self, member: &Member) -> Option<Shape> {
        let mut functions = member
            .implementations
            .iter()
            .map(|(_, function)| *function)
            .collect::<Vec<_>>();
        functions.sort_unstable();
        functions.dedup();
        match functions[..] {
            [] => Some(Shape::Unreachable),
            [only] => Some(Shape::Direct(only)),
            [first, second] => {
                [(first, second), (second, first)]
                    .into_iter()
                    .find_map(|(then, otherwise)| {
                        let classes = |function: u32| {
                            member
                                .implementations
                                .iter()
                                .filter(move |(_, known)| *known == function)
                                .map(|(class, _)| &self.classes[class.0 as usize])
                        };
                        let seed = classes(then).next()?;
                        let structure = seed.ancestors.iter().copied().find(|structure| {
                            classes(then).all(|class| class.ancestors.contains(structure))
                                && classes(otherwise)
                                    .all(|class| !class.ancestors.contains(structure))
                        })?;
                        Some(Shape::Tested {
                            structure,
                            then,
                            otherwise,
                        })
                    })
            }
            _ => None,
        }
    }

    /// `ref.test` of every struct implementing `interface`, but a subtype of one already tested.
    fn test_body(&self, interface: TypeName) -> Code {
        let implementors = self
            .classes
            .iter()
            .filter(|class| class.interfaces.contains(&interface))
            .collect::<Vec<_>>();
        let mut code = Code::default();
        code.i32_const(0);
        let mut tested = Vec::new();
        for class in &implementors {
            let structure = class.ancestors[0];
            let covered = implementors
                .iter()
                .any(|other| class.ancestors[1..].contains(&other.ancestors[0]));
            if covered || tested.contains(&structure) {
                continue;
            }
            tested.push(structure);
            code.local_get(0)
                .ref_test(false, HeapType::Concrete(structure))
                .op(OR32);
        }
        code
    }
}

/// Push every parameter of a stub carried as `member` is.
fn forward(code: &mut Code, member: &Member) {
    for local in 0..member.params.len() as u32 {
        code.local_get(local);
    }
}

fn stub_body(member: &Member, shape: &Shape, objects: &ObjectRoot) -> Code {
    let mut code = Code::default();
    match *shape {
        Shape::Unreachable => {
            code.unreachable();
        }
        Shape::Direct(function) => {
            forward(&mut code, member);
            code.call(function);
        }
        Shape::Tested {
            structure,
            then,
            otherwise,
        } => {
            let result = match member.results[..] {
                [] => BlockType::Empty,
                [value] => BlockType::Value(value),
                _ => unreachable!("a Kotlin member answers one value at most"),
            };
            code.local_get(0)
                .ref_test(false, HeapType::Concrete(structure))
                .if_(result);
            forward(&mut code, member);
            code.call(then).else_();
            forward(&mut code, member);
            code.call(otherwise).end();
        }
        Shape::Table(index) => {
            forward(&mut code, member);
            code.local_get(0)
                .ref_cast(false, HeapType::Concrete(objects.object))
                .struct_get(objects.object, 0)
                .struct_get(objects.any_table, ITABLE_FIELD)
                .i32_const(index as i32)
                .array_get(objects.itable)
                .ref_cast(false, HeapType::Concrete(member.ty))
                .call_ref(member.ty);
        }
    }
    code
}
