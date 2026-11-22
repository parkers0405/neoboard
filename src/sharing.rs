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
use std::{net::SocketAddr, path::{Path, PathBuf}, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{sync::{watch, Mutex}, time::timeout};

const PROFILE: &str = "board";
pub const CONTROL_FILE: &str = "share-control.json";
const SHARE_PATH: &str = "/v1/share";
const STATUS: &str = "/v1/share/status";
const STOP: &str = "/v1/share/stop";
const LOOKUP: &str = "/v1/share/lookup";
const DESKTOP: &str = "/v1/share/desktop";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum RouteState { Stopped, Connecting, Connected, Reconnecting }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RouteStatus { pub state: RouteState, pub endpoint: Option<String> }

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BoardSelection { project: String }
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
    control_call(data_dir, true, SHARE_PATH, &ShareRequest { project: project.into(), actor: actor.into() })
}

/// Blocking host control calls. Only an explicit share may launch a daemon.
pub fn status_local(data_dir: Option<&Path>) -> Result<RouteStatus, String> {
    control_call(data_dir, false, STATUS, &())
}
pub fn stop_local(data_dir: Option<&Path>) -> Result<(), String> {
    control_call(data_dir, false, STOP, &())
}
pub fn lookup_local(data_dir: Option<&Path>, project: &str) -> Result<Option<InviteFile>, String> {
    control_call(data_dir, false, LOOKUP, &BoardSelection { project: project.into() })
}
pub fn share_desktop_local(data_dir: Option<&Path>, project: &str) -> Result<InviteFile, String> {
    if !valid_project(project) { return Err("Invalid board ID.".into()); }
    control_call(data_dir, true, DESKTOP, &BoardSelection { project: project.into() })
}

fn control_call<T: serde::de::DeserializeOwned>(data_dir: Option<&Path>, start: bool, path: &str, body: &impl Serialize) -> Result<T, String> {
    let dir = local_data_dir(data_dir).map_err(|e| e.to_string())?;
    if start { connect_local(Some(&dir), None).map_err(|e| e.to_string())?; }
    let control: ControlDiscovery = serde_json::from_slice(
        &private_record(&dir.join(CONTROL_FILE))
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
    let response = http.post(format!("http://{}{path}", control.addr))
        .bearer_auth(control.token)
        .json(body)
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
    response.json().map_err(|_| "Invalid local share control response.".into())
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
    status: watch::Receiver<opentunnel::Status>,
    // Owns the SDK handle. Awaiting this closes and joins the tunnel supervisor;
    // dropping an unpolled future also drops the handle (SDK shutdown on drop).
    close: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), opentunnel::Error>> + Send>>,
    endpoint: String,
}

