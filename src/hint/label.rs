//! Hint label generation and progressive matching (Plan §3.2).
//!
//! Given an alphabet and a number of targets, [`generate_labels`] produces a set
//! of short strings with two guarantees:
//!
//! 1. **Uniqueness** — no two labels are equal.
//! 2. **Prefix-freedom** — no label is a prefix of another, so the moment the
//!    typed keys equal a label the choice is unambiguous and we can fire
//!    immediately.
//!
//! Labels are kept as short as possible: with a 9-letter alphabet the first 9
//! targets get single keys, the next ones get two, and so on. The plan calls for
//! "2-key combinations"; this generalises that (1 key when few targets, 2+ when
//! many) while honouring the same idea.

/// Generate `count` prefix-free labels from `alphabet`, shortest first.
///
/// The algorithm grows a frontier of candidate strings breadth-first. Starting
/// from the single characters, it repeatedly takes the lexicographically first
/// candidate and expands it into `alphabet.len()` children, until enough leaves
/// remain to label every target. Because a string is only ever used *or*
/// expanded (never both), the result is prefix-free by construction.
pub fn generate_labels(alphabet: &[char], count: usize) -> Vec<String> {
    assert!(alphabet.len() >= 2, "alphabet needs at least two characters");
    if count == 0 {
        return Vec::new();
    }

    // Frontier of candidate labels, in assignment order.
    let mut frontier: Vec<String> = alphabet.iter().map(|c| c.to_string()).collect();
    // Index of the next candidate to expand.
    let mut expand_at = 0usize;

    // Expand until the number of *available* leaves (frontier minus the ones
    // we've consumed by expanding) is at least `count`.
    while frontier.len() - expand_at < count {
        let parent = frontier[expand_at].clone();
        expand_at += 1;
        for &c in alphabet {
            frontier.push(format!("{parent}{c}"));
        }
    }

    frontier
        .into_iter()
        .skip(expand_at)
        .take(count)
        .collect()
}

/// The result of feeding one keystroke to a [`HintMatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchState {
    /// Exactly one label equals the typed sequence: its index is the target.
    Selected(usize),
    /// Several labels still start with the typed sequence; keep typing.
    Pending,
    /// No label starts with the typed sequence: the input was a dead end.
    NoMatch,
}

/// Tracks the keys typed so far and reports which hints still match.
#[derive(Debug, Clone)]
pub struct HintMatcher {
    labels: Vec<String>,
    typed: String,
}

impl HintMatcher {
    pub fn new(labels: Vec<String>) -> Self {
        HintMatcher {
            labels,
            typed: String::new(),
        }
    }

    pub fn typed(&self) -> &str {
        &self.typed
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Indices of the hints still consistent with what's been typed.
    pub fn candidates(&self) -> Vec<usize> {
        self.labels
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with(&self.typed))
            .map(|(i, _)| i)
            .collect()
    }

    /// Feed one character. Returns the resulting [`MatchState`]; on `NoMatch`
    /// the character is *not* appended, so the caller can beep and let the user
    /// continue from the previous valid prefix.
    pub fn push(&mut self, c: char) -> MatchState {
        let mut candidate = self.typed.clone();
        candidate.push(c);

        let still_match: Vec<usize> = self
            .labels
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with(&candidate))
            .map(|(i, _)| i)
            .collect();

        if still_match.is_empty() {
            return MatchState::NoMatch;
        }

        self.typed = candidate;

        // Prefix-freedom guarantees that an exact equality is unique.
        if let Some(&idx) = still_match
            .iter()
            .find(|&&i| self.labels[i] == self.typed)
        {
            MatchState::Selected(idx)
        } else {
            MatchState::Pending
        }
    }

    /// Remove the last typed character (backspace).
    pub fn pop(&mut self) {
        self.typed.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alphabet(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn no_label_is_prefix_of_another(labels: &[String]) -> bool {
        for (i, a) in labels.iter().enumerate() {
            for (j, b) in labels.iter().enumerate() {
                if i != j && b.starts_with(a.as_str()) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn few_targets_get_single_keys() {
        let labels = generate_labels(&alphabet("abc"), 3);
        assert_eq!(labels, vec!["a", "b", "c"]);
    }

    #[test]
    fn many_targets_extend_to_two_keys() {
        let labels = generate_labels(&alphabet("ab"), 4);
        // 2 single chars can't cover 4, so all become 2-char.
        assert_eq!(labels.len(), 4);
        assert!(labels.iter().all(|l| l.len() == 2));
        assert!(no_label_is_prefix_of_another(&labels));
    }

    #[test]
    fn labels_are_unique_and_prefix_free_across_sizes() {
        let ab = alphabet("asdfghjkl");
        for count in [1usize, 5, 9, 10, 50, 81, 200, 730] {
            let labels = generate_labels(&ab, count);
            assert_eq!(labels.len(), count, "count {count}");
            let set: std::collections::HashSet<_> = labels.iter().collect();
            assert_eq!(set.len(), count, "labels not unique at count {count}");
            assert!(
                no_label_is_prefix_of_another(&labels),
                "prefix collision at count {count}"
            );
        }
    }

    #[test]
    fn zero_targets_is_empty() {
        assert!(generate_labels(&alphabet("ab"), 0).is_empty());
    }

    #[test]
    fn matcher_selects_on_full_label() {
        let mut m = HintMatcher::new(vec!["aa".into(), "ab".into(), "ba".into()]);
        assert_eq!(m.push('a'), MatchState::Pending);
        assert_eq!(m.candidates(), vec![0, 1]);
        assert_eq!(m.push('b'), MatchState::Selected(1));
    }

    #[test]
    fn matcher_rejects_dead_end_without_consuming() {
        let mut m = HintMatcher::new(vec!["aa".into(), "ab".into()]);
        assert_eq!(m.push('a'), MatchState::Pending);
        assert_eq!(m.push('z'), MatchState::NoMatch);
        // 'z' was not recorded, so we can still finish from "a".
        assert_eq!(m.typed(), "a");
        assert_eq!(m.push('a'), MatchState::Selected(0));
    }

    #[test]
    fn single_char_label_selects_immediately() {
        let mut m = HintMatcher::new(vec!["a".into(), "b".into()]);
        assert_eq!(m.push('b'), MatchState::Selected(1));
    }

    #[test]
    fn backspace_reopens_candidates() {
        let mut m = HintMatcher::new(vec!["aa".into(), "ab".into()]);
        m.push('a');
        m.push('a');
        m.pop();
        assert_eq!(m.candidates(), vec![0, 1]);
    }
}
