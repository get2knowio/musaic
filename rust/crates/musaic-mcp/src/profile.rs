//! The MCP revisions this server speaks, and how they are named in the
//! descriptor and in `doctor` (research R1). The negotiation itself is rmcp's:
//! an `initialize` request selects the 2025-11-25 session, a request carrying
//! the 2026-07-28 `_meta` is served statelessly after `server/discover`.

use rmcp::model::ProtocolVersion;

/// The revisions served, newest first.
pub fn supported() -> Vec<ProtocolVersion> {
    vec![ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25]
}

/// The name a revision has in `agent-instance.json` and in `doctor`.
pub fn profile_name(v: &ProtocolVersion) -> String {
    format!("mcp-{}", v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_the_contract_constants() {
        let names: Vec<String> = supported().iter().map(profile_name).collect();
        assert_eq!(names, musaic_commands::PROTOCOL_PROFILES);
    }
}
