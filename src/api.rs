//! Small management API for the approval policy.
//!
//! It is meant for a handful of administrators on a private network, not as a
//! general purpose control plane: every request either comes from the loopback
//! interface or has to carry the configured `--api-key`.

use crate::{acl, peer::PeerMap};
use axum::{
    extract::{Extension, Path},
    http::{header, HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use hbb_common::{log, tokio};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;

/// A peer is reported online while it keeps refreshing its registration.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(30);

type Reply = (StatusCode, Json<Value>);

fn unauthorized() -> Reply {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "missing or invalid api key" })),
    )
}

fn internal(err: impl std::fmt::Display) -> Reply {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": err.to_string() })),
    )
}

fn check_auth(headers: &HeaderMap) -> Option<Reply> {
    let expected = acl::api_key();
    if expected.is_empty() {
        // Only reachable on loopback in this mode, see `spawn`.
        return None;
    }
    let provided = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(str::to_owned)
        });
    match provided {
        Some(provided) if provided == expected => None,
        _ => Some(unauthorized()),
    }
}

pub fn spawn(pm: PeerMap) {
    let port = crate::common::get_arg_or("api-port", "21114".to_owned());
    let bind = crate::common::get_arg_or("api-bind", "127.0.0.1".to_owned());
    let addr = match format!("{}:{}", bind, port).parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(err) => {
            log::error!("invalid api bind address {}:{}: {}", bind, port, err);
            return;
        }
    };
    if acl::api_key().is_empty() && !addr.ip().is_loopback() {
        log::error!(
            "refusing to expose the management API on {} without --api-key",
            addr
        );
        return;
    }
    let router = routes(pm);
    tokio::spawn(async move {
        log::info!("management API listening on http://{}", addr);
        if let Err(err) = axum::Server::bind(&addr)
            .serve(router.into_make_service())
            .await
        {
            log::error!("management API stopped: {}", err);
        }
    });
}

fn routes(pm: PeerMap) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/policy", get(policy))
        .route("/api/peers", get(list_peers))
        .route("/api/peers/:id/approve", post(approve))
        .route("/api/peers/:id/block", post(block))
        .route("/api/peers/:id", delete(forget))
        .layer(Extension(pm))
}

async fn health() -> Reply {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "policy": format!("{:?}", acl::policy()).to_lowercase(),
            "version": crate::version::VERSION,
        })),
    )
}

async fn policy(headers: HeaderMap) -> Reply {
    if let Some(denied) = check_auth(&headers) {
        return denied;
    }
    (
        StatusCode::OK,
        Json(json!({
            "policy": format!("{:?}", acl::policy()).to_lowercase(),
        })),
    )
}

async fn list_peers(Extension(pm): Extension<PeerMap>, headers: HeaderMap) -> Reply {
    if let Some(denied) = check_auth(&headers) {
        return denied;
    }
    let stored = match acl::list_all().await {
        Ok(stored) => stored,
        Err(err) => return internal(err),
    };
    let live = pm.online_snapshot(ONLINE_TIMEOUT).await;
    let mut peers = Vec::with_capacity(stored.len() + live.len());
    for peer in &stored {
        let online = live
            .iter()
            .find(|entry| entry.id == peer.id)
            .map(|entry| entry.online)
            .unwrap_or(false);
        peers.push(json!({
            "id": peer.id,
            "status": acl::status_label(peer.status),
            "online": online,
            "info": peer.info,
        }));
    }
    // Peers seen only in memory are usually mid-registration; surface them too.
    for entry in &live {
        if stored.iter().any(|peer| peer.id == entry.id) {
            continue;
        }
        peers.push(json!({
            "id": entry.id,
            "status": "unknown",
            "online": entry.online,
            "info": json!({ "ip": entry.ip }).to_string(),
        }));
    }
    (
        StatusCode::OK,
        Json(json!({ "count": peers.len(), "peers": peers })),
    )
}

async fn approve(Path(id): Path<String>, headers: HeaderMap) -> Reply {
    if let Some(denied) = check_auth(&headers) {
        return denied;
    }
    match acl::approve(&id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "id": id, "status": "approved" })),
        ),
        Err(err) => internal(err),
    }
}

async fn block(Path(id): Path<String>, headers: HeaderMap) -> Reply {
    if let Some(denied) = check_auth(&headers) {
        return denied;
    }
    match acl::block(&id).await {
        Ok(true) => (
            StatusCode::OK,
            Json(json!({ "id": id, "status": "blocked" })),
        ),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "id": id, "error": "unknown id, approve it first" })),
        ),
        Err(err) => internal(err),
    }
}

async fn forget(Path(id): Path<String>, headers: HeaderMap) -> Reply {
    if let Some(denied) = check_auth(&headers) {
        return denied;
    }
    match acl::forget(&id).await {
        Ok(true) => (
            StatusCode::OK,
            Json(json!({ "id": id, "status": "removed" })),
        ),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({ "id": id }))),
        Err(err) => internal(err),
    }
}
