//! The skill as a zip the person uploads in Claude's skills settings: the
//! guide and every bundled reference under one folder named for the skill,
//! each file byte for byte what Musaic serves, nothing else. The same app
//! version gives the same bytes.

use musaic_commands::skill;

/// The archive and the name it is saved under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillArchive {
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub version: String,
}

/// Build the archive from the embedded bundle.
pub fn build() -> Result<SkillArchive, String> {
    let bundle = skill::bundle();
    let entries: Vec<(String, Vec<u8>, u32)> = bundle
        .entries
        .iter()
        .map(|e| {
            (
                format!("{}/{}", skill::SKILL_NAME, e.path),
                e.bytes.clone(),
                0o644,
            )
        })
        .collect();
    Ok(SkillArchive {
        file_name: format!("{}-{}.zip", skill::SKILL_NAME, bundle.version()),
        bytes: super::claude_desktop::zip_entries(&entries)?,
        version: bundle.version().to_string(),
    })
}
