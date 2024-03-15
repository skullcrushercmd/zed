pub mod assistant_panel;
pub mod assistant_settings;
mod codegen;
mod prompts;
mod streaming_diff;

use ai::providers::open_ai::Role;
use anyhow::Result;
pub use assistant_panel::AssistantPanel;
use assistant_settings::OpenAiModel;
use chrono::{DateTime, Local};
use collections::HashMap;
use command_palette_hooks::CommandPaletteFilter;
use fs::Fs;
use futures::StreamExt;
use gpui::{actions, AppContext, Global, SharedString};
use regex::Regex;
use serde::{Deserialize, Serialize};
use settings::{Settings, SettingsStore};
use std::{cmp::Reverse, ffi::OsStr, path::PathBuf, sync::Arc};
use util::paths::CONVERSATIONS_DIR;

use crate::assistant_settings::AssistantSettings;

actions!(
    assistant,
    [
        NewConversation,
        Assist,
        Split,
        CycleMessageRole,
        QuoteSelection,
        ToggleFocus,
        ResetKey,
        InlineAssist,
        ToggleIncludeConversation,
        ToggleRetrieveContext,
    ]
);

#[derive(
    Copy, Clone, Debug, Default, Eq, PartialEq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
struct MessageId(usize);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MessageMetadata {
    role: Role,
    sent_at: DateTime<Local>,
    status: MessageStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum MessageStatus {
    Pending,
    Done,
    Error(SharedString),
}

#[derive(Serialize, Deserialize)]
struct SavedMessage {
    id: MessageId,
    start: usize,
}

#[derive(Serialize, Deserialize)]
struct SavedConversation {
    id: Option<String>,
    zed: String,
    version: String,
    text: String,
    messages: Vec<SavedMessage>,
    message_metadata: HashMap<MessageId, MessageMetadata>,
    summary: String,
    api_url: Option<String>,
    model: OpenAiModel,
}

impl SavedConversation {
    const VERSION: &'static str = "0.1.0";
}

struct SavedConversationMetadata {
    title: String,
    path: PathBuf,
    mtime: chrono::DateTime<chrono::Local>,
}

impl SavedConversationMetadata {
    pub async fn list(fs: Arc<dyn Fs>) -> Result<Vec<Self>> {
        fs.create_dir(&CONVERSATIONS_DIR).await?;

        let mut paths = fs.read_dir(&CONVERSATIONS_DIR).await?;
        let mut conversations = Vec::<SavedConversationMetadata>::new();
        while let Some(path) = paths.next().await {
            let path = path?;
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }

            let pattern = r" - \d+.zed.json$";
            let re = Regex::new(pattern).unwrap();

            let metadata = fs.metadata(&path).await?;
            if let Some((file_name, metadata)) = path
                .file_name()
                .and_then(|name| name.to_str())
                .zip(metadata)
            {
                let title = re.replace(file_name, "");
                conversations.push(Self {
                    title: title.into_owned(),
                    path,
                    mtime: metadata.mtime.into(),
                });
            }
        }
        conversations.sort_unstable_by_key(|conversation| Reverse(conversation.mtime));

        Ok(conversations)
    }
}

/// The state pertaining to the Assistant.
#[derive(Default)]
struct Assistant {
    /// Whether the Assistant is enabled.
    enabled: bool,
}

impl Global for Assistant {}

impl Assistant {
    const NAMESPACE: &'static str = "assistant";

    fn set_enabled(&mut self, enabled: bool, cx: &mut AppContext) {
        if self.enabled == enabled {
            return;
        }

        if !enabled {
            cx.update_global::<CommandPaletteFilter, _>(|filter, _| {
                filter.hidden_namespaces.insert(Self::NAMESPACE);
            });

            return;
        }

        cx.update_global::<CommandPaletteFilter, _>(|filter, _| {
            filter.hidden_namespaces.remove(Self::NAMESPACE);
        });
    }
}

pub fn init(cx: &mut AppContext) {
    assistant_panel::init(cx);

    cx.update_global::<CommandPaletteFilter, _>(|filter, _| {
        filter.hidden_namespaces.insert(Assistant::NAMESPACE);
    });
    cx.update_global(|assistant: &mut Assistant, cx: &mut AppContext| {
        let settings = AssistantSettings::get_global(cx);

        assistant.set_enabled(settings.button, cx);
    });
    cx.observe_global::<SettingsStore>(|cx| {
        cx.update_global(|assistant: &mut Assistant, cx: &mut AppContext| {
            let settings = AssistantSettings::get_global(cx);

            assistant.set_enabled(settings.button, cx);
        });
    })
    .detach();
}

#[cfg(test)]
#[ctor::ctor]
fn init_logger() {
    if std::env::var("RUST_LOG").is_ok() {
        env_logger::init();
    }
}
