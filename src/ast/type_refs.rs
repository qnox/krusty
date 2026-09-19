//! Construction and projection helpers for [`TypeRef`].
//!
//! A `TypeRef` is the syntactic shape of a written type, and these are the operations that build
//! one from another piece of syntax or refine its projection flags. They are pure syntax-to-syntax
//! transformations with no resolution in them, which is why they sit beside the node rather than in
//! the AST facade.

use super::{AnnotationRef, TrFlags, TypeRef};

impl TypeRef {
    pub(crate) fn from_annotation(annotation: &AnnotationRef) -> Self {
        Self {
            name: annotation.name.clone(),
            flags: TrFlags::default().with_annotation(true),
            arg: None,
            targs: Vec::new(),
            span: annotation.span,
            fun_params: Vec::new(),
            fun_context_count: 0,
        }
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
