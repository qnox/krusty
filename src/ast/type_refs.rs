//! The packed flags of [`TypeRef`], and construction and projection helpers for it.
//!
//! A `TypeRef` is the syntactic shape of a written type, and these are its flags and the operations
//! that build one from another piece of syntax or refine its projection flags. They are pure syntax-to-syntax
//! transformations with no resolution in them, which is why they sit beside the node rather than in
//! the AST facade.

use super::AnnotationRef;
use crate::diag::Span;

#[derive(Clone, Debug)]
pub struct TypeRef {
    pub name: String,
    /// Nullability, function-type shape, and use-site projection flags.
    pub flags: TrFlags,
    /// Function result or the abbreviated classifier argument retained by parser syntax.
    pub arg: Option<Box<TypeRef>>,
    /// Classifier arguments in semantic application order.
    pub targs: Vec<TypeRef>,
    pub span: Span,
    /// Written function parameters; `arg` owns the result.
    pub fun_params: Box<[TypeRef]>,
    pub fun_context_count: u32,
    /// Type-argument lists written before the last qualified-name segment.
    /// This syntax is rare and must not widen every expression carrying an inline `TypeRef`.
    /// The indirect owner keeps the common empty representation to one word. Qualified-segment
    /// syntax is rare, so paying one owner allocation when present is preferable to widening every
    /// expression that embeds a `TypeRef`.
    pub qualifier_arguments: QualifierArgumentLists,
}

/// One `<…>` list written on a segment of a qualified type or annotation reference.
///
/// The arguments remain syntax owned by this exact occurrence even though package-owned results
/// never participate in classifier application. Resolution must still visit them for diagnostics;
/// annotation names have no ordinary `TypeRef::targs` path from which they could be recovered.
#[derive(Clone, Debug)]
pub struct QualifierSegmentArguments {
    pub segment: u32,
    pub span: Span,
    pub arguments: Box<[TypeRef]>,
}

/// Rare type-argument lists written on qualified-name segments before the selected classifier.
///
/// `TypeRef` is embedded directly in several expression variants. A `Vec` or boxed slice would
/// reserve two or three words in every such expression even though almost every type has no
/// qualified-segment arguments. This exact syntax owner is one word when empty.
#[derive(Clone, Debug, Default)]
pub struct QualifierArgumentLists(Option<Box<QualifierArgumentListStorage>>);

#[derive(Clone, Debug)]
struct QualifierArgumentListStorage(Vec<QualifierSegmentArguments>);

impl QualifierArgumentLists {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, QualifierSegmentArguments> {
        self.0
            .as_deref()
            .map_or([].as_slice(), |storage| storage.0.as_slice())
            .iter()
    }
}

impl FromIterator<QualifierSegmentArguments> for QualifierArgumentLists {
    fn from_iter<T: IntoIterator<Item = QualifierSegmentArguments>>(iter: T) -> Self {
        let values = iter.into_iter().collect::<Vec<_>>();
        Self((!values.is_empty()).then(|| Box::new(QualifierArgumentListStorage(values))))
    }
}

impl From<Vec<QualifierSegmentArguments>> for QualifierArgumentLists {
    fn from(values: Vec<QualifierSegmentArguments>) -> Self {
        values.into_iter().collect()
    }
}

impl<'a> IntoIterator for &'a QualifierArgumentLists {
    type Item = &'a QualifierSegmentArguments;
    type IntoIter = std::slice::Iter<'a, QualifierSegmentArguments>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Bit-packed [`TypeRef`] flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrFlags(u16);

impl TrFlags {
    const NULLABLE: u16 = 1 << 0;
    const DEFINITELY_NON_NULL: u16 = 1 << 1;
    const FUN_HAS_RECEIVER: u16 = 1 << 2;
    const FUN_SUSPEND: u16 = 1 << 3;
    const IN_PROJECTION: u16 = 1 << 4;
    const OUT_PROJECTION: u16 = 1 << 5;
    const IMPORT: u16 = 1 << 6;
    // Parsing still represents `*` as its semantic upper bound (`Any?`) for ordinary type
    // resolution, but an `is FunctionN<*, ...>` check must distinguish that runtime-checkable
    // projection from an explicitly written `Any?`. Preserve the source distinction in the last
    // available flag bit instead of recovering it from source text in individual consumers.
    const STAR_PROJECTION: u16 = 1 << 7;
    // Annotation classifier references are also present in `detached_type_refs` so Pass 1 can bind
    // their identities before compact header projection. The declaration annotation checker is the
    // sole diagnostic authority, however; marking the duplicate detached occurrence prevents a
    // file-scope fallback from rechecking it outside its declaration's lexical suppression scope.
    const ANNOTATION: u16 = 1 << 8;
    /// The element type written on a `vararg` parameter; the parameter's type is its array.
    const VARARG_ELEMENT: u16 = 1 << 9;
    // A classifier's superclass reference (`: Base(args)`) is retained by its `ClassDecl` and also
    // present in `detached_type_refs` so Pass 1 can bind it. Signature solving is its sole
    // diagnostic authority, under its declaration's lexical policy; the mark keeps the file-scope
    // fallback from rechecking it outside that policy, as for annotations.
    const SUPERCLASS: u16 = 1 << 10;
    // The left-hand side of a class literal. A generic typealias written there without type
    // arguments is applied to star projections of its own parameters (`Arr::class` is
    // `KClass<Arr<*>>`) instead of failing the ordinary arity check. When arguments are written,
    // the class-literal checker is their diagnostic authority because kotlinc judges them by its
    // own rule rather than as an ordinary type's arity.
    const CLASS_LITERAL_LHS: u16 = 1 << 11;

