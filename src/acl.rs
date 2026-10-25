//! Device approval for small self-hosted deployments.
//!
//! The stock OSS server lets any client that knows the key register an id and
//! become reachable. This module adds an optional policy on top of that, plus the
//! storage helpers used by the management API in [`crate::api`].
//!
//! The `status` column of the `peer` table is reused as the approval state:
//! `1` approved, `0` blocked, `NULL` pending an administrator's decision.

use crate::database::{Database, Peer};
use hbb_common::{anyhow::anyhow, log, tokio::sync::RwLock, ResultType};
use once_cell::sync::{Lazy, OnceCell};
use std::collections::HashMap;

/// How devices that the server has never seen before are treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Upstream behaviour: anybody with the right key may register.
    Open,
    /// Unknown devices are recorded as pending and refused until approved.
    Approve,
    /// Unknown devices are refused and never recorded.
    Whitelist,
}

/// What the server decided about one device id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Known to the server, waiting for an administrator.
    Pending,
    /// Refused by an administrator.
    Blocked,
    /// Never seen by this server.
    Unknown,
}

const STATUS_APPROVED: i64 = 1;
const STATUS_BLOCKED: i64 = 0;

static DB: OnceCell<Database> = OnceCell::new();
static POLICY: OnceCell<Policy> = OnceCell::new();
static API_KEY: OnceCell<String> = OnceCell::new();
static CACHE: Lazy<RwLock<HashMap<String, Decision>>> = Lazy::new(Default::default);

pub fn init(db: Database) -> ResultType<()> {
    let policy = parse_policy(&crate::common::get_arg("peer-policy"))?;
    let api_key = crate::common::get_arg("api-key");
    let _ = DB.set(db);
    let _ = POLICY.set(policy);
    let _ = API_KEY.set(api_key);
    log::info!("peer policy: {:?}", policy);
    Ok(())
}

fn parse_policy(value: &str) -> ResultType<Policy> {
    match value.trim().to_lowercase().as_str() {
        "" | "open" => Ok(Policy::Open),
        "approve" => Ok(Policy::Approve),
        "whitelist" => Ok(Policy::Whitelist),
        other => Err(anyhow!(
            "invalid peer-policy `{}`, expected one of: open, approve, whitelist",
            other
        )),
    }
}

pub fn policy() -> Policy {
    POLICY.get().copied().unwrap_or(Policy::Open)
}

pub fn api_key() -> &'static str {
    API_KEY.get().map(String::as_str).unwrap_or("")
}

fn db() -> ResultType<&'static Database> {
    DB.get().ok_or_else(|| anyhow!("the database is not initialised"))
}

/// Cached approval decision for one device.
pub async fn decide(id: &str) -> Decision {
    if policy() == Policy::Open {
        return Decision::Allowed;
    }
    if let Some(decision) = CACHE.read().await.get(id).copied() {
        return decision;
    }
    let decision = load(id).await;
    CACHE.write().await.insert(id.to_owned(), decision);
    decision
}

async fn load(id: &str) -> Decision {
    let database = match db() {
        Ok(database) => database,
        Err(err) => {
            log::error!("approval lookup failed: {}", err);
            return Decision::Blocked;
        }
    };
    match database.get_peer(id).await {
        Ok(Some(peer)) => match peer.status {
            Some(STATUS_APPROVED) => Decision::Allowed,
            Some(_) => Decision::Blocked,
            None => Decision::Pending,
        },
        Ok(None) => Decision::Unknown,
        Err(err) => {
            log::error!("approval lookup for {} failed: {}", id, err);
            Decision::Blocked
        }
    }
}

pub async fn is_allowed(id: &str) -> bool {
    decide(id).await == Decision::Allowed
}

async fn invalidate(id: &str) {
    CACHE.write().await.remove(id);
}

/// Record an unknown device so that an administrator can see and approve it.
pub async fn record_pending(id: &str, uuid: &[u8], pk: &[u8], ip: &str) -> ResultType<()> {
    if policy() != Policy::Approve {
        return Ok(());
    }
    let info = serde_json::to_string(&serde_json::json!({ "ip": ip })).unwrap_or_default();
    db()?.insert_or_ignore(id, uuid, pk, &info, None).await?;
    invalidate(id).await;
    Ok(())
}

/// Approve a device, creating the record when it is not known yet.
pub async fn approve(id: &str) -> ResultType<()> {
    let database = db()?;
    if !database.set_status(id, STATUS_APPROVED).await? {
        database
            .insert_or_ignore(id, &[], &[], "{}", Some(STATUS_APPROVED))
            .await?;
    }
    invalidate(id).await;
    log::info!("approved device {}", id);
    Ok(())
}

/// Block a device. Returns false when the server does not know the id.
pub async fn block(id: &str) -> ResultType<bool> {
    let database = db()?;
    let updated = database.set_status(id, STATUS_BLOCKED).await?;
    invalidate(id).await;
    if updated {
        log::info!("blocked device {}", id);
    }
    Ok(updated)
}

/// Forget a device completely, including its key record.
pub async fn forget(id: &str) -> ResultType<bool> {
    let database = db()?;
    let deleted = database.delete_peer(id).await?;
    invalidate(id).await;
    if deleted {
        log::info!("removed device {}", id);
    }
    Ok(deleted)
}

pub async fn list_all() -> ResultType<Vec<Peer>> {
    db()?.list_peers(1000).await
}

/// Stable label for the approval state of a stored peer.
pub fn status_label(status: Option<i64>) -> &'static str {
    match status {
        Some(STATUS_APPROVED) => "approved",
        Some(_) => "blocked",
        None => "pending",
    }
}
