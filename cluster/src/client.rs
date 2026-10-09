//! The [`AeronCluster`] client — owns the Aeron transport and drives the
//! full SBE session handshake. Java-parity entry point for the **Ergo Aeron
//! Cluster** client (experimental prototype on `rusteron-client`):
//!
//! → [`cluster-test-harness/tests/connect_to_cluster.rs`](https://github.com/mimran1980/ergon/blob/main/cluster-test-harness/tests/connect_to_cluster.rs)
//!
//! Hot path: [`AeronCluster::try_claim`] writes SessionMessageHeader via ergo-sbe
//! into the claim; fill app payload then [`ClusterClaim::commit`].

use std::ffi::{CStr, CString};
use std::time::{Duration, Instant};

use rusteron_client::bindings::aeron_controlled_fragment_handler_action_en as AeronAction;
use rusteron_client::{
    Aeron, AeronAsyncAddExclusivePublication, AeronAsyncAddSubscription, AeronClaim, AeronContext,
    AeronControlledFragmentClosureAssembler, AeronExclusivePublication, AeronFragmentClosureAssembler, AeronImage,
    AeronSubscription, Handlers, cformat,
};

use crate::uri;

use crate::codecs::session::{
    AdminRequestEncoder, AdminRequestFixedFields, AdminRequestType, ChallengeResponseEncoder,
    ChallengeResponseFixedFields, SessionCloseRequestEncoder, SessionConnectRequestEncoder,
    SessionConnectRequestFixedFields, SessionKeepAliveEncoder, SessionMessageHeaderEncoder,
};
/// Re-offer interval for SessionConnectRequest — roughly `message_timeout / 4`,
/// clamped to [50, 1000] ms (mirrors Java `AeronCluster.AsyncConnect`).
fn connect_reoffer_interval_ms(message_timeout_ms: u64) -> u64 {
    (message_timeout_ms / 4).clamp(50, 1_000)
}

/// Create a deadline from now + `timeout_ms` milliseconds, or return
/// [`ClusterError::InvalidTimeout`] if the timeout is zero or would overflow
/// [`Instant`].
fn checked_deadline(phase: &'static str, timeout_ms: u64) -> Result<Instant, ClusterError> {
    if timeout_ms == 0 {
        return Err(ClusterError::InvalidTimeout {
            phase,
            reason: "timeout must be positive",
        });
    }
    Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or(ClusterError::InvalidTimeout {
            phase,
            reason: "deadline exceeds Instant::MAX",
        })
}

use crate::controlled::{ControlledEgressAdapter, ControlledEgressListener, ControlledPollAction};
use crate::egress::{EgressAdapter, EgressListener};
use crate::error::ClusterError;
use crate::error::PublicationFailure;
use crate::state::SessionState;

/// Populate the `clientInfo` field in `SessionConnectRequest` matching
/// Java's `"name=<clientName> <versionInfo>"` pattern.
fn client_info_bytes() -> Vec<u8> {
    format!("name={} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")).into_bytes()
}

/// Header-inclusive SessionMessageHeader (prefer generated `ENCODED_LENGTH`).
const MSG_HDR_TOTAL: usize = SessionMessageHeaderEncoder::ENCODED_LENGTH;

/// The ingress publication's `(max_message_length, max_payload_length)`.
/// Fails closed: a limit of 0 would read as a real limit to callers sizing
/// messages from [`AeronCluster::ingress_max_message_length`].
fn publication_limits(publication: &AeronExclusivePublication) -> Result<(usize, usize), ClusterError> {
    let constants = publication
        .get_constants()
        .map_err(|e| ClusterError::aeron("ingress_constants", e))?;
    Ok((constants.max_message_length, constants.max_payload_length))
}

/// Map [`ControlledPollAction`] to Aeron's C enum.
fn to_aeron_action(action: ControlledPollAction) -> AeronAction {
    match action {
        ControlledPollAction::Continue => AeronAction::AERON_ACTION_CONTINUE,
        ControlledPollAction::Abort => AeronAction::AERON_ACTION_ABORT,
        ControlledPollAction::Break => AeronAction::AERON_ACTION_BREAK,
        ControlledPollAction::Commit => AeronAction::AERON_ACTION_COMMIT,
    }
}

/// The egress image a session is bound to: the one that delivered its
/// SessionEvent(OK) or latest NewLeaderEvent (Java `AeronCluster.egressImage`).
/// While a failover is in progress the old leader's image lingers beside the
/// new one, so only this image's closure means the session lost its leader.
struct EgressImage {
    image: AeronImage,
    correlation_id: i64,
    session_id: i32,
}

impl EgressImage {
    /// Retain the image for the session id of the fragment's header, or `None`
    /// when that image is already gone.
    fn bind(egress: &AeronSubscription, session_id: Option<i32>) -> Option<Self> {
        let session_id = session_id?;
        let image = egress.image_by_session_id(session_id)?;
        let correlation_id = image.get_constants().ok()?.correlation_id();
        Some(Self {
            image,
            correlation_id,
            session_id,
        })
    }
}

/// A NewLeaderEvent for this session, applied once the poll returns. The last
/// one in a poll wins, as Java applies each in order.
struct NewLeader {
    leadership_term_id: i64,
    leader_member_id: i32,
    ingress_endpoints: String,
    /// Session id of the egress image that delivered the event.
    image_session_id: Option<i32>,
}

/// The new leader's ingress publication while Aeron adds it and it connects.
/// The session adopts the new term and leader only once it has connected.
struct PendingIngress {
    publication: AeronAsyncAddExclusivePublication,
    added: Option<AeronExclusivePublication>,
    leadership_term_id: i64,
    leader_member_id: i32,
}

/// Context for the dispatch function — set up before each poll cycle.
struct PollCtx<'a, L: EgressListener> {
    adapter: &'a mut EgressAdapter<L>,
    new_leader: &'a mut Option<NewLeader>,
    decode_err: &'a mut Option<ClusterError>,
    expected_session_id: i64,
    session_closed: &'a mut bool,
}

struct ControlledPollCtx<'a, L: ControlledEgressListener> {
    adapter: &'a mut ControlledEgressAdapter<L>,
    new_leader: &'a mut Option<NewLeader>,
    decode_err: &'a mut Option<ClusterError>,
    expected_session_id: i64,
    session_closed: &'a mut bool,
}

fn dispatch_regular<L: EgressListener>(ctx: &mut PollCtx<L>, data: &[u8], hdr: rusteron_client::AeronHeader) {
    // Decode once — the Fragment is used for state tracking AND listener dispatch.
    let frag = match crate::fragment::Fragment::decode(data) {
        Ok(Some(f)) => f,
        Ok(None) => return, // unknown template — forward-compat, not an error
        Err(e) => {
            if ctx.decode_err.is_none() {
                *ctx.decode_err = Some(e);
            }
            return;
        }
    };
    // State tracking — no second decode
    match &frag {
        crate::fragment::Fragment::NewLeader {
            leadership_term_id,
            leader_member_id,
            ingress_endpoints,
            cluster_session_id,
        } if *cluster_session_id == ctx.expected_session_id => {
            *ctx.new_leader = Some(NewLeader {
                leadership_term_id: *leadership_term_id,
                leader_member_id: *leader_member_id,
                ingress_endpoints: ingress_endpoints.to_string(),
                image_session_id: hdr.session_id(),
            });
        }
        crate::fragment::Fragment::SessionEvent {
            code: crate::codecs::session::EventCode::CLOSED,
            cluster_session_id,
            ..
        } if *cluster_session_id == ctx.expected_session_id => {
            *ctx.session_closed = true;
        }
        _ => {}
    }
    // Pass the already-decoded fragment — no second decode
    if let Err(e) = ctx.adapter.dispatch_fragment(frag)
        && ctx.decode_err.is_none()
    {
        *ctx.decode_err = Some(e);
    }
}

fn dispatch_controlled<L: ControlledEgressListener>(
    ctx: &mut ControlledPollCtx<L>,
    data: &[u8],
    hdr: rusteron_client::AeronHeader,
) -> AeronAction {
    // Decode once — shared between state tracking and listener dispatch.
    let frag = match crate::fragment::Fragment::decode(data) {
        Ok(Some(f)) => f,
        Ok(None) => return AeronAction::AERON_ACTION_CONTINUE, // unknown template
        Err(e) => {
            if ctx.decode_err.is_none() {
                *ctx.decode_err = Some(e);
            }
            return AeronAction::AERON_ACTION_ABORT;
        }
    };
    // State tracking — no second decode
    match &frag {
        crate::fragment::Fragment::NewLeader {
            leadership_term_id,
            leader_member_id,
            ingress_endpoints,
            cluster_session_id,
        } if *cluster_session_id == ctx.expected_session_id => {
            *ctx.new_leader = Some(NewLeader {
                leadership_term_id: *leadership_term_id,
                leader_member_id: *leader_member_id,
                ingress_endpoints: ingress_endpoints.to_string(),
                image_session_id: hdr.session_id(),
            });
        }
        crate::fragment::Fragment::SessionEvent {
            code: crate::codecs::session::EventCode::CLOSED,
            cluster_session_id,
            ..
        } if *cluster_session_id == ctx.expected_session_id => {
            *ctx.session_closed = true;
        }
        _ => {}
    }
    match ctx.adapter.dispatch_fragment(frag) {
        Ok(action) => to_aeron_action(action),
        Err(e) => {
            if ctx.decode_err.is_none() {
                *ctx.decode_err = Some(e);
            }
            AeronAction::AERON_ACTION_ABORT
        }
    }
}