    #[inline]
    const fn with(mut self, mask: u16, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u16) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_nullable(self, on: bool) -> Self {
        self.with(Self::NULLABLE, on)
    }
    #[inline]
    pub const fn with_definitely_non_null(self, on: bool) -> Self {
        self.with(Self::DEFINITELY_NON_NULL, on)
    }
    #[inline]
    pub const fn with_fun_has_receiver(self, on: bool) -> Self {
        self.with(Self::FUN_HAS_RECEIVER, on)
    }
    #[inline]
    pub const fn with_fun_suspend(self, on: bool) -> Self {
        self.with(Self::FUN_SUSPEND, on)
    }
    #[inline]
    pub const fn with_in_projection(self, on: bool) -> Self {
        self.with(Self::IN_PROJECTION, on)
    }
    #[inline]
    pub const fn with_out_projection(self, on: bool) -> Self {
        self.with(Self::OUT_PROJECTION, on)
    }
    #[inline]
    pub const fn with_import(self, on: bool) -> Self {
        self.with(Self::IMPORT, on)
    }
    #[inline]
    pub const fn with_star_projection(self, on: bool) -> Self {
        self.with(Self::STAR_PROJECTION, on)
    }
    #[inline]
    pub const fn with_annotation(self, on: bool) -> Self {
        self.with(Self::ANNOTATION, on)
    }
    #[inline]
    pub const fn with_vararg_element(self, on: bool) -> Self {
        self.with(Self::VARARG_ELEMENT, on)
    }
    #[inline]
    pub const fn with_superclass(self, on: bool) -> Self {
        self.with(Self::SUPERCLASS, on)
    }
    #[inline]
    pub const fn with_class_literal_lhs(self, on: bool) -> Self {
        self.with(Self::CLASS_LITERAL_LHS, on)
    }
}

impl TypeRef {
    pub(crate) fn from_annotation(annotation: &AnnotationRef) -> Self {
        Self {
            name: annotation.name.clone(),
            flags: TrFlags::default().with_annotation(true),
            arg: None,
            targs: Vec::new(),
            span: annotation.span,
            fun_params: Box::new([]),
            fun_context_count: 0,
            qualifier_arguments: QualifierArgumentLists::default(),
        }
    }

    #[inline]
    pub(crate) fn is_class_literal_lhs(&self) -> bool {
        self.flags.has(TrFlags::CLASS_LITERAL_LHS)
    }
    #[inline]
    pub fn nullable(&self) -> bool {
        self.flags.has(TrFlags::NULLABLE)
    }
    #[inline]
    pub fn definitely_non_null(&self) -> bool {
        self.flags.has(TrFlags::DEFINITELY_NON_NULL)
    }
    #[inline]
    pub fn fun_has_receiver(&self) -> bool {
        self.flags.has(TrFlags::FUN_HAS_RECEIVER)
    }
    #[inline]
    pub fn fun_suspend(&self) -> bool {
        self.flags.has(TrFlags::FUN_SUSPEND)
    }
    /// Whether this node retains a source arrow-function shape. Ordinary function types use the
    /// parser-internal `<fun>` spelling; a function type in a supertype list additionally keeps its
    /// nominal `FunctionN` classifier spelling, so spelling alone is not the structural identity.
    #[inline]
    pub(crate) fn is_function_type_syntax(&self) -> bool {
        self.name == "<fun>" || !self.fun_params.is_empty()
    }
    #[inline]
    pub fn in_projection(&self) -> bool {
        self.flags.has(TrFlags::IN_PROJECTION)
    }
    #[inline]
    pub fn out_projection(&self) -> bool {
        self.flags.has(TrFlags::OUT_PROJECTION)
    }
    #[inline]
    pub fn is_import(&self) -> bool {
        self.flags.has(TrFlags::IMPORT)
    }
    #[inline]
    pub fn is_star_projection(&self) -> bool {
        self.flags.has(TrFlags::STAR_PROJECTION)
    }
    #[inline]
    pub fn is_annotation(&self) -> bool {
        self.flags.has(TrFlags::ANNOTATION)
    }
    #[inline]
    pub fn is_vararg_element(&self) -> bool {
        self.flags.has(TrFlags::VARARG_ELEMENT)
    }
    #[inline]
    pub fn is_superclass(&self) -> bool {
        self.flags.has(TrFlags::SUPERCLASS)
    }
    /// An underscore in a call-site type-argument list asks inference to solve this position.
    /// It is neither an unresolved classifier nor a star projection; checked call data must replace
    /// it with the inferred semantic argument.
    #[inline]
    pub fn is_inference_placeholder(&self) -> bool {
        self.name == "_"
            && self.arg.is_none()
            && self.targs.is_empty()
            && self.fun_params.is_empty()
            && !self.is_star_projection()
    }
    #[inline]
    pub fn set_nullable(&mut self, on: bool) {
        self.flags = self.flags.with_nullable(on);
    }
    #[inline]
    pub fn set_definitely_non_null(&mut self, on: bool) {
        self.flags = self.flags.with_definitely_non_null(on);
    }
    #[inline]
    pub fn set_projection(&mut self, in_projection: bool, out_projection: bool) {
        self.flags = self
            .flags
            .with_in_projection(in_projection)
            .with_out_projection(out_projection);
    }
}
