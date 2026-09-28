//! A static property's storage and how each of its accessors comes about: the facade (or owner
//! class) field a top-level or hoisted property lives in.

use super::ExprId;
use crate::types::{Ty, TypeName};

/// How each of a static property's accessors comes about.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrStaticAccessors {
    pub getter: IrStaticAccessor,
    /// Meaningful only for a `var`.
    pub setter: IrStaticAccessor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrStaticAccessor {
    /// The compiler supplies it, unless the property's own visibility hides it.
    Default,
    /// Source declares it: an ordinary function whose body sees `field` as this static.
    Declared,
    /// No method exists: compiler-internal storage, or a bodiless `private set`, whose default
    /// setter is private and therefore never generated.
    Absent,
}

impl IrStaticAccessors {
    pub const DEFAULT: Self = Self {
        getter: IrStaticAccessor::Default,
        setter: IrStaticAccessor::Default,
    };
    pub const ABSENT: Self = Self {
        getter: IrStaticAccessor::Absent,
        setter: IrStaticAccessor::Absent,
    };

    /// Whether source declared either accessor.
    pub fn any_declared(self) -> bool {
        self.getter == IrStaticAccessor::Declared || self.setter == IrStaticAccessor::Declared
    }
}

/// A top-level (module) property: a static field on the file facade, initialized in `<clinit>`.
#[derive(Clone, Debug)]
pub struct IrStatic {
    pub name: String,
    pub ty: Ty,
    /// The initializer expression, run in `<clinit>` in declaration order. `None` when source
    /// declares none (a `lateinit var`): the field starts at its JVM default and `<clinit>` has
    /// nothing to run for it.
    pub init: Option<ExprId>,
    /// `var` (mutable) ⇒ a setter is emitted and the backing field is non-`final`.
    pub is_var: bool,
    /// `const val` ⇒ kotlinc keeps the field `static final` (inlined at use) with no accessor, at the
    /// DECLARATION's own visibility: `private const val` is a private field, while `internal` and
    /// `public` are both public (`internal` is a Kotlin boundary with no JVM spelling). A plain
    /// top-level `val`/`var` is `private static [final]` + a `public static` getter/setter whatever
    /// the source said, because every reader goes through the accessor.
    pub is_const: bool,
    /// The class this static field belongs to. `None` = the file facade (a top-level property). `Some`
    /// = a specific class — a `companion object`'s `const val` lives on the OUTER class (kotlinc emits
    /// `public static final` + `ConstantValue` there), not the facade.
    pub owner: Option<TypeName>,
    /// Declaration visibility (`public` by default). A PRIVATE top-level property gets NO public
    /// accessors; cross-class reads inside the file go through a synthesized `access$get<X>$p` bridge
    /// (kotlinc's shape).
    pub visibility: crate::types::Visibility,
    /// The setter's JVM name when it is not the ordinary `set<X>` spelling — a value-class-typed
    /// property mangles it, because a value-class PARAMETER always does. `None` ⇒ the ordinary name.
    pub setter_jvm_name: Option<String>,
    /// The type this static was DECLARED with, when the JVM pass erased its storage to a value
    /// class's carrier (a file facade's property, which kotlinc erases the same way). `None` ⇒ the
    /// storage keeps the boxed value-class object, or holds no value class at all.
    ///
    /// The whole declared type, not just the classifier: once `ty` holds the carrier, every fact
    /// the accessors still need — which value class it is AND whether it was nullable — is only
    /// here. Reading them back off the erased type made a `var x: Label?` publish a non-null
    /// `String` setter, which then refused the `null` the property accepts. Every reader consults
    /// this rather than re-deciding, so a read of the field and the field itself cannot disagree.
    pub erased_declared_ty: Option<Ty>,
    /// Which of the property's accessors source declared. A declared `getX`/`setX` is an ordinary
    /// function (its body lowered with `field` bound to this static); the one source left out is
    /// the compiler default.
    pub accessors: IrStaticAccessors,
    /// 1-based source line of the property declaration (0 = unknown). kotlinc maps the accessors'
    /// LineNumberTables and the `<clinit>` initializer store to this line.
    pub line: u32,
    /// Source byte offset of the declaration (`u32::MAX` for a target/plugin synthetic). This is the
    /// exact ordering key when class metadata interleaves static and instance properties.
    pub source_order: u32,
}

impl IrStatic {
    pub fn is_facade_owned(&self) -> bool {
        self.owner.is_none()
    }

    pub fn owner_matches(&self, internal: &str) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| owner.matches(internal))
    }
}