/// Map a raw offer return: `Ok(pos)` if `r > 0`, else typed publication error.
#[inline]
fn offer_result(context: &'static str, r: i64) -> Result<i64, ClusterError> {
    if r > 0 {
        Ok(r)
    } else {
        Err(ClusterError::from_offer_raw(context, r))
    }
}

#[inline]
fn tolerate_retryable_keep_alive(result: Result<(), ClusterError>) -> Result<(), ClusterError> {
    match result {
        Err(error) if error.is_retryable() => Ok(()),
        result => result,
    }
}

/// A connected Aeron cluster client. Owns the Aeron client, ingress
/// exclusive publication, and egress subscription.
pub struct AeronCluster {
    /// Declared first so the retained image is released before `egress`.
    egress_image: Option<EgressImage>,
    pending_ingress: Option<PendingIngress>,
    _aeron: Aeron,
    ingress: AeronExclusivePublication,
    egress: AeronSubscription,
    cluster_session_id: i64,
    leadership_term_id: i64,
    leader_member_id: i32,
    state: SessionState,
    /// Wall-clock when the session entered `AwaitingNewLeader` (None while
    /// Connected). Drives the `new_leader_timeout` enforced by
    /// [`Self::poll_state_changes`].
    awaiting_leader_since: Option<Instant>,
    /// Configured ingress stream id, retained so a `NewLeaderEvent`
    /// reconnect uses the same stream the session was established on.
    ingress_stream_id: i32,
    /// Configured ingress channel, the template a new leader's publication
    /// keeps every setting of but the endpoint.
    ingress_channel: Option<CString>,
    /// `newLeaderTimeout` from the builder (ms). Enforced by
    /// [`Self::poll_state_changes`].
    new_leader_timeout_ms: u64,
    /// `messageTimeout` from the builder (ms): how long a new leader's
    /// ingress publication may take to connect.
    message_timeout_ms: u64,
    /// When the last keep-alive was sent (drives auto-scheduling).
    last_keep_alive: Instant,
    /// Interval between keep-alive sends (derived from message_timeout_ms).
    keep_alive_interval_ms: u64,
    /// Cached from the ingress publication's constants — the maximum total
    /// frame length (header + payload) that Aeron will accept in one offer.
    max_message_length: usize,
    /// Cached from the ingress publication's constants — the maximum payload
    /// that fits in a single claim (excludes the 32-byte session header).
    max_payload_length: usize,
    /// rusteron fragment assembler for regular egress.
    regular_assembler: AeronFragmentClosureAssembler,
    /// rusteron fragment assembler for controlled egress.
    controlled_assembler: AeronControlledFragmentClosureAssembler,
}

impl Drop for AeronCluster {
    fn drop(&mut self) {
        if self.state != SessionState::Closed && self.state != SessionState::PendingClose {
            let _ = self.close();
        }
    }
}

impl AeronCluster {
    /// Bytes of SessionMessageHeader every application message carries ahead
    /// of its payload (Java `AeronCluster.SESSION_HEADER_LENGTH`).
    pub const SESSION_HEADER_LENGTH: usize = MSG_HDR_TOTAL;

    /// Connect to a cluster whose media driver lives at `aeron_dir`.
    /// Performs the full SBE handshake: SessionConnectRequest → poll
    /// egress for SessionEvent(OK) → extract session/leadership IDs.
    pub fn connect(builder: &crate::SessionBuilder, aeron_dir: &str) -> Result<Self, ClusterError> {
        builder.validate()?;

        let dir_cstr = cformat!("{aeron_dir}");
        let ctx = AeronContext::new().map_err(|e| ClusterError::aeron("AeronContext", e))?;
        ctx.set_dir(&dir_cstr).map_err(|e| ClusterError::aeron("set_dir", e))?;
        let aeron = Aeron::new(&ctx).map_err(|e| ClusterError::aeron("Aeron::new", e))?;
        aeron.start().map_err(|e| ClusterError::aeron("Aeron::start", e))?;

        // Builder already holds CString; pass &CStr to rusteron with no second alloc.
        let egress_c = builder.egress_for_aeron()?;
        let ingress_c = builder.resolve_initial_ingress_for_aeron()?;

        let egress = aeron
            .add_subscription(
                egress_c,
                builder.egress_stream_id,
                Handlers::NONE,
                Handlers::NONE,
                Duration::from_secs(5),
            )
            .map_err(|e| ClusterError::aeron("add_subscription", e))?;

        let ingress = aeron
            .add_exclusive_publication(&ingress_c, builder.ingress_stream_id, Duration::from_secs(5))
            .map_err(|e| ClusterError::aeron("add_exclusive_publication", e))?;
        let (max_message_length, max_payload_length) = publication_limits(&ingress)?;

        let mut client = Self {
            egress_image: None,
            pending_ingress: None,
            _aeron: aeron,
            ingress,
            egress,
            cluster_session_id: -1,
            leadership_term_id: -1,
            leader_member_id: -1,
            state: SessionState::Closed,
            awaiting_leader_since: None,
            ingress_stream_id: builder.ingress_stream_id,
            ingress_channel: builder.ingress_channel_c_str().map(CStr::to_owned),
            new_leader_timeout_ms: builder.new_leader_timeout_ms,
            message_timeout_ms: builder.message_timeout_ms,
            last_keep_alive: Instant::now(),
            keep_alive_interval_ms: connect_reoffer_interval_ms(builder.message_timeout_ms),
            max_message_length,
            max_payload_length,
            regular_assembler: AeronFragmentClosureAssembler::new()
                .map_err(|e| ClusterError::aeron("AeronFragmentClosureAssembler", e))?,
            controlled_assembler: AeronControlledFragmentClosureAssembler::new()
                .map_err(|e| ClusterError::aeron("AeronControlledFragmentClosureAssembler", e))?,
        };
        client.handshake(builder)?;
        Ok(client)
    }

