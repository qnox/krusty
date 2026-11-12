//! `kotlin.reflect.KType` objects: what `typeOf<T>()` makes on this target.
//!
//! Every `typeOf` site names one fixed type (inline specialization has replaced a reified
//! parameter by the call site's type, and the frontend has approximated an intersection), so the
//! whole description is known when the module is written. A description is numbered once per
//! module: two sites naming one type get one number, and two types never share one. An object is
//! then `{ table, number, text }`, where `text` is what Kotlin/Wasm's `KTypeImpl.toString` prints
//! for the type. `equals` compares numbers, as the library's structural comparison of classifier,
//! arguments and nullability would; `hashCode` is the text's, so equal types hash alike; and
//! `toString` answers the text.
//!
//! The object's struct is declared the first time a module names a type, with its own dispatch
//! table and an interface record saying its instances are `KType`s. The table carries the type
//! information of `kotlin.wasm.internal.KTypeImpl`, the class Kotlin/Wasm gives every one of these
//! objects, so a bound `::class` answers that class.

use krusty::backend::class_names::{member_class_names, ReflectionNames};
use krusty::fx_hash::HashMap;
use krusty::types::TypeName;

use super::encode::{BlockType, Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::objects::{immutable, ObjectRoot};
use super::runtime::{EQ32, EQZ32};

const NUMBER_FIELD: u32 = 1;
const TEXT_FIELD: u32 = 2;

/// A type as `typeOf` describes it: its classifier, its arguments and whether it is marked
/// nullable.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct TypeDescription {
    pub(super) classifier: Classifier,
    pub(super) arguments: Vec<Projection>,
    pub(super) nullable: bool,
}

/// The classifier a [`TypeDescription`] names, by identity. Only classifiers with a Kotlin
/// qualified name are described, so each names exactly one classifier of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Classifier {
    /// A declared classifier other than a `FunctionN`.
    Declared(TypeName),
    /// The `FunctionN` of this arity, whether the type is spelled `(A) -> B` or `Function1<A, B>`.
    Function(u8),
}

impl Classifier {
    /// The Kotlin qualified name Kotlin/Wasm prints for the classifier.
    fn text(self) -> String {
        match self {
            Self::Declared(name) => name.kotlin_qualified_name(),
            Self::Function(arity) => format!("kotlin.Function{arity}"),
        }
    }
}

/// One type argument of a [`TypeDescription`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Projection {
    Star,
    Invariant(TypeDescription),
    In(TypeDescription),
    Out(TypeDescription),
}

impl TypeDescription {
    /// What Kotlin/Wasm's `KTypeImpl.toString` prints: the classifier's qualified name, the
    /// arguments as `KTypeProjection.toString` prints each (`*`, `T`, `in T`, `out T`) between
    /// `<` and `>`, then `?` when marked nullable.
    pub(super) fn text(&self) -> String {
        let mut text = self.classifier.text();
        if !self.arguments.is_empty() {
            text.push('<');
            for (index, argument) in self.arguments.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                match argument {
                    Projection::Star => text.push('*'),
                    Projection::Invariant(ty) => text.push_str(&ty.text()),
                    Projection::In(ty) => {
                        text.push_str("in ");
                        text.push_str(&ty.text());
                    }
                    Projection::Out(ty) => {
                        text.push_str("out ");
                        text.push_str(&ty.text());
                    }
                }
            }
            text.push('>');
        }
        if self.nullable {
            text.push('?');
        }
        text
    }
}

/// The object a `typeOf` site pushes: `global.get table; i32.const number; <text>; struct.new
/// structure`.
pub(super) struct TypeObject {
    pub(super) structure: u32,
    pub(super) table: u32,
    pub(super) number: i32,
    pub(super) text: String,
}

/// The module's `KType` objects: their struct and table once one is named, and every description
/// numbered so far.
#[derive(Default)]
pub(super) struct TypeObjects {
    declared: Option<Declared>,
    numbers: HashMap<TypeDescription, i32>,
}

