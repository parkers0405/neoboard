//! Passwords admit new project members; they are never bearer credentials.
use super::*;
use axum::extract::Request;
use ring::pbkdf2::{self, PBKDF2_HMAC_SHA256};
use serde::de::DeserializeOwned;
use std::{collections::VecDeque, num::NonZeroU32, time::Instant};
use tokio::sync::Semaphore;

const ROUNDS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_BODY_BYTES: usize = 8192; // includes JSON escape overhead
const JOIN_BURST: usize = 12;
const JOIN_WINDOW: Duration = Duration::from_secs(60);

// One budget for the whole daemon, regardless of project, socket or forwarding
// headers. Reserve a separate admin slot so public traffic cannot block rotation.
pub(super) struct Admission {
    joins: Arc<Semaphore>,
    admin: Arc<Semaphore>,
    recent: Mutex<VecDeque<Instant>>,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            joins: Arc::new(Semaphore::new(2)),
            admin: Arc::new(Semaphore::new(1)),
            recent: Mutex::default(),
        }
    }
}

fn busy() -> BoardError { BoardError(StatusCode::TOO_MANY_REQUESTS, "too many password requests; try again later".into()) }
fn denied() -> BoardError { BoardError(StatusCode::UNAUTHORIZED, "invalid project or password".into()) }

impl Admission {
    fn charge(&self, at: Instant) -> Result<()> {
        let mut recent = self.recent.lock().map_err(|_| busy())?;
        while recent.front().is_some_and(|t| at.duration_since(*t) >= JOIN_WINDOW) { recent.pop_front(); }
        if recent.len() == JOIN_BURST { return Err(busy()); }
        recent.push_back(at);
        Ok(())
    }
}

pub(super) fn routes() -> Router<Board> {
    Router::new()
        .route("/v1/admin/init-board", post(init))
        .route("/v1/admin/board-password", post(set))
        .route("/v1/join/password", post(join))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Setting {
    project: String,
    // Missing is an error; only an explicit JSON null disables the password.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    password: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt { project: String, password: String }

async fn body<T: DeserializeOwned>(request: Request) -> Result<T> {
    let malformed = || invalid("invalid password request");
    let json = request.headers().get("content-type").and_then(|h| h.to_str().ok())
        .and_then(|mime| mime.split(';').next()).is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
    if !json { return Err(malformed()); }
    let bytes = tokio::time::timeout(Duration::from_secs(5), axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES))
        .await.map_err(|_| malformed())?.map_err(|_| malformed())?;
    // Never echo or trace serde diagnostics; they can quote client-supplied secrets.
    serde_json::from_slice(&bytes).map_err(|_| malformed())
}

fn valid_password(password: &str) -> bool { !password.is_empty() && password.len() <= MAX_PASSWORD_BYTES }

async fn init(State(board): State<Board>, request: Request) -> Result<StatusCode> {
    if digest(&bearer(request.headers())?) != board.admin_hash { return Err(unauthorized()); }
    let attempt: Attempt = body(request).await?;
    if !crate::valid_project(&attempt.project) || !valid_password(&attempt.password) {
        return Err(invalid("invalid project or password length"));
    }
    // Serialize admin KDFs; a simultaneous init can observe the winner's commit.
    let slot = tokio::time::timeout(Duration::from_secs(5), board.passwords.admin.clone().acquire_owned())
        .await.map_err(|_| busy())?.map_err(|_| busy())?;
    blocking(move || {
        let _slot = slot;
        board.init_board(attempt)
    }).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set(State(board): State<Board>, request: Request) -> Result<StatusCode> {
    if digest(&bearer(request.headers())?) != board.admin_hash { return Err(unauthorized()); }
    let slot = board.passwords.admin.clone().try_acquire_owned().map_err(|_| busy())?;
    let setting: Setting = body(request).await?;
    if !crate::valid_project(&setting.project) || setting.password.as_deref().is_some_and(|p| !valid_password(p)) {
        return Err(invalid("invalid project or password length"));
    }
    blocking(move || {
        let _slot = slot; // still held if client disconnects during spawn_blocking
        board.set_password(setting)
    }).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn join(State(board): State<Board>, request: Request) -> Result<impl IntoResponse> {
    let slot = board.passwords.joins.clone().try_acquire_owned().map_err(|_| busy())?;
    board.passwords.charge(Instant::now())?;
    let attempt: Attempt = body(request).await.map_err(|_| denied())?;
    if !crate::valid_project(&attempt.project) || !valid_password(&attempt.password) { return Err(denied()); }
    let member = blocking(move || {
        let _slot = slot;
        board.password_join(attempt)
    }).await?;
    Ok(([("cache-control", "no-store")], Json(member)))
}

pub(super) fn project_exists(db: &Connection, project: &str) -> Result<bool> {
    // Match sharing's existing-project check: at least one project/ channel.
    // A binary prefix range uses the primary key without SQL LIKE metacharacters.
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM channels WHERE name>=?1 AND name<?2)",
        params![format!("{project}/"), format!("{project}0")], |r| r.get(0))?)
}

