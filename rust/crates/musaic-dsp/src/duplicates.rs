//! The duplicate-tracks clustering rule, ported from
//! `musaic/checks/duplicate_tracks.py::_cluster_duplicates`.
//!
//! Only fingerprints from one implementation may be clustered together;
//! the caller is responsible for not mixing them.

use crate::fingerprint::chromaprint_similarity;

/// `fingerprint.SIMILARITY_DUPLICATE_THRESHOLD`.
pub const SIMILARITY_DUPLICATE_THRESHOLD: f64 = 0.95;
/// `duplicate_tracks._MAX_DURATION_DELTA_SEC`.
pub const MAX_DURATION_DELTA_SEC: f64 = 2.0;

/// One fingerprinted track of an album, as the check reads it from
/// `track_fingerprints` (`duration_s` is `float(duration_s or 0.0)`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterTrack<'a> {
    pub filename: &'a str,
    pub duration_s: f64,
    pub fingerprint: &'a str,
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Cluster one album's tracks. Pass them in the Python app's order
/// (`ORDER BY filename`); the result lists clusters in order of first
/// appearance of their union-find root and each cluster's members in input
/// order, as the Python app's insertion-ordered `defaultdict` does. Returned
/// values are indices into `tracks`.
///
/// A pair is linked when `|d_i - d_j| <= 2.0` and similarity `>= 0.95`;
/// pairs already in one component are skipped (so transitive links count,
/// exactly like the Python app's union-find). Components of two or more tracks
/// are clusters.
pub fn cluster_duplicates(tracks: &[ClusterTrack<'_>]) -> Vec<Vec<usize>> {
    let n = tracks.len();
    if n < 2 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let mut edges_added = 0usize;
    for i in 0..n {
        for j in (i + 1)..n {
            if find(&mut parent, i) == find(&mut parent, j) {
                continue;
            }
            if (tracks[i].duration_s - tracks[j].duration_s).abs() > MAX_DURATION_DELTA_SEC {
                continue;
            }
            let sim = chromaprint_similarity(tracks[i].fingerprint, tracks[j].fingerprint);
            if sim >= SIMILARITY_DUPLICATE_THRESHOLD {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
                edges_added += 1;
            }
        }
    }
    if edges_added == 0 {
        return Vec::new();
    }
    let mut roots: Vec<usize> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        match roots.iter().position(|&x| x == r) {
            Some(k) => groups[k].push(i),
            None => {
                roots.push(r);
                groups.push(vec![i]);
            }
        }
    }
    groups.retain(|g| g.len() >= 2);
    groups
}
