//! `ai_conversations` and `ai_messages` — the AI analyst's memory.
//!
//! A conversation belongs to one person on one site (see the migration for
//! why the site is part of it). Every read here takes the owner as well as the
//! id, so a conversation id that leaked into a log or a screenshot cannot be
//! read by anybody else: the row is simply not found.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// One conversation, without its messages.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Conversation {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// The person it belongs to.
    pub user_id: Uuid,
    /// The first question, shortened.
    pub title: String,
    /// When it opened.
    pub created_at: DateTime<Utc>,
    /// When the last message was added.
    pub updated_at: DateTime<Utc>,
}

/// A conversation in a list, with its message count.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConversationSummary {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The first question, shortened.
    pub title: String,
    /// When it opened.
    pub created_at: DateTime<Utc>,
    /// When the last message was added.
    pub updated_at: DateTime<Utc>,
    /// How many messages it holds, questions and answers both.
    pub messages: i64,
}

/// One question or answer.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Message {
    /// UUIDv7 primary key; also the order within the conversation.
    pub id: Uuid,
    /// The conversation it belongs to.
    pub conversation_id: Uuid,
    /// `user` or `assistant`.
    pub role: String,
    /// The question, or the answer's text.
    pub content: String,
    /// The tools the model ran for an answer, with their queries.
    pub tool_calls: Json,
    /// Why an answer failed, when it did.
    pub error: Option<String>,
    /// When it was written.
    pub created_at: DateTime<Utc>,
}

const CONV_COLS: &str = "id, site_id, user_id, title, created_at, updated_at";
const MSG_COLS: &str = "id, conversation_id, role, content, tool_calls, error, created_at";

/// Open a conversation.
pub async fn create_conversation(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    user_id: Uuid,
    title: &str,
) -> Result<Conversation> {
    let row = sqlx::query_as::<_, Conversation>(&format!(
        "INSERT INTO ai_conversations (id, site_id, user_id, title)
         VALUES ($1, $2, $3, $4) RETURNING {CONV_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(user_id)
    .bind(title)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// One conversation, only if it belongs to `user_id`.
pub async fn get_conversation(pool: &PgPool, id: Uuid, user_id: Uuid) -> Result<Conversation> {
    let row = sqlx::query_as::<_, Conversation>(&format!(
        "SELECT {CONV_COLS} FROM ai_conversations WHERE id = $1 AND user_id = $2"
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "conversation")
}

/// A person's conversations on one site, most recently active first.
pub async fn list_conversations(
    pool: &PgPool,
    user_id: Uuid,
    site_id: Uuid,
    limit: i64,
) -> Result<Vec<ConversationSummary>> {
    let rows = sqlx::query_as::<_, ConversationSummary>(
        "SELECT c.id, c.title, c.created_at, c.updated_at,
                (SELECT count(*) FROM ai_messages m WHERE m.conversation_id = c.id) AS messages
         FROM ai_conversations c
         WHERE c.user_id = $1 AND c.site_id = $2
         ORDER BY c.updated_at DESC
         LIMIT $3",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Delete a conversation and its messages, only if it belongs to `user_id`.
///
/// Returns whether a row went. Deleting somebody else's conversation and
/// deleting one that never existed look the same to the caller.
pub async fn delete_conversation(pool: &PgPool, id: Uuid, user_id: Uuid) -> Result<bool> {
    let done = sqlx::query("DELETE FROM ai_conversations WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Append a message and mark the conversation active.
pub async fn add_message(
    pool: &PgPool,
    id: Uuid,
    conversation_id: Uuid,
    role: &str,
    content: &str,
    tool_calls: &Json,
    error: Option<&str>,
) -> Result<Message> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query_as::<_, Message>(&format!(
        "INSERT INTO ai_messages (id, conversation_id, role, content, tool_calls, error)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {MSG_COLS}"
    ))
    .bind(id)
    .bind(conversation_id)
    .bind(role)
    .bind(content)
    .bind(tool_calls)
    .bind(error)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE ai_conversations SET updated_at = now() WHERE id = $1")
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(row)
}

/// Every message in a conversation, oldest first.
pub async fn list_messages(pool: &PgPool, conversation_id: Uuid) -> Result<Vec<Message>> {
    let rows = sqlx::query_as::<_, Message>(&format!(
        "SELECT {MSG_COLS} FROM ai_messages WHERE conversation_id = $1 ORDER BY created_at, id"
    ))
    .bind(conversation_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
