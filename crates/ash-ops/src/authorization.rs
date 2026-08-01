use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ash_engine::Program;
use ash_protocol::request::Request;
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, RESULT_RETAINED, RetryClass, Status,
};
use ash_protocol::{
    ALL_CAPABILITY_MASK, APPROVAL_SIGNING_BYTES, APPROVAL_TOKEN_BYTES, ApprovalChallenge,
    ApprovalToken,
};
use thiserror::Error;

use crate::OperationError;
use crate::projection::{charge, presentation_limit};

const ACTION_DOMAIN: &[u8] = b"ash-approval-action-v1\0";
const POLICY_DOMAIN: &[u8] = b"ash-approval-policy-v1\0";
const NONCE_DOMAIN: &[u8] = b"ash-approval-nonce-v1\0";
const TOKEN_DOMAIN: &[u8] = b"ash-approval-token-v1\0";
const DEFAULT_CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_POLICY_ID_BYTES: usize = 128;
const MAX_PENDING_PERMITS: usize = 4096;
const MAX_USED_PERMITS: usize = 4096;

/// Session policy split between direct grants and per-action approval grants.
#[derive(Clone, Debug)]
pub struct AuthorizationPolicy {
    policy_digest: [u8; 16],
    granted: u64,
    approval_required: u64,
    authority: Option<PermitAuthority>,
    challenge_ttl: Duration,
}

impl AuthorizationPolicy {
    /// Allows the selected capabilities without per-action approval.
    pub fn allow(capabilities: u64) -> Result<Self, AuthorizationError> {
        validate_mask(capabilities)?;
        Ok(Self {
            policy_digest: policy_digest("ash-direct-grant"),
            granted: capabilities,
            approval_required: 0,
            authority: None,
            challenge_ttl: DEFAULT_CHALLENGE_TTL,
        })
    }

    /// Requires a valid one-time permit for `approval_required` capabilities.
    pub fn with_approvals(
        policy_id: &str,
        granted: u64,
        approval_required: u64,
        authority: PermitAuthority,
    ) -> Result<Self, AuthorizationError> {
        validate_policy_id(policy_id)?;
        validate_mask(granted)?;
        validate_mask(approval_required)?;
        if granted & approval_required != 0
            || approval_required == 0
            || authority.policy_digest() != policy_digest(policy_id)
        {
            return Err(AuthorizationError::InvalidPolicy);
        }
        Ok(Self {
            policy_digest: policy_digest(policy_id),
            granted,
            approval_required,
            authority: Some(authority),
            challenge_ttl: DEFAULT_CHALLENGE_TTL,
        })
    }

    pub fn with_challenge_ttl(mut self, ttl: Duration) -> Result<Self, AuthorizationError> {
        if ttl.is_zero() || ttl > Duration::from_secs(60 * 60) {
            return Err(AuthorizationError::InvalidPolicy);
        }
        self.challenge_ttl = ttl;
        Ok(self)
    }

    #[must_use]
    pub const fn capability_mask(&self) -> u64 {
        self.granted | self.approval_required
    }

    #[must_use]
    pub fn restrict(mut self, capabilities: u64) -> Self {
        let capabilities = capabilities & ALL_CAPABILITY_MASK;
        self.granted &= capabilities;
        self.approval_required &= capabilities;
        if self.approval_required == 0 {
            self.authority = None;
        }
        self
    }
}

impl Default for AuthorizationPolicy {
    fn default() -> Self {
        Self {
            policy_digest: policy_digest("ash-default"),
            granted: ALL_CAPABILITY_MASK,
            approval_required: 0,
            authority: None,
            challenge_ttl: DEFAULT_CHALLENGE_TTL,
        }
    }
}

/// Keyed issuer/verifier for opaque, one-time, session-bound approval permits.
#[derive(Clone)]
pub struct PermitAuthority {
    inner: Arc<PermitAuthorityInner>,
}

struct PermitAuthorityInner {
    key: [u8; 32],
    session_binding: [u8; 16],
    policy_digest: [u8; 16],
    counter: AtomicU64,
    state: Mutex<PermitState>,
}

#[derive(Default)]
struct PermitState {
    pending: HashMap<[u8; 16], ApprovalChallenge>,
    used: HashMap<[u8; 16], u64>,
}

