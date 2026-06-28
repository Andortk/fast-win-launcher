//! A small, dependency-free fuzzy matcher.
//!
//! Scores a lowercase query against a lowercase candidate as a subsequence
//! match with bonuses for prefix hits, word-boundary hits, and contiguous runs.
//! Returns `None` when the query isn't a subsequence of the candidate at all.

/// Higher is better. `None` means no match.
pub fn score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().collect();
    let c: Vec<char> = candidate.chars().collect();

    let mut qi = 0usize;
    let mut total = 0i32;
    let mut run = 0i32;
    let mut prev_match: Option<usize> = None;

    for (ci, &ch) in c.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if ch == q[qi] {
            let mut s = 10;
            // Start-of-string or start-of-word is a strong signal.
            let at_boundary =
                ci == 0 || matches!(c[ci - 1], ' ' | '-' | '_' | '.' | '/' | '\\' | '(');
            if at_boundary {
                s += 15;
            }
            if ci == 0 {
                s += 10;
            }
            // Reward contiguous matches.
            if prev_match == Some(ci.wrapping_sub(1)) {
                run += 1;
                s += 5 * run;
            } else {
                run = 0;
            }
            total += s;
            prev_match = Some(ci);
            qi += 1;
        }
    }

    if qi != q.len() {
        return None;
    }

    // Prefer shorter candidates and earlier first-match.
    total -= (c.len() as i32) / 4;
    Some(total)
}

/// Rank `apps` against `query`, returning indices best-first. With an empty
/// query the original (alphabetical) order is preserved.
pub fn rank(query: &str, apps: &[crate::state::AppEntry]) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..apps.len()).collect();
    }
    let mut scored: Vec<(i32, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, a)| score(&q, &a.lower_name).map(|s| (s, i)))
        .collect();
    // Sort by score desc, then by name length asc (stable index as tiebreak).
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| apps[a.1].lower_name.len().cmp(&apps[b.1].lower_name.len()))
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, i)| i).collect()
}
