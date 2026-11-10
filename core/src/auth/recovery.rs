use std::future::Future;
use std::sync::{Arc, Mutex};

use aws_credential_types::provider::{self, ProvideCredentials, SharedCredentialsProvider};
use tokio::sync::Notify;

use super::SsoLoginTarget;

tokio::task_local! { static LOGIN: SsoLogin; }

/// Host interaction for one top-level operation. The host polls `pending`, runs
/// AWS's login tool if permitted, then calls `complete`. SQL stays suspended in
/// its original future, preserving query IDs, destinations and request tokens.
#[derive(Clone, Debug)]
pub struct SsoLogin {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    allowed: bool,
}

#[derive(Debug, Default)]
struct State {
    used: bool,
    pending: Option<SsoLoginTarget>,
    completed: Option<bool>,
    required: Option<SsoLoginTarget>,
}

impl Default for SsoLogin {
    fn default() -> Self {
        Self::new(true)
    }
}

impl SsoLogin {
    pub fn new(allowed: bool) -> Self {
        Self {
            state: Default::default(),
            changed: Default::default(),
            allowed,
        }
    }

    pub fn allowed(&self) -> bool {
        self.allowed
    }

    /// Retain typed sign-in guidance even when interaction is disabled or a
    /// later SDK request wraps the credential error in its own service error.
    pub fn required(&self) -> Option<SsoLoginTarget> {
        self.state.lock().ok()?.required.clone()
    }

    pub(crate) fn enabled() -> bool {
        LOGIN.try_with(|login| login.allowed).unwrap_or(false)
    }

    pub async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
        LOGIN.scope(self.clone(), future).await
    }

    pub fn pending(&self) -> Option<SsoLoginTarget> {
        self.state.lock().ok()?.pending.clone()
    }

    pub fn complete(&self, success: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.pending = None;
            state.completed = Some(success);
        }
        self.changed.notify_one();
    }

    async fn request(&self, target: SsoLoginTarget) -> bool {
        {
            let Ok(mut state) = self.state.lock() else {
                return false;
            };
            if !self.allowed || state.used || state.completed == Some(false) {
                state.required = Some(target);
                return false;
            }
            state.used = true;
            state.pending = Some(target);
        }
        loop {
            let changed = self.changed.notified();
            if let Ok(state) = self.state.lock() {
                if let Some(success) = state.completed {
                    return success;
                }
            } else {
                return false;
            }
            changed.await;
        }
    }
}

pub(super) type ProviderFactory = Arc<dyn Fn() -> SharedCredentialsProvider + Send + Sync>;

pub(super) struct RecoveringCredentials {
    factory: ProviderFactory,
    current: Mutex<SharedCredentialsProvider>,
    target: Option<SsoLoginTarget>,
}

impl std::fmt::Debug for RecoveringCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveringCredentials")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl RecoveringCredentials {
    pub(super) fn new(factory: ProviderFactory, target: Option<SsoLoginTarget>) -> Self {
        let current = Mutex::new(factory());
        Self {
            factory,
            current,
            target,
        }
    }
}

impl ProvideCredentials for RecoveringCredentials {
    fn provide_credentials<'a>(&'a self) -> provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        provider::future::ProvideCredentials::new(async move {
            let provider = self.current.lock().unwrap().clone();
            match provider.provide_credentials().await {
                Ok(credentials) => Ok(credentials),
                Err(error) => {
                    let login = LOGIN.try_with(Clone::clone).ok();
                    // Classify using typed causes without consuming the error.
                    let recoverable = super::is_sso_failure(&error);
                    if recoverable
                        && let (Some(login), Some(target)) = (login, self.target.clone())
                        && login.request(target.clone()).await
                    {
                        let provider = (self.factory)();
                        *self.current.lock().unwrap() = provider.clone();
                        let result = provider.provide_credentials().await;
                        if result.as_ref().is_err_and(super::is_sso_failure)
                            && let Ok(mut state) = login.state.lock()
                        {
                            state.required = Some(target);
                        }
                        return result;
                    }
                    Err(error)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