#[derive(Clone, Copy)]
struct Declared {
    structure: u32,
    table: u32,
}

impl TypeObjects {
    /// The object describing `description`, declaring the struct and its table on first use.
    /// `string_hash` is the runtime's `(String?) -> Int`.
    pub(super) fn object(
        &mut self,
        module: &mut Module,
        objects: &mut ObjectRoot,
        string: ValType,
        string_hash: u32,
        description: TypeDescription,
    ) -> Result<TypeObject, String> {
        let declared = match self.declared {
            Some(declared) => declared,
            None => {
                let declared = Self::declare(module, objects, string, string_hash);
                self.declared = Some(declared);
                declared
            }
        };
        let next = i32::try_from(self.numbers.len()).map_err(|_| "too many `typeOf` types")?;
        let text = description.text();
        let number = *self.numbers.entry(description).or_insert(next);
        Ok(TypeObject {
            structure: declared.structure,
            table: declared.table,
            number,
            text,
        })
    }

    fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        string: ValType,
        string_hash: u32,
    ) -> Declared {
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                immutable(string),
            ]),
            supertype: Some(objects.object),
            open: false,
        });
        let this = || {
            let mut code = Code::default();
            code.local_get(0)
                .ref_cast(false, HeapType::Concrete(structure));
            code
        };
        let [equals_ty, hash_code_ty, to_string_ty] = objects.any_slot_types;

        let equals = module.declare();
        let mut code = Code::default();
        code.local_get(0)
            .local_get(1)
            .ref_eq()
            .if_(BlockType::Empty)
            .i32_const(1)
            .return_()
            .end()
            .local_get(1)
            .ref_test(false, HeapType::Concrete(structure))
            .op(EQZ32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();
        for value in [0, 1] {
            code.local_get(value)
                .ref_cast(false, HeapType::Concrete(structure))
                .struct_get(structure, NUMBER_FIELD);
        }
        code.op(EQ32);
        module.define(
            equals,
            Function {
                ty: equals_ty,
                locals: Vec::new(),
                code,
            },
        );

        let hash_code = module.declare();
        let mut code = this();
        code.struct_get(structure, TEXT_FIELD).call(string_hash);
        module.define(
            hash_code,
            Function {
                ty: hash_code_ty,
                locals: Vec::new(),
                code,
            },
        );

        let to_string = module.declare();
        let mut code = this();
        code.struct_get(structure, TEXT_FIELD);
        module.define(
            to_string,
            Function {
                ty: to_string_ty,
                locals: Vec::new(),
                code,
            },
        );

        let (class, itable) = objects
            .interfaces
            .class(module, objects.itable, vec![structure]);
        objects.interfaces.instance_of(class, ktype());
        let mut init = Code::default();
        for function in [equals, hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable);
        // Every `typeOf` object is an instance of Kotlin/Wasm's internal `KTypeImpl`. That class
        // is not published to a dependent compilation, so its names are this runtime's fact about
        // the objects it builds, read from the classifier identity rather than guessed.
        let info = objects.type_infos.global(module, ktype_impl());
        objects
            .type_infos
            .name(module, ktype_impl(), &ktype_impl_names());
        objects.push_type_info(&mut init, Some(info));
        init.struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        Declared { structure, table }
    }
}

/// `kotlin.reflect.KType`, the interface every object here is an instance of.
pub(super) fn ktype() -> TypeName {
    krusty::types::wk::ktype()
}

/// `kotlin.wasm.internal.KTypeImpl`, the class of every object here.
fn ktype_impl() -> TypeName {
    krusty::types::type_name("kotlin/wasm/internal/KTypeImpl")
}

/// The reflection names of [`ktype_impl`], from its classifier identity.
fn ktype_impl_names() -> ReflectionNames {
    let classifier = ktype_impl();
    let names = member_class_names(classifier);
    ReflectionNames {
        // Boundary conversion: the package path is spelled with `/` in the name tree.
        package: classifier.package().replace('/', "."),
        simple: Some(names.simple),
        qualified: Some(names.qualified),
    }
}
