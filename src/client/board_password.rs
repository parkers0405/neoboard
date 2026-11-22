use super::{blocking, Client, ClientError, Duration, Url};
use crate::invite_file::{https_endpoint, InviteFile};
use crate::protocol::Invite;
use serde::Serialize;
use std::io::Read;

pub const MAX_BOARD_PASSWORD_BYTES: usize = 1024;

#[derive(Serialize)]
struct PasswordRequest<'a> {
    project: &'a str,
    password: Option<&'a str>,
}

fn password_request<'a>(project: &'a str, password: Option<&'a str>) -> Result<PasswordRequest<'a>, ClientError> {
    if !crate::valid_project(project) {
        return Err(ClientError("invalid board project".into()));
    }
    if password.is_some_and(|p| p.is_empty() || p.len() > MAX_BOARD_PASSWORD_BYTES) {
        return Err(ClientError("board password must be 1 to 1024 UTF-8 bytes".into()));
    }
    Ok(PasswordRequest { project, password })
}

// Unlike ordinary API errors, neither the URL nor arbitrary server text may
// reach the caller here: either could contain submitted or returned credentials.
fn send_password(request: blocking::RequestBuilder) -> Result<blocking::Response, ClientError> {
    let response = request.send().map_err(|_| ClientError("could not reach board password service".into()))?;
    if !response.status().is_success() {
        return Err(ClientError(format!("board password request failed (HTTP {})", response.status().as_u16())));
    }
    Ok(response)
}

impl Client {
    /// Create a board and enable password joins in one local admin operation.
    pub fn init_board(&self, project: &str, password: &str) -> Result<(), ClientError> {
        if !super::is_loopback(&self.endpoint) { return Err(ClientError("board initialization requires a local daemon".into())); }
        let body = password_request(project, Some(password))?;
        let response = self.http.post(self.url("v1/admin/init-board")).json(&body).send()
            .map_err(|_| ClientError("could not reach local board daemon".into()))?;
        match response.status().as_u16() {
            204 => Ok(()),
            409 => Err(ClientError("board already exists; init did not change it".into())),
            code => Err(ClientError(format!("board initialization failed (HTTP {code})"))),
        }
    }

    /// Set/rotate a project's join password, or disable password joins with `None`.
    pub fn set_board_password(&self, project: &str, password: Option<&str>) -> Result<(), ClientError> {
        let body = password_request(project, password)?;
        send_password(self.http.post(self.url("v1/admin/board-password")).json(&body))?;
        Ok(())
    }
}

/// Exchange a board password over HTTPS for a personal invite. Blocking; call on
/// the GUI network worker. Does not discover credentials, follow redirects or persist anything.
pub fn join_with_password(endpoint: &str, project: &str, password: &str) -> Result<InviteFile, ClientError> {
    let body = password_request(project, Some(password))?;
    let endpoint = https_endpoint(endpoint).map_err(|e| ClientError(e.into()))?;
    let response = send_password(join_request(&endpoint, &body)?)?;
    personal_invite(response, endpoint, project)
}

fn join_request(endpoint: &str, body: &PasswordRequest<'_>) -> Result<blocking::RequestBuilder, ClientError> {
    let http = blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(35))
        .build().map_err(|_| ClientError("could not initialize board password client".into()))?;
    let url = Url::parse(&format!("{}/v1/join/password", endpoint.trim_end_matches('/')))
        .map_err(|_| ClientError("invalid board endpoint".into()))?;
    Ok(http.post(url).json(body))
}

fn personal_invite(reader: impl Read, endpoint: String, project: &str) -> Result<InviteFile, ClientError> {
    let invalid = || ClientError("invalid personal invite from board".into());
    let mut bytes = Vec::new();
    reader.take(8193).read_to_end(&mut bytes).map_err(|_| invalid())?;
    if bytes.len() > 8192 { return Err(invalid()); }
    let invite: Invite = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if invite.channels != [format!("{project}/*")] { return Err(invalid()); }
    let mut file = InviteFile {
        version: 1, endpoint, project: Some(project.into()), token: invite.token,
        expires_at: invite.expires_at.unwrap_or(i64::MAX as u64),
    }.validate().map_err(|_| invalid())?;
    // As with desktop invite exchanges, a personal membership's expiry rolls
    // forward on the server. Don't make a stored invite expire prematurely.
    file.expires_at = i64::MAX as u64;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;
    use serde_json::{json, Value};

    #[test]
    fn password_join_rejects_unsafe_endpoints_before_io() {
        for endpoint in ["http://127.0.0.1/", "https://secret@board.invalid/", "https://board.invalid/?secret", "https://board.invalid/#secret"] {
            let err = join_with_password(endpoint, "team", "secret").err().unwrap();
            assert!(!format!("{err:?}").contains("secret"));
        }
        assert!(password_request("team/*", Some("secret")).is_err());
        assert!(password_request("team", Some("")).is_err());
    }

    #[test]
    fn password_join_posts_only_json_to_prefixed_api() {
        let request = join_request("https://board.invalid/base/", &password_request("team", Some("secret")).unwrap()).unwrap().build().unwrap();
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.url().as_str(), "https://board.invalid/base/v1/join/password");
        assert!(!request.headers().contains_key(AUTHORIZATION));
        assert_eq!(serde_json::from_slice::<Value>(request.body().unwrap().as_bytes().unwrap()).unwrap(), json!({"project":"team","password":"secret"}));
    }

    #[test]
    fn password_join_accepts_only_valid_personal_board_invite() {
        let endpoint = "https://board.invalid/base/";
        let raw = json!({"id": 1, "actor":"member", "channels":["team/*"], "token":"private", "expires_at":null});
        let decode = |v: &Value| personal_invite(&serde_json::to_vec(v).unwrap()[..], endpoint.into(), "team");
        let file = decode(&raw).unwrap();
        assert_eq!(file.version, 1);
        assert_eq!(file.project.as_deref(), Some("team"));
        assert_eq!(file.endpoint, endpoint);
        assert_eq!(file.token, "private");
        for (field, bad) in [("channels",json!([])), ("channels",json!(["other/*"])), ("token",json!(" private")), ("expires_at",json!(1)), ("expires_at",json!("private"))] {
            let mut reply = raw.clone();
            reply[field] = bad;
            assert_eq!(decode(&reply).err().unwrap().to_string(), "invalid personal invite from board");
        }
    }
}