/// Only constructed by neochatd. The control router must be served on its own
/// loopback listener, never added to the tunneled board router.
#[doc(hidden)]
pub struct Sharing {
    dir: PathBuf,
    operation: Mutex<()>,
    #[cfg(not(unix))]
    cache: std::sync::Mutex<std::collections::HashMap<String, InviteFile>>,
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
            dir: dir.to_owned(), operation: Mutex::new(()),
            #[cfg(not(unix))]
            cache: Default::default(),
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
            .route(STATUS, post(handle_status)).route(STOP, post(handle_stop))
            .route(LOOKUP, post(handle_lookup)).route(DESKTOP, post(handle_desktop))
            .layer(DefaultBodyLimit::max(2048))
            .route_layer(middleware::from_fn_with_state(self.clone(), authenticate))
            .with_state(self.clone())
    }

    /// Restore a previously provisioned identity, but never publish a fresh
    /// data directory (or resume an incomplete first share) automatically.
    pub async fn resume(&self) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        if self.dir.join("share-disabled").try_exists().map_err(|_| "Cannot read sharing preference.")? { return Ok(()); }
        if self.sdk.get(PROFILE).map_err(|_| "Could not read the saved OpenTunnel identity.")?.is_some() {
            self.endpoint().await?;
        }
        Ok(())
    }

    pub async fn stop(&self) {
        self.stopping.send_replace(true);
        if let Ok(mut running) = timeout(CLOSE_TIMEOUT, self.running.lock()).await {
            if let Some(running) = running.take() {
                let _ = timeout(CLOSE_TIMEOUT, running.close).await;
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
        if running.as_ref().is_some_and(|r| r.status.borrow().state == opentunnel::State::Stopped) {
            *running = None;
        }
        if running.is_none() {
            let identity = timeout(PROVISION_TIMEOUT, self.sdk.ensure(PROFILE)).await
                .map_err(|_| "OpenTunnel provisioning timed out; retry to resume it.")?
                .map_err(|error| sdk_error("provisioning", error))?;
            let endpoint = public_endpoint(&identity.hostname)?;
            let tunnel = self.sdk.connect(PROFILE, [("@".into(), self.target.to_string().into())].into())
                .map_err(|error| sdk_error("connection setup", error))?;
            *running = Some(Running { status: tunnel.status_watch(), close: Box::pin(tunnel.close()), endpoint });
        }
        let mut status = running.as_ref().unwrap().status.clone();
        let ready = timeout(CONNECT_TIMEOUT, status.wait_for(|status| {
            matches!(status.state, opentunnel::State::Connected | opentunnel::State::Stopped)
        })).await;
        match ready {
            Ok(Ok(status)) if status.state == opentunnel::State::Connected => Ok(running.as_ref().unwrap().endpoint.clone()),
            Ok(_) => Err("OpenTunnel stopped before becoming ready; retry sharing to reconnect.".into()),
            Err(_) => Err("OpenTunnel connection timed out; retry once the tunnel reconnects.".into()),
        }
    }

    async fn share(&self, request: ShareRequest, reuse: bool) -> Result<InviteFile, (StatusCode, String)> {
        let _operation = self.operation.lock().await;
        validate_share(&request.project, &request.actor).map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        let board = self.board.clone();
        let token = self.board_token.clone();
        let prefix = format!("{}/", request.project);
        let observed = tokio::task::spawn_blocking(move || {
            let snapshot = board.snapshot(&token)?;
            Ok(snapshot.channels.iter().any(|channel| channel.starts_with(&prefix)).then_some(snapshot.cursor))
        }).await.map_err(|_| internal_error())?.map_err(board_error)?;
        let Some(observed) = observed else {
            return Err((StatusCode::NOT_FOUND, "Unknown board; create it before sharing.".into()));
        };
        let endpoint = self.endpoint().await.map_err(|error| (StatusCode::BAD_GATEWAY, error))?;
        match std::fs::remove_file(self.dir.join("share-disabled")) {
            Ok(()) => (), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(_) => return Err(internal_error()),
        }
        self.invite_for_endpoint(endpoint, request, reuse, observed).await
    }

    async fn invite_for_endpoint(&self, endpoint: String, request: ShareRequest, reuse: bool, observed: u64) -> Result<InviteFile, (StatusCode, String)> {
        if reuse {
            if let Some(invite) = self.lookup(&request.project, &endpoint)? { return Ok(invite); }
        }
        let invite = self.issue_invite(endpoint, request, reuse, observed).await?;
        self.remember(&invite).map_err(|_| internal_error())?;
        Ok(invite)
    }

    async fn issue_invite(&self, endpoint: String, request: ShareRequest, shared: bool, observed: u64) -> Result<InviteFile, (StatusCode, String)> {
        let board = self.board.clone();
        let admin = self.admin_token.clone();
        let expires_at = now() + 86_400;
        let scope = format!("{}/*", request.project);
        let actor = request.actor.clone();
        let project = request.project.clone();
        // The desktop link is handed to several people; a named CLI invite is one actor.
        let invite = tokio::task::spawn_blocking(move || {
            let request = InviteRequest { actor, channels: vec![scope], expires_at: Some(expires_at) };
            board.issue_checked(&admin, request, None, shared, Some((&project, observed)))
        }).await.map_err(|_| internal_error())?.map_err(board_error)?;

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

    #[cfg(unix)]
    fn cache_path(&self, project: &str) -> PathBuf {
        self.dir.join(format!("share-guest-{:x}.json", Sha256::digest(project.as_bytes())))
    }

    fn remember(&self, invite: &InviteFile) -> std::io::Result<()> {
        let project = invite.project.as_deref().ok_or(std::io::ErrorKind::InvalidData)?;
        #[cfg(unix)] {
            // NamedTempFile creates mode 0600; atomic replacement never follows a destination symlink.
            let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)?;
            serde_json::to_writer(&mut tmp, invite)?;
            tmp.as_file().sync_all()?;
            tmp.persist(self.cache_path(project))?;
        }
        #[cfg(not(unix))]
        self.cache.lock().map_err(|_| std::io::ErrorKind::Other)?.insert(project.into(), invite.clone());
        Ok(())
    }

    fn lookup(&self, project: &str, endpoint: &str) -> Result<Option<InviteFile>, (StatusCode, String)> {
        #[cfg(unix)]
        let invite = match private_record(&self.cache_path(project)) {
            Ok(data) => serde_json::from_slice::<InviteFile>(&data).ok(),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData) => None,
            Err(_) => return Err(internal_error()),
        };
        #[cfg(not(unix))]
        let invite = self.cache.lock().map_err(|_| internal_error())?.get(project).cloned();
        let Some(invite) = invite else { return Ok(None); };
        if invite.version == 1 && invite.project.as_deref() == Some(project) && invite.endpoint == endpoint
            && self.board.reusable_guest(&invite.token, project, invite.expires_at).map_err(board_error)? {
            Ok(Some(invite))
        } else { Ok(None) }
    }

    async fn status(&self) -> RouteStatus {
        use opentunnel::State::*;
        let guard = self.running.lock().await;
        let route = guard.as_ref();
        let state = match route.map(|r| r.status.borrow().state.clone()) {
            Some(Connected) => RouteState::Connected,
            Some(Connecting | WaitingRoutes) => RouteState::Connecting,
            Some(Reconnecting) => RouteState::Reconnecting,
            _ => RouteState::Stopped,
        };
        RouteStatus { state, endpoint: route.filter(|_| state != RouteState::Stopped).map(|r| r.endpoint.clone()) }
    }
}

