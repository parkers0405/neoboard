//! Local control protocol and daemon-owned OpenTunnel publishing.
use crate::{
    board::{now, BoardError},
    client::{connect_local, local_data_dir},
    invite_file::{https_endpoint, InviteFile},
    protocol::{InviteRequest, Member},
    valid_project, Board,
};
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{sync::{watch, Mutex}, time::timeout};

const PROFILE: &str = "board";
pub const CONTROL_FILE: &str = "share-control.json";
const SHARE_PATH: &str = "/v1/share";
const PROVISION_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const SHARE_TIMEOUT: Duration = Duration::from_secs(95);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(100);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[doc(hidden)]
#[derive(Serialize, Deserialize)]
pub struct ControlDiscovery {
    pub addr: SocketAddr,
    pub token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareRequest {
    project: String,
    actor: String,
}

#[derive(Serialize, Deserialize)]
struct ShareError {
    error: String,
}

/// Start or reuse this data directory's board daemon, then ask it for a 24-hour
/// invite scoped to the selected project. Run on a blocking worker, not a UI thread.
pub fn share_local(data_dir: Option<&Path>, project: &str, actor: &str) -> Result<InviteFile, String> {
    validate_share(project, actor)?;
    let dir = local_data_dir(data_dir).map_err(|e| e.to_string())?;
    connect_local(Some(&dir), None).map_err(|e| e.to_string())?;
    let control: ControlDiscovery = serde_json::from_slice(
        &std::fs::read(dir.join(CONTROL_FILE))
            .map_err(|_| "Missing local share control record; restart the board with the current neochatd.")?,
    ).map_err(|_| "Invalid local share control record.")?;
    if !control.addr.ip().is_loopback() || control.addr.port() == 0 || control.token.is_empty() {
        return Err("Invalid local share control record.".into());
    }
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(CONTROL_TIMEOUT)
        .build()
        .map_err(|_| "Could not initialize local share control.")?;
    let response = http.post(format!("http://{}{SHARE_PATH}", control.addr))
        .bearer_auth(control.token)
        .json(&ShareRequest { project: project.into(), actor: actor.into() })
        .send().map_err(|error| if error.is_timeout() {
            "Timed out waiting for the board daemon to share this project."
        } else {
            "Could not reach local share control; check that the current neochatd is running."
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(response.json::<ShareError>().map(|e| e.error)
            .unwrap_or_else(|_| format!("Local share control returned {status}.")));
    }
    response.json().map_err(|_| "The board daemon returned an invalid share invite.".into())
}

fn validate_share(project: &str, actor: &str) -> Result<(), String> {
    if !valid_project(project) {
        return Err("Invalid board ID; use 1–80 ASCII letters, digits, dots, dashes or underscores.".into());
    }
    if actor.is_empty() || actor.len() > 80
        || !actor.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        || matches!(actor, "local" | "system")
    {
        return Err("Invalid guest actor; use a new name of 1–80 ASCII letters, digits, dots, dashes or underscores (not local or system).".into());
    }
    Ok(())
}

struct Running {
    tunnel: opentunnel::Tunnel,
    endpoint: String,
}

/// Only constructed by neochatd. The control router must be served on its own
/// loopback listener, never added to the tunneled board router.
#[doc(hidden)]
pub struct Sharing {
    board: Board,
    board_token: String,
    admin_token: String,
    control_hash: [u8; 32],
    target: SocketAddr,
    sdk: opentunnel::Client,
    http: reqwest::Client,
    running: Mutex<Option<Running>>,
    stopping: watch::Sender<bool>,
}

impl Sharing {
    pub fn new(
        dir: &Path, target: SocketAddr, board: Board, board_token: String, admin_token: String,
        control_token: &str,
    ) -> Result<Arc<Self>, reqwest::Error> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(PROBE_TIMEOUT)
            .build()?;
        Ok(Arc::new(Self {
            board, board_token, admin_token, target, http,
            control_hash: Sha256::digest(control_token).into(),
            sdk: opentunnel::Client::new(opentunnel::ClientOptions {
                storage: Some(opentunnel::Storage::at(dir.join("opentunnel"))),
                ..Default::default()
            }),
            running: Mutex::new(None),
            stopping: watch::channel(false).0,
        }))
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new().route(SHARE_PATH, post(handle_share))
            .layer(DefaultBodyLimit::max(2048))
            .route_layer(middleware::from_fn_with_state(self.clone(), authenticate))
            .with_state(self.clone())
    }

    /// Restore a previously provisioned identity, but never publish a fresh
    /// data directory (or resume an incomplete first share) automatically.
    pub async fn resume(&self) -> Result<(), String> {
        if self.sdk.get(PROFILE).map_err(|_| "Could not read the saved OpenTunnel identity.")?.is_some() {
            self.endpoint().await?;
        }
        Ok(())
    }

    pub async fn stop(&self) {
        self.stopping.send_replace(true);
        if let Ok(mut running) = timeout(CLOSE_TIMEOUT, self.running.lock()).await {
            if let Some(running) = running.take() {
                let _ = timeout(CLOSE_TIMEOUT, running.tunnel.close()).await;
            }
        }
    }

    async fn endpoint(&self) -> Result<String, String> {
        let mut stopping = self.stopping.subscribe();
        tokio::select! {
            biased;
            _ = stopping.wait_for(|value| *value) => Err("The board daemon is shutting down.".into()),
            result = self.start_tunnel() => result,
        }
    }

    async fn start_tunnel(&self) -> Result<String, String> {
        if !self.target.ip().is_loopback() {
            return Err("OpenTunnel requires the board HTTP port to be reachable over loopback.".into());
        }
        // Serializes provision/attach for concurrent share calls and startup resume.
        let mut running = self.running.lock().await;
        if running.as_ref().is_some_and(|r| r.tunnel.status().state == opentunnel::State::Stopped) {
            *running = None;
        }
        if running.is_none() {
            let identity = timeout(PROVISION_TIMEOUT, self.sdk.ensure(PROFILE)).await
                .map_err(|_| "OpenTunnel provisioning timed out; retry to resume it.")?
                .map_err(|error| sdk_error("provisioning", error))?;
            let endpoint = public_endpoint(&identity.hostname)?;
            let tunnel = self.sdk.connect(PROFILE, [("@".into(), self.target.to_string().into())].into())
                .map_err(|error| sdk_error("connection setup", error))?;
            *running = Some(Running { tunnel, endpoint });
        }
        let mut status = running.as_ref().unwrap().tunnel.status_watch();
        let ready = timeout(CONNECT_TIMEOUT, status.wait_for(|status| {
            matches!(status.state, opentunnel::State::Connected | opentunnel::State::Stopped)
        })).await;
        match ready {
            Ok(Ok(status)) if status.state == opentunnel::State::Connected => Ok(running.as_ref().unwrap().endpoint.clone()),
            Ok(_) => Err("OpenTunnel stopped before becoming ready; retry sharing to reconnect.".into()),
            Err(_) => Err("OpenTunnel connection timed out; retry once the tunnel reconnects.".into()),
        }
    }

    async fn share(&self, request: ShareRequest) -> Result<InviteFile, (StatusCode, String)> {
        validate_share(&request.project, &request.actor).map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        let board = self.board.clone();
        let token = self.board_token.clone();
        let prefix = format!("{}/", request.project);
        let exists = tokio::task::spawn_blocking(move || {
            Ok(board.snapshot(&token)?.channels.iter().any(|channel| channel.starts_with(&prefix)))
        }).await.map_err(|_| internal_error())?.map_err(board_error)?;
        if !exists {
            return Err((StatusCode::NOT_FOUND, "Unknown board; create it before sharing.".into()));
        }
        let endpoint = self.endpoint().await.map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
        self.issue_invite(endpoint, request).await
    }

    async fn issue_invite(&self, endpoint: String, request: ShareRequest) -> Result<InviteFile, (StatusCode, String)> {
        let board = self.board.clone();
        let admin = self.admin_token.clone();
        let expires_at = now() + 86_400;
        let scope = format!("{}/*", request.project);
        let actor = request.actor.clone();
        let invite = tokio::task::spawn_blocking(move || board.create_invite(&admin, InviteRequest {
            actor, channels: vec![scope], expires_at: Some(expires_at),
        })).await.map_err(|_| internal_error())?.map_err(board_error)?;

        // Only the scoped guest credential ever traverses the tunnel. /v1/me
        // checks that the public origin serves this board without fetching its history.
        let reachable = async {
            let response = self.http.get(format!("{endpoint}v1/me")).bearer_auth(&invite.token).send().await?;
            Ok::<_, reqwest::Error>(response.error_for_status()?.json::<Member>().await?.actor == request.actor)
        }.await.unwrap_or(false);
        if !reachable {
            let board = self.board.clone();
            let admin = self.admin_token.clone();
            let id = invite.id;
            let revoked = tokio::task::spawn_blocking(move || board.revoke_invite(&admin, id)).await;
            let error = if matches!(revoked, Ok(Ok(()))) {
                "Could not verify the public board endpoint. The invite was revoked; use a new guest actor when retrying.".into()
            } else {
                format!("Could not verify the public board endpoint or revoke invite #{id}; revoke it with `neochat invite revoke {id}`.")
            };
            return Err((StatusCode::BAD_GATEWAY, error));
        }
        Ok(InviteFile { project: Some(request.project), version: 1, endpoint, token: invite.token, expires_at })
    }
}

fn public_endpoint(hostname: &str) -> Result<String, String> {
    if !hostname.contains('.') || hostname.split('.').any(|label| {
        label.is_empty() || !label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    }) {
        return Err("OpenTunnel returned an invalid public hostname.".into());
    }
    https_endpoint(&format!("https://{hostname}/")).map_err(str::to_owned)
}

fn sdk_error(phase: &str, error: opentunnel::Error) -> String {
    // The SDK's error payloads can contain provider responses; don't relay them.
    match error {
        opentunnel::Error::Api { status, .. } => format!("OpenTunnel {phase} failed (HTTP {status})."),
        _ => format!("OpenTunnel {phase} failed; check network access and the saved tunnel identity."),
    }
}

fn internal_error() -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "Local board operation failed.".into())
}

