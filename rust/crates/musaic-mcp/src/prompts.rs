//! The prompt fallback for hosts that do not load skills: the same guide as a
//! user message. The only interpolated value is a validated album id.

use musaic_commands::skill;
use rmcp::ErrorData as McpError;
use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, ListPromptsResult, Prompt, PromptArgument,
    PromptMessage, Role,
};

pub const PROMPT_NAME: &str = "musaic_research_album";

pub(crate) fn list() -> ListPromptsResult {
    ListPromptsResult::with_all_items(vec![Prompt::new(
        PROMPT_NAME,
        Some("Process pending Musaic research, or research one album, following the work-delegation guide."),
        Some(vec![
            PromptArgument::new("album_id")
                .with_description("A Musaic album id (16 lowercase hex characters). Omit to process all pending work.")
                .with_required(false),
        ]),
    )])
}

/// An album id is 16 lowercase hex characters (the first 16 of a SHA-1).
pub fn valid_album_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn get(request: &GetPromptRequestParams) -> Result<GetPromptResult, McpError> {
    if request.name != PROMPT_NAME {
        return Err(McpError::invalid_params("unknown prompt", None));
    }
    let album = match request.arguments.as_ref().and_then(|a| a.get("album_id")) {
        None => None,
        Some(v) => match v.as_str() {
            Some(id) if valid_album_id(id) => Some(id),
            _ => {
                return Err(McpError::invalid_params(
                    "album_id is not a Musaic album id",
                    None,
                ));
            }
        },
    };
    let instruction = match album {
        Some(id) => format!(
            "Research the Musaic album with id {id} using the musaic-work-delegation guide below."
        ),
        None => {
            "Process pending Musaic work using the musaic-work-delegation guide below.".to_string()
        }
    };
    let text = format!("{instruction}\n\n{}", skill::bundle().skill_body());
    Ok(
        GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
            .with_description("Musaic work delegation"),
    )
}