#[cfg(unix)]
fn private_record(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::{io::{ErrorKind, Read}, os::unix::fs::{MetadataExt, OpenOptionsExt}};
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
        .map_err(|e| if e.raw_os_error() == Some(libc::ELOOP) { ErrorKind::InvalidData.into() } else { e })?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } { return Err(ErrorKind::InvalidData.into()); }
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 { return Err(ErrorKind::InvalidData.into()); }
    Ok(bytes)
}

#[cfg(not(unix))]
fn private_record(path: &Path) -> std::io::Result<Vec<u8>> { std::fs::read(path) }

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
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}

async fn handle_share(State(state): State<Arc<Sharing>>, Json(request): Json<ShareRequest>) -> Response {
    share_response(&state, request, false).await
}

async fn handle_desktop(State(state): State<Arc<Sharing>>, Json(selection): Json<BoardSelection>) -> Response {
    let request = ShareRequest { project: selection.project, actor: format!("guest-{}", crate::board::random_token()) };
    share_response(&state, request, true).await
}

async fn handle_status(State(state): State<Arc<Sharing>>) -> Json<RouteStatus> { Json(state.status().await) }

async fn handle_stop(State(state): State<Arc<Sharing>>) -> Result<Json<()>, StatusCode> {
    let _operation = state.operation.lock().await;
    // Preserve the SDK identity and local board; inhibit auto-resume until the next explicit share.
    let saved = tempfile::NamedTempFile::new_in(&state.dir).and_then(|f| Ok(f.persist(state.dir.join("share-disabled"))?));
    let mut running = state.running.lock().await;
    if let Some(r) = running.take() {
        r.close.await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    }
    saved.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(()))
}

async fn handle_lookup(State(state): State<Arc<Sharing>>, Json(selection): Json<BoardSelection>) -> Response {
    if !valid_project(&selection.project) { return StatusCode::BAD_REQUEST.into_response(); }
    let _operation = state.operation.lock().await;
    let status = state.status().await;
    if let Some(endpoint) = status.endpoint.filter(|_| status.state == RouteState::Connected) {
        match state.lookup(&selection.project, &endpoint) {
            Ok(invite) => return Json(invite).into_response(),
            Err((code, error)) => return (code, Json(ShareError { error })).into_response(),
        }
    }
    Json(None::<InviteFile>).into_response()
}

