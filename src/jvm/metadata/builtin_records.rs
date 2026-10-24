//! The records a `.kotlin_builtins` fragment decodes into: its classes with their members and
//! constructors, and its top-level functions and properties.

use crate::libraries::TypeKind;
use crate::types::Visibility;

/// A type decoded from a `.kotlin_builtins` fragment. A bare internal name cannot express the two
/// facets the fragment actually records — a class's type ARGUMENTS (`Set<Map.Entry<K, V>>`) and a
/// reference to a declared type PARAMETER (`E` of `List<E>`) — so both are modelled here. Class names
/// are Kotlin internal names (`kotlin/Int`, `kotlin/collections/Map.Entry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuiltinTy {
    Class {
        internal: String,
        args: Vec<BuiltinTy>,
        nullable: bool,
        shape: crate::metadata::semantic::KotlinFunctionTypeShape,
    },
    Param {
        name: String,
        id: crate::metadata::semantic::KotlinTypeParameterId,
        nullable: bool,
    },
    InProjection(Box<BuiltinTy>),
    OutProjection(Box<BuiltinTy>),
}

impl BuiltinTy {
    pub fn class(internal: impl Into<String>) -> BuiltinTy {
        BuiltinTy::Class {
            internal: internal.into(),
            args: Vec::new(),
            nullable: false,
            shape: crate::metadata::semantic::KotlinFunctionTypeShape::default(),
        }
    }

    /// The declared internal name when this is a class type, `None` for a type parameter.
    pub fn internal(&self) -> Option<&str> {
        match self {
            BuiltinTy::Class { internal, .. } => Some(internal),
            BuiltinTy::Param { .. } | BuiltinTy::InProjection(_) | BuiltinTy::OutProjection(_) => {
                None
            }
        }
    }

    pub fn nullable(&self) -> bool {
        match self {
            BuiltinTy::Class { nullable, .. } | BuiltinTy::Param { nullable, .. } => *nullable,
            BuiltinTy::InProjection(_) | BuiltinTy::OutProjection(_) => false,
        }
    }

    /// A readable source-shaped rendering (`kotlin/collections/Set<kotlin/collections/Map.Entry<K,V>>`).
    pub fn render(&self) -> String {
        let (base, args, nullable) = match self {
            BuiltinTy::Class {
                internal,
                args,
                nullable,
                ..
            } => (internal.clone(), args.as_slice(), *nullable),
            BuiltinTy::Param { name, nullable, .. } => (name.clone(), &[][..], *nullable),
            BuiltinTy::InProjection(inner) => return format!("in {}", inner.render()),
            BuiltinTy::OutProjection(inner) => return format!("out {}", inner.render()),
        };
        let mut out = base;
        if !args.is_empty() {
            let inner: Vec<String> = args.iter().map(BuiltinTy::render).collect();
            out.push('<');
            out.push_str(&inner.join(","));
            out.push('>');
        }
        if nullable {
            out.push('?');
        }
        out
    }
}

/// One member of a builtins `Class`: its Kotlin name, value-parameter types, and return type, each
/// decoded through the fragment's type table.
pub struct BuiltinMember {
    pub name: String,
    pub params: Vec<BuiltinTy>,
    pub ret: BuiltinTy,
    pub is_property: bool,
    /// A `var` property; never set on a function.
    pub is_var: bool,
    /// Kotlin's `operator` modifier from the function flags. Properties never set it.
    pub is_operator: bool,
    /// Kotlin's `infix` modifier from the function flags. Properties never set it.
    pub is_infix: bool,
    /// Semantic declaration modality from the builtins protobuf. A mapped JVM method with the same
    /// descriptor is only the physical realization and cannot replace this Kotlin fact.
    pub is_abstract: bool,
    /// The declaration's modality, of which [`Self::is_abstract`] is one case.
    pub modality: crate::metadata::semantic::KotlinModality,
    pub visibility: crate::types::Visibility,
    pub return_value_status: crate::types::ReturnValueStatus,
    /// The member's OWN type parameters (`<R>` of `fold`), with their declared upper bounds — kept
    /// apart from the class's so a consumer can build a generic signature whose formals shadow
    /// correctly.
    pub formals: Vec<BuiltinTypeParam>,
    /// Whether the declared return type is nullable (`V?`) — the JVM descriptor erases it, only the
    /// `.kotlin_builtins` `Type.nullable` flag carries it (`Map.get(K): V?`, `firstOrNull(): T?`).
    pub ret_nullable: bool,
    pub constant: Option<crate::libraries::LibConst>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    pub annotations: Vec<crate::types::TypeName>,
}

