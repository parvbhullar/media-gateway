use super::{
    ProxyAction, ProxyModule,
    auth_rejection_limiter::AuthRejectionRateLimiter,
    dialog_auth_cache::{AuthCacheKey, DialogAuthCache},
    server::SipServerRef,
};
use crate::call::cookie::SpamResult;
use crate::call::user::SipUser;
use crate::call::{CalleeDisplayName, TransactionCookie, TrunkContext};
use crate::callrecord::{
    CallDetails, CallRecord, CallRecordHangupMessage, CallRecordHangupReason, CallRecordLastError,
};
use crate::config::ProxyConfig;
use crate::models::call_record::extract_sip_username;
use anyhow::{Error, Result};
use async_trait::async_trait;
use chrono::Utc;
use rsipstack::dialog::authenticate::verify_digest;
use rsipstack::sip::Header;
use rsipstack::sip::headers::{ProxyAuthenticate, WwwAuthenticate};
use rsipstack::sip::prelude::{HeadersExt, ToTypedHeader};
use rsipstack::sip::typed::Authorization;
use rsipstack::transaction::transaction::Transaction;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace};

#[derive(Debug)]
pub enum AuthError {
    NotFound,
    Disabled,
    InvalidCredentials,
    SpamDetected,
    PaymentRequired,
    Other(Error),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::NotFound => write!(f, "User not found"),
            AuthError::InvalidCredentials => write!(f, "Invalid credentials"),
            AuthError::SpamDetected => write!(f, "Spam detected"),
            AuthError::PaymentRequired => write!(f, "Payment required"),
            AuthError::Disabled => write!(f, "User is disabled"),
            AuthError::Other(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for AuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AuthError::Other(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<Error> for AuthError {
    fn from(e: Error) -> Self {
        AuthError::Other(e)
    }
}

#[async_trait]
pub trait AuthBackend: Send + Sync {
    async fn authenticate(
        &self,
        original: &rsipstack::sip::Request,
        cookie: &TransactionCookie,
    ) -> Result<Option<SipUser>, AuthError>;
}

#[derive(Clone)]
pub struct AuthModule {
    server: SipServerRef,
    dialog_auth_cache: Option<DialogAuthCache>,
    /// Bounds how many auth-rejection CDRs a single source IP can write,
    /// preventing a unique-Call-ID flood from amplifying into unbounded DB rows.
    rejection_limiter: AuthRejectionRateLimiter,
}

impl AuthModule {
    pub fn create(server: SipServerRef, config: Arc<ProxyConfig>) -> Result<Box<dyn ProxyModule>> {
        let module = AuthModule::new(server, config);
        Ok(Box::new(module))
    }

    pub fn new(server: SipServerRef, config: Arc<ProxyConfig>) -> Self {
        let dialog_auth_cache = config.dialog_auth_cache.as_ref().and_then(|cache_config| {
            if cache_config.enabled {
                Some(DialogAuthCache::new(cache_config))
            } else {
                None
            }
        });

        Self {
            server,
            dialog_auth_cache,
            rejection_limiter: AuthRejectionRateLimiter::default(),
        }
    }

    pub async fn authenticate_request(
        &self,
        tx: &Transaction,
    ) -> Result<Option<SipUser>, AuthError> {
        let mut auth_inner: Option<(Authorization, &str)> = None;
        for header in tx.original.headers.iter() {
            match header {
                Header::Authorization(h) => {
                    auth_inner = Authorization::parse(h.value())
                        .ok()
                        .map(|auth| (auth, h.value()));
                    break;
                }
                Header::ProxyAuthorization(h) => {
                    auth_inner = Authorization::parse(h.value())
                        .ok()
                        .map(|auth| (auth, h.value()));
                    break;
                }
                _ => {}
            }
        }
        let (auth_inner, raw_auth_header) = match auth_inner {
            Some(auth) => auth,
            None => {
                return Ok(None);
            }
        };
        let user = SipUser::try_from(tx).map_err(AuthError::Other)?;
        // Check if user exists and is enabled
        match self
            .server
            .user_backend
            .get_user(&user.username, user.realm.as_deref(), Some(&tx.original))
            .await?
        {
            Some(mut stored_user) => {
                if !stored_user.enabled {
                    info!(username = user.username, realm = ?user.realm, "User is disabled");
                    return Ok(None);
                }
                if let Some(realm) = user.realm.as_ref()
                    && !self.server.is_same_realm(realm).await
                {
                    info!(username = user.username, realm = ?user.realm, "User is not in the same realm");
                    return Ok(None);
                }
                stored_user.merge_with(&user);
                match self.verify_credentials(
                    &stored_user,
                    &tx.original.method,
                    &auth_inner,
                    raw_auth_header,
                ) {
                    true => Ok(Some(stored_user)),
                    false => Ok(None),
                }
            }
            None => {
                info!(username = user.username, realm = ?user.realm, "authenticate_request missing");
                Ok(None)
            }
        }
    }

    fn verify_credentials(
        &self,
        user: &SipUser,
        method: &rsipstack::sip::Method,
        auth: &Authorization,
        raw_auth_header: &str,
    ) -> bool {
        let empty_string = "".to_string();
        let password = user.password.as_ref().unwrap_or(&empty_string);

        verify_digest(auth, password, method, raw_auth_header)
    }

    /// Check if a request is an in-dialog request (has To tag)
    fn is_in_dialog_request(&self, tx: &Transaction) -> bool {
        if let Ok(to_header) = tx.original.to_header() {
            if let Ok(typed_to) = to_header.typed() {
                // In-dialog requests have a tag parameter in the To header
                return typed_to
                    .params
                    .iter()
                    .any(|p| matches!(p, rsipstack::sip::Param::Tag(_)));
            }
        }
        false
    }

    /// Get the source address from the transaction
    fn get_source_addr(&self, tx: &Transaction) -> Option<rsipstack::transport::SipAddr> {
        tx.connection.as_ref().map(|conn| conn.get_addr().clone())
    }

    /// True when the request carried credentials (an Authorization or
    /// Proxy-Authorization header). Used to distinguish a genuine auth
    /// failure (credentials presented but rejected) from the routine first
    /// challenge every normal call receives before retrying with credentials.
    fn request_has_auth_credentials(tx: &Transaction) -> bool {
        tx.original.headers.iter().any(|h| {
            matches!(h, Header::Authorization(_) | Header::ProxyAuthorization(_))
        })
    }

    /// Persist a minimal call-record for an inbound INVITE that presented
    /// credentials which failed authentication (e.g. an unprovisioned user).
    /// Without this the attempt vanishes: auth aborts the module pipeline
    /// before the CallModule that normally emits CDRs. The record is keyed on
    /// the inbound SIP Call-ID — the same key a successful call uses — so if
    /// the provider later re-INVITEs with valid credentials, that call's CDR
    /// upserts (supersedes) this row.
    ///
    /// Caveat: this relies on the authenticated retry reusing the same Call-ID
    /// (which RFC 3261 requires for the same call attempt, and LiveKit/Vapi do).
    /// A UA that mints a fresh Call-ID per attempt would leave this row
    /// un-superseded — an orphaned but not incorrect "failed attempt".
    /// Source IP of the request, used as the rate-limit key. Requests whose
    /// source is a domain or unknown share a single catch-all bucket
    /// (`0.0.0.0`) so they remain bounded too.
    fn source_ip(&self, tx: &Transaction) -> IpAddr {
        use rsipstack::sip::uri::Host;
        match self.get_source_addr(tx).map(|a| a.addr.host) {
            Some(Host::IpAddr(ip)) => ip,
            _ => IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        }
    }

    async fn record_auth_rejection(
        &self,
        tx: &Transaction,
        status_code: &rsipstack::sip::StatusCode,
    ) {
        let Some(sender) = self.server.callrecord_sender.as_ref() else {
            return;
        };

        // Throttle per source IP so a flood of unique-Call-ID INVITEs can't
        // amplify into unbounded CDR rows. The 407 is still sent regardless.
        let source_ip = self.source_ip(tx);
        if !self.rejection_limiter.allow(source_ip).await {
            debug!(%source_ip, "auth-rejection CDR suppressed: per-source rate limit");
            return;
        }

        let call_id = tx
            .original
            .call_id_header()
            .map(|h| h.value().to_string())
            .unwrap_or_default();
        if call_id.is_empty() {
            return;
        }

        let caller = tx
            .original
            .from_header()
            .ok()
            .and_then(|h| h.uri().ok())
            .map(|u| u.to_string())
            .unwrap_or_default();
        let callee = tx
            .original
            .to_header()
            .ok()
            .and_then(|h| h.uri().ok())
            .map(|u| u.to_string())
            .unwrap_or_default();
        let endpoint = self.get_source_addr(tx).map(|a| a.addr.to_string());
        let record =
            build_auth_rejection_record(call_id, &caller, &callee, endpoint, status_code, Utc::now());

        if sender.send(record).is_err() {
            debug!("auth-rejection CDR not recorded: call_record channel closed");
        }
    }

    /// Extract cache key (call_id, from_tag) from a transaction.
    /// Uses from_tag because it is stable throughout the dialog lifetime,
    /// unlike to_tag which is absent in the initial INVITE.
    fn extract_auth_cache_key(&self, tx: &Transaction) -> Option<AuthCacheKey> {
        let call_id = tx.original.call_id_header().ok()?.value().to_string();
        let from_tag = tx
            .original
            .from_header()
            .ok()?
            .tag()
            .ok()??
            .value()
            .to_string();
        Some((call_id, from_tag))
    }

    pub fn create_proxy_auth_challenge(&self, realm: &str) -> Result<ProxyAuthenticate> {
        let nonce = rsipstack::transaction::random_text(16);
        let proxy_auth = ProxyAuthenticate::new(format!(
            r#"Digest realm="{}", nonce="{}", algorithm=MD5"#,
            realm, nonce
        ));
        Ok(proxy_auth)
    }

    pub fn create_www_auth_challenge(&self, realm: &str) -> Result<WwwAuthenticate> {
        let nonce = rsipstack::transaction::random_text(16);
        let www_auth = WwwAuthenticate::new(format!(
            r#"Digest realm="{}", nonce="{}", algorithm=MD5"#,
            realm, nonce
        ));
        Ok(www_auth)
    }

    fn is_cluster_peer_source(&self, tx: &Transaction) -> bool {
        let Some(source) = self.get_source_addr(tx) else {
            return false;
        };
        let source_ip: IpAddr = source.addr.host.clone().try_into().ok().unwrap_or_else(|| {
            // Host isn't an IP (domain/invalid) — treat as non-cluster source.
            IpAddr::from([0, 0, 0, 0])
        });
        if source_ip == IpAddr::from([0, 0, 0, 0]) {
            return false;
        }

        self.server
            .cluster_peer_ips
            .iter()
            .any(|peer_ip| *peer_ip == source_ip)
    }
}

#[async_trait]
impl ProxyModule for AuthModule {
    fn name(&self) -> &str {
        "auth"
    }

    fn allow_methods(&self) -> Vec<rsipstack::sip::Method> {
        vec![
            rsipstack::sip::Method::Invite,
            rsipstack::sip::Method::Register,
            rsipstack::sip::Method::Bye,
            rsipstack::sip::Method::Options,
            rsipstack::sip::Method::Ack,
            rsipstack::sip::Method::Cancel,
            rsipstack::sip::Method::Update,
            rsipstack::sip::Method::Refer,
            rsipstack::sip::Method::Notify,
            rsipstack::sip::Method::Message,
            rsipstack::sip::Method::Info,
            rsipstack::sip::Method::Subscribe,
            rsipstack::sip::Method::Publish,
        ]
    }

    async fn on_start(&mut self) -> Result<()> {
        debug!("Auth module started");
        Ok(())
    }

    async fn on_stop(&self) -> Result<()> {
        debug!("Auth module stopped");
        Ok(())
    }

    async fn on_transaction_begin(
        &self,
        _token: CancellationToken,
        tx: &mut Transaction,
        cookie: TransactionCookie,
    ) -> Result<ProxyAction> {
        let tx_user = SipUser::try_from(&*tx)?;
        let source = tx_user
            .destination
            .as_ref()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Check if this is an in-dialog request and if we can skip authentication via cache
        if let Some(ref cache) = self.dialog_auth_cache {
            if self.is_in_dialog_request(tx) {
                if let (Some(cache_key), Some(source_addr)) =
                    (self.extract_auth_cache_key(tx), self.get_source_addr(tx))
                {
                    trace!(
                        call_id = %cache_key.0,
                        from_tag = %cache_key.1,
                        method = %tx.original.method,
                        %source,
                        "Checking in-dialog request against auth cache"
                    );

                    if cache.is_authenticated(&cache_key, &source_addr).await {
                        debug!(
                            call_id = %cache_key.0,
                            from_tag = %cache_key.1,
                            method = %tx.original.method,
                            %source,
                            "In-dialog request authenticated via cache, skipping auth"
                        );
                        cookie.set_user(tx_user.clone());
                        return Ok(ProxyAction::Continue);
                    }
                }
            }
        }

        // Only authenticate INVITE and REGISTER requests (out-of-dialog)
        if tx.original.method != rsipstack::sip::Method::Invite
            && tx.original.method != rsipstack::sip::Method::Register
        {
            return Ok(ProxyAction::Continue);
        }

        for backend in self.server.auth_backend.iter() {
            match backend.authenticate(&tx.original, &cookie).await {
                Ok(Some(mut user)) => {
                    user.merge_with(&tx_user);
                    cookie.set_user(user);

                    // Cache the authenticated dialog for in-dialog requests
                    if let (Some(ref cache), Some(source_addr)) =
                        (self.dialog_auth_cache.as_ref(), self.get_source_addr(tx))
                    {
                        if let Some(cache_key) = self.extract_auth_cache_key(tx) {
                            cache.put(cache_key, source_addr).await;
                        }
                    }

                    return Ok(ProxyAction::Continue);
                }
                Err(e) => {
                    if matches!(e, AuthError::SpamDetected) {
                        cookie.mark_as_spam(SpamResult::Spam);
                    }
                    info!(error=%e, key = %tx.key, %source, "auth_backend authenticate failed");
                }
                _ => {}
            }
        }

        if cookie.is_spam() {
            return Ok(ProxyAction::Abort);
        }

        let is_from_trunk = cookie.get_extension::<TrunkContext>().is_some();
        if is_from_trunk {
            cookie.set_user(tx_user.clone());
            return Ok(ProxyAction::Continue);
        }

        if self.is_cluster_peer_source(tx) {
            let request_host = tx.original.uri().host().to_string();
            if self.server.is_same_realm(&request_host).await {
                cookie.set_user(tx_user.clone());
                return Ok(ProxyAction::Continue);
            }
        }

        match self.authenticate_request(tx).await {
            Ok(authenticated) => {
                if let Some(user) = authenticated {
                    cookie.set_user(user);

                    // Cache the authenticated dialog for in-dialog requests
                    if let (Some(ref cache), Some(source_addr)) =
                        (self.dialog_auth_cache.as_ref(), self.get_source_addr(tx))
                    {
                        if let Some(cache_key) = self.extract_auth_cache_key(tx) {
                            cache.put(cache_key, source_addr).await;
                        }
                    }

                    return Ok(ProxyAction::Continue);
                }

                let to_header = tx.original.to_header()?.uri()?;
                let callee_user = to_header.user().unwrap_or("");
                let callee_realm = to_header.host().to_string();

                if tx.original.method == rsipstack::sip::Method::Invite {
                    match self
                        .server
                        .user_backend
                        .get_user(callee_user, Some(&callee_realm), Some(&tx.original))
                        .await
                    {
                        Ok(Some(callee_profile)) if callee_profile.allow_guest_calls => {
                            info!(
                                caller = %tx_user.username,
                                extension = %callee_user,
                                %source,
                                "Allowing guest call without authentication"
                            );
                            cookie.set_user(tx_user.clone());
                            if let Some(display_name) = callee_profile.display_name {
                                cookie.insert_extension(CalleeDisplayName(display_name));
                            }
                            return Ok(ProxyAction::Continue);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            info!(
                                extension = %callee_user,
                                error = %e,
                                %source,
                                "Failed to evaluate guest call permission"
                            );
                        }
                    }
                }

                let from_uri = tx.original.from_header()?.uri()?;
                let request_host = tx.original.uri().host().to_string();
                let realm = self.server.proxy_config.select_realm(request_host.as_str());

                if self.server.proxy_config.ensure_user.unwrap_or_default() {
                    match self
                        .server
                        .user_backend
                        .get_user(
                            from_uri.user().unwrap_or(""),
                            Some(&realm),
                            Some(&tx.original),
                        )
                        .await
                    {
                        Ok(Some(_)) => {}
                        _ => {
                            info!(
                                from = %from_uri,
                                %source,
                                "User not found, don't send authentication challenge"
                            );
                            cookie.mark_as_spam(SpamResult::Spam);
                            return Ok(ProxyAction::Abort);
                        }
                    };
                }

                let (status_code, headers) =
                    if tx.original.method == rsipstack::sip::Method::Register {
                        let www_auth = self.create_www_auth_challenge(&realm)?;
                        (
                            rsipstack::sip::StatusCode::Unauthorized,
                            vec![Header::WwwAuthenticate(www_auth)],
                        )
                    } else {
                        let www_auth = self.create_www_auth_challenge(&realm)?;
                        let proxy_auth = self.create_proxy_auth_challenge(&realm)?;
                        (
                            rsipstack::sip::StatusCode::ProxyAuthenticationRequired,
                            vec![
                                Header::WwwAuthenticate(www_auth),
                                Header::ProxyAuthenticate(proxy_auth),
                            ],
                        )
                    };

                info!(
                    from = from_uri.to_string(),
                    realm = realm,
                    status = %status_code,
                    %source,
                    "Authentication failed, sending challenge"
                );
                // Record a CDR for INVITEs that presented credentials which
                // failed (e.g. an unprovisioned user). The routine no-creds
                // first challenge is skipped — that call still succeeds on its
                // authenticated re-INVITE and is logged normally.
                if tx.original.method == rsipstack::sip::Method::Invite
                    && Self::request_has_auth_credentials(tx)
                {
                    self.record_auth_rejection(tx, &status_code).await;
                }
                tx.reply_with(status_code, headers, None).await.ok();
                Ok(ProxyAction::Abort)
            }
            Err(e) => {
                info!(error=%e, key = %tx.key, %source, "Authentication error");
                if matches!(e, AuthError::SpamDetected) {
                    cookie.mark_as_spam(SpamResult::Spam);
                }
                Err(anyhow::anyhow!("Authentication error: {}", e))
            }
        }
    }
}

/// Build the minimal call-record emitted when an inbound INVITE fails
/// authentication. Pure (no I/O) so it can be unit-tested. `call_id` is the
/// inbound SIP Call-ID, which a later successful re-INVITE shares — letting
/// the success CDR supersede this row via the upsert keyed on `call_id`.
fn build_auth_rejection_record(
    call_id: String,
    caller: &str,
    callee: &str,
    endpoint: Option<String>,
    status_code: &rsipstack::sip::StatusCode,
    now: chrono::DateTime<Utc>,
) -> CallRecord {
    let code = status_code.code();
    let reason = status_code.text().to_string();
    CallRecord {
        call_id,
        start_time: now,
        ring_time: None,
        answer_time: None,
        end_time: now,
        caller: caller.to_string(),
        callee: callee.to_string(),
        status_code: code,
        hangup_reason: Some(CallRecordHangupReason::Rejected),
        hangup_messages: vec![CallRecordHangupMessage {
            code,
            reason: Some(reason.clone()),
            target: Some("auth".to_string()),
            endpoint,
        }],
        recorder: vec![],
        sip_leg_roles: HashMap::new(),
        leg_timeline: crate::callrecord::LegTimeline::default(),
        details: CallDetails {
            direction: "inbound".to_string(),
            status: "failed".to_string(),
            from_number: extract_sip_username(caller),
            to_number: extract_sip_username(callee),
            last_error: Some(CallRecordLastError {
                code,
                reason: Some(reason),
            }),
            ..Default::default()
        },
        extensions: http::Extensions::new(),
    }
}

#[cfg(test)]
mod auth_rejection_tests {
    use super::*;

    #[test]
    fn auth_rejection_record_has_leg_origin_and_endpoint() {
        let rec = build_auth_rejection_record(
            "call-abc@provider".to_string(),
            "sip:+918071539101@44.238.177.138:5060",
            "sip:+919311429006@sip-lb1.unpod.tel",
            Some("44.238.177.138:5060".to_string()),
            &rsipstack::sip::StatusCode::ProxyAuthenticationRequired,
            Utc::now(),
        );

        assert_eq!(rec.call_id, "call-abc@provider");
        assert_eq!(rec.status_code, 407);
        assert_eq!(rec.details.direction, "inbound");
        assert_eq!(rec.details.status, "failed");
        assert_eq!(rec.details.from_number.as_deref(), Some("+918071539101"));
        assert_eq!(rec.details.to_number.as_deref(), Some("+919311429006"));
        assert!(matches!(
            rec.hangup_reason,
            Some(CallRecordHangupReason::Rejected)
        ));
        assert_eq!(rec.answer_time, None, "auth-rejected call never answered");

        let msg = rec.hangup_messages.first().expect("one hangup message");
        assert_eq!(msg.code, 407);
        assert_eq!(msg.target.as_deref(), Some("auth"));
        assert_eq!(msg.endpoint.as_deref(), Some("44.238.177.138:5060"));
        assert_eq!(
            rec.details.last_error.as_ref().map(|e| e.code),
            Some(407)
        );
    }
}