struct Verifier { salt: Vec<u8>, key: Vec<u8> }

impl Verifier {
    fn derive(password: String) -> Self {
        let mut salt = vec![0; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let mut key = vec![0; 32];
        pbkdf2::derive(PBKDF2_HMAC_SHA256, ROUNDS, &salt, password.as_bytes(), &mut key);
        Self { salt, key }
    }
}

impl Board {
    fn init_board(&self, Attempt { project, password }: Attempt) -> Result<()> {
        let channel = format!("{project}/general");
        validate_channel(&channel)?;
        let verifier = Verifier::derive(password); // outside the writer lock
        let mut db = self.lock()?;
        let tx = db.transaction()?; // BEGIN IMMEDIATE: check and both inserts share the writer
        // project/* grants also expose the bare root and board-wide metadata, even
        // when no nested channel exists. Never adopt such a namespace via init.
        if project_exists(&tx, &project)? || tx.query_row("SELECT EXISTS(SELECT 1 FROM channels WHERE name=?1)
            OR EXISTS(SELECT 1 FROM areas WHERE board=?1) OR EXISTS(SELECT 1 FROM sprints WHERE board=?1)
            OR EXISTS(SELECT 1 FROM messages WHERE channel=?1)", [&project], |r| r.get::<_, bool>(0))? {
            return Err(conflict("board already exists"));
        }
        insert_channel(&tx, "local", &channel)?;
        if tx.execute("INSERT INTO board_passwords VALUES (?1,?2,?3) ON CONFLICT(project) DO NOTHING",
            params![project, verifier.salt, verifier.key])? == 0 {
            return Err(conflict("board already exists"));
        }
        tx.commit()?;
        Ok(())
    }

    fn set_password(&self, setting: Setting) -> Result<()> {
        let Setting { project, password } = setting;
        let require_project = |db: &Connection| -> Result<()> {
            if project_exists(db, &project)? { Ok(()) } else { Err(BoardError(StatusCode::NOT_FOUND, "unknown project".into())) }
        };
        { let db = self.lock()?; require_project(&db)?; }
        let verifier = password.map(Verifier::derive);
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        require_project(&tx)?;
        if let Some(v) = verifier {
            tx.execute("INSERT INTO board_passwords VALUES (?1,?2,?3) ON CONFLICT(project) DO UPDATE SET salt=excluded.salt,pbkdf2_sha256_600k=excluded.pbkdf2_sha256_600k",
                params![project, v.salt, v.key])?;
        } else {
            tx.execute("DELETE FROM board_passwords WHERE project=?1", [project])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn verifier(&self, project: &str) -> Result<Option<Verifier>> {
        Ok(self.lock()?.query_row("SELECT salt,pbkdf2_sha256_600k FROM board_passwords WHERE project=?1", [project],
            |r| Ok(Verifier { salt: r.get(0)?, key: r.get(1)? })).optional()?)
    }

    fn password_join(&self, attempt: Attempt) -> Result<Invite> {
        let stored = self.verifier(&attempt.project)?;
        // Same KDF cost and error for unknown, disabled and wrong passwords.
        let (salt, key) = stored.as_ref().map(|v| (v.salt.as_slice(), v.key.as_slice())).unwrap_or((&[0; 16], &[0; 32]));
        pbkdf2::verify(PBKDF2_HMAC_SHA256, ROUNDS, salt, attempt.password.as_bytes(), key).map_err(|_| denied())?;
        self.admit_verified(&attempt.project, stored.as_ref().ok_or_else(denied)?)
    }

    fn admit_verified(&self, project: &str, verified: &Verifier) -> Result<Invite> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        // The random salt identifies this setting, even on rotation to the same
        // password. Recheck and mint in one write transaction, never across a KDF.
        let current: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM board_passwords WHERE project=?1 AND salt=?2 AND pbkdf2_sha256_600k=?3)",
            params![project, verified.salt, verified.key], |r| r.get(0))?;
        if !current || !project_exists(&tx, project)? { return Err(denied()); }
        let member = Self::personal_credential(&tx, vec![format!("{project}/*")])?;
        tx.commit()?;
        Ok(member)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_verifier_conflict_rolls_back_channel_and_event() {
        let dir = tempfile::tempdir().unwrap();
        let host = random_token();
        let board = Board::open(dir.path().join("test.db"), &host, &random_token()).unwrap();
        // Force a conflict on the final write, after the channel and event inserts.
        let old = Verifier::derive("previous-secret".into());
        board.lock().unwrap().execute("INSERT INTO board_passwords VALUES ('pending',?1,?2)",
            params![old.salt.clone(), old.key.clone()]).unwrap();
        let error = board.init_board(Attempt { project: "pending".into(), password: "different".into() }).unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(board.snapshot(&host).unwrap().channels, ["general"]);
        assert!(board.events(&host, 0).unwrap().is_empty());
        let unchanged = board.verifier("pending").unwrap().unwrap();
        assert_eq!((unchanged.salt, unchanged.key), (old.salt, old.key));
    }

