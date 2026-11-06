use crate::CoreError;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct Settings {
    pub base_url: String,
    pub model: String,
    pub system_prompt: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-flash".into(),
            system_prompt: String::new(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), CoreError> {
        super::provider::completion_url(&self.base_url)?;
        if self.model.trim().is_empty() || self.model.len() > 256 {
            return Err(CoreError::new(
                "Enter a model name with at most 256 characters.",
            ));
        }
        if self.system_prompt.len() > 64 * 1024 {
            return Err(CoreError::new(
                "The system prompt is too long. Shorten it and try again.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Complete,
    Streaming,
    Cancelled,
    Interrupted,
    Error,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    pub status: MessageStatus,
    pub created_at: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    pub updated_at: u64,
    #[serde(default)]
    pub draft: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct State {
    version: u32,
    settings: Settings,
    sessions: Vec<Session>,
}

/// One private profile. A held lock prevents two processes from losing updates.
pub struct Store {
    directory: PathBuf,
    state: State,
    _lock: File,
}

impl Store {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, CoreError> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)
            .and_then(|()| fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)))
            .map_err(|_| {
                CoreError::new("The app cannot create its private conversation folder.")
            })?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(directory.join("profile.lock"))
            .map_err(|_| CoreError::new("The conversation folder cannot be opened."))?;
        lock.try_lock().map_err(|_| {
            CoreError::new("This native profile is already open. Return to its existing window.")
        })?;
        let state_path = directory.join("state.json");
        let mut state = match File::open(&state_path) {
            Ok(file) => {
                if file.metadata().map_err(|_| read_error())?.len() > MAX_STATE_BYTES {
                    return Err(read_error());
                }
                let mut bytes = Vec::new();
                file.take(MAX_STATE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| read_error())?;
                if bytes.len() as u64 > MAX_STATE_BYTES {
                    return Err(read_error());
                }
                let state: State = serde_json::from_slice(&bytes).map_err(|_| read_error())?;
                if state.version != 1 {
                    return Err(CoreError::new("This native profile version is unsupported. Open it with a matching app version."));
                }
                state.settings.validate()?;
                state
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State {
                version: 1,
                settings: Settings::default(),
                sessions: Vec::new(),
            },
            Err(_) => return Err(read_error()),
        };
        let mut recovered = false;
        for session in &mut state.sessions {
            for message in &mut session.messages {
                if message.status == MessageStatus::Streaming {
                    message.status = MessageStatus::Interrupted;
                    recovered = true;
                }
            }
        }
        state
            .sessions
            .sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        let store = Self {
            directory,
            state,
            _lock: lock,
        };
        if recovered {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn settings(&self) -> &Settings {
        &self.state.settings
    }

    pub fn save_settings(&mut self, settings: Settings) -> Result<(), CoreError> {
        settings.validate()?;
        self.change(|state| {
            state.settings = settings;
            Ok(())
        })
    }

    pub fn sessions(&self) -> &[Session] {
        &self.state.sessions
    }

    pub fn session(&self, id: &str) -> Option<&Session> {
        self.state.sessions.iter().find(|session| session.id == id)
    }

    pub fn create_session(&mut self) -> Result<Session, CoreError> {
        let session = Session {
            id: Uuid::new_v4().to_string(),
            title: "New conversation".into(),
            messages: Vec::new(),
            updated_at: now(),
            draft: String::new(),
        };
        self.change(|state| {
            state.sessions.insert(0, session.clone());
            Ok(())
        })?;
        Ok(session)
    }

    pub fn append_message(
        &mut self,
        session_id: &str,
        role: Role,
        content: impl Into<String>,
        status: MessageStatus,
    ) -> Result<(), CoreError> {
        let content = content.into();
        validate_message(&content)?;
        self.change(|state| {
            let session = state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
                .ok_or_else(session_error)?;
            if role == Role::User
                && !session
                    .messages
                    .iter()
                    .any(|message| message.role == Role::User)
            {
                session.title = title(&content);
            }
            session.messages.push(Message {
                id: Uuid::new_v4().to_string(),
                role,
                content,
                status,
                created_at: now(),
            });
            session.updated_at = now();
            state
                .sessions
                .sort_by_key(|session| std::cmp::Reverse(session.updated_at));
            Ok(())
        })
    }

    pub fn draft(&self, session_id: &str) -> &str {
        self.session(session_id)
            .map_or("", |session| session.draft.as_str())
    }

    pub fn save_draft(
        &mut self,
        session_id: &str,
        content: impl Into<String>,
    ) -> Result<(), CoreError> {
        let content = content.into();
        validate_message(&content)?;
        self.change(|state| {
            let session = state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
                .ok_or_else(session_error)?;
            session.draft = content;
            Ok(())
        })
    }

    /// Begin a complete persisted turn or leave both the draft and history intact.
    pub fn begin_turn(
        &mut self,
        session_id: &str,
        input: impl Into<String>,
    ) -> Result<(), CoreError> {
        let input = input.into();
        validate_message(&input)?;
        if input.trim().is_empty() {
            return Err(CoreError::new(
                "Write a message before starting a response.",
            ));
        }
        self.change(|state| {
            let session = state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
                .ok_or_else(session_error)?;
            if !session
                .messages
                .iter()
                .any(|message| message.role == Role::User)
            {
                session.title = title(&input);
            }
            let timestamp = now();
            session.messages.push(Message {
                id: Uuid::new_v4().to_string(),
                role: Role::User,
                content: input,
                status: MessageStatus::Complete,
                created_at: timestamp,
            });
            session.messages.push(Message {
                id: Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: String::new(),
                status: MessageStatus::Streaming,
                created_at: timestamp,
            });
            session.draft.clear();
            session.updated_at = timestamp;
            state
                .sessions
                .sort_by_key(|session| std::cmp::Reverse(session.updated_at));
            Ok(())
        })
    }

    pub fn replace_last_assistant(
        &mut self,
        session_id: &str,
        content: impl Into<String>,
        status: MessageStatus,
    ) -> Result<(), CoreError> {
        let content = content.into();
        validate_message(&content)?;
        self.change(|state| {
            let session = state.sessions.iter_mut().find(|session| session.id == session_id)
                .ok_or_else(session_error)?;
            let message = session.messages.last_mut().filter(|message| message.role == Role::Assistant)
                .ok_or_else(|| CoreError::new("The assistant response cannot be updated. Reopen the conversation and try again."))?;
            message.content = content;
            message.status = status;
            session.updated_at = now();
            state.sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
            Ok(())
        })
    }

    fn change(
        &mut self,
        update: impl FnOnce(&mut State) -> Result<(), CoreError>,
    ) -> Result<(), CoreError> {
        let previous = self.state.clone();
        update(&mut self.state)?;
        if let Err(error) = self.persist() {
            self.state = previous;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), CoreError> {
        let bytes = serde_json::to_vec(&self.state).map_err(|_| save_error())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(CoreError::new("This native profile has reached its current storage limit. Start a separate profile to continue."));
        }
        let temporary = self
            .directory
            .join(format!(".state-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.directory.join("state.json"))?;
            File::open(&self.directory)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(save_error());
        }
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn validate_message(content: &str) -> Result<(), CoreError> {
    if content.len() > MAX_MESSAGE_BYTES {
        return Err(CoreError::new(
            "This message is too large. Shorten it and try again.",
        ));
    }
    Ok(())
}

fn title(content: &str) -> String {
    let text: String = content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(64)
        .collect();
    if text.is_empty() {
        "New conversation".into()
    } else {
        text
    }
}

fn session_error() -> CoreError {
    CoreError::new(
        "This conversation is unavailable. Select another conversation or start a new one.",
    )
}

fn read_error() -> CoreError {
    CoreError::new("Saved conversations could not be read. Keep the profile folder intact and restore it from a backup.")
}

fn save_error() -> CoreError {
    CoreError::new(
        "Changes could not be saved. Check free disk space and folder permissions, then try again.",
    )
}
