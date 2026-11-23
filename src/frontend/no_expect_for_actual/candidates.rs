//! The candidate listing beneath an owed member of an `actual typealias`: the members of the
//! alias's target that share the `expect` member's name and implement it by none of its facts.

use crate::fir::{
    DeclarationId, DeclarationKind, IncompatibilityReason, IncompatibleActual,
    IncompatibleCandidate, ShapeMismatch,
};

/// Renders the listing, or says why this one cannot be rendered faithfully. The reference compiler
/// groups candidates by reason in an order not yet pinned, so more than one group is refused
/// rather than guessed.
pub(super) fn listing(candidates: &[IncompatibleCandidate]) -> Result<String, &'static str> {
    let Some(first) = candidates.first() else {
        return Ok(String::new());
    };
    if candidates
        .iter()
        .any(|candidate| candidate.reason != first.reason)
    {
        return Err(
            "lists incompatible members for more than one reason, whose order is unmeasured",
        );
    }
    let reason = match first.reason {
        IncompatibilityReason::ParameterTypes => "parameter types are different",
        IncompatibilityReason::ValueParameterCount => "the number of value parameters is different",
        IncompatibilityReason::FunctionVersusProperty => {
            "callable kinds are different (function vs property)"
        }
        IncompatibilityReason::Unmeasured => {
            return Err("has an incompatibility whose wording is unmeasured")
        }
    };
    let header = if candidates.len() == 1 {
        "The following declaration is"
    } else {
        "The following declarations are"
    };
    let mut rendered = format!("\n\n    {header} incompatible because {reason}:");
    for candidate in candidates {
        rendered.push_str("\n        ");
        rendered.push_str(&candidate_source(candidate)?);
    }
    Ok(rendered)
}

fn candidate_source(candidate: &IncompatibleCandidate) -> Result<String, &'static str> {
    let member = &candidate.member;
    if !member.plain {
        return Err("lists a member the record cannot state");
    }
    let parameters = || -> Result<String, &'static str> {
        if member.parameter_names.len() != member.parameters.len() {
            return Err("lists a member whose parameter names are unknown");
        }
        Ok(member
            .parameters
            .iter()
            .zip(member.parameter_names.iter())
            .map(|(ty, name)| format!("{name}: {}", ty.source_name()))
            .collect::<Vec<_>>()
            .join(", "))
    };
    Ok(match member.kind {
        DeclarationKind::Function => format!(
            "fun {}({}): {}",
            member.name,
            parameters()?,
            member.result.source_name()
        ),
        DeclarationKind::Property => format!(
            "{} {}: {}",
            if member.mutable { "var" } else { "val" },
            member.name,
            member.result.source_name()
        ),
        DeclarationKind::Constructor => format!("constructor({})", parameters()?),
        _ => return Err("lists a member of a kind that is unmeasured"),
    })
}

/// The listing beneath an `expect` member a source classifier owes: the classifier's own `actual`
/// members that were compared with it and rejected, each as it was written.
pub(super) fn actual_listing(
    expect: DeclarationId,
    rejected: &[IncompatibleActual],
    written: &crate::fx_hash::HashMap<DeclarationId, String>,
) -> Result<String, &'static str> {
    let own = rejected
        .iter()
        .filter(|entry| entry.expect == expect)
        .collect::<Vec<_>>();
    let Some(first) = own.first() else {
        return Ok(String::new());
    };
    if own.iter().any(|entry| entry.mismatch != first.mismatch) {
        return Err(
            "lists incompatible declarations for more than one reason, whose order is unmeasured",
        );
    }
    let reason = mismatch_reason(first.mismatch)?;
    let header = if own.len() == 1 {
        "The following declaration is"
    } else {
        "The following declarations are"
    };
    let mut rendered = format!("\n\n    {header} incompatible because {reason}:");
    for entry in own {
        let Some(text) = written.get(&entry.actual) else {
            return Err("lists an actual declaration that has no written form");
        };
        rendered.push_str("\n        ");
        rendered.push_str(text);
    }
    Ok(rendered)
}

fn mismatch_reason(mismatch: ShapeMismatch) -> Result<&'static str, &'static str> {
    match mismatch {
        ShapeMismatch::ParameterTypes => Ok("parameter types are different"),
        ShapeMismatch::UpperBounds => Ok("the upper bounds of type parameters are different"),
        ShapeMismatch::ValueParameterCount => Ok("the number of value parameters is different"),
        ShapeMismatch::Unmeasured => Err("has an incompatibility whose wording is unmeasured"),
    }
}

/// The listing beneath an implementation that paired with nothing: the `expect` declarations it
/// was compared with, each as the common source wrote it, grouped by why it was rejected.
///
/// kotlinc's order across several reason groups is not pinned, so a listing with more than one,
/// or with a reason it has not been measured for, is refused rather than guessed.
pub(super) fn expect_listing(
    actual: DeclarationId,
    rejected: &[IncompatibleActual],
    expected: &crate::fx_hash::HashMap<DeclarationId, String>,
) -> Result<String, &'static str> {
    let own = rejected
        .iter()
        .filter(|entry| entry.actual == actual)
        .collect::<Vec<_>>();
    let Some(first) = own.first() else {
        return Ok(String::new());
    };
    if own.iter().any(|entry| entry.mismatch != first.mismatch) {
        return Err(
            "lists incompatible declarations for more than one reason, whose order is unmeasured",
        );
    }
    let reason = mismatch_reason(first.mismatch)?;
    let header = if own.len() == 1 {
        "The following declaration is"
    } else {
        "The following declarations are"
    };
    let mut rendered = format!("\n{header} incompatible because {reason}:");
    for entry in own {
        let Some(written) = expected.get(&entry.expect) else {
            return Err("lists an expect declaration that has no written form");
        };
        rendered.push_str("\n    ");
        rendered.push_str(written);
    }
    Ok(rendered)
}
