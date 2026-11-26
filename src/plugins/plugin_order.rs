//! kotlinc's `-Xcompiler-plugin-order=<before>><after>`: an execution-order constraint between two
//! loaded compiler plugins, named by their plugin ids. kotlinc (`PluginCliParser`) parses every
//! constraint before it loads a plugin, then registers each plugin set (the `-Xplugin` classpath's
//! registrars, and the `-Xcompiler-plugin` registrations) in a topological order of the constraints
//! between plugins of that set. A constraint naming a plugin the set does not load has no effect.

use std::fmt;

/// One `-Xcompiler-plugin-order` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOrderConstraint {
    pub before: String,
    pub after: String,
    /// The value as written, which kotlinc quotes back in its cycle error.
    pub raw: String,
}

impl PluginOrderConstraint {
    /// kotlinc's reading: the value splits on `>` into exactly two parts, each trimmed. An empty part
    /// is accepted and names no plugin.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split('>');
        let (Some(before), Some(after), None) = (parts.next(), parts.next(), parts.next()) else {
            return None;
        };
        Some(Self {
            before: before.trim().to_string(),
            after: after.trim().to_string(),
            raw: raw.to_string(),
        })
    }
}

/// Every constraint, or kotlinc's error for the first value [`PluginOrderConstraint::parse`] refuses.
/// kotlinc parses them before it loads any plugin, so that error is the only one it reports.
pub fn parse_constraints(raw: &[String]) -> Result<Vec<PluginOrderConstraint>, String> {
    raw.iter()
        .map(|raw| {
            PluginOrderConstraint::parse(raw)
                .ok_or_else(|| format!("could not parse plugin order constraint: {raw}"))
        })
        .collect()
}

/// kotlinc renders a constraint in its cycle error as its data class's `toString`.
impl fmt::Display for PluginOrderConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PluginOrderConstraint(before={}, after={}, rawArgument={})",
            self.before, self.after, self.raw
        )
    }
}

/// A constraint cycle among one set's plugins: the plugin kotlinc reports, and every constraint of
/// the command line, which its error lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOrderCycle {
    pub plugin_id: Option<String>,
    pub constraints: Vec<PluginOrderConstraint>,
}

impl PluginOrderCycle {
    pub fn message(&self) -> String {
        let constraints = self
            .constraints
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "compiler plugin '{}' is part of an constraint cycle: {constraints}",
            self.plugin_id.as_deref().unwrap_or("null")
        )
    }
}

/// The order in which kotlinc registers one plugin set, as indices into `plugins`: each plugin's id,
/// in load order, or `None` for a plugin whose id is unknown or empty (no constraint reaches it).
///
/// A plugin depends on every plugin a constraint puts before it; when several loaded plugins share an
/// id, a constraint names the last of them. The plugins are visited depth-first in load order and
/// each is registered after its dependencies, so a set without constraints keeps its load order.
/// Reaching a plugin again while its dependencies are being visited is a cycle, reported for that
/// plugin.
pub fn registration_order(
    plugins: &[Option<&str>],
    constraints: &[PluginOrderConstraint],
) -> Result<Vec<usize>, PluginOrderCycle> {
    let by_id = |id: &str| {
        plugins
            .iter()
            .rposition(|plugin| plugin.is_some_and(|plugin| !plugin.is_empty() && plugin == id))
    };
    let dependencies = |index: usize| -> Vec<usize> {
        let Some(id) = plugins[index].filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        constraints
            .iter()
            .filter(|constraint| constraint.after == id)
            .filter_map(|constraint| by_id(&constraint.before))
            .collect()
    };
    struct Visit<'a> {
        dependencies: &'a dyn Fn(usize) -> Vec<usize>,
        in_progress: Vec<bool>,
        done: Vec<bool>,
        order: Vec<usize>,
    }
    impl Visit<'_> {
        fn visit(&mut self, index: usize) -> Result<(), usize> {
            if self.done[index] {
                return Ok(());
            }
            if self.in_progress[index] {
                return Err(index);
            }
            self.in_progress[index] = true;
            for dependency in (self.dependencies)(index) {
                self.visit(dependency)?;
            }
            self.done[index] = true;
            self.order.push(index);
            Ok(())
        }
    }
    let mut visit = Visit {
        dependencies: &dependencies,
        in_progress: vec![false; plugins.len()],
        done: vec![false; plugins.len()],
        order: Vec::with_capacity(plugins.len()),
    };
    for index in 0..plugins.len() {
        visit.visit(index).map_err(|cycle| PluginOrderCycle {
            plugin_id: plugins[cycle].map(str::to_string),
            constraints: constraints.to_vec(),
        })?;
    }
    Ok(visit.order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constraint(raw: &str) -> PluginOrderConstraint {
        PluginOrderConstraint::parse(raw).expect("a constraint")
    }

    #[test]
    fn a_constraint_is_two_trimmed_ids_around_one_angle_bracket() {
        assert_eq!(
            PluginOrderConstraint::parse(" a > b "),
            Some(PluginOrderConstraint {
                before: "a".to_string(),
                after: "b".to_string(),
                raw: " a > b ".to_string(),
            })
        );
        assert_eq!(constraint(">b").before, "");
        for refused in ["", "foo", "a>b>c", "a>b,b>a"] {
            assert_eq!(PluginOrderConstraint::parse(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn the_first_unparsable_constraint_is_the_error() {
        let raw = ["a>b", "foo", "a>b>c"].map(String::from);
        assert_eq!(
            parse_constraints(&raw),
            Err("could not parse plugin order constraint: foo".to_string())
        );
        assert_eq!(parse_constraints(&raw[..1]), Ok(vec![constraint("a>b")]));
    }

    #[test]
    fn plugins_keep_their_load_order_unless_a_constraint_moves_one_before_another() {
        let plugins = [Some("a"), Some("b"), Some("c")];
        assert_eq!(registration_order(&plugins, &[]), Ok(vec![0, 1, 2]));
        assert_eq!(
            registration_order(&plugins, &[constraint("c>a")]),
            Ok(vec![2, 0, 1])
        );
        assert_eq!(
            registration_order(&plugins, &[constraint("c>b"), constraint("b>a")]),
            Ok(vec![2, 1, 0])
        );
    }

    #[test]
    fn a_constraint_on_a_plugin_the_set_does_not_load_has_no_effect() {
        let plugins = [Some("a"), None, Some("b")];
        assert_eq!(
            registration_order(&plugins, &[constraint("x>a"), constraint("b>y")]),
            Ok(vec![0, 1, 2])
        );
    }

    #[test]
    fn a_cycle_is_reported_for_the_plugin_reached_again_with_every_constraint() {
        let constraints = [constraint("x>y"), constraint("a>b"), constraint("b>a")];
        let cycle = registration_order(&[Some("a"), Some("b")], &constraints).unwrap_err();
        assert_eq!(
            cycle.message(),
            "compiler plugin 'a' is part of an constraint cycle: \
             PluginOrderConstraint(before=x, after=y, rawArgument=x>y), \
             PluginOrderConstraint(before=a, after=b, rawArgument=a>b), \
             PluginOrderConstraint(before=b, after=a, rawArgument=b>a)"
        );
        let cycle = registration_order(&[Some("b"), Some("a")], &constraints).unwrap_err();
        assert_eq!(cycle.plugin_id.as_deref(), Some("b"));
        let own = registration_order(&[Some("a")], &[constraint("a>a")]).unwrap_err();
        assert_eq!(own.plugin_id.as_deref(), Some("a"));
    }
}
