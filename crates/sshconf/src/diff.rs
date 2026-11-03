//! Line diff (longest common subsequence) used to keep unchanged lines when a
//! block is rewritten, and to show changes before saving or on a conflict.

use std::ops::Range;

/// One step from `old` to `new`; indices point into the two inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOp {
    Same { old: usize, new: usize },
    Removed { old: usize },
    Added { new: usize },
}

impl DiffOp {
    pub fn is_change(self) -> bool {
        !matches!(self, DiffOp::Same { .. })
    }
}

/// A minimal edit script from `old` to `new`. Common prefix and suffix are
/// matched first, so the quadratic part only covers the changed middle.
pub fn diff<T: PartialEq>(old: &[T], new: &[T]) -> Vec<DiffOp> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];

    // lcs[i][j]: length of the LCS of a[i..] and b[j..].
    let width = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * width + j] = if a[i] == b[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }

    let mut ops: Vec<DiffOp> = (0..prefix)
        .map(|i| DiffOp::Same { old: i, new: i })
        .collect();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push(DiffOp::Same {
                old: prefix + i,
                new: prefix + j,
            });
            i += 1;
            j += 1;
        } else if i < a.len()
            && (j == b.len() || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
        {
            ops.push(DiffOp::Removed { old: prefix + i });
            i += 1;
        } else {
            ops.push(DiffOp::Added { new: prefix + j });
            j += 1;
        }
    }
    let (tail_old, tail_new) = (old.len() - suffix, new.len() - suffix);
    ops.extend((0..suffix).map(|k| DiffOp::Same {
        old: tail_old + k,
        new: tail_new + k,
    }));
    ops
}

/// Ranges of `ops` to show: every change plus `context` unchanged ops around
/// it; ranges closer than that are merged.
pub fn hunks(ops: &[DiffOp], context: usize) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if !op.is_change() {
            continue;
        }
        let start = i.saturating_sub(context);
        let end = (i + context + 1).min(ops.len());
        match out.last_mut() {
            Some(last) if start <= last.end => last.end = end,
            _ => out.push(start..end),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders ops as ` x`, `-x`, `+x` lines.
    fn render(old: &[&str], new: &[&str]) -> Vec<String> {
        diff(old, new)
            .into_iter()
            .map(|op| match op {
                DiffOp::Same { old: i, .. } => format!(" {}", old[i]),
                DiffOp::Removed { old: i } => format!("-{}", old[i]),
                DiffOp::Added { new: j } => format!("+{}", new[j]),
            })
            .collect()
    }

    #[test]
    fn identical_inputs_are_all_same() {
        assert_eq!(render(&["a", "b"], &["a", "b"]), [" a", " b"]);
        assert!(diff::<&str>(&[], &[]).is_empty());
    }

    #[test]
    fn changes_in_the_middle() {
        assert_eq!(
            render(&["a", "b", "c", "d"], &["a", "x", "c", "d", "e"]),
            [" a", "-b", "+x", " c", " d", "+e"]
        );
        assert_eq!(render(&["a"], &[]), ["-a"]);
        assert_eq!(render(&[], &["a"]), ["+a"]);
    }

    #[test]
    fn moved_line_keeps_the_longest_common_part() {
        assert_eq!(
            render(&["h", "1", "2", "3"], &["h", "2", "3", "1"]),
            [" h", "-1", " 2", " 3", "+1"]
        );
    }

    #[test]
    fn hunks_merge_nearby_changes() {
        let old: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let mut new = old.clone();
        new[3] = "x".into();
        new[6] = "y".into();
        new[17] = "z".into();
        let ops = diff(&old, &new);
        let ranges = hunks(&ops, 2);
        assert_eq!(ranges.len(), 2);
        assert!(ranges[0].start <= 1 && ranges[1].end == ops.len());
        assert!(hunks(&diff(&old, &old), 3).is_empty());
    }
}