#[derive(Default)]
pub struct BuiltinPackage {
    pub classes: crate::fx_hash::HashMap<String, BuiltinClass>,
    /// Top-level functions in their common semantic shape. A builtin function has no JVM facade
    /// method; resolution consumes this complete declaration and the backend supplies its physical
    /// realization.
    pub functions: Vec<crate::metadata::semantic::KotlinFunction>,
    pub properties: Vec<BuiltinProperty>,
}

pub struct BuiltinProperty {
    pub name: String,
    pub receiver: Option<BuiltinTy>,
    pub ty: BuiltinTy,
    pub formals: Vec<BuiltinTypeParam>,
    pub visibility: crate::types::Visibility,
    pub is_var: bool,
    pub context_count: usize,
    pub constant: Option<crate::libraries::LibConst>,
}

/// One constructor declared by a builtin class. Unlike a function it has no return type or name;
/// its value-parameter types and source visibility are the complete semantic signature.
pub struct BuiltinConstructor {
    pub params: Vec<BuiltinTy>,
    pub param_names: Vec<String>,
    pub param_defaults: Vec<bool>,
    pub vararg: Option<usize>,
    pub visibility: crate::types::Visibility,
}

/// One declared type parameter of a builtin class or member: its source name and decoded upper bounds
/// (`E` unbounded, `T : Comparable<T>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinTypeParam {
    pub id: crate::metadata::semantic::KotlinTypeParameterId,
    pub name: String,
    pub bounds: Vec<BuiltinTy>,
    pub variance: crate::types::TypeVariance,
    pub only_input: bool,
    pub reified: bool,
}

/// A builtin `Class` decoded from a `.kotlin_builtins` fragment: its direct supertypes and declared
/// members — the two facets the front end needs (the read-only/mutable hierarchy AND each type's API).
pub struct BuiltinClass {
    pub supertypes: Vec<String>,
    /// The supertypes WITH their type arguments (`MutableList<E> : List<E>`), which the name-only
    /// `supertypes` list cannot carry — the chain a receiver's type argument travels up.
    pub supertype_tys: Vec<BuiltinTy>,
    pub members: Vec<BuiltinMember>,
    pub constructors: Vec<BuiltinConstructor>,
    /// The declared companion object's simple source name (`Companion`, or a named companion), from
    /// `Class.companion_object_name` (field 4).
    pub companion_name: Option<String>,
    /// The class's own type parameters, in declaration order (`Map` → `[K, V]`).
    pub type_params: Vec<BuiltinTypeParam>,
    /// Whether the builtin is an interface (`List`, `CharSequence`, `Comparable`) vs a class (`Number`,
    /// `Enum`) — from the `@Metadata` `CLASS_KIND` flag. Needed when reporting a classless builtin whose
    /// JVM class is absent (a no-JDK compile), so member calls emit the right invoke opcode.
    pub kind: TypeKind,
    pub is_fun_interface: bool,
    /// Source visibility from the metadata flag word. This is deliberately separate from `access`:
    /// Kotlin `internal` declarations are public in classfiles after name mangling.
    pub visibility: Visibility,
    /// Kotlin metadata's `IS_EXPECT_CLASS` declaration flag. KLIB consumers use this semantic bit
    /// to distinguish common declarations from platform-only classifiers in the same archive.
    pub is_expect: bool,
    pub enum_entries: Vec<String>,
    pub sealed_subclasses: Vec<String>,
    pub inline_class_property: Option<String>,
    pub modality: crate::metadata::semantic::KotlinModality,
    pub is_nested: bool,
    /// The JVM class access flags the same `Class.flags` word describes (`public static interface
    /// abstract` for `kotlin/collections/Map.Entry`) — what an `InnerClasses` entry naming this builtin
    /// has to carry when the mapped JVM owner has no class file to read it off.
    pub access: u16,
    /// Nullable returns for declared function members keyed by `(name, value-arity)` (`Map.get(K): V?`,
    /// `firstOrNull(): T?`). A call may still resolve to the ERASED classpath method (`java/util/Map.get`
    /// returns `Object`), which carries no Kotlin nullability — this is then the only surviving record
    /// that the source return is `T?`. Consulted by the member walk to null-annotate that resolved return.
    pub nullable_member_returns: Vec<(String, usize)>,
}
