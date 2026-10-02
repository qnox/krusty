//! One instance field of an IR class and its declaration facts.

use super::{IrConst, IrfFlags};
use crate::types::Ty;

/// One instance field of an [`super::IrClass`]. Groups what were parallel `Vec`s keyed by field index, so a
/// field's type / generic-param name / constant default / finality / visibility can't desync.
#[derive(Clone, Debug)]
pub struct IrField {
    pub name: String,
    pub ty: Ty,
    /// Source line for the compiler-generated constructor store into this exact field: a PRIMARY-
    /// CONSTRUCTOR property's, or a delegated property's delegate. Zero otherwise. This semantic
    /// debug role is recorded on the resolved field coordinate so a backend never has to recover
    /// the property from its emitted field name.
    pub constructor_store_line: u32,
    /// The source type-parameter NAME the field was declared with (`val x: T` → `Some("T")`), else
    /// `None`. Platform-neutral; lets the value-class pass pick the CORRECT bound for a generic
    /// underlying (vs guessing), independent of erasure dropping the name.
    pub type_param: Option<String>,
    /// The CONSTANT default from a primary-constructor default (`val b: Int = 5` → `Some(Int(5))`,
    /// `val t: T? = null` → `Some(Null)`), else `None` (no default, or a non-constant one). Later
    /// compiler passes may use it; the core backend ignores it.
    pub default: Option<IrConst>,
    /// Bit-packed `has_default`/`is_final`/`is_private`/`is_lateinit`/`is_compiler_generated` (read
    /// via the accessors below).
    /// `has_default` — the primary-constructor parameter declared ANY default (constant or not, e.g.
    /// `routes: List<String> = emptyList()`); distinct from `default` (constant-only), needed so the
    /// `@Metadata` emitter sets the `DECLARES_DEFAULT_VALUE` value-parameter flag as kotlinc does.
    /// `is_final` — the backing field is immutable (`val`), emitted `final`. `is_private` — private
    /// backing field (the Kotlin default, reached via accessors); `false` for a field read/written
    /// cross-class (a coroutine continuation's `result`/`label`). `is_lateinit` — backs a `lateinit
    /// var`; every backend read null-checks it and throws when still unset, matching kotlinc.
    pub flags: IrfFlags,
}

impl IrField {
    /// A plain backing field with Kotlin defaults: mutable-unknown (`is_final = false`), `private`, no
    /// generic-param name, no constant default. Synthesized classes build fields from this.
    pub fn new(name: String, ty: Ty) -> IrField {
        IrField {
            name,
            ty,
            constructor_store_line: 0,
            type_param: None,
            default: None,
            flags: IrfFlags::default().with_is_private(true),
        }
    }

    #[inline]
    pub fn has_default(&self) -> bool {
        self.flags.has(IrfFlags::HAS_DEFAULT)
    }
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.has(IrfFlags::IS_FINAL)
    }
    #[inline]
    pub fn is_private(&self) -> bool {
        self.flags.has(IrfFlags::IS_PRIVATE)
    }
    #[inline]
    pub fn is_lateinit(&self) -> bool {
        self.flags.has(IrfFlags::IS_LATEINIT)
    }
    /// Chainable override of `is_final` on top of [`IrField::new`] (replaces a `..IrField::new` spread).
    #[inline]
    pub fn with_is_final(mut self, on: bool) -> Self {
        self.flags = self.flags.with_is_final(on);
        self
    }
    /// Chainable override of `is_private` on top of [`IrField::new`].
    #[inline]
    pub fn with_is_private(mut self, on: bool) -> Self {
        self.flags = self.flags.with_is_private(on);
        self
    }
}
