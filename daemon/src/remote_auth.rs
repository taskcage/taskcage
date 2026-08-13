//! Remote principal secret 검증과 session revocation 상태를 관리한다.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use tokio::sync::watch;

use crate::remote_config::PrincipalPolicy;

const DUMMY_ARGON2ID: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZS1maXhlZC1zYWx0$u1FGeu2AahxZdZmkrOJioBgcvDvGiwSu9tSlA6aLJkI";

#[derive(Clone)]
pub struct CredentialStore {
    inner: Arc<CredentialStoreInner>,
}

struct CredentialStoreInner {
    principals: RwLock<BTreeMap<String, PrincipalPolicy>>,
    revisions: RwLock<BTreeMap<String, watch::Sender<u64>>>,
}

impl CredentialStore {
    pub fn new(principals: BTreeMap<String, PrincipalPolicy>) -> Self {
        let revisions = principals
            .keys()
            .map(|client_id| {
                let (sender, _) = watch::channel(0_u64);
                (client_id.clone(), sender)
            })
            .collect();
        Self {
            inner: Arc::new(CredentialStoreInner {
                principals: RwLock::new(principals),
                revisions: RwLock::new(revisions),
            }),
        }
    }

    pub fn authenticate(&self, client_id: &str, secret: &str) -> Option<AuthenticatedPrincipal> {
        let policy = self
            .inner
            .principals
            .read()
            .expect("credential store poisoned")
            .get(client_id)
            .cloned();
        let verifier = policy
            .as_ref()
            .map(|principal| principal.secret_verifier.as_str())
            .unwrap_or(DUMMY_ARGON2ID);
        let verified = PasswordHash::new(verifier).ok().is_some_and(|hash| {
            Argon2::default()
                .verify_password(secret.as_bytes(), &hash)
                .is_ok()
        });
        if !verified {
            return None;
        }
        let policy = policy?;
        let revocation = self
            .inner
            .revisions
            .read()
            .expect("credential revisions poisoned")
            .get(client_id)
            .map(watch::Sender::subscribe)?;
        Some(AuthenticatedPrincipal { policy, revocation })
    }

    /// 새 verifier는 새 연결에 즉시 적용하고 기존 session은 유지한다.
    pub fn rotate(&self, policy: PrincipalPolicy) {
        let client_id = policy.client_id.clone();
        self.inner
            .principals
            .write()
            .expect("credential store poisoned")
            .insert(client_id.clone(), policy);
        self.inner
            .revisions
            .write()
            .expect("credential revisions poisoned")
            .entry(client_id)
            .or_insert_with(|| watch::channel(0_u64).0);
    }

    /// revoke는 새 인증을 막고 현재 session receiver를 깨운다.
    pub fn revoke(&self, client_id: &str) {
        self.inner
            .principals
            .write()
            .expect("credential store poisoned")
            .remove(client_id);
        if let Some(revision) = self
            .inner
            .revisions
            .read()
            .expect("credential revisions poisoned")
            .get(client_id)
        {
            revision.send_modify(|value| *value = value.saturating_add(1));
        }
    }

    pub fn replace_all(&self, principals: BTreeMap<String, PrincipalPolicy>) {
        let existing = self
            .inner
            .principals
            .read()
            .expect("credential store poisoned")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for client_id in existing {
            if !principals.contains_key(&client_id) {
                self.revoke(&client_id);
            }
        }
        for policy in principals.into_values() {
            self.rotate(policy);
        }
    }
}

pub struct AuthenticatedPrincipal {
    pub policy: PrincipalPolicy,
    revocation: watch::Receiver<u64>,
}

impl AuthenticatedPrincipal {
    pub async fn revoked(&mut self) {
        let _ = self.revocation.changed().await;
    }
}
