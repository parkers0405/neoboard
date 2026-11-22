//! Pull-only notification ledger, always addressed with this client's board credential.
use super::{Client, ClientError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct InboxPage {
    pub actor: String,
    pub cursor: u64,
    pub items: Vec<InboxItem>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InboxItem {
    pub id: u64,
    pub cursor: u64,
    pub kind: String,
    pub read: bool,
    pub thread: u64,
    pub message_id: u64,
    pub channel: String,
    pub actor: String,
    pub author: String,
    pub text: String,
    pub created_at: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Subscription {
    pub id: u64,
    pub thread: u64,
    pub kind: String,
    pub author: Option<String>,
    pub after_cursor: u64,
    pub registered_cursor: u64,
}

impl Client {
    /// `after` is an exclusive delivery ID, not the board event cursor.
    pub fn inbox(&self, read: bool, after: u64, limit: usize) -> Result<InboxPage, ClientError> {
        self.request(self.http.get(self.url("v1/inbox")).query(&[("read", read)]).query(&[("after", after)]).query(&[("limit", limit)]))
    }

    pub fn set_inbox_read(&self, id: u64, read: bool) -> Result<(), ClientError> {
        self.send(self.http.post(self.url(&format!("v1/inbox/{id}/read"))).json(&serde_json::json!({"read": read})))?;
        Ok(())
    }

    /// Omit `after_cursor` for future-only matching. `author` is required only for replies.
    pub fn register_subscription(&self, thread: u64, kind: &str, author: Option<&str>, after_cursor: Option<u64>) -> Result<Subscription, ClientError> {
        let mut filter = serde_json::json!({"thread": thread, "kind": kind});
        if let Some(author) = author { filter["author"] = author.into(); }
        if let Some(cursor) = after_cursor { filter["after_cursor"] = cursor.into(); }
        self.request(self.http.post(self.url("v1/subscriptions")).json(&filter))
    }

    pub fn subscriptions(&self) -> Result<Vec<Subscription>, ClientError> {
        self.request(self.http.get(self.url("v1/subscriptions")))
    }

    pub fn cancel_subscription(&self, id: u64) -> Result<(), ClientError> {
        self.send(self.http.delete(self.url(&format!("v1/subscriptions/{id}"))))?;
        Ok(())
    }
}