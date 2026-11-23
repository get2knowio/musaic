//! The skill over MCP: `skills/list`, `skills/get`, and the plain Resources
//! fallback (`resources/list`, `resources/read`) that serves the same bytes on
//! both profiles. Every answer is built from [`musaic_commands::skill::bundle`],
//! so the digest and size in the manifest are those of the bytes read.

use musaic_commands::skill::{self, Bundle, Entry, Unknown};
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CacheScope, GetSkillResult, ListResourcesResult, ListSkillsResult, ReadResourceResult,
    Resource, ResourceContents, Skill, SkillFrontmatter, SkillResource, SkillResources,
};

const MARKDOWN: &str = "text/markdown";

fn refuse(_: Unknown) -> McpError {
    McpError::invalid_params(skill::UNKNOWN_RESOURCE, None)
}

fn frontmatter(b: &Bundle) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::new(b.name(), b.description());
    if let Some(obj) = b.frontmatter.as_object() {
        for (k, v) in obj {
            if k != "name" && k != "description" {
                fm = fm.with_field(k.clone(), v.clone());
            }
        }
    }
    fm
}

fn the_skill() -> Skill {
    let b = skill::bundle();
    let files = b
        .entries
        .iter()
        .map(|e| SkillResource::new(e.uri.clone(), e.digest.clone(), e.size as u64))
        .collect();
    Skill::new(
        b.skill_md().uri.clone(),
        frontmatter(b),
        SkillResources::Files(files),
    )
}

pub(crate) fn list() -> ListSkillsResult {
    let mut r = ListSkillsResult::with_all_items(vec![the_skill()]);
    r.ttl_ms = Some(skill::TTL_MS);
    r.cache_scope = Some(CacheScope::Public);
    r
}

pub(crate) fn get(uri: &str) -> Result<GetSkillResult, McpError> {
    let b = skill::bundle();
    let entry = b.lookup(uri).map_err(refuse)?;
    if entry.uri != b.skill_md().uri {
        return Err(refuse(Unknown));
    }
    let mut r = GetSkillResult::new(the_skill());
    r.ttl_ms = Some(skill::TTL_MS);
    r.cache_scope = Some(CacheScope::Public);
    Ok(r)
}

fn file_name(e: &Entry) -> &str {
    e.path.rsplit('/').next().unwrap_or(e.path)
}

pub(crate) fn resources() -> ListResourcesResult {
    let b = skill::bundle();
    let items = b
        .entries
        .iter()
        .map(|e| {
            let r = Resource::new(e.uri.clone(), file_name(e))
                .with_mime_type(MARKDOWN)
                .with_size(e.size as u64);
            if e.path == skill::ENTRY {
                r.with_description(b.description())
            } else {
                r
            }
        })
        .collect();
    ListResourcesResult::with_all_items(items)
}

pub(crate) fn read(uri: &str) -> Result<ReadResourceResult, McpError> {
    let entry = skill::bundle().lookup(uri).map_err(refuse)?;
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(entry.text(), entry.uri.clone()).with_mime_type(MARKDOWN),
    ]))
}

/// The URI of the skill's `SKILL.md`, exactly.
pub(crate) fn is_skill_md(uri: &str) -> bool {
    uri == skill::bundle().skill_md().uri
}
