//! The candidate listing beneath an owed member of an `actual typealias`: the members of the
//! alias's target that share the `expect` member's name and implement it by none of its facts.

use crate::fir::{DeclarationKind, IncompatibilityReason, IncompatibleCandidate};

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
