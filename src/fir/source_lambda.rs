//! What a checked body knows about the source lambda it was written as.

/// Which source form a lambda body was written in. Both lift alike; a target may name what they
/// capture differently (kotlinc treats an anonymous function as a local function there).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirLambdaForm {
    /// `{ … }`
    Literal,
    /// `fun(…) { … }`
    AnonymousFunction,
}

/// A body written as a lambda literal or an anonymous function, with the source facts its lifted
/// implementation is named and debugged by.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirSourceLambda {
    form: FirLambdaForm,
    explicit_suspend: bool,
    binding_name: Option<Box<str>>,
    label: Option<Box<str>>,
}

impl FirSourceLambda {
    /// `binding_name` is the local the lambda initializes, when it initializes one.
    /// `label` is kotlinc's label of a lambda literal: its own, else the call it is written in
    /// (see `parser::lambda_labels`). An anonymous function has none.
    pub fn new(
        form: FirLambdaForm,
        explicit_suspend: bool,
        binding_name: Option<impl Into<Box<str>>>,
        label: Option<impl Into<Box<str>>>,
    ) -> Self {
        let label = label.map(Into::into);
        assert!(
            form == FirLambdaForm::Literal || label.is_none(),
            "only a lambda literal is labelled"
        );
        Self {
            form,
            explicit_suspend,
            binding_name: binding_name.map(Into::into),
            label,
        }
    }

    pub const fn form(&self) -> FirLambdaForm {
        self.form
    }

    /// Whether the lambda carries the source `suspend` modifier. A lambda inferred against a
    /// suspend function type is suspending too, but Kotlin metadata distinguishes the two forms.
    pub const fn explicit_suspend(&self) -> bool {
        self.explicit_suspend
    }

    pub fn binding_name(&self) -> Option<&str> {
        self.binding_name.as_deref()
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub(super) fn text_bytes(&self) -> usize {
        self.binding_name.as_deref().map_or(0, str::len) + self.label.as_deref().map_or(0, str::len)
    }
}
