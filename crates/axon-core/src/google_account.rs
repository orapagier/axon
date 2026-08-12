//! Per-call Google account override.
//!
//! Google API helpers all resolve their bearer token through a single choke
//! point (`axon_google::auth::access_token`), which reads the one globally
//! authenticated account from `Storage::tokens.google`. That is the account the
//! Credentials/Services page signs in, and it stays the default.
//!
//! To let an individual workflow node act as a *different* Gmail account, the
//! caller scopes its work with [`scoped`] and the choke point picks the override
//! up from there. A task-local is used rather than a parameter because the
//! alternative is threading `Option<&str>` through ~45 `gmail::*` functions plus
//! every Calendar/Drive/Sheets sibling; the override instead applies uniformly to
//! all of them for free.
//!
//! The override carries an already-resolved (and already-refreshed) access
//! token. Resolving a stored credential and refreshing it needs the SQLite
//! `credentials` table, which lives in `axon-agent`, so that half deliberately
//! stays out of this crate.
//!
//! Scope inheritance follows `tokio::task_local` rules: the value is visible to
//! everything awaited inside the scope, but **not** to a `tokio::spawn`ed task,
//! which starts with an empty task-local set. Spawn outside the scope and apply
//! [`scoped`] inside the new task.

tokio::task_local! {
    /// Access token that supersedes the globally authenticated Google account
    /// for the duration of a [`scoped`] future.
    static GOOGLE_ACCESS_TOKEN: String;
}

/// Run `fut` with every Google API call inside it authenticated as `access_token`.
///
/// `None` runs `fut` untouched, on the global account — so callers can pass an
/// optional selection straight through without branching.
pub async fn scoped<F: std::future::Future>(access_token: Option<String>, fut: F) -> F::Output {
    match access_token {
        Some(token) => GOOGLE_ACCESS_TOKEN.scope(token, fut).await,
        None => fut.await,
    }
}

/// The access token for the current scope, or `None` to use the global account.
pub fn current() -> Option<String> {
    GOOGLE_ACCESS_TOKEN.try_with(|t| t.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unscoped_falls_back_to_the_global_account() {
        assert_eq!(current(), None);
        // An explicit `None` must not establish a scope either, otherwise the
        // "no credential picked" path would stop resolving the global token.
        assert_eq!(scoped(None, async { current() }).await, None);
    }

    #[tokio::test]
    async fn scope_applies_inside_and_is_dropped_after() {
        let inside = scoped(Some("tok-a".into()), async { current() }).await;
        assert_eq!(inside.as_deref(), Some("tok-a"));
        assert_eq!(current(), None);
    }

    #[tokio::test]
    async fn inner_scope_shadows_the_outer_one() {
        let seen = scoped(Some("outer".into()), async {
            let inner = scoped(Some("inner".into()), async { current() }).await;
            (inner, current())
        })
        .await;
        assert_eq!(seen.0.as_deref(), Some("inner"));
        assert_eq!(seen.1.as_deref(), Some("outer"));
    }

    #[tokio::test]
    async fn nested_none_keeps_the_enclosing_scope() {
        let seen = scoped(Some("outer".into()), async {
            scoped(None, async { current() }).await
        })
        .await;
        assert_eq!(seen.as_deref(), Some("outer"));
    }
}