    /// Send the SessionConnectRequest and poll egress for the result,
    /// handling challenge-response and leader redirect along the way.
    ///
    /// While waiting, re-offers the connect request on a cadence of
    /// [`connect_reoffer_interval_ms`] so a first offer that lands on a
    /// pre-election / silent non-leader peer does not stall until timeout.
    fn handshake(&mut self, builder: &crate::SessionBuilder) -> Result<(), ClusterError> {
        let creds: Vec<u8> = builder
            .credentials
            .as_ref()
            .and_then(|c| c.encoded_credentials().map(|cow| cow.into_owned()))
            .unwrap_or_default();

        self.send_connect_request(builder, &creds)?;
        let mut last_offer = Instant::now();
        let reoffer_ms = connect_reoffer_interval_ms(builder.message_timeout_ms);

        let deadline = checked_deadline("connect", builder.message_timeout_ms)?;
        let idle_clone = builder.idle.clone();
        let mut captured: Option<crate::poller::EgressEvent> = None;
        let mut image_session_id: Option<i32> = None;
        while Instant::now() < deadline {
            // Fail-closed: propagate Aeron poll and decode errors rather than
            // masking them as a later connect timeout.
            let mut decode_err: Option<ClusterError> = None;
            self.egress
                .poll_fn(
                    |data, hdr| {
                        if captured.is_none() && decode_err.is_none() {
                            match crate::poller::parse_event(data) {
                                Ok(e) => {
                                    captured = Some(e);
                                    image_session_id = hdr.session_id();
                                }
                                Err(e) => decode_err = Some(e),
                            }
                        }
                    },
                    1,
                )
                .map_err(|e| ClusterError::aeron("connect_poll", e))?;
            if let Some(e) = decode_err {
                return Err(e);
            }

            match captured.take() {
                Some(crate::poller::EgressEvent::SessionEvent {
                    code,
                    cluster_session_id,
                    leadership_term_id,
                    leader_member_id,
                    detail,
                    ..
                }) => {
                    use crate::codecs::session::EventCode;
                    match code {
                        EventCode::OK => {
                            self.cluster_session_id = cluster_session_id;
                            self.leadership_term_id = leadership_term_id;
                            self.leader_member_id = leader_member_id;
                            self.egress_image = EgressImage::bind(&self.egress, image_session_id);
                            self.state = SessionState::Connected;
                            return Ok(());
                        }
                        EventCode::AUTHENTICATIONREJECTED => {
                            return Err(ClusterError::AuthRejected);
                        }
                        EventCode::REDIRECT => {
                            // Resolve the LEADER's endpoint by member id — the
                            // detail lists all members in id order, so a
                            // position-based parse would redirect back to the
                            // follower we just asked (an infinite loop).
                            let ep = crate::poller::parse_leader_endpoint(&detail, leader_member_id)?
                                .ok_or_else(|| ClusterError::ReconnectFailed {
                                    reason: format!(
                                        "connect redirect listed no endpoint for leader member {leader_member_id}: {detail}"
                                    ),
                                })?;
                            self.reconnect_ingress(builder, &ep)?;
                            self.send_connect_request(builder, &creds)?;
                            last_offer = Instant::now();
                        }
                        _ => { /* keep polling */ }
                    }
                }
                Some(crate::poller::EgressEvent::Challenge {
                    correlation_id,
                    cluster_session_id,
                    encoded_challenge,
                }) => {
                    // Respond with credentials from the supplier.
                    let resp: Vec<u8> = builder
                        .credentials
                        .as_ref()
                        .and_then(|c| c.on_challenge(&encoded_challenge).map(|cow| cow.into_owned()))
                        .unwrap_or_default();
                    self.send_challenge_response(correlation_id, cluster_session_id, &resp)?;
                    // keep polling for the resulting SessionEvent
                }
                _ => {
                    // No event: re-offer connect if the interval elapsed so a
                    // pre-election peer that neither leads nor redirects does
                    // not burn the full timeout on a single silent offer.
                    if last_offer.elapsed() >= Duration::from_millis(reoffer_ms) {
                        match self.try_offer_connect_request(builder, &creds) {
                            Ok(true) => {}
                            Ok(false) => {}
                            Err(e) if e.is_retryable() => { /* keep polling */ }
                            Err(e) => return Err(e), // fatal — surface immediately
                        }
                        last_offer = Instant::now();
                    }
                }
            }
            if let Some(ref idle) = idle_clone {
                idle.lock().expect("idle mutex poisoned").idle(0);
            } else {
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        Err(ClusterError::ConnectFailed {
            reason: "timeout waiting for SessionEvent(OK)".into(),
        })
    }

    /// Encode SessionConnectRequest and retry offer until it lands or timeout.
    fn send_connect_request(
        &mut self,
        builder: &crate::SessionBuilder,
        credentials: &[u8],
    ) -> Result<(), ClusterError> {
        let buf = Self::encode_connect_request(builder, credentials)?;
        let deadline = checked_deadline("connect_request", builder.message_timeout_ms)?;
        let idle_clone = builder.idle.clone();
        while Instant::now() < deadline {
            if self.ingress.offer_raw(&buf, Handlers::NONE) > 0 {
                return Ok(());
            }
            if let Some(ref idle) = idle_clone {
                idle.lock().expect("idle mutex poisoned").idle(0);
            } else {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Err(ClusterError::ConnectFailed {
            reason: "connect request offer timed out".into(),
        })
    }

    /// Best-effort single offer of a fresh SessionConnectRequest (re-offer path).
    fn try_offer_connect_request(
        &mut self,
        builder: &crate::SessionBuilder,
        credentials: &[u8],
    ) -> Result<bool, ClusterError> {
        let buf = Self::encode_connect_request(builder, credentials)?;
        Ok(self.ingress.offer_raw(&buf, Handlers::NONE) > 0)
    }

    fn encode_connect_request(builder: &crate::SessionBuilder, credentials: &[u8]) -> Result<Vec<u8>, ClusterError> {
        let ch = builder.egress_channel_bytes();
        let info = client_info_bytes();
        let len = SessionConnectRequestEncoder::compute_encoded_length_with_message_header(
            ch.len(),
            credentials.len(),
            info.len(),
        );
        let mut buf = vec![0u8; len];
        let _ = SessionConnectRequestEncoder::wrap_and_apply_header(&mut buf, 0)
            .fixed(&SessionConnectRequestFixedFields {
                correlation_id: 0,
                response_stream_id: builder.egress_stream_id,
                version: Some(0),
            })
            .response_channel(ch)?
            .encoded_credentials(credentials)?
            .client_info(&client_info_bytes())?;
        Ok(buf)
    }

    fn send_challenge_response(
        &mut self,
        correlation_id: i64,
        cluster_session_id: i64,
        credentials: &[u8],
    ) -> Result<(), ClusterError> {
        let len = ChallengeResponseEncoder::compute_encoded_length_with_message_header(credentials.len());
        let mut buf = vec![0u8; len];
        let _ = ChallengeResponseEncoder::wrap_and_apply_header(&mut buf, 0)
            .fixed(&ChallengeResponseFixedFields {
                correlation_id,
                cluster_session_id,
            })
            .encoded_credentials(credentials)?;
        let r = self.ingress.offer_raw(&buf, Handlers::NONE);
        offer_result("challenge_response", r).map(|_| ())
    }

    /// Recreate the ingress publication pointed at the leader's endpoint
    /// (blocking connect's REDIRECT), keeping the configured channel settings.
    fn reconnect_ingress(&mut self, builder: &crate::SessionBuilder, endpoint: &str) -> Result<(), ClusterError> {
        let channel = uri::ingress_channel_for_endpoint(builder.ingress_channel_c_str(), endpoint)?;
        let ingress = self
            ._aeron
            .add_exclusive_publication(&channel, builder.ingress_stream_id, Duration::from_secs(5))
            .map_err(|e| ClusterError::reconnect(format!("ingress to {endpoint}: {e}")))?;
        let (max_message_length, max_payload_length) = publication_limits(&ingress)?;
        self.ingress = ingress;
        self.max_message_length = max_message_length;
        self.max_payload_length = max_payload_length;
        Ok(())
    }

    /// Publish an application message. Prepends the SessionMessageHeader
    /// (leadershipTermId + clusterSessionId + timestamp).
    ///
    /// Messages that fit in one term-buffer fragment use the zero-alloc
    /// [`Self::try_claim`] path; larger messages fall back to an assembled
    /// header+payload buffer via `offer_raw` (Aeron handles transport-level
    /// fragmentation automatically). Returns the stream **position** assigned
    /// by Aeron, not the payload length. Mirrors Java
    /// `AeronCluster.offer(buffer)`.
    pub fn offer(&mut self, payload: &[u8]) -> Result<i64, ClusterError> {
        if self.state != SessionState::Connected {
            return Err(ClusterError::NotConnected);
        }
        let total = MSG_HDR_TOTAL
            .checked_add(payload.len())
            .ok_or(ClusterError::PayloadTooLarge {
                operation: "offer",
                requested: payload.len(),
                maximum: self.max_message_length,
            })?;
        // Reject frames that exceed the publication's max message length
        // before making any Aeron call.
        if total > self.max_message_length {
            return Err(ClusterError::PayloadTooLarge {
                operation: "offer",
                requested: total,
                maximum: self.max_message_length,
            });
        }
        // Fast path: zero-alloc claim when the header+payload fits in one fragment.
        if total <= self.max_payload_length {
            return match self.try_claim(payload.len()) {
                Ok(mut claim) => {
                    claim.payload_mut().copy_from_slice(payload);
                    claim.commit().map_err(|e| self.track_publication_error(e))
                }
                Err(e) => Err(e),
            };
        }
        // Fallback: gather header + payload in two slices via offer_parts.
        // Aeron fragments automatically; no heap allocation, no payload copy.
        let mut header = [0u8; MSG_HDR_TOTAL];
        SessionMessageHeaderEncoder::wrap_and_apply_header(&mut header, 0)
            .leadership_term_id(self.leadership_term_id)
            .cluster_session_id(self.cluster_session_id)
            .timestamp(0);
        let r = self
            .ingress
            .offer_parts(&[&header, payload])
            .map_err(|e| ClusterError::from_offer_error("offer", e));
        self.track_ingress_publication_result_offer(r)
    }

    /// Publish a sub-range of application data — equivalent to Java
    /// `offer(DirectBuffer, int offset, int length)`. The slice
    /// `payload[offset..offset+length]` is bounds-checked then published
    /// via the allocation-free [`Self::offer`] path.
    pub fn offer_range(&mut self, payload: &[u8], offset: usize, length: usize) -> Result<i64, ClusterError> {
        let Some(end) = offset.checked_add(length) else {
            return Err(ClusterError::BufferTooSmall {
                needed: offset.saturating_add(length),
                actual: payload.len(),
            });
        };
        if end > payload.len() {
            return Err(ClusterError::BufferTooSmall {
                needed: end,
                actual: payload.len(),
            });
        }
        self.offer(&payload[offset..end])
    }

    /// Send a SessionKeepAlive to hold the session open.
    ///
    /// Encoded into a stack array sized to the generated
    /// [`SessionKeepAliveEncoder::ENCODED_LENGTH`] — no heap allocation.
    pub fn send_keep_alive(&mut self) -> Result<(), ClusterError> {
        let mut buf = [0u8; SessionKeepAliveEncoder::ENCODED_LENGTH];
        SessionKeepAliveEncoder::wrap_and_apply_header(&mut buf, 0)
            .leadership_term_id(self.leadership_term_id)
            .cluster_session_id(self.cluster_session_id);
        let r = self.ingress.offer_raw(&buf, Handlers::NONE);
        self.track_ingress_publication_result(r).map(|_| ())
    }

    /// Send keep-alive if the interval has elapsed since the last send.
    /// Called from `poll_egress` — mirrors Java's automatic session keep-alive.
    pub fn keep_alive_if_due(&mut self) -> Result<(), ClusterError> {
        let now = Instant::now();
        if self.state == SessionState::Connected
            && now.saturating_duration_since(self.last_keep_alive) >= Duration::from_millis(self.keep_alive_interval_ms)
        {
            self.send_keep_alive()?;
            self.last_keep_alive = now;
        }
        Ok(())
    }

    /// Classify an ingress publication result (raw offer/try_claim return
    /// or commit position). On a fatal sentinel — [`PublicationFailure::Closed`]
    /// or [`PublicationFailure::MaxPositionExceeded`] — the session is
    /// transitioned to `AwaitingNewLeader` so a subsequent
    /// `NewLeaderEvent` can reconnect. Java `trackIngressPublicationResult`
    /// analogue. Non-fatal retryable sentinels (NotConnected/BackPressured)
    /// are returned as an error but do not change session state.
    pub fn track_ingress_publication_result(&mut self, result: i64) -> Result<i64, ClusterError> {
        if result > 0 {
            return Ok(result);
        }
        let failure = PublicationFailure::from_raw(result);
        if matches!(
            failure,
            PublicationFailure::Closed | PublicationFailure::MaxPositionExceeded
        ) && self.state == SessionState::Connected
        {
            self.state = SessionState::AwaitingNewLeader;
            self.awaiting_leader_since = Some(Instant::now());
        }
        Err(ClusterError::Publication {
            failure,
            context: "track_ingress",
        })
    }

    /// Mirror [`Self::track_ingress_publication_result`] for the typed
    /// `ClusterError::Publication` path used by `offer` / `try_claim`: on a
    /// fatal sentinel (Closed / MaxPositionExceeded) transition a Connected
    /// session to `AwaitingNewLeader` so a `NewLeaderEvent` can recover it.
    /// Retryable sentinels (NotConnected / BackPressured / AdminAction) leave
    /// the session intact.
    fn track_publication_error(&mut self, err: ClusterError) -> ClusterError {
        if let ClusterError::Publication { failure, .. } = &err
            && self.state == SessionState::Connected
            && matches!(
                *failure,
                PublicationFailure::Closed | PublicationFailure::MaxPositionExceeded
            )
        {
            self.state = SessionState::AwaitingNewLeader;
            self.awaiting_leader_since = Some(Instant::now());
        }
        err
    }

    /// Track the result of an `offer_parts` call. Unlike `track_ingress_publication_result`,
    /// this takes a pre-mapped `Result<i64, ClusterError>`. The error is passed through
    /// the fatal-publication tracker so `Closed` / `MaxPositionExceeded` transition the
    /// session to `AwaitingNewLeader`.
    fn track_ingress_publication_result_offer(
        &mut self,
        result: Result<i64, ClusterError>,
    ) -> Result<i64, ClusterError> {
        match result {
            Ok(pos) => Ok(pos),
            Err(e) => {
                // Propagate fatal sentinels to the session state machine.
                if let ClusterError::Publication { failure, .. } = &e
                    && self.state == SessionState::Connected
                    && matches!(
                        *failure,
                        PublicationFailure::Closed | PublicationFailure::MaxPositionExceeded
                    )
                {
                    self.state = SessionState::AwaitingNewLeader;
                    self.awaiting_leader_since = Some(Instant::now());
                }
                Err(e)
            }
        }
    }

    /// Send an AdminRequest (e.g. snapshot) on the ingress publication.
    ///
    /// Responses arrive as admin events on the egress listener
    /// ([`crate::EgressListener::on_admin_response`]).
    pub fn send_admin_request(
        &mut self,
        correlation_id: i64,
        request_type: AdminRequestType,
        payload: &[u8],
    ) -> Result<i64, ClusterError> {
        if self.state != SessionState::Connected {
            return Err(ClusterError::NotConnected);
        }
        let len = AdminRequestEncoder::compute_encoded_length_with_message_header(payload.len());
        let mut buf = vec![0u8; len];
        let complete = AdminRequestEncoder::wrap_and_apply_header(&mut buf, 0)
            .fixed(&AdminRequestFixedFields {
                leadership_term_id: self.leadership_term_id,
                cluster_session_id: self.cluster_session_id,
                correlation_id,
                request_type,
            })
            .payload(payload)?;
        let bytes = complete.as_bytes_with_header();
        let r = self.ingress.offer_raw(bytes, Handlers::NONE);
        offer_result("admin_request", r)
    }

    /// Send a SessionCloseRequest and mark the session PendingClose.
    pub fn close(&mut self) -> Result<(), ClusterError> {
        if self.state == SessionState::Closed {
            return Err(ClusterError::SessionClosed);
        }
        let mut buf = [0u8; SessionCloseRequestEncoder::HEADER_LENGTH + SessionCloseRequestEncoder::BLOCK_LENGTH];
        SessionCloseRequestEncoder::wrap_and_apply_header(&mut buf, 0)
            .leadership_term_id(self.leadership_term_id)
            .cluster_session_id(self.cluster_session_id);
        // Local close always proceeds: the SessionCloseRequest is an advisory
        // notification (mirrors Java AeronCluster.close()). A failed offer must
        // not block local cleanup or leave the session half-closed, so set the
        // state first and send best-effort.
        self.state = SessionState::PendingClose;
        self.pending_ingress = None;
        let _ = self.ingress.offer_raw(&buf, Handlers::NONE);
        Ok(())
    }

    /// Poll egress and dispatch decoded messages through `adapter`.
    /// Fragments are reassembled into complete logical messages before
    /// decoding. Application messages not matching the connected
    /// `cluster_session_id` are silently dropped.
    ///
    /// Handles `NewLeaderEvent` internally: binds the session to the egress
    /// image that delivered it and starts adding an ingress publication for
    /// the new leader without waiting; [`Self::poll_state_changes`] adopts the
    /// new term and leader once that publication has connected. Returns
    /// fragments polled.
    pub fn poll_egress<L: EgressListener>(
        &mut self,
        adapter: &mut EgressAdapter<L>,
        limit: usize,
    ) -> Result<i32, ClusterError> {
        adapter.set_expected_session_id(self.cluster_session_id);
        tolerate_retryable_keep_alive(self.keep_alive_if_due())?;
        let mut new_leader: Option<NewLeader> = None;
        let mut decode_err: Option<ClusterError> = None;
        let mut session_closed = false;
        let mut ctx = PollCtx {
            adapter,
            new_leader: &mut new_leader,
            decode_err: &mut decode_err,
            expected_session_id: self.cluster_session_id,
            session_closed: &mut session_closed,
        };
        let n = self
            .regular_assembler
            .poll(&self.egress, &mut ctx, dispatch_regular::<L>, limit)
            .map_err(|e| ClusterError::aeron("poll", e))?;
        self.apply_polled(session_closed, new_leader, decode_err)?;
        Ok(n)
    }

    /// Controlled variant — application messages are dispatched through a
    /// [`ControlledEgressListener`] whose `on_message` returns a
    /// [`ControlledPollAction`] for backpressure. Lifecycle, challenge, and
    /// admin callbacks are no-ops by default.
    pub fn poll_egress_controlled<L: ControlledEgressListener>(
        &mut self,
        adapter: &mut ControlledEgressAdapter<L>,
        fragment_limit: usize,
    ) -> Result<i32, ClusterError> {
        adapter.set_expected_session_id(self.cluster_session_id);
        tolerate_retryable_keep_alive(self.keep_alive_if_due())?;
        let mut new_leader: Option<NewLeader> = None;
        let mut decode_err: Option<ClusterError> = None;
        let mut session_closed = false;
        let mut ctx = ControlledPollCtx {
            adapter,
            new_leader: &mut new_leader,
            decode_err: &mut decode_err,
            expected_session_id: self.cluster_session_id,
            session_closed: &mut session_closed,
        };
        let n = self
            .controlled_assembler
            .poll(&self.egress, &mut ctx, dispatch_controlled::<L>, fragment_limit)
            .map_err(|e| ClusterError::aeron("controlled_poll", e))?;
        self.apply_polled(session_closed, new_leader, decode_err)?;
        Ok(n)
    }

    /// Apply what one egress poll saw. A CLOSED for this session ends it first,
    /// whatever the state (Java sets `PENDING_CLOSE` unconditionally), so
    /// neither an error below nor a NewLeaderEvent in the same poll can lose
    /// or undo it.
    fn apply_polled(
        &mut self,
        session_closed: bool,
        new_leader: Option<NewLeader>,
        decode_err: Option<ClusterError>,
    ) -> Result<(), ClusterError> {
        if session_closed && matches!(self.state, SessionState::Connected | SessionState::AwaitingNewLeader) {
            self.state = SessionState::PendingClose;
            self.pending_ingress = None;
        }
        if let Some(event) = new_leader {
            self.on_new_leader_event(event)?;
        }
        if let Some(e) = decode_err {
            return Err(e);
        }
        self.poll_state_changes()
    }

    /// Handle a `NewLeaderEvent` without blocking (Java `onNewLeader`, whose
    /// publication add waits; this one is polled). Every fallible step runs
    /// before any session field changes. The session stays
    /// `AwaitingNewLeader`, with `message_timeout` from now, until
    /// [`Self::poll_pending_ingress`] swaps the connected publication in. A
    /// later event replaces a pending add. The fragment assemblers are kept:
    /// they buffer per session id, so the old leader's leftovers are harmless.
    fn on_new_leader_event(&mut self, event: NewLeader) -> Result<(), ClusterError> {
        if !matches!(self.state, SessionState::Connected | SessionState::AwaitingNewLeader) {
            return Ok(());
        }
        let NewLeader {
            leadership_term_id,
            leader_member_id,
            ingress_endpoints,
            image_session_id,
        } = event;
        let endpoint =
            crate::poller::parse_leader_endpoint(&ingress_endpoints, leader_member_id)?.ok_or_else(|| {
                ClusterError::ReconnectFailed {
                    reason: format!(
                        "NewLeaderEvent listed no endpoint for leader member {leader_member_id}: {ingress_endpoints}"
                    ),
                }
            })?;
        let channel = uri::ingress_channel_for_endpoint(self.ingress_channel.as_deref(), &endpoint)?;
        let publication = self
            ._aeron
            .async_add_exclusive_publication(&channel, self.ingress_stream_id)
            .map_err(|e| ClusterError::reconnect(format!("ingress to {endpoint}: {e}")))?;
        self.pending_ingress = Some(PendingIngress {
            publication,
            added: None,
            leadership_term_id,
            leader_member_id,
        });
        self.egress_image = EgressImage::bind(&self.egress, image_session_id);
        self.state = SessionState::AwaitingNewLeader;
        self.awaiting_leader_since = Some(Instant::now());
        Ok(())
    }

    /// Swap in the new leader's ingress publication once Aeron has added it
    /// and it has connected (Java `AWAIT_NEW_LEADER_CONNECTION`), adopting its
    /// term and leader and returning the session to `Connected`. One
    /// non-blocking poll; a failed add is reported once and leaves the session
    /// to time out unless another `NewLeaderEvent` comes.
    fn poll_pending_ingress(&mut self) -> Result<(), ClusterError> {
        let Some(mut pending) = self.pending_ingress.take() else {
            return Ok(());
        };
        if pending.added.is_none() {
            pending.added = pending
                .publication
                .poll()
                .map_err(|e| ClusterError::reconnect(format!("new leader ingress publication: {e}")))?;
        }
        let Some(ingress) = pending.added.take_if(|publication| publication.is_connected()) else {
            self.pending_ingress = Some(pending);
            return Ok(());
        };
        let (max_message_length, max_payload_length) = publication_limits(&ingress)?;
        self.ingress = ingress;
        self.max_message_length = max_message_length;
        self.max_payload_length = max_payload_length;
        self.leadership_term_id = pending.leadership_term_id;
        self.leader_member_id = pending.leader_member_id;
        if self.state == SessionState::AwaitingNewLeader {
            self.state = SessionState::Connected;
            self.awaiting_leader_since = None;
        }
        Ok(())
    }

    /// The cluster-assigned session id (set after connect completes).
    #[must_use = "discarding this value is almost always a mistake"]
    pub fn cluster_session_id(&self) -> i64 {
        self.cluster_session_id
    }
    /// The current leadership term (set after connect completes).
    #[must_use = "discarding this value is almost always a mistake"]
    pub fn leadership_term_id(&self) -> i64 {
        self.leadership_term_id
    }
    /// The current leader's member id (set after connect completes).
    #[must_use = "discarding this value is almost always a mistake"]
    pub fn leader_member_id(&self) -> i32 {
        self.leader_member_id
    }

    /// True when the ingress publication has a connected subscriber (the
    /// leader). Java `publication.isConnected()` analogue for
    /// backpressure-aware callers.
    #[must_use = "the connectivity decision is the point of calling this"]
    #[inline]
    pub fn is_ingress_connected(&self) -> bool {
        self.ingress.is_connected()
    }

    /// True when the ingress publication is closed (fatal for this handle).
    #[must_use = "the closed-state decision is the point of calling this"]
    #[inline]
    pub fn is_ingress_closed(&self) -> bool {
        self.ingress.is_closed()
    }

    /// Current ingress publication position.
    #[must_use = "discarding this value is almost always a mistake"]
    #[inline]
    pub fn ingress_position(&self) -> i64 {
        self.ingress.position()
    }

    /// Largest message the ingress publication accepts, session header
    /// included (Java `ingressPublication().maxMessageLength()`, a function
    /// of the channel's term length). [`Self::offer`] takes payloads up to
    /// this minus [`Self::SESSION_HEADER_LENGTH`]; check the largest message
    /// against it at start-up rather than meet
    /// [`ClusterError::PayloadTooLarge`] later. A REDIRECT or new leader
    /// re-adds the configured ingress channel, changing only a unicast
    /// endpoint, so the limit holds across failover.
    #[must_use = "discarding this value is almost always a mistake"]
    #[inline]
    pub fn ingress_max_message_length(&self) -> usize {
        self.max_message_length
    }

    /// Largest single-frame message the ingress publication accepts, session
    /// header included: the channel's MTU less the frame header (Java
    /// `ingressPublication().maxPayloadLength()`). [`Self::try_claim`] takes
    /// payloads up to the smaller of this and
    /// [`Self::ingress_max_message_length`], minus
    /// [`Self::SESSION_HEADER_LENGTH`]; [`Self::offer`] fragments anything
    /// larger.
    #[must_use = "discarding this value is almost always a mistake"]
    #[inline]
    pub fn ingress_max_payload_length(&self) -> usize {
        self.max_payload_length
    }

    /// True while the egress image this session is bound to is open (Java
    /// `!egressImage.isClosed()`). Going false while `Connected` is the
    /// leader-loss signal consumed by [`Self::poll_state_changes`]. Other
    /// images on the egress subscription do not count: the image a failed
    /// leader leaves behind can close while its replacement is open.
    #[must_use = "the connectivity decision is the point of calling this"]
    #[inline]
    pub fn is_egress_connected(&self) -> bool {
        self.egress_image.as_ref().is_some_and(|bound| !bound.image.is_closed())
    }

    /// Correlation id of the egress image this session is bound to: the image
    /// that delivered its SessionEvent(OK) or, after a failover, its latest
    /// NewLeaderEvent. Unique per image, so a change means the session moved
    /// to another image even when the session id is reused. `None` when that
    /// image had already closed when the event was handled.
    #[must_use = "discarding this value is almost always a mistake"]
    #[inline]
    pub fn egress_image_correlation_id(&self) -> Option<i64> {
        self.egress_image.as_ref().map(|bound| bound.correlation_id)
    }

    /// Session id of the egress image this session is bound to (see
    /// [`Self::egress_image_correlation_id`]): the cluster member's egress
    /// publication.
    #[must_use = "discarding this value is almost always a mistake"]
    #[inline]
    pub fn egress_image_session_id(&self) -> Option<i32> {
        self.egress_image.as_ref().map(|bound| bound.session_id)
    }

    /// Request the cluster take a snapshot (Java
    /// `sendAdminRequestToTakeASnapshot`). The response arrives on the egress
    /// listener as [`crate::EgressListener::on_admin_response`].
    pub fn send_admin_request_to_take_snapshot(&mut self, correlation_id: i64) -> Result<i64, ClusterError> {
        self.send_admin_request(correlation_id, AdminRequestType::SNAPSHOT, &[])
    }

    /// Drive session-state transitions without polling fragments (Java
    /// `pollStateChanges`): adopting a new leader's ingress publication once
    /// it has connected, leader-loss detection, `new_leader_timeout` (and,
    /// after a NewLeaderEvent, `message_timeout`) enforcement, and
    /// `PendingClose` → `Closed` finalisation. Never blocks.
    /// Call this from your event loop alongside [`Self::poll_egress`].
    pub fn poll_state_changes(&mut self) -> Result<(), ClusterError> {
        self.poll_pending_ingress()?;
        let connected = self.is_egress_connected();
        // Once a NewLeaderEvent has come, the wait is for its publication,
        // bounded by message_timeout as Java bounds AWAIT_NEW_LEADER_CONNECTION.
        let awaiting = if self.pending_ingress.is_some() {
            Awaiting {
                what: "connection to the new leader within message_timeout",
                timeout_ms: self.message_timeout_ms,
            }
        } else {
            Awaiting {
                what: "NewLeaderEvent within new_leader_timeout",
                timeout_ms: self.new_leader_timeout_ms,
            }
        };
        let result = apply_state_transition(&mut self.state, &mut self.awaiting_leader_since, awaiting, connected);
        if self.state == SessionState::Closed {
            self.pending_ingress = None;
        }
        result
    }

    /// Begin a poll-driven async connect. Returns an
    /// [`AsyncClusterConnect`] whose `poll()` advances the handshake.
    pub fn connect_async(builder: crate::SessionBuilder, aeron_dir: impl Into<String>) -> AsyncClusterConnect {
        AsyncClusterConnect::new(builder, aeron_dir.into())
    }
    /// Current session state: `Connected`, `NotConnected`, `Closed`.
    #[must_use = "the state decision is the point of calling this"]
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Zero-copy publish: claim a region of the ingress term buffer,
    /// write the SessionMessageHeader into the first 32 bytes via ergon,
    /// and expose the remaining `payload_len` bytes for the caller to fill
    /// directly. Mirrors Java `AeronCluster.tryClaim(length, claim)`.
    ///
    /// # Errors
    ///
    /// - [`ClusterError::NotConnected`] if the session is not Connected
    /// - [`ClusterError::PayloadTooLarge`] if header + payload exceeds
    ///   [`Self::ingress_max_payload_length`] or
    ///   [`Self::ingress_max_message_length`]
    /// - [`ClusterError::Publication`] on claim failure / backpressure
    /// - [`ClusterError::BufferTooSmall`] if the claim buffer is shorter than
    ///   the 32-byte session header
    ///
    /// # Hot path
    ///
    /// This is the hot publish path. On success there is no temp buffer copy of
    /// the application payload — fill [`ClusterClaim::payload_mut`] then
    /// [`ClusterClaim::commit`]. Abort with [`ClusterClaim::abort`] (or drop).
    ///
    /// ```rust,no_run
    /// use ergo_aeron_cluster::{AeronCluster, ClusterError};
    ///
    /// fn publish(client: &mut AeronCluster, app_bytes: &[u8]) -> Result<(), ClusterError> {
    ///     let mut claim = client.try_claim(app_bytes.len())?;
    ///     claim.payload_mut().copy_from_slice(app_bytes);
    ///     claim.commit()?;
    ///     Ok(())
    /// }
    /// ```
    pub fn try_claim(&mut self, payload_len: usize) -> Result<ClusterClaim, ClusterError> {
        if self.state != SessionState::Connected {
            return Err(ClusterError::NotConnected);
        }
        // A claim is one frame, so Aeron also caps it at the max payload length.
        let maximum = self.max_message_length.min(self.max_payload_length);
        let total = MSG_HDR_TOTAL
            .checked_add(payload_len)
            .ok_or(ClusterError::PayloadTooLarge {
                operation: "try_claim",
                requested: payload_len,
                maximum,
            })?;
        if total > maximum {
            return Err(ClusterError::PayloadTooLarge {
                operation: "try_claim",
                requested: total,
                maximum,
            });
        }
        let mut claim = self
            .ingress
            .try_claim_owned(total)
            .map_err(|e| self.track_publication_error(ClusterError::from_offer_error("try_claim", e)))?;

        // Write the SessionMessageHeader (schema 111) into the claim's
        // first 32 bytes via the ergon encoder.
        if claim.data().len() < MSG_HDR_TOTAL {
            return Err(ClusterError::BufferTooSmall {
                needed: MSG_HDR_TOTAL,
                actual: claim.data().len(),
            });
        }
        SessionMessageHeaderEncoder::wrap_into_claim(&mut claim.data()[..MSG_HDR_TOTAL])?
            .leadership_term_id(self.leadership_term_id)
            .cluster_session_id(self.cluster_session_id)
            .timestamp(0);

        Ok(ClusterClaim { claim, payload_len })
    }
}

/// What an `AwaitingNewLeader` session waits for, and for how long.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Awaiting {
    what: &'static str,
    timeout_ms: u64,
}

/// Pure state-machine transition logic, extracted for unit testing.
/// Called by [`AeronCluster::poll_state_changes`].
pub(crate) fn apply_state_transition(
    state: &mut SessionState,
    awaiting_leader_since: &mut Option<Instant>,
    awaiting: Awaiting,
    egress_connected: bool,
) -> Result<(), ClusterError> {
    match *state {
        SessionState::Connected if !egress_connected => {
            *state = SessionState::AwaitingNewLeader;
            *awaiting_leader_since = Some(Instant::now());
        }
        SessionState::AwaitingNewLeader => {
            if let Some(since) = *awaiting_leader_since
                && since.elapsed() >= Duration::from_millis(awaiting.timeout_ms)
            {
                *state = SessionState::Closed;
                return Err(ClusterError::Disconnected {
                    reason: format!("no {} ({}ms)", awaiting.what, awaiting.timeout_ms),
                });
            }
        }
        SessionState::PendingClose => {
            *state = SessionState::Closed;
        }
        _ => {}
    }
    Ok(())
}

/// A zero-copy claim on the ingress publication. The caller writes the
/// application payload into `payload_mut()` (the bytes after the 32-byte
/// SessionMessageHeader) then calls `commit()`.
#[must_use = "call commit() to publish or abort() to discard the claim"]
pub struct ClusterClaim {
    claim: AeronClaim,
    payload_len: usize,
}

impl ClusterClaim {
    /// The writable payload region (after the SessionMessageHeader).
    pub fn payload_mut(&mut self) -> &mut [u8] {
        let start = MSG_HDR_TOTAL;
        &mut self.claim.data()[start..start + self.payload_len]
    }

    /// Stream position Aeron assigned to this claim.
    #[must_use = "discarding this value is almost always a mistake"]
    pub fn position(&self) -> i64 {
        self.claim.position()
    }

    /// Commit the claimed bytes, publishing them to the cluster.
    /// Note: commit errors cannot be classified as retryable — commit
    /// returns a generic Aeron error (`AeronCError`), not a publication
    /// sentinel like `offer`/`try_claim`. The caller should treat a failed
    /// commit as terminal for this claim.
    pub fn commit(self) -> Result<i64, ClusterError> {
        self.claim.commit().map_err(|e| ClusterError::aeron("claim_commit", e))
    }

    /// Abort the claim, discarding it as padding.
    pub fn abort(self) -> Result<(), ClusterError> {
        self.claim.abort().map_err(|e| ClusterError::aeron("claim_abort", e))
    }
}

/// Poll-driven **Aeron** async connect — mirrors Java `AeronCluster.AsyncConnect`.
///
/// Not Tokio/`async`/`await`. Drive [`Self::poll`] from your event loop until
/// [`Self::is_complete`], then [`Self::finish`]. Handles challenge-response and
/// redirect between polls.
///
/// No poll waits on the media driver for a subscription or publication: they
/// are added with Aeron's async add and picked up by later polls, including
/// the publication a REDIRECT asks for. The first poll creates the Aeron
/// client itself, which maps the driver's CnC file and waits for the driver
/// only when it is not running yet.
#[must_use = "poll until is_complete(), then finish() to obtain the cluster client"]
pub struct AsyncClusterConnect {
    /// Declared first so the retained image is released before `egress`.
    egress_image: Option<EgressImage>,
    pending_egress: Option<AeronAsyncAddSubscription>,
    pending_ingress: Option<AeronAsyncAddExclusivePublication>,
    aeron: Option<Aeron>,
    ingress: Option<AeronExclusivePublication>,
    egress: Option<AeronSubscription>,
    builder: crate::SessionBuilder,
    aeron_dir: String,
    credentials: Vec<u8>,
    step: ConnectStep,
    connect_sent: bool,
    /// Wall-clock of last SessionConnectRequest offer attempt (success or not).
    last_connect_offer: Instant,
    reoffer_interval_ms: u64,
    started: Instant,
    /// Deadline computed on first poll (lazy to avoid panicking in `new`).
    deadline: Option<Instant>,
    cluster_session_id: i64,
    leadership_term_id: i64,
    leader_member_id: i32,
}

/// State machine step for [`AsyncClusterConnect`]. Each call to
/// [`AsyncClusterConnect::poll`] may advance one step; a step that waits on
/// the media driver spans several polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectStep {
    /// Building the Aeron transport (client, publication, subscription).
    CreateTransport,
    /// Offering the session-connect request, once a REDIRECT's publication
    /// exists.
    SendConnect,
    /// Waiting for the NewLeaderEvent / OK response.
    PollResponse,
    /// Connect complete — [`AsyncClusterConnect::finish`] may be called.
    Done,
}

impl AsyncClusterConnect {
    pub(crate) fn new(builder: crate::SessionBuilder, aeron_dir: String) -> Self {
        let timeout_ms = builder.message_timeout_ms;
        let creds = builder
            .credentials
            .as_ref()
            .and_then(|c| c.encoded_credentials().map(|cow| cow.into_owned()))
            .unwrap_or_default();
        // Epoch-like past so the first SendConnect is not blocked by re-offer gate.
        let past = Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        Self {
            egress_image: None,
            pending_egress: None,
            pending_ingress: None,
            aeron: None,
            ingress: None,
            egress: None,
            builder,
            aeron_dir,
            credentials: creds,
            step: ConnectStep::CreateTransport,
            connect_sent: false,
            last_connect_offer: past,
            reoffer_interval_ms: connect_reoffer_interval_ms(timeout_ms),
            started: Instant::now(),
            deadline: None, // set on first poll via checked_deadline
            cluster_session_id: -1,
            leadership_term_id: -1,
            leader_member_id: -1,
        }
    }

    /// Current connect step.
    ///
    /// ```rust
    /// # use ergo_aeron_cluster::{SessionBuilder, ConnectStep, ClusterError};
    /// # fn poll_loop(builder: SessionBuilder) -> Result<(), ClusterError> {
    /// let mut conn = builder.connect_async("/dev/shm/aeron-driver");
    /// while !conn.is_complete() {
    ///     conn.poll()?;
    ///     match conn.step() {
    ///         ConnectStep::CreateTransport => { /* Aeron init */ }
    ///         ConnectStep::SendConnect     => { /* offered connect */ }
    ///         ConnectStep::PollResponse    => { /* waiting for leader */ }
    ///         ConnectStep::Done            => { /* ready */ }
    ///         _ => { /* forward-compat: future steps */ }
    ///     }
    /// }
    /// let _client = conn.finish()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Always include a wildcard (`_`) arm — the enum is
    /// [`#[non_exhaustive]`](ConnectStep) so future releases may add steps
    /// without a semver break.
    #[must_use = "the connect-step decision is the point of calling this"]
    pub fn step(&self) -> ConnectStep {
        self.step
    }

    /// True once the connect has completed and `finish()` can be called.
    #[must_use = "the completion decision is the point of calling this"]
    pub fn is_complete(&self) -> bool {
        self.step == ConnectStep::Done
    }

    /// Advance the connect by one unit of work. Returns `Ok(true)` if
    /// more polling is needed, `Ok(false)` once complete. Idempotent after
    /// completion — calling `poll()` on an already-completed connect
    /// returns `Ok(false)` regardless of the deadline.
    pub fn poll(&mut self) -> Result<bool, ClusterError> {
        // Lazy deadline — computed on first poll to avoid panicking in `new()`.
        if self.deadline.is_none() {
            self.deadline = Some(checked_deadline("async_connect", self.builder.message_timeout_ms)?);
        }
        // Skip deadline after completion — an extra defensive poll after a
        // successful-but-slow connect must not throw away the recovered cluster.
        if !self.is_complete() && Instant::now() > self.deadline.expect("deadline set above") {
            return Err(ClusterError::Timeout {
                phase: "async_connect",
                after_ms: self.started.elapsed().as_millis() as u64,
            });
        }
        match self.step {
            ConnectStep::CreateTransport => {
                if self.aeron.is_none() {
                    self.builder.validate()?;
                    let dir_cstr = cformat!("{}", self.aeron_dir);
                    let ctx = AeronContext::new().map_err(|e| ClusterError::aeron("ctx", e))?;
                    ctx.set_dir(&dir_cstr).map_err(|e| ClusterError::aeron("set_dir", e))?;
                    let aeron = Aeron::new(&ctx).map_err(|e| ClusterError::aeron("new", e))?;
                    aeron.start().map_err(|e| ClusterError::aeron("start", e))?;
                    let egr = self.builder.egress_for_aeron()?;
                    let ing = self.builder.resolve_initial_ingress_for_aeron()?;
                    self.pending_egress = Some(
                        aeron
                            .async_add_subscription(egr, self.builder.egress_stream_id, Handlers::NONE, Handlers::NONE)
                            .map_err(|e| ClusterError::aeron("sub", e))?,
                    );
                    self.pending_ingress = Some(
                        aeron
                            .async_add_exclusive_publication(&ing, self.builder.ingress_stream_id)
                            .map_err(|e| ClusterError::aeron("pub", e))?,
                    );
                    self.aeron = Some(aeron);
                }
                self.poll_pending_transport()?;
                if self.egress.is_some() && self.ingress.is_some() {
                    self.step = ConnectStep::SendConnect;
                }
                Ok(true)
            }
            ConnectStep::SendConnect => {
                // Retry the offer across polls until the publication
                // connects and the connect request lands.
                self.poll_pending_transport()?;
                if self.encode_and_send_connect()? {
                    self.step = ConnectStep::PollResponse;
                }
                Ok(true)
            }
            ConnectStep::PollResponse => {
                if let Some((ev, image_session_id)) = self.poll_one_event()? {
                    use crate::poller::EgressEvent;
                    match ev {
                        EgressEvent::SessionEvent {
                            code,
                            cluster_session_id,
                            leadership_term_id,
                            leader_member_id,
                            detail,
                            ..
                        } => {
                            use crate::codecs::session::EventCode;
                            match code {
                                EventCode::OK => {
                                    self.cluster_session_id = cluster_session_id;
                                    self.leadership_term_id = leadership_term_id;
                                    self.leader_member_id = leader_member_id;
                                    self.egress_image = self
                                        .egress
                                        .as_ref()
                                        .and_then(|egress| EgressImage::bind(egress, image_session_id));
                                    self.step = ConnectStep::Done;
                                    return Ok(false);
                                }
                                EventCode::AUTHENTICATIONREJECTED => {
                                    return Err(ClusterError::AuthRejected);
                                }
                                EventCode::REDIRECT => {
                                    let ep = crate::poller::parse_leader_endpoint(&detail, leader_member_id)?
                                        .ok_or_else(|| ClusterError::ReconnectFailed {
                                            reason: format!(
                                                "connect redirect listed no endpoint for leader member {leader_member_id}: {detail}"
                                            ),
                                        })?;
                                    let c =
                                        uri::ingress_channel_for_endpoint(self.builder.ingress_channel_c_str(), &ep)?;
                                    let aeron = self.aeron.as_ref().ok_or_else(|| ClusterError::ReconnectFailed {
                                        reason: "no aeron client for redirect".into(),
                                    })?;
                                    let p = aeron
                                        .async_add_exclusive_publication(&c, self.builder.ingress_stream_id)
                                        .map_err(|e| ClusterError::reconnect(format!("redirect publication: {e}")))?;
                                    self.pending_ingress = Some(p);
                                    self.ingress = None;
                                    self.leader_member_id = leader_member_id;
                                    self.connect_sent = false;
                                    self.step = ConnectStep::SendConnect;
                                }
                                _ => {}
                            }
                        }
                        EgressEvent::Challenge {
                            correlation_id,
                            cluster_session_id,
                            encoded_challenge,
                        } => {
                            let resp = self
                                .builder
                                .credentials
                                .as_ref()
                                .and_then(|c| c.on_challenge(&encoded_challenge).map(|cow| cow.into_owned()))
                                .unwrap_or_default();
                            self.send_challenge_response(correlation_id, cluster_session_id, &resp)?;
                        }
                        _ => {}
                    }
                } else if self.last_connect_offer.elapsed() >= Duration::from_millis(self.reoffer_interval_ms) {
                    // Pre-election / silent non-leader: re-offer connect.
                    self.connect_sent = false;
                    let _ = self.encode_and_send_connect()?;
                }
                Ok(true)
            }
            ConnectStep::Done => Ok(false),
        }
    }

    /// Consume the in-progress connect and yield the connected client.
    pub fn finish(self) -> Result<AeronCluster, ClusterError> {
        if self.step != ConnectStep::Done {
            return Err(ClusterError::ConnectFailed {
                reason: "connect not complete".into(),
            });
        }
        let keep_alive_interval_ms = connect_reoffer_interval_ms(self.builder.message_timeout_ms);
        let Self {
            egress_image,
            aeron,
            ingress,
            egress,
            builder,
            cluster_session_id,
            leadership_term_id,
            leader_member_id,
            ..
        } = self;
        let ingress =
            ingress.ok_or_else(|| ClusterError::connect("async connect finished without ingress publication"))?;
        let (max_message_length, max_payload_length) = publication_limits(&ingress)?;
        Ok(AeronCluster {
            egress_image,
            pending_ingress: None,
            _aeron: aeron.ok_or_else(|| ClusterError::connect("async connect finished without Aeron client"))?,
            ingress,
            egress: egress
                .ok_or_else(|| ClusterError::connect("async connect finished without egress subscription"))?,
            cluster_session_id,
            leadership_term_id,
            leader_member_id,
            state: SessionState::Connected,
            awaiting_leader_since: None,
            ingress_stream_id: builder.ingress_stream_id,
            ingress_channel: builder.ingress_channel_c_str().map(CStr::to_owned),
            new_leader_timeout_ms: builder.new_leader_timeout_ms,
            message_timeout_ms: builder.message_timeout_ms,
            last_keep_alive: Instant::now(),
            max_message_length,
            max_payload_length,
            keep_alive_interval_ms,
            regular_assembler: AeronFragmentClosureAssembler::new()
                .map_err(|e| ClusterError::aeron("AeronFragmentClosureAssembler", e))?,
            controlled_assembler: AeronControlledFragmentClosureAssembler::new()
                .map_err(|e| ClusterError::aeron("AeronControlledFragmentClosureAssembler", e))?,
        })
    }

    fn encode_and_send_connect(&mut self) -> Result<bool, ClusterError> {
        if self.connect_sent {
            return Ok(true);
        }
        let Some(ingress) = &self.ingress else {
            return Ok(false);
        };
        let ch = self.builder.egress_channel_bytes();
        let info = client_info_bytes();
        let len = SessionConnectRequestEncoder::compute_encoded_length_with_message_header(
            ch.len(),
            self.credentials.len(),
            info.len(),
        );
        let mut buf = vec![0u8; len];
        let _ = SessionConnectRequestEncoder::wrap_and_apply_header(&mut buf, 0)
            .fixed(&SessionConnectRequestFixedFields {
                correlation_id: 0,
                response_stream_id: self.builder.egress_stream_id,
                version: Some(0),
            })
            .response_channel(ch)?
            .encoded_credentials(&self.credentials)?
            .client_info(&client_info_bytes())?;
        let sent = ingress.offer_raw(&buf, Handlers::NONE) > 0;
        // Always stamp the attempt so re-offer cadence advances even under
        // backpressure (avoids tight spin when publication is not connected).
        self.last_connect_offer = Instant::now();
        self.connect_sent = sent;
        Ok(sent)
    }

    fn send_challenge_response(&mut self, cid: i64, csid: i64, creds: &[u8]) -> Result<(), ClusterError> {
        let len = ChallengeResponseEncoder::compute_encoded_length_with_message_header(creds.len());
        let mut buf = vec![0u8; len];
        let _ = ChallengeResponseEncoder::wrap_and_apply_header(&mut buf, 0)
            .fixed(&ChallengeResponseFixedFields {
                correlation_id: cid,
                cluster_session_id: csid,
            })
            .encoded_credentials(creds)?;
        if let Some(ingress) = &self.ingress {
            let r = ingress.offer_raw(&buf, Handlers::NONE);
            offer_result("challenge_response", r)?;
        }
        Ok(())
    }

    /// Pick up subscriptions and publications Aeron has finished adding,
    /// without waiting (Java `getSubscription(registrationId)`).
    fn poll_pending_transport(&mut self) -> Result<(), ClusterError> {
        if let Some(add) = &self.pending_egress
            && let Some(egress) = add.poll().map_err(|e| ClusterError::aeron("sub", e))?
        {
            self.egress = Some(egress);
            self.pending_egress = None;
        }
        if let Some(add) = &self.pending_ingress
            && let Some(ingress) = add.poll().map_err(|e| ClusterError::aeron("pub", e))?
        {
            self.ingress = Some(ingress);
            self.pending_ingress = None;
        }
        Ok(())
    }

    /// One egress event and the session id of the image that delivered it.
    fn poll_one_event(&mut self) -> Result<Option<(crate::poller::EgressEvent, Option<i32>)>, ClusterError> {
        let mut ev: Option<(crate::poller::EgressEvent, Option<i32>)> = None;
        let mut err: Option<ClusterError> = None;
        if let Some(egress) = &self.egress {
            egress
                .poll_fn(
                    |data, hdr| {
                        if ev.is_none() && err.is_none() {
                            match crate::poller::parse_event(data) {
                                Ok(e) => ev = Some((e, hdr.session_id())),
                                Err(e) => err = Some(e),
                            }
                        }
                    },
                    1,
                )
                .map_err(|e| ClusterError::aeron("async_connect_poll", e))?;
        }
        if let Some(e) = err {
            return Err(e);
        }
        Ok(ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AWAITING_LEADER: Awaiting = Awaiting {
        what: "NewLeaderEvent within new_leader_timeout",
        timeout_ms: 5_000,
    };

    #[test]
    fn test_msg_header_total_is_32() -> Result<(), Box<dyn std::error::Error>> {
        // MessageHeader(8) + SessionMessageHeader body(24) = 32
        assert_eq!(MSG_HDR_TOTAL, 32);

        Ok(())
    }

    #[test]
    fn test_session_constants() -> Result<(), Box<dyn std::error::Error>> {
        use crate::codecs::session::{
            SessionCloseRequestEncoder, SessionKeepAliveEncoder, SessionMessageHeaderEncoder,
        };
        assert_eq!(SessionMessageHeaderEncoder::TEMPLATE_ID, 1);
        assert_eq!(SessionKeepAliveEncoder::TEMPLATE_ID, 5);
        assert_eq!(SessionCloseRequestEncoder::TEMPLATE_ID, 4);

        Ok(())
    }

    #[test]
    fn retryable_keep_alive_failure_does_not_block_egress_poll() -> Result<(), Box<dyn std::error::Error>> {
        for code in [-1, -2, -3] {
            tolerate_retryable_keep_alive(Err(ClusterError::from_offer_raw("track_ingress", code)))?;
        }
        for code in [-4, -5] {
            assert!(tolerate_retryable_keep_alive(Err(ClusterError::from_offer_raw("track_ingress", code))).is_err());
        }
        Ok(())
    }

    #[test]
    fn test_encoded_length_matches_encode_connect_request() -> Result<(), Box<dyn std::error::Error>> {
        use crate::codecs::session::SessionConnectRequestEncoder;

        let ch = b"aeron:udp?endpoint=localhost:9999";
        let creds = b"user:pass";
        let info = client_info_bytes();
        let len =
            SessionConnectRequestEncoder::compute_encoded_length_with_message_header(ch.len(), creds.len(), info.len());
        let mut buf = vec![0u8; len];
        use crate::codecs::session::SessionConnectRequestFixedFields;
        let complete = SessionConnectRequestEncoder::wrap_and_apply_header(&mut buf, 0)
            .fixed(&SessionConnectRequestFixedFields {
                correlation_id: 0,
                response_stream_id: 102,
                version: Some(0),
            })
            .response_channel(ch)?
            .encoded_credentials(creds)?
            .client_info(&info)?;
        assert_eq!(
            complete.as_bytes_with_header().len(),
            len,
            "EncodedLength compute must equal as_bytes_with_header for connect request"
        );
        Ok(())
    }

    #[test]
    fn test_encoded_length_matches_encode_challenge_response() -> Result<(), Box<dyn std::error::Error>> {
        use crate::codecs::session::{ChallengeResponseEncoder, ChallengeResponseFixedFields};

        let creds = b"challenge-response-bytes";
        let len = ChallengeResponseEncoder::compute_encoded_length_with_message_header(creds.len());
        let mut buf = vec![0u8; len];
        let complete = ChallengeResponseEncoder::wrap_and_apply_header(&mut buf, 0)
            .fixed(&ChallengeResponseFixedFields {
                correlation_id: 1,
                cluster_session_id: 2,
            })
            .encoded_credentials(creds)?;
        assert_eq!(
            complete.as_bytes_with_header().len(),
            len,
            "EncodedLength compute must equal as_bytes_with_header for challenge response"
        );
        Ok(())
    }

    #[test]
    fn test_keep_alive_buffer_is_stack_sized_by_encoded_length() -> Result<(), Box<dyn std::error::Error>> {
        use crate::codecs::session::SessionKeepAliveEncoder;
        // Fixed message — the stack array must use the generated ENCODED_LENGTH.
        assert_eq!(
            SessionKeepAliveEncoder::ENCODED_LENGTH,
            8 + SessionKeepAliveEncoder::BLOCK_LENGTH,
        );
        Ok(())
    }

    #[test]
    fn test_pending_close_finalises_to_closed() -> Result<(), Box<dyn std::error::Error>> {
        // T1: PendingClose → Closed via poll_state_changes → apply_state_transition.
        let mut state = SessionState::PendingClose;
        let mut awaiting = None;
        apply_state_transition(&mut state, &mut awaiting, AWAITING_LEADER, true)?;
        assert_eq!(state, SessionState::Closed);
        Ok(())
    }

    #[test]
    fn test_new_leader_timeout_disconnects() -> Result<(), Box<dyn std::error::Error>> {
        // T7: After deadline, AwaitingNewLeader → Closed + Disconnected error.
        let mut state = SessionState::AwaitingNewLeader;
        let mut awaiting = Some(Instant::now() - Duration::from_secs(6));
        let result = apply_state_transition(&mut state, &mut awaiting, AWAITING_LEADER, false);
        match result {
            Err(ClusterError::Disconnected { .. }) => {}
            other => panic!("expected Disconnected, got {other:?}"),
        }
        assert_eq!(state, SessionState::Closed);
        Ok(())
    }

    #[test]
    fn test_reconnect_prepares_before_swapping_state() -> Result<(), Box<dyn std::error::Error>> {
        // T6: Structural proof — `on_new_leader_event` must run every fallible
        // step BEFORE writing any session field, so a failure leaves the
        // session as it was, and must start the publication add without
        // waiting for it.
        let src = include_str!("client.rs");
        let off = src
            .find("fn on_new_leader_event")
            .ok_or("on_new_leader_event not found")?;
        let end = src[off..].find("\n    }\n").ok_or("end of on_new_leader_event")?;
        let body = &src[off..off + end];
        let first_write = [
            "self.pending_ingress =",
            "self.egress_image =",
            "self.state =",
            "self.awaiting_leader_since =",
            "self.leadership_term_id =",
            "self.leader_member_id =",
        ]
        .iter()
        .filter_map(|write| body.find(write))
        .min()
        .ok_or("no session write")?;
        for step in [
            "parse_leader_endpoint(",
            "ingress_channel_for_endpoint(",
            "async_add_exclusive_publication(",
        ] {
            let at = body.find(step).ok_or(step)?;
            assert!(at < first_write, "{step} must precede the first session write");
        }
        assert_eq!(
            body.matches("add_exclusive_publication(").count(),
            body.matches("async_add_exclusive_publication(").count(),
            "a NewLeaderEvent must not add its publication with a blocking wait"
        );
        Ok(())
    }

    #[test]
    fn test_offer_gathers_without_heap_allocation() -> Result<(), Box<dyn std::error::Error>> {
        // Structural proof — the `offer` method's primary path delegates to
        // `try_claim` (claim-based, allocation-free). Large messages that
        // exceed the max payload use `offer_parts` with a stack-allocated
        // header, avoiding any heap allocation or payload copy.
        let src = include_str!("client.rs");
        let off_start = src
            .find("pub fn offer(&mut self, payload: &[u8])")
            .ok_or("offer signature not found")?;
        let body_snippet = &src[off_start..];
        let next_pub = body_snippet[20..].find("pub fn ").unwrap_or(body_snippet.len());
        let offer_body = &body_snippet[..next_pub + 20];
        assert!(
            offer_body.contains("self.try_claim"),
            "offer must delegate to try_claim as primary path"
        );
        assert!(
            offer_body.contains("offer_parts"),
            "fallback must use offer_parts gather (no heap allocation)"
        );
        assert!(
            !offer_body.contains("vec!["),
            "offer must not allocate on the heap for fragmented messages"
        );
        Ok(())
    }

    #[test]
    fn test_new_leader_timeout_not_yet_elapsed_keeps_awaiting() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = SessionState::AwaitingNewLeader;
        let mut awaiting = Some(Instant::now());
        apply_state_transition(&mut state, &mut awaiting, AWAITING_LEADER, false)?;
        assert_eq!(state, SessionState::AwaitingNewLeader);
        Ok(())
    }
}
