//! Identity vectors: storage, similarity, and grouping faces nobody has named yet.
//!
//! Grouping is leader clustering (each face joins the most similar group above the
//! threshold, or starts one), then groups whose centres are that similar are merged, then
//! every face goes to its best final group. Faces × groups dot products: fine for tens of
//! thousands of faces in the background.

/// Scales `v` to unit length (cosine similarity is then a dot product).
pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > f32::EPSILON {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A vector as little-endian f16 (half the size; plenty for similarity).
pub fn pack(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| half::f16::from_f32(*x).to_le_bytes()).collect()
}

pub fn unpack(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<2>().0.iter().map(|c| half::f16::from_le_bytes(*c).to_f32()).collect()
}

/// The unit-length mean of `vecs`.
pub fn centroid<'a>(vecs: impl IntoIterator<Item = &'a [f32]>) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for v in vecs {
        let s = sum.get_or_insert_with(|| vec![0.0; v.len()]);
        if s.len() != v.len() {
            continue;
        }
        s.iter_mut().zip(v).for_each(|(a, b)| *a += b);
    }
    let mut s = sum?;
    normalize(&mut s);
    Some(s)
}

/// The most similar of `centres` at or above `min`: its index and the similarity.
pub fn nearest(v: &[f32], centres: &[Vec<f32>], min: f32) -> Option<(usize, f32)> {
    centres
        .iter()
        .enumerate()
        .map(|(i, c)| (i, dot(v, c)))
        .filter(|(_, s)| *s >= min)
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Groups unit vectors: `Some(group)` per vector, groups numbered from 0 by size (largest
/// first); `None` for vectors in groups smaller than `min_size`.
pub fn group(vecs: &[Vec<f32>], threshold: f32, min_size: usize) -> Vec<Option<usize>> {
    // 1. Leader pass.
    let mut sums: Vec<Vec<f32>> = Vec::new();
    let mut centres: Vec<Vec<f32>> = Vec::new();
    for v in vecs {
        match nearest(v, &centres, threshold) {
            Some((g, _)) => {
                sums[g].iter_mut().zip(v).for_each(|(a, b)| *a += b);
                let mut c = sums[g].clone();
                normalize(&mut c);
                centres[g] = c;
            }
            None => {
                sums.push(v.clone());
                centres.push(v.clone());
            }
        }
    }
    // 2. Merge groups whose centres are alike.
    let mut parent: Vec<usize> = (0..centres.len()).collect();
    for i in 0..centres.len() {
        for j in i + 1..centres.len() {
            if dot(&centres[i], &centres[j]) >= threshold {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[b] = a;
                }
            }
        }
    }
    let mut merged: Vec<Vec<f32>> = Vec::new();
    let mut root_of: std::collections::HashMap<usize, usize> = Default::default();
    for (i, s) in sums.iter().enumerate() {
        let r = find(&mut parent, i);
        let idx = *root_of.entry(r).or_insert_with(|| {
            merged.push(vec![0.0; s.len()]);
            merged.len() - 1
        });
        merged[idx].iter_mut().zip(s).for_each(|(a, b)| *a += b);
    }
    merged.iter_mut().for_each(|c| normalize(c));
    // 3. Every vector to its best final group.
    let assigned: Vec<Option<usize>> = vecs.iter().map(|v| nearest(v, &merged, threshold).map(|(g, _)| g)).collect();
    // Number groups by size; drop the small ones.
    let mut sizes = vec![0usize; merged.len()];
    for g in assigned.iter().flatten() {
        sizes[*g] += 1;
    }
    let mut order: Vec<usize> = (0..merged.len()).filter(|g| sizes[*g] >= min_size.max(1)).collect();
    order.sort_by(|a, b| sizes[*b].cmp(&sizes[*a]).then(a.cmp(b)));
    let mut renumber = vec![None; merged.len()];
    for (n, g) in order.into_iter().enumerate() {
        renumber[g] = Some(n);
    }
    assigned.into_iter().map(|g| g.and_then(|g| renumber[g])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let mut v = v.to_vec();
        normalize(&mut v);
        v
    }

    #[test]
    fn packing_keeps_values_close() {
        let v = unit(&[0.3, -0.5, 0.8, 0.1]);
        let back = unpack(&pack(&v));
        assert_eq!(back.len(), 4);
        assert!(v.iter().zip(&back).all(|(a, b)| (a - b).abs() < 1e-3));
        assert!(dot(&v, &back) > 0.999);
    }

    #[test]
    fn groups_alike_vectors_and_drops_strays() {
        // Two people (along x and along y) with small variations, and one stray.
        let mut vecs = Vec::new();
        for i in 0..5 {
            vecs.push(unit(&[1.0, 0.05 * i as f32, 0.0]));
        }
        for i in 0..3 {
            vecs.push(unit(&[0.0, 1.0, 0.05 * i as f32]));
        }
        vecs.push(unit(&[0.0, 0.0, 1.0]));
        let g = group(&vecs, 0.8, 2);
        assert!(g[..5].iter().all(|x| *x == Some(0)), "the larger group comes first");
        assert!(g[5..8].iter().all(|x| *x == Some(1)));
        assert_eq!(g[8], None, "a single face is not a group");
    }

    #[test]
    fn nearest_respects_the_threshold() {
        let centres = vec![unit(&[1.0, 0.0]), unit(&[0.0, 1.0])];
        assert_eq!(nearest(&unit(&[0.9, 0.1]), &centres, 0.5).map(|x| x.0), Some(0));
        assert_eq!(nearest(&unit(&[1.0, 1.0]), &centres, 0.9), None);
        let c = centroid([centres[0].as_slice(), centres[1].as_slice()]).unwrap();
        assert!((c[0] - c[1]).abs() < 1e-6);
    }
}