fn board_error(error: BoardError) -> (StatusCode, String) {
    if error.0.is_server_error() { internal_error() } else { (error.0, error.1) }
}

async fn authenticate(State(state): State<Arc<Sharing>>, headers: HeaderMap, request: Request, next: Next) -> Response {
    let authorized = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| Sha256::digest(token).as_slice().ct_eq(&state.control_hash).into());
    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

async fn handle_share(State(state): State<Arc<Sharing>>, Json(request): Json<ShareRequest>) -> Response {
    let result = match timeout(SHARE_TIMEOUT, state.share(request)).await {
        Ok(result) => result,
        Err(_) => Err((StatusCode::GATEWAY_TIMEOUT, "Sharing timed out; retry when the board and tunnel are ready.".into())),
    };
    let mut response = match result {
        Ok(invite) => Json(invite).into_response(),
        Err((status, error)) => (status, Json(ShareError { error })).into_response(),
    };
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Action;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{net::TcpListener, task::JoinHandle};

    const HOST: &str = "test-host-credential-not-a-real-token";
    const ADMIN: &str = "test-admin-credential-not-a-real-token";

    async fn serve(router: Router) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        (endpoint, tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }))
    }

    fn service(dir: &Path) -> Arc<Sharing> {
        let board = Board::open(dir.join("test.sqlite3"), HOST, ADMIN).unwrap();
        for channel in ["team/general", "team/updates", "team-sandbox/general"] {
            board.action(HOST, None, None, Action::CreateChannel { channel: channel.into() }).unwrap();
        }
        Sharing::new(dir, "127.0.0.1:41580".parse().unwrap(), board, HOST.into(), ADMIN.into(), "control").unwrap()
    }

    fn request() -> ShareRequest {
        ShareRequest { project: "team".into(), actor: "reviewer".into() }
    }

    #[tokio::test]
    async fn publishing_control_is_private_and_provisions_only_known_projects() {
        let dir = tempfile::tempdir().unwrap();
        let mut sharing = service(dir.path());
        let sdk_calls = Arc::new(AtomicUsize::new(0));
        let count = sdk_calls.clone();
        let (api, api_task) = serve(Router::new().route("/api/tunnel", post(move || async move {
            count.fetch_add(1, Ordering::SeqCst);
            (StatusCode::FORBIDDEN, "private provider diagnostic")
        }))).await;
        Arc::get_mut(&mut sharing).unwrap().sdk = opentunnel::Client::new(opentunnel::ClientOptions {
            storage: Some(opentunnel::Storage::at(dir.path().join("opentunnel"))),
            api: Some(reqwest::Url::parse(&api).unwrap()),
        });
        sharing.resume().await.unwrap();
        let (control, control_task) = serve(sharing.router()).await;
        let (board, board_task) = serve(crate::router(sharing.board.clone())).await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let post_share = |origin: &str, token: &str, request: &ShareRequest| {
            http.post(format!("{}{SHARE_PATH}", origin.trim_end_matches('/'))).bearer_auth(token).json(request)
        };
        assert_eq!(post_share(&board, "control", &request()).send().await.unwrap().status(), StatusCode::NOT_FOUND);
        for token in ["", "guest", HOST, ADMIN] {
            assert_eq!(post_share(&control, token, &request()).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        }
        let unknown = ShareRequest { project: "missing".into(), ..request() };
        assert_eq!(post_share(&control, "control", &unknown).send().await.unwrap().status(), StatusCode::NOT_FOUND);
        assert_eq!(sdk_calls.load(Ordering::SeqCst), 0);

        let response = post_share(&control, "control", &request()).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.json::<ShareError>().await.unwrap().error, "OpenTunnel provisioning failed (HTTP 403).");
        assert_eq!(sdk_calls.load(Ordering::SeqCst), 1);
        // A provisioning failure must not consume the actor identity.
        assert!(sharing.board.create_invite(ADMIN, InviteRequest {
            actor: request().actor, channels: vec!["team/*".into()], expires_at: None,
        }).is_ok());
        sharing.stop().await;
        api_task.abort();
        control_task.abort();
        board_task.abort();
    }

    #[tokio::test]
    async fn invites_only_expose_the_selected_project() {
        let dir = tempfile::tempdir().unwrap();
        let sharing = service(dir.path());
        // Stand in for the SDK's forwarding leg; no public service is called.
        let (endpoint, task) = serve(crate::router(sharing.board.clone())).await;
        let invite = sharing.issue_invite(endpoint, request()).await.unwrap();
        assert_eq!(invite.project.as_deref(), Some("team"));
        assert!(invite.expires_at > now() + 86_000);
        sharing.board.action(HOST, None, None, Action::CreateChannel { channel: "team/new".into() }).unwrap();
        assert_eq!(sharing.board.snapshot(&invite.token).unwrap().channels, ["team/general", "team/new", "team/updates"]);
        task.abort();
    }

    #[test]
    fn public_origin_cannot_include_credentials_paths_or_ports() {
        assert_eq!(public_endpoint("board.opentunnel.xyz").unwrap(), "https://board.opentunnel.xyz/");
        for invalid in ["localhost", "other/path", "x@public.example", "board.example:443", "board.example#secret"] {
            assert!(public_endpoint(invalid).is_err());
        }
    }
}