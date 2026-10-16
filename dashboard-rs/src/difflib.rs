//! The part of Python's difflib.SequenceMatcher (isjunk=None, autojunk=False)
//! that worksignals needs: how many non-equal opcodes turn `a` into `b`.

use std::collections::HashMap;

fn find_longest_match(
    a: &[&str],
    b: &[&str],
    b2j: &HashMap<&str, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for i in alo..ahi {
        let mut newj2len: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(a[i]) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = if j > 0 { j2len.get(&(j - 1)).copied().unwrap_or(0) } else { 0 } + 1;
                newj2len.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = newj2len;
    }
    while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
        besti -= 1;
        bestj -= 1;
        bestsize += 1;
    }
    while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] {
        bestsize += 1;
    }
    (besti, bestj, bestsize)
}

fn matching_blocks(a: &[&str], b: &[&str]) -> Vec<(usize, usize, usize)> {
    let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in b.iter().enumerate() {
        b2j.entry(e).or_default().push(i);
    }
    let (la, lb) = (a.len(), b.len());
    let mut queue = vec![(0, la, 0, lb)];
    let mut blocks = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = find_longest_match(a, b, &b2j, alo, ahi, blo, bhi);
        if k > 0 {
            blocks.push((i, j, k));
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    blocks.sort();
    let (mut i1, mut j1, mut k1) = (0, 0, 0);
    let mut out = Vec::new();
    for (i2, j2, k2) in blocks {
        if i1 + k1 == i2 && j1 + k1 == j2 {
            k1 += k2;
        } else {
            if k1 > 0 {
                out.push((i1, j1, k1));
            }
            (i1, j1, k1) = (i2, j2, k2);
        }
    }
    if k1 > 0 {
        out.push((i1, j1, k1));
    }
    out.push((la, lb, 0));
    out
}

/// sum(1 for op in get_opcodes() if op[0] != "equal")
pub fn changed_regions(a: &[&str], b: &[&str]) -> usize {
    let (mut i, mut j) = (0, 0);
    let mut n = 0;
    for (ai, bj, size) in matching_blocks(a, b) {
        if i < ai || j < bj {
            n += 1;
        }
        i = ai + size;
        j = bj + size;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_like_python() {
        // python: difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes()
        assert_eq!(changed_regions(&["x = 1", "y = 2", "z = 3"], &["x = 9", "y = 2", "z = 8"]), 2);
        assert_eq!(changed_regions(&["a"], &["b"]), 1);
        assert_eq!(changed_regions(&["a", "b"], &["a", "b"]), 0);
        assert_eq!(changed_regions(&[], &["a"]), 1);
        assert_eq!(changed_regions(&["a", "b", "c", "d"], &["a", "x", "c", "y", "d"]), 2);
    }
}