async fn share_response(state: &Sharing, request: ShareRequest, reuse: bool) -> Response {
    let result = match timeout(SHARE_TIMEOUT, state.share(request, reuse)).await {
        Ok(result) => result,
        Err(_) => Err((StatusCode::GATEWAY_TIMEOUT, "Sharing timed out; retry when the board and tunnel are ready.".into())),
    };
    match result {
        Ok(invite) => Json(invite).into_response(),
        Err((status, error)) => (status, Json(ShareError { error })).into_response(),
    }
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
    async fn stale_share_view_cannot_issue_after_delete_and_recreation() {
        let dir = tempfile::tempdir().unwrap();
        let sharing = service(dir.path());
        let observed = sharing.board.snapshot(HOST).unwrap().cursor;
        sharing.board.delete_board(ADMIN,"team").unwrap();
        sharing.board.action(HOST,None,None,Action::CreateChannel {channel:"team/general".into()}).unwrap();
        // Fail before probing a public endpoint or touching a real tunnel.
        let err = sharing.issue_invite("unused".into(), request(), true, observed).await.err().unwrap();
        assert_eq!(err.0,StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn invites_only_expose_the_selected_project() {
        let dir = tempfile::tempdir().unwrap();
        let sharing = service(dir.path());
        // Stand in for the SDK's forwarding leg; no public service is called.
        let (endpoint, task) = serve(crate::router(sharing.board.clone())).await;
        let invite = sharing.issue_invite(endpoint, request(), false, 0).await.unwrap();
        assert_eq!(invite.project.as_deref(), Some("team"));
        assert!(invite.expires_at > now() + 86_000);
        sharing.board.action(HOST, None, None, Action::CreateChannel { channel: "team/new".into() }).unwrap();
        assert_eq!(sharing.board.snapshot(&invite.token).unwrap().channels, ["team/general", "team/new", "team/updates"]);
        task.abort();
    }

    #[tokio::test]
    async fn desktop_reuses_but_explicit_actors_stay_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let sharing = service(dir.path());
        let (endpoint, task) = serve(crate::router(sharing.board.clone())).await;
        let first = sharing.invite_for_endpoint(endpoint.clone(), request(), false, 0).await.unwrap();
        // An invite issued to one named actor is never handed to everyone using the desktop link.
        let guest = |actor: &str| ShareRequest { actor: actor.into(), ..request() };
        let desktop = sharing.invite_for_endpoint(endpoint.clone(), guest("guest-a"), true, 0).await.unwrap();
        assert!(desktop.token != first.token);
        assert!(sharing.board.join(&first.token).is_err());
        assert!(sharing.board.join(&desktop.token).is_ok());
        let again = sharing.invite_for_endpoint(endpoint.clone(), guest("guest-b"), true, 0).await.unwrap();
        assert!(again.token == desktop.token);
        let writer = sharing.invite_for_endpoint(endpoint.clone(), guest("writer"), false, 0).await.unwrap();
        assert!(writer.token != first.token);
        assert!(sharing.lookup("team", &endpoint).unwrap().is_none());
        let second = sharing.invite_for_endpoint(endpoint.clone(), guest("guest-c"), true, 0).await.unwrap();
        assert!(second.token != desktop.token);
        assert!(sharing.lookup("team", &endpoint).unwrap().unwrap().token == second.token);
        assert!(sharing.lookup("team-sandbox", &endpoint).unwrap().is_none());
        assert!(sharing.lookup("team", "https://other.example/").unwrap().is_none());

        // A forged file scope or expiry cannot change the board's actual grants.
        let mut forged = second.clone();
        forged.project = Some("team-sandbox".into());
        sharing.remember(&forged).unwrap();
        assert!(sharing.lookup("team-sandbox", &endpoint).unwrap().is_none());
        forged = second.clone();
        forged.expires_at += 300;
        sharing.remember(&forged).unwrap();
        assert!(sharing.lookup("team", &endpoint).unwrap().is_none());
        forged.expires_at = now();
        sharing.remember(&forged).unwrap();
        assert!(sharing.lookup("team", &endpoint).unwrap().is_none());

        let record = sharing.board.create_shared_invite(ADMIN, InviteRequest { actor: "revocable".into(), channels: vec!["team/*".into()], expires_at: Some(second.expires_at) }).unwrap();
        let revocable = InviteFile { token: record.token, ..second };
        sharing.remember(&revocable).unwrap();
        assert!(sharing.lookup("team", &endpoint).unwrap().is_some());
        sharing.board.revoke_invite(ADMIN, record.id).unwrap();
        assert!(sharing.lookup("team", &endpoint).unwrap().is_none());
        task.abort();
    }

    #[cfg(unix)]
    #[test]
    fn cache_recovers_only_owner_private_regular_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let sharing = service(dir.path());
        let expiry = now() + 600;
        let guest = sharing.board.create_shared_invite(ADMIN, InviteRequest { actor: "reviewer".into(), channels: vec!["team/*".into()], expires_at: Some(expiry) }).unwrap();
        let endpoint = "https://fixture.example/";
        sharing.remember(&InviteFile { version: 1, project: Some("team".into()), endpoint: endpoint.into(), expires_at: expiry, token: guest.token }).unwrap();
        let path = sharing.cache_path("team");
        let restarted = Sharing::new(dir.path(), sharing.target, sharing.board.clone(), HOST.into(), ADMIN.into(), "control").unwrap();
        assert!(restarted.lookup("team", endpoint).unwrap().is_some());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(restarted.lookup("team", endpoint).unwrap().is_none());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let other = dir.path().join("other");
        std::fs::rename(&path, &other).unwrap();
        symlink(other, path).unwrap();
        assert!(restarted.lookup("team", endpoint).unwrap().is_none());
    }

    #[tokio::test]
    async fn host_management_is_not_on_board_and_disconnect_preserves_board() {
        let dir = tempfile::tempdir().unwrap();
        let sharing = service(dir.path());
        let (control, control_task) = serve(sharing.router()).await;
        let (board, board_task) = serve(crate::router(sharing.board.clone())).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for path in [STOP, STATUS, LOOKUP, DESKTOP] {
            let url = |origin: &str| format!("{}{path}", origin.trim_end_matches('/'));
            assert_eq!(client.post(url(&board)).bearer_auth("control").send().await.unwrap().status(), StatusCode::NOT_FOUND);
            assert_eq!(client.post(url(&control)).bearer_auth(HOST).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        }
        let addr = control.trim_start_matches("http://").trim_end_matches('/').parse().unwrap();
        let mut file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        serde_json::to_writer(&mut file, &ControlDiscovery { addr, token: "control".into() }).unwrap();
        file.persist(dir.path().join(CONTROL_FILE)).unwrap();
        // Inject just the SDK boundary, never a live route. The host control calls
        // below use the real router, discovery record, lifecycle lock and board.
        let endpoint = "https://fixture.example/";
        let (_tx, status) = watch::channel(opentunnel::Status {
            state: opentunnel::State::Connected, hostname: "fixture.example".into(), routes: Default::default(),
            session: None, connections: 0, last_error: None, connected_at: None,
        });
        let (closed, mut observed) = tokio::sync::oneshot::channel();
        *sharing.running.lock().await = Some(Running { status, endpoint: endpoint.into(), close: Box::pin(async move {
            let _ = closed.send(()); Ok(())
        }) });
        let data_dir = dir.path().to_owned();
        tokio::task::spawn_blocking(move || {
            let route = status_local(Some(&data_dir)).unwrap();
            assert_eq!(route.state, RouteState::Connected);
            assert_eq!(route.endpoint.as_deref(), Some(endpoint));
            assert!(lookup_local(Some(&data_dir), "team").unwrap().is_none());
            stop_local(Some(&data_dir)).unwrap();
            assert_eq!(status_local(Some(&data_dir)).unwrap().state, RouteState::Stopped);
        }).await.unwrap();
        observed.try_recv().unwrap();
        assert!(dir.path().join("share-disabled").exists());
        assert!(sharing.board.snapshot(HOST).unwrap().channels.contains(&"team/general".to_owned()));
        control_task.abort(); board_task.abort();
    }

    #[test]
    fn public_origin_cannot_include_credentials_paths_or_ports() {
        assert_eq!(public_endpoint("board.opentunnel.xyz").unwrap(), "https://board.opentunnel.xyz/");
        for invalid in ["localhost", "other/path", "x@public.example", "board.example:443", "board.example#secret"] {
            assert!(public_endpoint(invalid).is_err());
        }
    }
}