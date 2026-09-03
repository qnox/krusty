//! Checked realization of compiler-provided array constructors.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn compiler_synthetic_array(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        use crate::synthetics::SyntheticKind;

        let Some(ExprLowering::CompilerSynthetic(kind)) = self.info.expr_lowers.get(&expression)
        else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        let array_type = self.expression_type(expression)?;
        crate::trace_compiler!(
            "fir",
            "compiler synthetic array expression={expression:?} kind={kind:?} type={:?} arguments={}",
            array_type.get(),
            arguments.len(),
        );
        let element_type = array_type
            .get()
            .array_elem()
            .and_then(|element| ResolvedTy::new(element).ok())
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        let cause = self.expression_origin(expression)?;
        match (*kind, arguments) {
            (
                SyntheticKind::PrimitiveVararg(_)
                | SyntheticKind::ReferenceVararg
                | SyntheticKind::EmptyReference,
                arguments,
            ) => {
                let elements = arguments
                    .iter()
                    .copied()
                    .map(|argument| {
                        let spread = self.file.is_spread_arg(argument)
                            || self
                                .info
                                .resolved_whole_array_vararg_args
                                .contains(&argument);
                        let target = if spread { array_type } else { element_type };
                        Ok(FirArrayElement {
                            value: self.expression(argument)?,
                            spread,
                            conversion: self.selected_value_conversion(argument, target, cause)?,
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?
                    .into_boxed_slice();
                Ok(FirExprKind::ArrayLiteral {
                    array_type,
                    elements,
                })
            }
            (SyntheticKind::PrimitiveSize(_), [size])
            | (SyntheticKind::NullableReference, [size]) => Ok(FirExprKind::ArrayConstruction {
                array_type,
                element_type,
                size: self.expression(*size)?,
                size_conversion: self.selected_value_conversion(
                    *size,
                    self.resolved_type(
                        self.file.expr_span(*size).ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                        Ty::Int,
                    )?,
                    cause,
                )?,
                initializer: None,
            }),
            (SyntheticKind::PrimitiveSize(_), [size, initializer])
            | (SyntheticKind::ReferenceSize, [size, initializer]) => {
                Ok(FirExprKind::ArrayConstruction {
                    array_type,
                    element_type,
                    size: self.expression(*size)?,
                    size_conversion: self.selected_value_conversion(
                        *size,
                        self.resolved_type(
                            self.file.expr_span(*size).ok_or_else(|| {
                                self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                            })?,
                            Ty::Int,
                        )?,
                        cause,
                    )?,
                    initializer: Some(self.expression(*initializer)?),
                })
            }
            _ => Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )),
        }
    }
}