impl PermitAuthority {
    /// The trusted harness supplies a secret key and a fresh per-session
    /// binding. Neither value crosses the ASH protocol.
    pub fn new(
        key: [u8; 32],
        session_binding: [u8; 16],
        policy_id: &str,
    ) -> Result<Self, AuthorizationError> {
        validate_policy_id(policy_id)?;
        if key == [0; 32] || session_binding == [0; 16] {
            return Err(AuthorizationError::InvalidAuthority);
        }
        Ok(Self {
            inner: Arc::new(PermitAuthorityInner {
                key,
                session_binding,
                policy_digest: policy_digest(policy_id),
                counter: AtomicU64::new(0),
                state: Mutex::new(PermitState::default()),
            }),
        })
    }

    /// Signs a challenge only after the embedding harness has obtained the
    /// external approval represented by that challenge.
    pub fn issue(
        &self,
        challenge: &ApprovalChallenge,
    ) -> Result<ApprovalToken, AuthorizationError> {
        let now = unix_millis()?;
        if challenge.session_binding() != self.inner.session_binding
            || challenge.policy_digest() != self.inner.policy_digest
            || challenge.expires_at_millis() <= now
        {
            return Err(AuthorizationError::InvalidChallenge);
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AuthorizationError::Poisoned)?;
        purge_expired(&mut state, now);
        if state.pending.get(&challenge.nonce()) != Some(challenge) {
            return Err(AuthorizationError::InvalidChallenge);
        }
        let signing = challenge.signing_bytes();
        let mac = token_mac(&self.inner.key, &signing);
        let mut token = [0_u8; APPROVAL_TOKEN_BYTES];
        token[..16].copy_from_slice(&challenge.nonce());
        token[16..].copy_from_slice(mac.as_bytes());
        Ok(ApprovalToken::from_bytes(token))
    }

    fn policy_digest(&self) -> [u8; 16] {
        self.inner.policy_digest
    }

    fn challenge(
        &self,
        session_id: u64,
        capabilities: u64,
        action_digest: [u8; 32],
        ttl: Duration,
    ) -> Result<ApprovalChallenge, AuthorizationError> {
        let now = unix_millis()?;
        let ttl = u64::try_from(ttl.as_millis()).map_err(|_| AuthorizationError::Clock)?;
        let expires = now.checked_add(ttl).ok_or(AuthorizationError::Clock)?;
        let counter = self
            .inner
            .counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| AuthorizationError::NonceExhausted)?;
        let mut material = Vec::with_capacity(128);
        material.extend_from_slice(NONCE_DOMAIN);
        material.extend_from_slice(&self.inner.session_binding);
        material.extend_from_slice(&session_id.to_be_bytes());
        material.extend_from_slice(&capabilities.to_be_bytes());
        material.extend_from_slice(&expires.to_be_bytes());
        material.extend_from_slice(&counter.to_be_bytes());
        material.extend_from_slice(&action_digest);
        let nonce_hash = blake3::keyed_hash(&self.inner.key, &material);
        let nonce = nonce_hash.as_bytes()[..16]
            .try_into()
            .map_err(|_| AuthorizationError::InvalidChallenge)?;
        let challenge = ApprovalChallenge::new(
            session_id,
            capabilities,
            expires,
            self.inner.session_binding,
            self.inner.policy_digest,
            action_digest,
            nonce,
        )?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AuthorizationError::Poisoned)?;
        purge_expired(&mut state, now);
        if state.pending.len() >= MAX_PENDING_PERMITS {
            return Err(AuthorizationError::Capacity);
        }
        if state.pending.contains_key(&nonce) {
            return Err(AuthorizationError::InvalidChallenge);
        }
        state.pending.insert(nonce, challenge);
        Ok(challenge)
    }

    fn verify(
        &self,
        token: &ApprovalToken,
        session_id: u64,
        capabilities: u64,
        action_digest: [u8; 32],
    ) -> Result<(), AuthorizationError> {
        let bytes = token.as_bytes();
        let nonce: [u8; 16] = bytes[..16]
            .try_into()
            .map_err(|_| AuthorizationError::InvalidToken)?;
        let now = unix_millis()?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AuthorizationError::Poisoned)?;
        purge_expired(&mut state, now);
        if state.used.contains_key(&nonce) {
            return Err(AuthorizationError::Replay);
        }
        let challenge = state
            .pending
            .get(&nonce)
            .copied()
            .ok_or(AuthorizationError::InvalidToken)?;
        if challenge.session_id() != session_id
            || challenge.capabilities() != capabilities
            || challenge.session_binding() != self.inner.session_binding
            || challenge.policy_digest() != self.inner.policy_digest
            || challenge.action_digest() != action_digest
            || challenge.expires_at_millis() <= now
        {
            return Err(AuthorizationError::InvalidToken);
        }
        let expected = token_mac(&self.inner.key, &challenge.signing_bytes());
        if !constant_time_eq(expected.as_bytes(), &bytes[16..]) {
            return Err(AuthorizationError::InvalidToken);
        }
        if state.used.len() >= MAX_USED_PERMITS {
            return Err(AuthorizationError::Capacity);
        }
        state.pending.remove(&nonce);
        state.used.insert(nonce, challenge.expires_at_millis());
        Ok(())
    }
}

