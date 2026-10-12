//! Fuzzy matching of object names, with nucleo's matcher.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};

use super::{Explorer, Load, Match};
use crate::action::Action;

/// Scores `names` against `query`, best first. Only matches are returned, as
/// indexes into `names`.
pub fn rank<'a>(
    matcher: &mut Matcher,
    query: &str,
    names: impl Iterator<Item = &'a str>,
) -> Vec<usize> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize, usize)> = names
        .enumerate()
        .filter_map(|(i, name)| {
            pattern
                .score(Utf32Str::new(name, &mut buf), matcher)
                .map(|score| (score, name.len(), i))
        })
        .collect();
    // Best score first; among equal scores the shorter name, as it is the
    // closer match, then the original (alphabetical) order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|(_, _, i)| i).collect()
}

pub fn matcher() -> Matcher {
    Matcher::new(Config::DEFAULT)
}

/// Typing a filter, and jumping to what it found.
impl Explorer {
    pub(super) fn update_matches(&mut self, query: &str) {
        let mut candidates = Vec::new();
        if let Some(Load::Loaded(schemas)) = self.active_db().map(|db| &db.schemas) {
            for schema in schemas {
                if let Load::Loaded(objects) = &schema.objects {
                    candidates.extend(objects.iter().map(|o| Match {
                        schema: schema.name.clone(),
                        object: o.object.clone(),
                    }));
                }
            }
        }
        let ranked = rank(
            &mut self.matcher.borrow_mut(),
            query,
            candidates.iter().map(|m| m.object.name.as_str()),
        );
        self.matches = ranked.into_iter().map(|i| candidates[i].clone()).collect();
        self.selected_match = 0;
    }

    /// Leaves the filter and selects the chosen match in the tree.
    pub(super) fn jump_to_match(&mut self) {
        let Some(found) = self.matches.get(self.selected_match).cloned() else {
            return;
        };
        let mut path = vec![Self::db_id(&self.active)];
        if self.only_schema().is_none() {
            path.push(format!("s/{}", found.schema));
        }
        path.push(format!("g/{:?}", found.object.kind));
        let mut state = self.state.borrow_mut();
        for depth in 1..=path.len() {
            state.open(path[..depth].to_vec());
        }
        path.push(format!("o/{}", found.object.name));
        state.select(path);
        drop(state);
        self.filter = None;
    }

    pub(super) fn handle_filter(&mut self, action: Action) {
        let Some(filter) = &mut self.filter else {
            return;
        };
        match action {
            Action::Input(c) => {
                filter.push(c);
                let query = filter.clone();
                self.update_matches(&query);
            }
            Action::Backspace => {
                filter.pop();
                let query = filter.clone();
                self.update_matches(&query);
            }
            Action::Up => self.selected_match = self.selected_match.saturating_sub(1),
            Action::Down => {
                self.selected_match =
                    (self.selected_match + 1).min(self.matches.len().saturating_sub(1))
            }
            Action::Confirm => self.jump_to_match(),
            Action::Back => self.filter = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn best_matches_come_first() {
        let names = ["order_items", "orders", "customers", "audit_order_log"];
        let ranked = rank(&mut matcher(), "ordr", names.iter().copied());
        let ranked: Vec<_> = ranked.into_iter().map(|i| names[i]).collect();
        assert_eq!(ranked[0], "orders");
        assert!(!ranked.contains(&"customers"));
    }

    #[test]
    fn ten_thousand_tables_filter_quickly() {
        let names: Vec<String> = (0..10_000)
            .map(|i| format!("schema_{}_table_{i}_{}", i % 37, i % 101))
            .collect();
        let mut matcher = matcher();
        let mut slowest = Duration::ZERO;
        let mut query = String::new();
        for c in "tbl_42".chars() {
            query.push(c);
            let started = Instant::now();
            rank(&mut matcher, &query, names.iter().map(String::as_str));
            slowest = slowest.max(started.elapsed());
        }
        // The plan's bound is 5 ms per keystroke in release builds.
        let bound = if cfg!(debug_assertions) { 100 } else { 5 };
        assert!(
            slowest < Duration::from_millis(bound),
            "slowest keystroke took {slowest:?}"
        );
    }
}
