//! Permission kinds, the decision a policy gives, and how a scope and a policy
//! combine (FR-031 to FR-033).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What an agent may be allowed to do. A pairing's scope is a subset of these.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub enum PermissionKind {
    #[serde(rename = "library.read")]
    LibraryRead,
    #[serde(rename = "proposal.submit")]
    ProposalSubmit,
    #[serde(rename = "compute.scan")]
    ComputeScan,
    #[serde(rename = "compute.analyze")]
    ComputeAnalyze,
    #[serde(rename = "tags.write")]
    TagsWrite,
    #[serde(rename = "covers.write")]
    CoversWrite,
    #[serde(rename = "files.rename")]
    FilesRename,
}

impl PermissionKind {
    pub const ALL: [PermissionKind; 7] = [
        Self::LibraryRead,
        Self::ProposalSubmit,
        Self::ComputeScan,
        Self::ComputeAnalyze,
        Self::TagsWrite,
        Self::CoversWrite,
        Self::FilesRename,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LibraryRead => "library.read",
            Self::ProposalSubmit => "proposal.submit",
            Self::ComputeScan => "compute.scan",
            Self::ComputeAnalyze => "compute.analyze",
            Self::TagsWrite => "tags.write",
            Self::CoversWrite => "covers.write",
            Self::FilesRename => "files.rename",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// FR-032: reading and proposing are Allow, everything that computes or
    /// writes is Ask.
    pub fn default_decision(self) -> Decision {
        match self {
            Self::LibraryRead | Self::ProposalSubmit => Decision::Allow,
            _ => Decision::Ask,
        }
    }
}

impl std::fmt::Display for PermissionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A policy decision. The order is the strictness: `Refuse` is the strongest.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Refuse,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Refuse => "refuse",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "refuse" => Some(Self::Refuse),
            _ => None,
        }
    }
}

/// The effective policy of a pairing: the decision stored for a kind, with the
/// default where nothing is stored.
pub fn policy_decision(
    policy: &BTreeMap<PermissionKind, Decision>,
    kind: PermissionKind,
) -> Decision {
    policy
        .get(&kind)
        .copied()
        .unwrap_or_else(|| kind.default_decision())
}

/// FR-033: the strictest decision over `kinds`, where a kind outside `scopes`
/// is `Refuse`. No kinds at all is `Allow` (nothing to decide).
pub fn decide(
    scopes: &[PermissionKind],
    policy: &BTreeMap<PermissionKind, Decision>,
    kinds: &[PermissionKind],
) -> Decision {
    kinds
        .iter()
        .map(|k| {
            if scopes.contains(k) {
                policy_decision(policy, *k)
            } else {
                Decision::Refuse
            }
        })
        .max()
        .unwrap_or(Decision::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use PermissionKind::*;

    #[test]
    fn defaults_follow_fr_032() {
        assert_eq!(LibraryRead.default_decision(), Decision::Allow);
        assert_eq!(ProposalSubmit.default_decision(), Decision::Allow);
        for k in [
            ComputeScan,
            ComputeAnalyze,
            TagsWrite,
            CoversWrite,
            FilesRename,
        ] {
            assert_eq!(k.default_decision(), Decision::Ask, "{k}");
        }
    }

    #[test]
    fn strings_round_trip() {
        for k in PermissionKind::ALL {
            assert_eq!(PermissionKind::parse(k.as_str()), Some(k));
            let j = serde_json::to_string(&k).unwrap();
            assert_eq!(j, format!("\"{}\"", k.as_str()));
        }
        assert_eq!(PermissionKind::parse("tags_write"), None);
    }

    #[test]
    fn kind_outside_scope_is_refused_even_when_policy_allows() {
        let mut policy = BTreeMap::new();
        policy.insert(TagsWrite, Decision::Allow);
        assert_eq!(
            decide(&[LibraryRead], &policy, &[TagsWrite]),
            Decision::Refuse
        );
        assert_eq!(decide(&[TagsWrite], &policy, &[TagsWrite]), Decision::Allow);
    }

    #[test]
    fn strictest_wins_over_kinds() {
        let mut policy = BTreeMap::new();
        policy.insert(TagsWrite, Decision::Allow);
        policy.insert(CoversWrite, Decision::Refuse);
        let scopes = [TagsWrite, CoversWrite, FilesRename];
        assert_eq!(decide(&scopes, &policy, &[TagsWrite]), Decision::Allow);
        assert_eq!(
            decide(&scopes, &policy, &[TagsWrite, FilesRename]),
            Decision::Ask
        );
        assert_eq!(
            decide(&scopes, &policy, &[TagsWrite, CoversWrite]),
            Decision::Refuse
        );
        assert_eq!(decide(&scopes, &policy, &[]), Decision::Allow);
    }
}