    #[test]
    fn sliding_window_expires_without_extending_lockout_on_rejection() {
        let admission = Admission::default();
        let start = Instant::now();
        for _ in 0..JOIN_BURST { admission.charge(start).unwrap(); }
        assert_eq!(admission.charge(start + JOIN_WINDOW / 2).unwrap_err().0, StatusCode::TOO_MANY_REQUESTS);
        admission.charge(start + JOIN_WINDOW).unwrap();
    }

    #[test]
    fn canceled_join_requests_still_bound_the_blocking_workers() {
        let dir = tempfile::tempdir().unwrap();
        let board = Board::open(dir.path().join("test.db"), &random_token(), &random_token()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let request = || Request::builder().header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"project":"missing","password":"attempt"}"#)).unwrap();

        // Block both workers at their first DB read, not on KDF timing. Dropping
        // each pending request models a client disconnect after the worker starts.
        let guard = board.lock().unwrap();
        rt.block_on(async {
            for _ in 0..2 {
                tokio::select! { biased;
                    _ = join(State(board.clone()), request()) => panic!("worker should be blocked"),
                    _ = std::future::ready(()) => ()
                }
            }
            let result = join(State(board.clone()), request()).await;
            assert!(matches!(result, Err(BoardError(StatusCode::TOO_MANY_REQUESTS, _))));
            assert_eq!(board.passwords.admin.available_permits(), 1);
        });
        drop(guard);
        drop(rt); // waits for detached blocking workers
        assert_eq!(board.passwords.joins.available_permits(), 2);
    }

    #[test]
    fn stale_verification_cannot_mint_after_rotation_disable_or_project_recreation() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let (host, admin) = (random_token(), random_token());
        let board = Board::open(&db_path, &host, &admin).unwrap();
        let channel = || Action::CreateChannel { channel: "team/chat".into() };
        board.action(&host, None, None, channel()).unwrap();
        let secret = "fixture-password-not-for-persistence";
        let setting = |enabled| Setting { project: "team".into(), password: if enabled { Some(secret.into()) } else { None } };
        board.set_password(setting(true)).unwrap();
        drop(board);
        let board = Board::open(&db_path, &host, &admin).unwrap();
        let first = board.verifier("team").unwrap().unwrap();
        pbkdf2::verify(PBKDF2_HMAC_SHA256, ROUNDS, &first.salt, secret.as_bytes(), &first.key).unwrap();
        board.set_password(setting(true)).unwrap(); // even the same password has a new salt
        let second = board.verifier("team").unwrap().unwrap();
        assert_ne!(first.salt, second.salt);
        assert_ne!(first.key, second.key);
        assert_eq!(board.admit_verified("team", &first).unwrap_err().0, StatusCode::UNAUTHORIZED);
        board.set_password(setting(false)).unwrap();
        assert_eq!(board.admit_verified("team", &second).unwrap_err().0, StatusCode::UNAUTHORIZED);
        board.set_password(setting(true)).unwrap();
        let third = board.verifier("team").unwrap().unwrap();
        board.action(&host, None, None, Action::DeleteChannel { channel: "team/chat".into() }).unwrap();
        board.action(&host, None, None, channel()).unwrap();
        assert!(board.verifier("team").unwrap().is_none());
        assert_eq!(board.admit_verified("team", &third).unwrap_err().0, StatusCode::UNAUTHORIZED);
        let n: u64 = board.lock().unwrap().query_row("SELECT count(*) FROM credentials", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1); // only bootstrap; stale attempts didn't create identities
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                assert!(!std::fs::read(path).unwrap().windows(secret.len()).any(|w| w == secret.as_bytes()));
            }
        }
    }
}