impl fmt::Debug for PermitAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PermitAuthority")
            .field("session_binding", &self.inner.session_binding)
            .field("policy_digest", &self.inner.policy_digest)
            .finish_non_exhaustive()
    }
}

pub(crate) fn authorize(
    policy: &AuthorizationPolicy,
    request: &Request,
    program: &Program,
) -> Result<Option<FinalResponse>, OperationError> {
    let required = request.required_capabilities();
    if required & !policy.capability_mask() != 0 {
        return Err(OperationError::CapabilityDenied);
    }
    let approval = required & policy.approval_required;
    if approval == 0 {
        return Ok(None);
    }
    let authority = policy
        .authority
        .as_ref()
        .ok_or(AuthorizationError::InvalidPolicy)?;
    if authority.policy_digest() != policy.policy_digest {
        return Err(AuthorizationError::InvalidPolicy.into());
    }
    let action_digest = action_digest(request)?;
    let error_code = if let Some(token) = request.permit() {
        match authority.verify(token, program.session_id(), approval, action_digest) {
            Ok(()) => return Ok(None),
            Err(_) => ErrorCode::PermitInvalid,
        }
    } else {
        ErrorCode::PermitRequired
    };
    let challenge = authority.challenge(
        program.session_id(),
        approval,
        action_digest,
        policy.challenge_ttl,
    )?;
    let evidence = challenge.encode()?.encode().into_bytes();

    let worst = approval_response(request.id(), error_code, u64::MAX)?;
    if worst.encode()?.encode().len() > presentation_limit(program) {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &worst, 1)?;
    let reference = program.store().retain(evidence)?;
    Ok(Some(approval_response(
        request.id(),
        error_code,
        reference,
    )?))
}

fn approval_response(
    request_id: u64,
    code: ErrorCode,
    evidence: u64,
) -> Result<FinalResponse, OperationError> {
    Ok(FinalResponse::failure(
        request_id,
        Status::Denied,
        ErrorRecord {
            code,
            retry: RetryClass::Approval,
            stage: ErrorStage::Authorize,
            evidence: Some(evidence),
            argument: None,
        },
        vec![],
        None,
        RESULT_RETAINED,
        None,
    )?)
}

fn action_digest(request: &Request) -> Result<[u8; 32], OperationError> {
    let target = request.authorization_target()?.encode();
    let mut hasher = blake3::Hasher::new();
    hasher.update(ACTION_DOMAIN);
    hasher.update(target.as_bytes());
    Ok(*hasher.finalize().as_bytes())
}

fn token_mac(key: &[u8; 32], signing: &[u8; APPROVAL_SIGNING_BYTES]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(TOKEN_DOMAIN);
    hasher.update(signing);
    hasher.finalize()
}

fn purge_expired(state: &mut PermitState, now: u64) {
    state
        .pending
        .retain(|_, challenge| challenge.expires_at_millis() > now);
    state.used.retain(|_, expiry| *expiry > now);
}

fn policy_digest(policy_id: &str) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(POLICY_DOMAIN);
    hasher.update(policy_id.as_bytes());
    let mut digest = [0_u8; 16];
    digest.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    digest
}

