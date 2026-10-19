//! A compilation's session in the host: kotlinc's JVM frontend and fir2ir run over the build's
//! classpath, which is what IR plugins look declarations up in, and the identities krusty names
//! those declarations by.

use std::path::PathBuf;

/// What a session is opened over: the build's exact classpath and JDK, and the settings that decide
/// how kotlinc reads them, spelled as kotlinc's command line spells them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRequest {
    /// Every classpath entry, the standard library included: the host adds none of its own.
    pub classpath: Vec<PathBuf>,
    pub jdk_home: PathBuf,
    pub module_name: String,
    /// `-language-version`, such as `2.4`.
    pub language_version: String,
    /// `-api-version`, such as `2.4`.
    pub api_version: String,
    /// `-jvm-target`, such as `1.8` or `21`.
    pub jvm_target: String,
}

/// A class by its package and its simple names from the outermost class in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassName {
    /// Dot-separated; empty for the root package.
    pub package: String,
    pub classes: Vec<String>,
}

/// A type in a declaration's signature, as kotlinc's IR spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeIdentity {
    pub classifier: Classifier,
    pub nullable: bool,
    /// The classes of the type's annotations, in order: kotlinc's IR marks extension function types
    /// and platform types with them.
    pub annotations: Vec<ClassName>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classifier {
    Class {
        class: ClassName,
        arguments: Vec<TypeArgument>,
    },
    /// The type parameter at `index` of the declaration that `owner` names.
    TypeParameter {
        owner: TypeParameterOwner,
        index: u32,
    },
}

/// The declaration a type parameter belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeParameterOwner {
    /// The callable whose signature names it.
    Callable,
    /// A class, by its full name: the callable's own class, or any class enclosing it, as the
    /// outer class's `T` in an inner class's members and constructors.
    Class(ClassName),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeArgument {
    Star,
    Invariant(TypeIdentity),
    In(TypeIdentity),
    Out(TypeIdentity),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallableKind {
    Function { name: String },
    Property { name: String },
    Constructor,
}

/// A declaration on the session's classpath, named by its owner and its signature: the parameter
/// types kotlinc declares, a vararg parameter by its array type. It names one declaration or none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallableIdentity {
    pub kind: CallableKind,
    /// Dot-separated; empty for the root package.
    pub package: String,
    /// The class the declaration is a member of, from the outermost class in; empty for a top-level
    /// declaration. A constructor names its class here.
    pub classes: Vec<String>,
    pub type_parameters: u32,
    pub context_parameters: Vec<TypeIdentity>,
    pub extension_receiver: Option<TypeIdentity>,
    pub value_parameters: Vec<TypeIdentity>,
}
