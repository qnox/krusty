//! Why a call's arguments could not be assigned to a callable's parameters, as kotlinc words it.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallArgMappingFailure {
    pub errors: Vec<CallArgMappingError>,
    /// First positional argument rejected after named arguments.
    pub recovery_argument: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallArgMappingError {
    NoParameterNamed { name: String, argument: usize },
    AlreadyPassed { argument: usize },
    PositionalAfterNamed { argument: usize },
    TooManyArguments { argument: usize, expected: usize },
    TrailingLambdaOnVararg { argument: usize },
    MissingRequired { name: String },
}

impl CallArgMappingError {
    pub(crate) fn highlights_callee(&self) -> bool {
        matches!(self, Self::MissingRequired { .. })
    }
}

impl std::fmt::Display for CallArgMappingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoParameterNamed { name, .. } => {
                write!(formatter, "no parameter with name '{name}' found.")
            }
            Self::AlreadyPassed { .. } => {
                formatter.write_str("argument already passed for this parameter.")
            }
            Self::PositionalAfterNamed { .. } => {
                formatter.write_str(
                    "mixing named and positional arguments is not allowed unless the order of the arguments matches the order of the parameters.",
                )
            }
            Self::TooManyArguments { expected, .. } => {
                write!(formatter, "too many arguments: expected at most {expected}")
            }
            Self::TrailingLambdaOnVararg { .. } => formatter.write_str(
                "passing value as a vararg is allowed only inside a parenthesized argument list.",
            ),
            Self::MissingRequired { name } => {
                write!(formatter, "no value passed for parameter '{name}'.")
            }
        }
    }
}