fn validate_policy_id(policy_id: &str) -> Result<(), AuthorizationError> {
    if policy_id.is_empty()
        || policy_id.len() > MAX_POLICY_ID_BYTES
        || policy_id.contains(['\0', '\n', '\r'])
    {
        Err(AuthorizationError::InvalidPolicy)
    } else {
        Ok(())
    }
}

fn validate_mask(mask: u64) -> Result<(), AuthorizationError> {
    if mask & !ALL_CAPABILITY_MASK == 0 {
        Ok(())
    } else {
        Err(AuthorizationError::InvalidPolicy)
    }
}

fn unix_millis() -> Result<u64, AuthorizationError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthorizationError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| AuthorizationError::Clock)
}

fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[derive(Debug, Error)]
pub enum AuthorizationError {
    #[error("authorization policy is invalid")]
    InvalidPolicy,
    #[error("permit authority key or session binding is invalid")]
    InvalidAuthority,
    #[error("approval challenge is invalid or belongs to another authority")]
    InvalidChallenge,
    #[error("approval token is invalid")]
    InvalidToken,
    #[error("approval token was already consumed")]
    Replay,
    #[error("approval replay cache reached its bounded capacity")]
    Capacity,
    #[error("approval nonce sequence is exhausted")]
    NonceExhausted,
    #[error("system clock cannot represent the approval deadline")]
    Clock,
    #[error("approval replay cache lock was poisoned")]
    Poisoned,
    #[error(transparent)]
    Value(#[from] ash_protocol::ApprovalValueError),
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{AuthorizationError, AuthorizationPolicy, PermitAuthority, policy_digest};
    use ash_protocol::{APPROVAL_TOKEN_BYTES, ApprovalChallenge, ApprovalToken, Capability};

    #[test]
    fn tokens_are_action_session_policy_and_replay_bound() {
        let authority = PermitAuthority::new([7; 32], [8; 16], "policy").expect("authority");
        let challenge = authority
            .challenge(
                9,
                Capability::WorkspaceWrite.mask(),
                [3; 32],
                Duration::from_secs(60),
            )
            .expect("challenge");
        let unissued = ApprovalChallenge::new(
            9,
            Capability::WorkspaceWrite.mask(),
            super::unix_millis().expect("time") + 60_000,
            [8; 16],
            policy_digest("policy"),
            [6; 32],
            [5; 16],
        )
        .expect("unissued challenge");
        assert!(matches!(
            authority.issue(&unissued),
            Err(AuthorizationError::InvalidChallenge)
        ));
        let token = authority.issue(&challenge).expect("token");

        let mut forged = *token.as_bytes();
        forged[APPROVAL_TOKEN_BYTES - 1] ^= 1;
        assert!(matches!(
            authority.verify(
                &ApprovalToken::from_bytes(forged),
                9,
                Capability::WorkspaceWrite.mask(),
                [3; 32]
            ),
            Err(AuthorizationError::InvalidToken)
        ));
        assert!(
            authority
                .verify(&token, 10, Capability::WorkspaceWrite.mask(), [3; 32])
                .is_err()
        );
        assert!(
            authority
                .verify(&token, 9, Capability::WorkspaceRead.mask(), [3; 32])
                .is_err()
        );
        assert!(
            authority
                .verify(&token, 9, Capability::WorkspaceWrite.mask(), [5; 32])
                .is_err()
        );
        authority
            .verify(&token, 9, Capability::WorkspaceWrite.mask(), [3; 32])
            .expect("verify");
        assert!(matches!(
            authority.verify(&token, 9, Capability::WorkspaceWrite.mask(), [3; 32]),
            Err(AuthorizationError::Replay)
        ));

        let other = PermitAuthority::new([7; 32], [9; 16], "policy").expect("other");
        assert!(
            other
                .verify(&token, 9, Capability::WorkspaceWrite.mask(), [3; 32])
                .is_err()
        );

        assert!(AuthorizationPolicy::allow(u64::MAX).is_err());
        assert!(
            AuthorizationPolicy::with_approvals(
                "policy",
                Capability::WorkspaceWrite.mask(),
                Capability::WorkspaceWrite.mask(),
                authority,
            )
            .is_err()
        );
    }
}
