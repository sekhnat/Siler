//! Saved server logins and the proxy password, kept in the desktop keyring through the
//! Secret Service API (KWallet, GNOME Keyring, KeePassXC…). Nothing is read until a server
//! asks for a login, so starting Siler never pops up an unlock prompt.

use crate::i18n::{tr, tr_args};
use oo7::Keyring;
use serde::Serialize;
use siler_core::Login;
use std::collections::HashMap;
use tokio::sync::OnceCell;

static KEYRING: OnceCell<Option<Keyring>> = OnceCell::const_new();

async fn keyring() -> Option<&'static Keyring> {
    KEYRING
        .get_or_init(|| async {
            match Keyring::new().await {
                Ok(k) => Some(k),
                Err(e) => {
                    tracing::warn!("no keyring available, logins last for this session only: {e}");
                    None
                }
            }
        })
        .await
        .as_ref()
}

pub async fn available() -> bool {
    keyring().await.is_some()
}

#[derive(Serialize, Clone, Debug)]
pub struct SavedLogin {
    pub host: String,
    pub username: String,
}

const SITE: &str = "site";
const PROXY: &str = "proxy";

fn attrs<'a>(kind: &'a str, host: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("application", "siler"), ("kind", kind), ("host", host)])
}

async fn read_secret(item: &oo7::Item) -> oo7::Result<String> {
    if item.is_locked().await? {
        item.unlock().await?; // may show the keyring's unlock prompt
    }
    Ok(String::from_utf8_lossy(item.secret().await?.as_bytes()).into_owned())
}

/// Saves (or replaces) the login for a server.
pub async fn save(host: &str, login: &Login) -> Result<(), String> {
    let k = keyring().await.ok_or_else(|| tr("logins-no-keyring"))?;
    let _ = k.delete(&attrs(SITE, host)).await;
    let mut a = attrs(SITE, host);
    a.insert("username", &login.username);
    k.create_item(
        &format!("Siler login for {host}"),
        &a,
        login.password.as_str(),
        true,
    )
    .await
    .map_err(|e| tr_args("logins-couldnt-save", vec![("why", e.to_string().into())]))
}

/// The saved login for a server, if any.
pub async fn find(host: &str) -> Option<Login> {
    let k = keyring().await?;
    let item = k
        .search_items(&attrs(SITE, host))
        .await
        .ok()?
        .into_iter()
        .next()?;
    let username = item.attributes().await.ok()?.get("username")?.clone();
    match read_secret(&item).await {
        Ok(password) => Some(Login { username, password }),
        Err(e) => {
            tracing::info!("couldn't read the saved login for {host}: {e}");
            None
        }
    }
}

/// Servers with a saved login (no passwords are read).
pub async fn list() -> Vec<SavedLogin> {
    let Some(k) = keyring().await else {
        return Vec::new();
    };
    let query = HashMap::from([("application", "siler"), ("kind", SITE)]);
    let mut out = Vec::new();
    for item in k.search_items(&query).await.unwrap_or_default() {
        if let Ok(a) = item.attributes().await
            && let (Some(host), Some(username)) = (a.get("host"), a.get("username"))
        {
            out.push(SavedLogin {
                host: host.clone(),
                username: username.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.host.cmp(&b.host));
    out
}

pub async fn forget(host: &str) -> Result<(), String> {
    let k = keyring().await.ok_or_else(|| tr("logins-no-keyring"))?;
    k.delete(&attrs(SITE, host))
        .await
        .map_err(|e| e.to_string())
}

pub async fn proxy_password() -> Option<String> {
    let k = keyring().await?;
    let item = k
        .search_items(&attrs(PROXY, PROXY))
        .await
        .ok()?
        .into_iter()
        .next()?;
    read_secret(&item).await.ok()
}

/// Whether a proxy password is saved (without reading it, so no unlock prompt).
pub async fn proxy_password_saved() -> bool {
    match keyring().await {
        Some(k) => k
            .search_items(&attrs(PROXY, PROXY))
            .await
            .is_ok_and(|items| !items.is_empty()),
        None => false,
    }
}

pub async fn set_proxy_password(password: Option<&str>) -> Result<(), String> {
    let k = keyring().await.ok_or_else(|| tr("logins-no-keyring"))?;
    match password.filter(|p| !p.is_empty()) {
        Some(p) => k
            .create_item("Siler proxy password", &attrs(PROXY, PROXY), p, true)
            .await
            .map_err(|e| {
                tr_args(
                    "logins-couldnt-save-proxy",
                    vec![("why", e.to_string().into())],
                )
            }),
        None => k
            .delete(&attrs(PROXY, PROXY))
            .await
            .map_err(|e| e.to_string()),
    }
}
