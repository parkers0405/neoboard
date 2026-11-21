//! Embedded Turso is the single writer authority. Mutations, derived task transitions and events share a transaction.
use crate::protocol::*;
use axum::{extract::{Path, Query, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::{delete, get, post}, Json, Router};
use rand::RngCore;
use crate::{params, storage::{Connection, OptionalExtension, Transaction}};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, path::Path as FsPath, sync::{Arc, Mutex}, time::{Duration, SystemTime, UNIX_EPOCH}};

#[derive(Debug)]
pub struct BoardError(pub StatusCode, pub String);
impl std::fmt::Display for BoardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.1) }
}
impl std::error::Error for BoardError {}
impl From<turso::Error> for BoardError {
    fn from(e: turso::Error) -> Self { Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()) }
}
impl From<serde_json::Error> for BoardError {
    fn from(e: serde_json::Error) -> Self { Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()) }
}
impl IntoResponse for BoardError {
    fn into_response(self) -> Response {
        let message = if self.0.is_server_error() { "internal board error".to_string() } else { self.1 };
        (self.0, Json(ApiError { error: message })).into_response()
    }
}
type Result<T> = std::result::Result<T, BoardError>;
fn invalid(s: &str) -> BoardError { BoardError(StatusCode::BAD_REQUEST, s.into()) }
fn conflict(s: &str) -> BoardError { BoardError(StatusCode::CONFLICT, s.into()) }
fn forbidden() -> BoardError { BoardError(StatusCode::FORBIDDEN, "credential does not permit this operation".into()) }
fn unauthorized() -> BoardError { BoardError(StatusCode::UNAUTHORIZED, "invalid or expired bearer credential".into()) }
pub fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
pub fn random_token() -> String {
    let mut bytes = [0u8; 32]; rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
/// Bounds how many identities one leaked link can mint.
const MAX_INVITE_MEMBERS: u64 = 10_000;
/// A member stays the same actor for as long as they keep coming back. The link they
/// joined with only admits new people and expires on its own, much sooner.
const MEMBER_LIFETIME: u64 = 30 * 86_400;
fn digest(token: &str) -> String { format!("{:x}", Sha256::digest(token.as_bytes())) }

#[derive(Clone)]
pub struct Board { db: Arc<Mutex<Connection>>, admin_hash: String }
#[derive(Clone)]
struct Identity { id: u64, actor: String, channels: Vec<String> }
impl Identity {
    fn permits(&self, channel: &str) -> bool {
        self.channels.is_empty() || self.channels.iter().any(|c| c == channel || project_scope(c).is_some_and(|project| {
            channel == project || channel.strip_prefix(project).is_some_and(|suffix| suffix.starts_with('/'))
        }))
    }
    fn check(&self, channel: &str) -> Result<()> { if self.permits(channel) { Ok(()) } else { Err(forbidden()) } }
    fn boards(&self) -> BTreeSet<&str> {
        self.channels.iter().map(|scope| board_namespace(scope)).collect()
    }
    fn permits_board(&self, board: &str) -> bool {
        self.channels.is_empty() || self.channels.iter().any(|scope| board_namespace(scope) == board)
    }
}

impl Board {
    /// The two bootstrap tokens must be distinct. The admin token is never a board credential.
    pub fn open(path: impl AsRef<FsPath>, board_token: &str, admin_token: &str) -> Result<Self> {
        if board_token.len() < 32 || admin_token.len() < 32 || board_token == admin_token { return Err(invalid("distinct strong bootstrap tokens required")); }
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        let mode: String = db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") { return Err(BoardError(StatusCode::INTERNAL_SERVER_ERROR, format!("unexpected journal mode: {mode}"))); }
        db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        if !db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_,bool>(0))? {
            return Err(BoardError(StatusCode::INTERNAL_SERVER_ERROR, "foreign key enforcement unavailable".into()));
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS channels(name TEXT PRIMARY KEY);
            INSERT OR IGNORE INTO channels VALUES ('general');
            CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, channel TEXT NOT NULL, creator TEXT NOT NULL, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events(cursor INTEGER PRIMARY KEY AUTOINCREMENT, channel TEXT NOT NULL, body TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS event_channels ON events(channel,cursor);
            CREATE TABLE IF NOT EXISTS credentials(id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE NOT NULL, actor TEXT UNIQUE NOT NULL, channels TEXT NOT NULL, expires INTEGER, revoked INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS actor_display_names(credential INTEGER PRIMARY KEY REFERENCES credentials(id), name TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS credential_grants(credential INTEGER PRIMARY KEY REFERENCES credentials(id), grants TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS shared_invites(credential INTEGER PRIMARY KEY REFERENCES credentials(id));
            CREATE TABLE IF NOT EXISTS invite_members(credential INTEGER PRIMARY KEY REFERENCES credentials(id), invite INTEGER NOT NULL REFERENCES credentials(id));
            CREATE TABLE IF NOT EXISTS task_blocks(message INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE, actor TEXT NOT NULL, reason TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS areas(board TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(board,name));
            CREATE TABLE IF NOT EXISTS sprints(id INTEGER PRIMARY KEY AUTOINCREMENT, board TEXT NOT NULL, title TEXT NOT NULL, description TEXT NOT NULL, finished INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS task_sprints(message INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE, sprint INTEGER NOT NULL REFERENCES sprints(id));
            CREATE INDEX IF NOT EXISTS task_sprints_by_sprint ON task_sprints(sprint);
            CREATE TABLE IF NOT EXISTS area_owners(board TEXT NOT NULL, name TEXT NOT NULL, owner TEXT NOT NULL REFERENCES credentials(actor),
                PRIMARY KEY(board,name), FOREIGN KEY(board,name) REFERENCES areas(board,name) ON DELETE CASCADE);
            CREATE INDEX IF NOT EXISTS areas_by_owner ON area_owners(owner,board,name);
            CREATE TABLE IF NOT EXISTS task_placements(
                message INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
                area TEXT, parent INTEGER REFERENCES messages(id) ON DELETE RESTRICT,
                global INTEGER NOT NULL CHECK(global IN (0,1)),
                CHECK(global=0 OR (area IS NULL AND parent IS NULL)));
            CREATE INDEX IF NOT EXISTS task_placement_parents ON task_placements(parent);
            CREATE TABLE IF NOT EXISTS task_acknowledgements(
                message INTEGER NOT NULL REFERENCES task_placements(message) ON DELETE CASCADE,
                actor TEXT NOT NULL, PRIMARY KEY(message,actor));
            CREATE TABLE IF NOT EXISTS idempotency(credential INTEGER NOT NULL, key TEXT NOT NULL, request TEXT NOT NULL, outcome TEXT NOT NULL, PRIMARY KEY(credential,key));
            CREATE TABLE IF NOT EXISTS reactions(message INTEGER NOT NULL, actor TEXT NOT NULL, reaction TEXT NOT NULL, PRIMARY KEY(message,actor,reaction));
            CREATE TABLE IF NOT EXISTS attachments(id INTEGER PRIMARY KEY AUTOINCREMENT, message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE, name TEXT NOT NULL, bytes BLOB NOT NULL CHECK(length(bytes)<=8388608));
            CREATE INDEX IF NOT EXISTS attachment_messages ON attachments(message_id,id);
            CREATE TABLE IF NOT EXISTS schedules(id INTEGER PRIMARY KEY AUTOINCREMENT, credential INTEGER NOT NULL, actor TEXT NOT NULL, channel TEXT NOT NULL, text TEXT NOT NULL, labels TEXT NOT NULL, publish_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS schedule_senders(schedule INTEGER PRIMARY KEY REFERENCES schedules(id) ON DELETE CASCADE, sender TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS schedule_threads(schedule INTEGER PRIMARY KEY REFERENCES schedules(id) ON DELETE CASCADE, thread INTEGER NOT NULL REFERENCES messages(id), internal INTEGER NOT NULL, fired INTEGER NOT NULL DEFAULT 0);")?;
        db.execute("INSERT OR IGNORE INTO credentials(hash,actor,channels) VALUES (?1,'local','[]')", [digest(board_token)])?;
        // Older boards stored file metadata only in the attachment table.
        db.execute("UPDATE messages SET body=json_set(body,'$.attachments',json((
            SELECT json_group_array(json_object('id',id,'message_id',message_id,'name',name,'size',length(bytes)))
            FROM attachments WHERE message_id=messages.id
        ))) WHERE id IN (SELECT message_id FROM attachments) AND json_type(body,'$.attachments') IS NULL", [])?;
        Ok(Self { db: Arc::new(Mutex::new(db)), admin_hash: digest(admin_token) })
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.db.lock().map_err(|_| BoardError(StatusCode::INTERNAL_SERVER_ERROR, "board lock poisoned".into()))
    }
    fn identity(db: &Connection, token: &str, required: Option<Grant>) -> Result<Identity> {
        let hash = digest(token);
        let row = db.query_row("SELECT id,actor,channels,hash,expires FROM credentials WHERE hash IN (?1,?2) AND revoked=0 AND (expires IS NULL OR expires>?3)",
            params![hash,format!("restricted:{hash}"),now()], |r| Ok((r.get(0)?,r.get(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<u64>>(4)?))).optional()?;
        let (id,actor,channels,stored_hash,expires): (u64,String,String,String,Option<u64>) = row.ok_or_else(unauthorized)?;
        // Using a membership renews it, at most once a day, so only an abandoned one lapses.
        if expires.is_some_and(|at| at < now() + MEMBER_LIFETIME - 86_400) {
            db.execute("UPDATE credentials SET expires=?1 WHERE id=?2 AND id IN (SELECT credential FROM invite_members)", params![now() + MEMBER_LIFETIME, id])?;
        }
        if stored_hash != hash {
            let json: String = db.query_row("SELECT grants FROM credential_grants WHERE credential=?1", [id], |r| r.get(0))?;
            let allowed: BTreeSet<Grant> = serde_json::from_str(&json)?;
            if !required.is_some_and(|op| allowed.contains(&op)) { return Err(forbidden()); }
        }
        Ok(Identity { id, actor, channels: serde_json::from_str(&channels)? })
    }
    pub fn create_invite(&self, admin: &str, request: InviteRequest) -> Result<Invite> {
        self.issue(admin, request, None)
    }
    /// An invite several people may open. Each of them exchanges it with `join` for an
    /// identity of their own, so a display name or a task never belongs to all of them.
    pub fn create_shared_invite(&self, admin: &str, request: InviteRequest) -> Result<Invite> {
        let invite = self.issue(admin, request, None)?;
        self.lock()?.execute("INSERT INTO shared_invites VALUES (?1)", [sql_id(invite.id)?])?;
        Ok(invite)
    }
    /// Exchange a shared invite for a personal credential with the invite's scope. It
    /// outlives the invite, renewing while it is used. Revoking the invite revokes every
    /// credential issued from it.
    pub fn join(&self, token: &str) -> Result<Invite> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let invite = Self::identity(&tx, token, None)?;
        let shared: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM shared_invites WHERE credential=?1)", [invite.id], |r| r.get(0))?;
        if !shared { return Err(conflict("this credential already identifies one actor")); }
        let members: u64 = tx.query_row("SELECT COUNT(*) FROM invite_members m JOIN credentials c ON c.id=m.credential WHERE m.invite=?1 AND c.revoked=0", [invite.id], |r| r.get(0))?;
        if members >= MAX_INVITE_MEMBERS { return Err(BoardError(StatusCode::TOO_MANY_REQUESTS, "this invite has been joined too many times; ask the host for a new one".into())); }
        let expires = Some(now() + MEMBER_LIFETIME);
        let personal = random_token();
        let actor = format!("guest-{}", &random_token()[..16]);
        let channels = serde_json::to_string(&invite.channels)?;
        tx.execute("INSERT INTO credentials(hash,actor,channels,expires) VALUES (?1,?2,?3,?4)", params![digest(&personal),actor,channels,expires])?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute("INSERT INTO invite_members VALUES (?1,?2)", params![id,invite.id])?;
        tx.commit()?;
        Ok(Invite { id, token: personal, actor, channels: invite.channels, expires_at: expires })
    }
    /// Match an unrestricted guest bearer to exactly one board, including its authoritative lifetime.
    /// Only a shared invite is handed out again: one issued before `join` existed would
    /// keep putting everyone who opens it behind a single identity.
    pub(crate) fn reusable_guest(&self, token: &str, project: &str, expires: u64) -> Result<bool> {
        if expires <= now() || expires > i64::MAX as u64 { return Ok(false); }
        Ok(self.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM credentials WHERE hash=?1 AND actor NOT IN ('local','system') AND channels=?2 AND expires=?3 AND revoked=0 AND id IN (SELECT credential FROM shared_invites))",
            params![digest(token), serde_json::to_string(&[format!("{project}/*")])?, expires], |row| row.get(0))?)
    }
    fn issue(&self, admin: &str, request: InviteRequest, grants: Option<BTreeSet<Grant>>) -> Result<Invite> {
        if digest(admin) != self.admin_hash { return Err(unauthorized()); }
        validate_name(&request.actor)?;
        if request.actor == "local" || request.actor == "system" { return Err(invalid("reserved actor")); }
        if request.expires_at.is_some_and(|v| v <= now() || v > i64::MAX as u64) { return Err(invalid("expiry must be in the future")); }
        if request.channels.len() > 100 { return Err(invalid("too many channels")); }
        for c in &request.channels { if project_scope(c).is_none() { validate_channel(c)?; } }
        let token = random_token();
        let mut conn = self.lock()?;
        let db = conn.transaction()?;
        let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM credentials WHERE actor=?1)", [&request.actor], |r| r.get(0))?;
        if exists { return Err(conflict("actor already registered; use a new actor identity")); }
        // Legacy daemons must not authenticate a restricted invite on downgrade.
        let hash = if grants.is_some() { format!("restricted:{}",digest(&token)) } else { digest(&token) };
        db.execute("INSERT INTO credentials(hash,actor,channels,expires) VALUES (?1,?2,?3,?4)", params![hash,request.actor,serde_json::to_string(&request.channels)?,request.expires_at])?;
        let id = db.last_insert_rowid() as u64;
        if let Some(grants) = grants { db.execute("INSERT INTO credential_grants VALUES (?1,?2)", params![id,serde_json::to_string(&grants)?])?; }
        db.commit()?;
        Ok(Invite { id, token, actor: request.actor, channels: request.channels, expires_at: request.expires_at })
    }
    fn explain(&self, admin: &str, id: u64) -> Result<serde_json::Value> {
        if digest(admin) != self.admin_hash { return Err(unauthorized()); }
        let db = self.lock()?;
        let (actor, channels, expires, revoked, grants) = db.query_row("SELECT actor,channels,expires,revoked,g.grants FROM credentials c LEFT JOIN credential_grants g ON c.id=g.credential WHERE id=?1", [sql_id(id)?], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,Option<u64>>(2)?, r.get::<_,bool>(3)?, r.get::<_,Option<String>>(4)?))).optional()?.ok_or_else(|| invalid("unknown invite"))?;
        Ok(serde_json::json!({"id":id,"actor":actor,"channels":serde_json::from_str::<Vec<String>>(&channels)?,"expires_at":expires,"status":if revoked { "revoked" } else if expires.is_some_and(|e| e<=now()) { "expired" } else { "active" },"grants":grants.map(|g| serde_json::from_str::<BTreeSet<Grant>>(&g)).transpose()?}))
    }
    pub fn revoke_invite(&self, admin: &str, id: u64) -> Result<()> {
        if digest(admin) != self.admin_hash { return Err(unauthorized()); }
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        if tx.execute("UPDATE credentials SET revoked=1 WHERE id=?1 AND actor!='local'", [sql_id(id)?])? == 0 { return Err(invalid("unknown invite")); }
        Self::cancel_pending(&tx,id)?;
        let members = tx.prepare("SELECT credential FROM invite_members WHERE invite=?1")?.query_map([sql_id(id)?], |r| r.get::<_,u64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        for member in members {
            tx.execute("UPDATE credentials SET revoked=1 WHERE id=?1", [sql_id(member)?])?;
            Self::cancel_pending(&tx,member)?;
        }
        tx.commit()?; Ok(())
    }
    fn revoke_grant(&self, admin: &str, id: u64, grant: Grant) -> Result<()> {
        if digest(admin) != self.admin_hash { return Err(unauthorized()); }
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let prior: String = tx.query_row("SELECT grants FROM credential_grants WHERE credential=?1", [sql_id(id)?], |r| r.get(0)).optional()?.ok_or_else(|| invalid("not an explicitly restricted invite"))?;
        let mut grants: BTreeSet<Grant> = serde_json::from_str(&prior)?;
        if grants.remove(&grant) {
            tx.execute("UPDATE credential_grants SET grants=?2 WHERE credential=?1", params![id,serde_json::to_string(&grants)?])?;
            if grant == Grant::Schedule { Self::cancel_pending(&tx,id)?; }
        }
        tx.commit()?; Ok(())
    }
    fn cancel_pending(tx: &Transaction<'_>, id: u64) -> Result<()> {
        let schedules = tx.prepare("SELECT id,channel FROM schedules WHERE credential=?1 AND id NOT IN (SELECT schedule FROM schedule_threads WHERE internal=1)")?.query_map([sql_id(id)?], |r| Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        for (id,channel) in schedules { emit(tx,"system",&channel,EventKind::ScheduleCanceled { id, channel:channel.clone() })?; }
        tx.execute("DELETE FROM schedules WHERE credential=?1", [sql_id(id)?])?;
        Ok(())
    }
    pub fn snapshot(&self, token: &str) -> Result<Snapshot> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, Some(Grant::Read))?;
        maintain(&tx)?;
        let channels = tx.prepare("SELECT name FROM channels ORDER BY name")?.query_map([], |r| r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?.into_iter().filter(|c| identity.permits(c)).collect();
        let messages = visible_messages(&tx, &identity)?;
        let cursor = cursor(&tx)?;
        tx.commit()?;
        Ok(Snapshot { cursor, channels, messages })
    }

    pub fn organization(&self, token: &str) -> Result<Organization> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, Some(Grant::Read))?;
        let mut organization = Organization::default();
        let mut areas = tx.prepare("SELECT board,name,owner FROM areas LEFT JOIN area_owners USING(board,name) ORDER BY board,name")?;
        let mut rows = areas.query([])?;
        while let Some(row) = rows.next()? {
            let board: String = row.get(0)?;
            if !identity.permits_board(&board) { continue; }
            let name: String = row.get(1)?;
            if let Some(owner) = row.get(2)? {
                organization.area_owners.entry(board.clone()).or_default().insert(name.clone(),owner);
            }
            organization.areas.entry(board).or_default().push(name);
        }
        let mut sprints = tx.prepare("SELECT id,board,title,description,finished FROM sprints")?;
        let mut rows = sprints.query([])?;
        while let Some(row) = rows.next()? {
            let board = row.get::<_, String>(1)?;
            if identity.permits_board(&board) {
                let id = row.get(0)?;
                organization.sprints.insert(id, Sprint { id, board, title: row.get(2)?, description: row.get(3)?, finished: row.get(4)? });
            }
        }
        let mut tasks = tx.prepare("SELECT m.id,m.channel,p.area,p.parent,COALESCE(p.global,0),json_extract(m.body,'$.labels.area'),s.sprint
            FROM messages m LEFT JOIN task_placements p ON p.message=m.id LEFT JOIN task_sprints s ON s.message=m.id WHERE json_type(m.body,'$.task')='object'")?;
        let mut rows = tasks.query([])?;
        while let Some(row) = rows.next()? {
            let global = row.get(4)?;
            if !permits_message(&identity, &row.get::<_, String>(1)?, global) { continue; }
            if let Some(sprint) = row.get(6)? { organization.task_sprints.insert(row.get(0)?, sprint); }
            let mut area: Option<String> = row.get(2)?;
            if !global && area.is_none() { area = row.get::<_, Option<String>>(5)?.filter(|s| !s.trim().is_empty()); }
            organization.tasks.insert(row.get(0)?, TaskPlacement { area, parent: row.get(3)?, global, done_by: Vec::new() });
        }
        let visible: BTreeSet<_> = organization.tasks.keys().copied().collect();
        for p in organization.tasks.values_mut() { p.parent = p.parent.filter(|id| visible.contains(id)); }
        let mut acks = tx.prepare("SELECT message,actor FROM task_acknowledgements ORDER BY message,actor")?;
        let mut rows = acks.query([])?;
        while let Some(row) = rows.next()? {
            if let Some(p) = organization.tasks.get_mut(&row.get(0)?) { p.done_by.push(row.get(1)?); }
        }
        Ok(organization)
    }

    pub fn owned_areas(&self, token: &str, owner: &str) -> Result<BTreeMap<String, Vec<String>>> {
        let db = self.lock()?;
        let who = Self::identity(&db, token, Some(Grant::Read))?;
        validate_name(owner)?;
        let mut stmt = db.prepare("SELECT board,name FROM area_owners WHERE owner=?1 ORDER BY board,name")?;
        let mut rows = stmt.query([owner])?;
        let mut result = BTreeMap::<String, Vec<String>>::new();
        while let Some(row) = rows.next()? {
            let board: String = row.get(0)?;
            if who.permits_board(&board) { result.entry(board).or_default().push(row.get(1)?); }
        }
        Ok(result)
    }
    /// Directory membership is based on live credentials, never message authors.
    /// A shared invite is a key rather than a person unless an older client named it.
    /// Check the caller before reading identities, including for future channels.
    pub fn members(&self, token: &str, channel: &str) -> Result<Vec<Member>> {
        let db = self.lock()?;
        let caller = Self::identity(&db, token, Some(Grant::Read))?;
        caller.check(channel)?;
        validate_channel(channel)?;
        let rows = db.prepare("SELECT c.id,c.actor,c.channels,n.name FROM credentials c LEFT JOIN actor_display_names n ON n.credential=c.id WHERE c.revoked=0 AND (c.expires IS NULL OR c.expires>?1) AND NOT (n.name IS NULL AND c.id IN (SELECT credential FROM shared_invites)) ORDER BY c.actor")?
            .query_map([now()], |r| Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?)))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        let mut members = Vec::new();
        for (id,actor,channels,display_name) in rows {
            let identity = Identity { id, actor, channels: serde_json::from_str(&channels)? };
            if identity.permits(channel) { members.push(Member { actor: identity.actor, display_name }); }
        }
        Ok(members)
    }
    pub fn set_display_name(&self, token: &str, name: &str) -> Result<()> {
        let db = self.lock()?;
        let identity = Self::identity(&db, token, Some(Grant::DisplayName))?;
        // Allow human names, including spaces and Unicode, but not control or
        // bidi override characters that can spoof surrounding UI text.
        if name.len() > 256 || name.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) {
            return Err(invalid("display name must be at most 256 bytes without control or bidi override characters"));
        }
        let name = name.trim();
        if name.is_empty() {
            db.execute("DELETE FROM actor_display_names WHERE credential=?1", [identity.id])?;
        } else {
            db.execute("INSERT INTO actor_display_names(credential,name) VALUES (?1,?2) ON CONFLICT(credential) DO UPDATE SET name=excluded.name", params![identity.id,name])?;
        }
        Ok(())
    }
    pub fn events(&self, token: &str, after: u64) -> Result<Vec<Event>> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, Some(Grant::Read))?;
        maintain(&tx)?;
        let projects: Vec<_> = identity.channels.iter().filter_map(|c| project_scope(c)).collect();
        // Apply visibility BEFORE paging. Filtering a global page afterwards can return an
        // empty/short page while visible events remain beyond it. Use literal comparisons,
        // not LIKE/GLOB: underscores and dots in valid slugs are not wildcard syntax.
        let rows = tx.prepare("SELECT cursor,body FROM events WHERE cursor>?1 AND (
            ?2='[]' OR channel IN (SELECT value FROM json_each(?2)) OR EXISTS (
                SELECT 1 FROM json_each(?3) AS project WHERE channel=project.value
                OR substr(channel,1,length(project.value)+1)=project.value || '/'
            ) OR (substr(channel,1,7)='@board/' AND substr(channel,8) IN (SELECT value FROM json_each(?4))))
            ORDER BY cursor LIMIT 1000")?.query_map(params![sql_id(after)?,serde_json::to_string(&identity.channels)?,serde_json::to_string(&projects)?,serde_json::to_string(&identity.boards())?], |r| Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut events = Vec::new();
        for (cursor,body) in rows {
            let mut event: Event = serde_json::from_str(&body)?; event.cursor = cursor; events.push(event);
        }
        tx.commit()?; Ok(events)
    }
    pub fn tick(&self) -> Result<()> {
        let mut db = self.lock()?; let tx = db.transaction()?; maintain(&tx)?; tx.commit()?; Ok(())
    }
    pub fn attach(&self, token: &str, id: u64, name: &str, bytes: Vec<u8>) -> Result<Attachment> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, Some(Grant::Attach))?;
        let mut msg = message(&tx, id, &identity)?;
        validate_attachment_name(name)?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(BoardError(StatusCode::PAYLOAD_TOO_LARGE, "attachment exceeds 8 MiB".into()));
        }
        let (count, total): (usize, usize) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(bytes)),0) FROM attachments WHERE message_id=?1", [sql_id(id)?], |r| Ok((r.get(0)?,r.get(1)?)))?;
        if count >= MAX_MESSAGE_ATTACHMENTS || total + bytes.len() > MAX_MESSAGE_ATTACHMENT_BYTES {
            return Err(BoardError(StatusCode::PAYLOAD_TOO_LARGE, "message attachment quota exceeded".into()));
        }
        tx.execute("INSERT INTO attachments(message_id,name,bytes) VALUES (?1,?2,?3)", params![sql_id(id)?,name,bytes])?;
        let attachment = Attachment { id: tx.last_insert_rowid() as u64, message_id: id, name: name.into(), size: bytes.len() as u64 };
        msg.attachments.push(attachment.clone());
        save(&tx, &identity.actor, &msg)?;
        tx.commit()?;
        Ok(attachment)
    }
    pub fn attachments(&self, token: &str, id: u64) -> Result<Vec<Attachment>> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, Some(Grant::Read))?;
        message(&tx, id, &identity)?;
        let rows = tx.prepare("SELECT id,message_id,name,length(bytes) FROM attachments WHERE message_id=?1 ORDER BY id")?.query_map([sql_id(id)?], |r| Ok(Attachment { id:r.get(0)?, message_id:r.get(1)?, name:r.get(2)?, size:r.get(3)? }))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(rows)
    }
    pub fn download_attachment(&self, token: &str, id: u64) -> Result<Vec<u8>> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, Some(Grant::Read))?;
        let message_id: Option<u64> = tx.query_row("SELECT message_id FROM attachments WHERE id=?1", [sql_id(id)?], |r| r.get(0)).optional()?;
        let missing = || BoardError(StatusCode::NOT_FOUND, "attachment not found".into());
        message(&tx, message_id.ok_or_else(missing)?, &identity).map_err(|e| if e.0 == StatusCode::NOT_FOUND { missing() } else { e })?;
        Ok(tx.query_row("SELECT bytes FROM attachments WHERE id=?1", [sql_id(id)?], |r| r.get(0))?)
    }
    pub fn reactions(&self, token: &str) -> Result<Vec<Reaction>> {
        let db = self.lock()?;
        let identity = Self::identity(&db, token, Some(Grant::Read))?;
        let mut stmt = db.prepare("SELECT r.message,m.channel,r.actor,r.reaction,COALESCE(p.global,0)
            FROM reactions r JOIN messages m ON m.id=r.message
            LEFT JOIN task_placements p ON p.message=COALESCE(json_extract(m.body,'$.parent'),m.id)
            ORDER BY r.message,r.actor,r.reaction")?;
        let rows = stmt.query_map([], |r| Ok((Reaction { id:r.get(0)?,channel:r.get(1)?,actor:r.get(2)?,reaction:r.get(3)? }, r.get::<_,bool>(4)?)))?;
        let mut result = Vec::new();
        for row in rows { let (r, global) = row?; if permits_message(&identity,&r.channel,global) { result.push(r); } }
        Ok(result)
    }
    pub fn schedules(&self, token: &str) -> Result<Vec<ScheduledPost>> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, Some(Grant::Read))?;
        maintain(&tx)?;
        let rows = tx.prepare("SELECT id,channel,actor,text,labels,publish_at,thread,coalesce(internal,0),coalesce(fired,0) FROM schedules LEFT JOIN schedule_threads ON schedule=id WHERE credential=?1 ORDER BY publish_at,id")?.query_map([identity.id], |r| Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,u64>(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut result=Vec::new();
        for (id,channel,author,text,labels,publish_at,thread,internal_only,fired) in rows {
            if identity.permits(&channel) { result.push(ScheduledPost { id,channel,author,text,labels:serde_json::from_str(&labels)?,publish_at,thread,internal_only,fired }); }
        }
        tx.commit()?; Ok(result)
    }
    pub fn action(&self, token: &str, profile: Option<&str>, key: Option<&str>, action: Action) -> Result<Outcome> {
        self.action_with_sender(token, profile, key, None, action)
    }
    fn action_with_sender(&self, token: &str, profile: Option<&str>, key: Option<&str>, sender: Option<&str>, action: Action) -> Result<Outcome> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let identity = Self::identity(&tx, token, action.grant())?;
        let author = if let Some(p) = profile { validate_name(p)?; format!("{}/{}", identity.actor, p) } else { identity.actor.clone() };
        if sender.is_some_and(|s| s.is_empty() || s.len() > 84 || s.trim() != s || !s.bytes().all(|b| b.is_ascii_graphic() || b == b' ')) {
            return Err(invalid("invalid sender metadata"));
        }
        let request = serde_json::to_string(&(profile, &action))?;
        if let Some(key) = key {
            if key.is_empty() || key.len() > 128 { return Err(invalid("idempotency key must be 1..128 bytes")); }
            let cached: Option<(String,String)> = tx.query_row("SELECT request,outcome FROM idempotency WHERE credential=?1 AND key=?2", params![identity.id,key], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((prior,outcome)) = cached { if prior != request { return Err(conflict("idempotency key reused with different action")); } return Ok(serde_json::from_str(&outcome)?); }
        }
        maintain(&tx)?;
        let id = apply(&tx, &identity, &author, sender, action)?;
        settle_tasks(&tx)?;
        let outcome = Outcome { id, cursor: cursor(&tx)? };
        if let Some(key) = key { tx.execute("INSERT INTO idempotency VALUES (?1,?2,?3,?4)", params![identity.id,key,request,serde_json::to_string(&outcome)?])?; }
        tx.commit()?; Ok(outcome)
    }
}

fn sql_id(id: u64) -> Result<i64> { i64::try_from(id).map_err(|_| invalid("integer exceeds SQLite range")) }
fn validate_attachment_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 255 || name.trim() != name || name == "." || name == ".."
        || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) {
        return Err(invalid("attachment name must be a plain filename of 1..255 bytes"));
    }
    Ok(())
}
fn validate_name(s: &str) -> Result<()> {
    if s.is_empty() || s.len()>80 || !s.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)) { return Err(invalid("name must be 1..80 ASCII letters, digits, dash, underscore or dot")); } Ok(())
}
fn validate_channel(s: &str) -> Result<()> {
    if s.len()>200 || s.split('/').any(|part| part == "." || part == ".." || validate_name(part).is_err()) { return Err(invalid("invalid channel path")); } Ok(())
}
/// Only a single validated channel segment may introduce a project scope. The
/// reserved marker is invite metadata, never a valid channel or general glob.
fn project_scope(scope: &str) -> Option<&str> {
    let project = scope.strip_suffix("/*")?;
    (!project.contains('/') && validate_channel(project).is_ok()).then_some(project)
}
fn board_namespace(channel: &str) -> &str { channel.split_once('/').map_or(channel, |(board, _)| board) }
// Invalid as a channel path; board events never widen access to ordinary channel events.
fn board_audience(board: &str) -> String { format!("@board/{board}") }
fn permits_message(identity: &Identity, channel: &str, global: bool) -> bool {
    identity.permits(channel) || (global && identity.permits_board(board_namespace(channel)))
}
fn validate_text(s: &str) -> Result<()> { if s.trim().is_empty() || s.len()>65536 || s.contains('\0') { return Err(invalid("text must be nonblank, NUL-free and at most 65536 bytes")); } Ok(()) }
fn validate_labels(labels: &BTreeMap<String,String>) -> Result<()> {
    if labels.len()>64 { return Err(invalid("at most 64 labels")); }
    for (k,v) in labels { validate_name(k)?; if v.len()>1024 || v.contains('\0') { return Err(invalid("invalid label value")); } } Ok(())
}
fn cursor(tx: &Transaction<'_>) -> Result<u64> { Ok(tx.query_row("SELECT COALESCE(MAX(cursor),0) FROM events", [], |r| r.get(0))?) }
fn emit(tx: &Transaction<'_>, actor: &str, channel: &str, kind: EventKind) -> Result<()> {
    let event = Event { cursor: 0, created_at: now(), actor: actor.into(), kind };
    tx.execute("INSERT INTO events(channel,body) VALUES (?1,?2)", params![channel,serde_json::to_string(&event)?])?; Ok(())
}
fn all_messages(tx: &Transaction<'_>) -> Result<Vec<Message>> {
    let rows = tx.prepare("SELECT body FROM messages ORDER BY id")?.query_map([], |r| r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    rows.into_iter().map(|s| Ok(serde_json::from_str(&s)?)).collect()
}
fn visible_messages(tx: &Transaction<'_>, identity: &Identity) -> Result<Vec<Message>> {
    let mut stmt = tx.prepare("SELECT m.body,m.channel,COALESCE(p.global,0) FROM messages m
        LEFT JOIN task_placements p ON p.message=COALESCE(json_extract(m.body,'$.parent'),m.id) ORDER BY m.id")?;
    let mut rows = stmt.query([])?;
    let mut messages = Vec::new();
    while let Some(row) = rows.next()? {
        if permits_message(identity, &row.get::<_, String>(1)?, row.get(2)?) {
            messages.push(serde_json::from_str(&row.get::<_, String>(0)?)?);
        }
    }
    Ok(messages)
}
fn placement(tx: &Transaction<'_>, id: u64) -> Result<TaskPlacement> {
    Ok(tx.query_row("SELECT area,parent,global FROM task_placements WHERE message=?1", [sql_id(id)?], |r| {
        Ok(TaskPlacement { area: r.get(0)?, parent: r.get(1)?, global: r.get(2)?, done_by: Vec::new() })
    }).optional()?.unwrap_or_default())
}
fn is_global(tx: &Transaction<'_>, id: u64) -> Result<bool> {
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM task_placements WHERE message=?1 AND global=1)", [sql_id(id)?], |r| r.get(0))?)
}
fn message_audience(tx: &Transaction<'_>, m: &Message) -> Result<String> {
    Ok(if is_global(tx, m.parent.unwrap_or(m.id))? { board_audience(board_namespace(&m.channel)) } else { m.channel.clone() })
}
fn message(tx: &Transaction<'_>, id: u64, identity: &Identity) -> Result<Message> {
    let body: Option<String> = tx.query_row("SELECT body FROM messages WHERE id=?1", [sql_id(id)?], |r| r.get(0)).optional()?;
    let msg: Message = serde_json::from_str(&body.ok_or_else(|| BoardError(StatusCode::NOT_FOUND,"message not found".into()))?)?;
    // Avoid distinguishing a hidden message from a nonexistent one.
    if !identity.permits(&msg.channel) && !permits_message(identity, &msg.channel, is_global(tx, msg.parent.unwrap_or(id))?) {
        return Err(BoardError(StatusCode::NOT_FOUND,"message not found".into()));
    }
    Ok(msg)
}
fn emit_message(tx: &Transaction<'_>, actor: &str, m: &Message) -> Result<()> {
    emit(tx,actor,&message_audience(tx,m)?,EventKind::Message { message: m.clone() })
}
fn save(tx: &Transaction<'_>, actor: &str, m: &Message) -> Result<()> {
    tx.execute("UPDATE messages SET body=?1 WHERE id=?2", params![serde_json::to_string(m)?,m.id])?;
    emit_message(tx,actor,m)
}
struct NewMessage {
    channel: String,
    parent: Option<u64>,
    author: String,
    text: String,
    labels: BTreeMap<String,String>,
    task: Option<Task>,
}
fn insert(tx: &Transaction<'_>, actor: &str, sender: Option<&str>, new: NewMessage) -> Result<u64> {
    insert_with_placement(tx,actor,sender,new,None)
}
fn insert_with_placement(tx: &Transaction<'_>, actor: &str, sender: Option<&str>, new: NewMessage, placement: Option<&TaskPlacement>) -> Result<u64> {
    let NewMessage { channel, parent, author, text, labels, task } = new;
    tx.execute("INSERT INTO messages(channel,creator,body) VALUES (?1,?2,'')", params![channel,actor])?;
    let m = Message { id: tx.last_insert_rowid() as u64, channel, parent, author, sender: sender.map(str::to_owned), text, labels, created_at: now(), task, attachments: vec![] };
    if let Some(p) = placement { save_placement(tx,m.id,p)?; }
    save(tx,actor,&m)?; Ok(m.id)
}
fn save_placement(tx: &Transaction<'_>, id: u64, p: &TaskPlacement) -> Result<()> {
    tx.execute("INSERT INTO task_placements(message,area,parent,global) VALUES (?1,?2,?3,?4)
        ON CONFLICT(message) DO UPDATE SET area=excluded.area,parent=excluded.parent", params![id,p.area,p.parent,p.global])?;
    Ok(())
}
fn validate_placement(tx: &Transaction<'_>, who: &Identity, channel: &str, id: Option<u64>, p: &TaskPlacement) -> Result<()> {
    if let Some(area) = &p.area {
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM areas WHERE board=?1 AND name=?2)", params![board_namespace(channel),area], |r| r.get(0))?;
        if !exists { return Err(invalid("area must exist on the task's board")); }
    }
    if let Some(parent) = p.parent {
        if let Some(id) = id {
            if task_sprint(tx,id)?.is_some() { return Err(invalid("remove sprint placement before making a task a subtask")); }
        }
        if Some(parent) == id { return Err(invalid("task cannot be its own parent")); }
        let m = message(tx,parent,who)?;
        let target = placement(tx,parent)?;
        if m.channel != channel || m.task.is_none() || target.global || target.parent.is_some() {
            return Err(invalid("parent must be a top-level non-global task in the same channel"));
        }
        // Forced completion preserves existing children; editing one is not a new attachment.
        if m.task.is_some_and(|t| t.state == TaskState::Completed)
            && !match id { Some(id) => placement(tx,id)?.parent == Some(parent), None => false } {
            return Err(conflict("cannot attach a subtask to a completed parent"));
        }
        let has_children: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM task_placements WHERE parent=?1)", [id], |r| r.get(0))?;
        if has_children { return Err(invalid("a task with subtasks cannot become a subtask")); }
    }
    Ok(())
}
fn validate_global_fields(labels: &BTreeMap<String,String>, needs: &[u64], assignee: Option<&str>, p: &TaskPlacement) -> Result<()> {
    if p.global && (p.area.is_some() || p.parent.is_some() || assignee.is_some() || !needs.is_empty() || labels.contains_key("area")) {
        return Err(invalid("global tasks cannot have an area, parent, assignee, or dependencies"));
    }
    Ok(())
}
fn ensure_channel(tx: &Transaction<'_>, identity: &Identity, channel: &str) -> Result<()> {
    validate_channel(channel)?; identity.check(channel)?;
    if tx.execute("INSERT OR IGNORE INTO channels VALUES (?1)", [channel])? != 0 { emit(tx,&identity.actor,channel,EventKind::ChannelCreated { channel: channel.into() })?; } Ok(())
}
fn require_creator(tx: &Transaction<'_>, m: &Message, identity: &Identity) -> Result<()> {
    let creator: String = tx.query_row("SELECT creator FROM messages WHERE id=?1", [m.id], |r| r.get(0))?;
    if creator != identity.actor { return Err(forbidden()); } Ok(())
}
fn active_actor(tx: &Transaction<'_>, actor: &str) -> Result<Option<Identity>> {
    validate_name(actor)?;
    let row: Option<(u64,String)> = tx.query_row("SELECT id,channels FROM credentials WHERE actor=?1 AND revoked=0 AND (expires IS NULL OR expires>?2)", params![actor,now()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    row.map(|(id,channels)| Ok(Identity { id, actor: actor.into(), channels: serde_json::from_str(&channels)? })).transpose()
}
fn validate_assignee(tx: &Transaction<'_>, actor: &str, channel: &str) -> Result<()> {
    if !active_actor(tx,actor)?.is_some_and(|target| target.permits(channel)) { return Err(invalid("assignee must be an active channel member")); }
    Ok(())
}
fn mutable_task(m: &Message) -> Result<&Task> {
    let t = m.task.as_ref().ok_or_else(|| invalid("not a task"))?;
    if matches!(t.state,TaskState::Completed|TaskState::Failed|TaskState::Canceled) { return Err(conflict("task is terminal")); }
    Ok(t)
}
// A creator may manage unleased work, but must not take another worker's live lease.
fn require_task_manager(tx: &Transaction<'_>, m: &Message, who: &Identity) -> Result<()> {
    let t = mutable_task(m)?;
    if t.owner.is_some() {
        if t.owner.as_deref() != Some(&who.actor) { return Err(conflict("only the active lease owner can change claimed work")); }
        Ok(())
    } else { require_creator(tx,m,who) }
}
fn task_sprint(tx: &Transaction<'_>, id: u64) -> Result<Option<u64>> {
    Ok(tx.query_row("SELECT sprint FROM task_sprints WHERE message=?1", [id], |r| r.get(0)).optional()?)
}

#[cfg(test)]
mod sprint_grant_tests {
    use super::*;

    #[test]
    fn restricted_grants_do_not_imply_sprint_management_or_read() {
        let host = random_token();
        let admin = random_token();
        let board = Board::open(":memory:", &host, &admin).unwrap();
        let issue = |actor: &str, grant| board.issue(&admin, InviteRequest {
            actor: actor.into(), channels: vec!["app/*".into()], expires_at: None,
        }, Some([grant].into())).unwrap().token;
        let catalog = issue("catalog", Grant::CreateSprint);
        let poster = issue("poster", Grant::PostTask);
        let closer = issue("closer", Grant::FinishSprint);
        let editor = issue("editor", Grant::EditTask);
        let act = |token: &str, action| board.action(token,None,None,action);
        let create = || Action::CreateSprint { board:"app".into(), title:"Test".into(), description:String::new() };
        assert_eq!(act(&poster,create()).unwrap_err().0,StatusCode::FORBIDDEN);
        let sprint = act(&catalog,create()).unwrap().id.unwrap();
        let post = || Action::PostSprintTask { channel:"app/dev".into(), text:"test".into(), labels:Default::default(), needs:vec![], assignee:None, sprint };
        assert_eq!(act(&editor,post()).unwrap_err().0,StatusCode::FORBIDDEN);
        let id = act(&poster,post()).unwrap().id.unwrap();
        let clear = || Action::SetTaskSprint {id,sprint:None};
        assert_eq!(act(&poster,clear()).unwrap_err().0,StatusCode::FORBIDDEN); // creator, but no edit grant
        assert_eq!(act(&editor,clear()).unwrap_err().0,StatusCode::FORBIDDEN); // grant, but not task manager
        assert_eq!(board.organization(&poster).unwrap_err().0,StatusCode::FORBIDDEN);
        let finish = || Action::FinishSprint {id:sprint,allow_unfinished:true};
        assert_eq!(act(&catalog,finish()).unwrap_err().0,StatusCode::FORBIDDEN);
        act(&closer,finish()).unwrap();
    }
}

fn open_sprint(tx: &Transaction<'_>, channel: &str, id: u64) -> Result<()> {
    match tx.query_row("SELECT finished FROM sprints WHERE board=?1 AND id=?2", params![board_namespace(channel),sql_id(id)?], |r| r.get::<_,bool>(0)).optional()? {
        Some(false) => Ok(()),
        Some(true) => Err(conflict("sprint is finished")),
        None => Err(invalid("sprint must exist on the task's board")),
    }
}

fn apply(tx: &Transaction<'_>, who: &Identity, author: &str, sender: Option<&str>, action: Action) -> Result<Option<u64>> {
    use Action::*;
    match action {
        CreateChannel { channel } => { ensure_channel(tx,who,&channel)?; Ok(None) }
        CreateSprint { board,title,description } => {
            validate_channel(&board)?;
            if board.contains('/') { return Err(invalid("board must be a single channel segment")); }
            if !who.permits_board(&board) { return Err(forbidden()); }
            validate_text(&title)?;
            if title.len() > 256 || title.chars().any(char::is_control) { return Err(invalid("invalid sprint title")); }
            if description.len() > 65536 || description.contains('\0') { return Err(invalid("invalid sprint description")); }
            tx.execute("INSERT INTO sprints(board,title,description) VALUES (?1,?2,?3)", params![board,title,description])?;
            let id = tx.last_insert_rowid() as u64;
            emit(tx,&who.actor,&board_audience(&board),EventKind::OrganizationChanged { board })?;
            Ok(Some(id))
        }
        FinishSprint { id,allow_unfinished } => {
            let missing = || BoardError(StatusCode::NOT_FOUND,"sprint not found".into());
            let (board,finished) = tx.query_row("SELECT board,finished FROM sprints WHERE id=?1", [sql_id(id)?], |r| Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?))).optional()?.ok_or_else(missing)?;
            if !who.permits_board(&board) { return Err(missing()); }
            // Finishing tests progress across private channels. Exact-channel credentials
            // must not use the result as an oracle for tasks they cannot see.
            if !who.channels.is_empty() && !who.channels.iter().any(|c| project_scope(c) == Some(board.as_str())) { return Err(forbidden()); }
            if !finished {
                if !allow_unfinished && tx.query_row("SELECT EXISTS(SELECT 1 FROM task_sprints s JOIN messages m ON m.id=s.message WHERE s.sprint=?1 AND json_extract(m.body,'$.task.state')!='completed')", [id], |r| r.get::<_,bool>(0))? {
                    return Err(conflict("sprint has unfinished tasks"));
                }
                tx.execute("UPDATE sprints SET finished=1 WHERE id=?1", [id])?;
                emit(tx,&who.actor,&board_audience(&board),EventKind::OrganizationChanged { board })?;
            }
            Ok(Some(id))
        }
        PostSprintTask {channel,text,labels,needs,assignee,sprint} => {
            who.check(&channel)?;
            open_sprint(tx,&channel,sprint)?;
            let result = apply(tx,who,author,sender,PostTask {channel,text,labels,needs,assignee,area:None,parent:None,global:false})?;
            tx.execute("INSERT INTO task_sprints(message,sprint) VALUES (?1,?2)",params![result,sprint])?;
            Ok(result)
        }
        SetTaskSprint { id,sprint } => {
            let m = message(tx,id,who)?;
            require_task_manager(tx,&m,who)?;
            if placement(tx,id)?.parent.is_some() { return Err(invalid("only top-level tasks have sprint placement")); }
            let previous = task_sprint(tx,id)?;
            if previous != sprint {
                if let Some(previous) = previous { open_sprint(tx,&m.channel,previous)?; }
                if let Some(next) = sprint {
                    open_sprint(tx,&m.channel,next)?;
                    tx.execute("INSERT INTO task_sprints(message,sprint) VALUES (?1,?2) ON CONFLICT(message) DO UPDATE SET sprint=excluded.sprint",params![id,next])?;
                } else {
                    tx.execute("DELETE FROM task_sprints WHERE message=?1",[id])?;
                }
                save(tx,&who.actor,&m)?;
            }
            Ok(Some(id))
        }
        DeleteChannel { channel } => {
            // `local` is the bootstrap credential's reserved actor, not a profile or display name.
            if who.actor != "local" { return Err(forbidden()); }
            validate_channel(&channel)?;
            if channel.split_once('/').is_none_or(|(_, path)| path == "general") {
                return Err(conflict("cannot delete a board namespace or its default channel"));
            }
            if tx.execute("DELETE FROM channels WHERE name=?1", [&channel])? == 0 {
                return Err(BoardError(StatusCode::NOT_FOUND, "channel not found".into()));
            }
            // Placements must go before messages: parent uses ON DELETE RESTRICT even
            // when the subtask and its parent are both being deleted. Needs, parents
            // and replies are channel-local by construction. Keep board metadata intact.
            tx.execute("DELETE FROM task_placements WHERE message IN (SELECT id FROM messages WHERE channel=?1)", [&channel])?;
            tx.execute("DELETE FROM reactions WHERE message IN (SELECT id FROM messages WHERE channel=?1)", [&channel])?;
            tx.execute("DELETE FROM schedules WHERE channel=?1", [&channel])?;
            tx.execute("DELETE FROM messages WHERE channel=?1", [&channel])?;
            tx.execute("DELETE FROM events WHERE channel=?1", [&channel])?;
            emit(tx,&who.actor,&channel,EventKind::ChannelDeleted { channel:channel.clone() })?;
            Ok(None)
        }
        CreateArea { board, name } => {
            if board.contains('/') { return Err(invalid("board must be a single channel segment")); }
            validate_channel(&board)?;
            if !who.permits_board(&board) { return Err(forbidden()); }
            if name.trim() != name || name.is_empty() || name.len() > 128
                || name.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) {
                return Err(invalid("area name must be 1..128 bytes without surrounding whitespace, control or bidi override characters"));
            }
            if tx.execute("INSERT OR IGNORE INTO areas(board,name) VALUES (?1,?2)", params![board,name])? != 0 {
                emit(tx,&who.actor,&board_audience(&board),EventKind::OrganizationChanged { board })?;
            }
            Ok(None)
        }
        SetAreaOwner { board,name,owner } => {
            if board.contains('/') { return Err(invalid("board must be a single channel segment")); }
            validate_channel(&board)?;
            if !who.permits_board(&board) { return Err(forbidden()); }
            let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM areas WHERE board=?1 AND name=?2)", params![board,name], |r| r.get(0))?;
            if !exists { return Err(invalid("area must exist on the board")); }
            let changed = if let Some(actor) = owner {
                if !active_actor(tx,&actor)?.is_some_and(|target| target.permits_board(&board)) { return Err(invalid("owner must be an active board member")); }
                tx.execute("INSERT INTO area_owners(board,name,owner) VALUES (?1,?2,?3) ON CONFLICT(board,name) DO UPDATE SET owner=excluded.owner WHERE owner!=excluded.owner",params![board,name,actor])?
            } else {
                tx.execute("DELETE FROM area_owners WHERE board=?1 AND name=?2",params![board,name])?
            };
            if changed != 0 { emit(tx,&who.actor,&board_audience(&board),EventKind::OrganizationChanged { board })?; }
            Ok(None)
        }
        Post { channel,text,labels,task,needs,assignee } => {
            if task { return apply(tx,who,author,sender,PostTask { channel,text,labels,needs,assignee,area:None,parent:None,global:false }); }
            validate_text(&text)?; validate_labels(&labels)?;
            if !needs.is_empty() || assignee.is_some() { return Err(invalid("dependencies and assignment require a task")); }
            ensure_channel(tx,who,&channel)?;
            Ok(Some(insert(tx,&who.actor,sender,NewMessage { channel,parent:None,author:author.into(),text,labels,task:None })?))
        }
        PostTask { mut channel,text,labels,needs,assignee,area,parent,global } => {
            validate_text(&text)?; validate_labels(&labels)?; validate_channel(&channel)?;
            let p = TaskPlacement { area,parent,global,done_by:Vec::new() };
            validate_global_fields(&labels,&needs,assignee.as_deref(),&p)?;
            if needs.len()>128 { return Err(invalid("at most 128 dependencies")); }
            if global {
                channel = board_namespace(&channel).to_owned();
                if !who.permits_board(&channel) { return Err(forbidden()); }
            } else {
                who.check(&channel)?;
            }
            if let Some(a) = &assignee { validate_assignee(tx,a,&channel)?; }
            let needs: Vec<_> = needs.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
            // Channel scopes must not expose dependency IDs from outside the task channel.
            for id in &needs { dependency(tx,who,&channel,*id)?; }
            validate_placement(tx,who,&channel,None,&p)?;
            if !global { ensure_channel(tx,who,&channel)?; }
            let task = Some(Task { state: if global { TaskState::Ready } else { TaskState::Waiting }, needs, assignee, owner: None, expires_at: None, started_at:None, finished_by:None,result:None });
            Ok(Some(insert_with_placement(tx,&who.actor,sender,NewMessage { channel,parent:None,author:author.into(),text,labels,task },Some(&p))?))
        }
        Reply { thread,text } => {
            apply(tx,who,author,sender,ReplyWithLabels { thread,text,labels:BTreeMap::new() })
        }
        ReplyWithLabels { thread,text,labels } => {
            validate_labels(&labels)?;
            validate_text(&text)?; let root = message(tx,thread,who)?;
            if root.parent.is_some() { return Err(invalid("reply thread must be a root message")); }
            Ok(Some(insert(tx,&who.actor,sender,NewMessage { channel:root.channel,parent:Some(thread),author:author.into(),text,labels,task:None })?))
        }
        Edit { id,text } => { validate_text(&text)?; let mut m = message(tx,id,who)?; require_creator(tx,&m,who)?; m.text=text; save(tx,&who.actor,&m)?; Ok(Some(id)) }
        DeleteThread { id } => {
            let root = message(tx,id,who)?;
            require_creator(tx,&root,who)?;
            if root.parent.is_some() { return Err(invalid("delete thread requires a root message")); }
            let messages = all_messages(tx)?;
            // Include terminal dependents: removing their prerequisite would leave dangling IDs.
            if messages.iter().any(|m| m.task.as_ref().is_some_and(|t| t.needs.contains(&id))) {
                return Err(conflict("thread has dependent tasks"));
            }
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM task_placements WHERE parent=?1)", [id], |r| r.get::<_,bool>(0))? {
                return Err(conflict("thread has subtasks"));
            }
            let audience = message_audience(tx,&root)?;
            tx.execute("DELETE FROM schedules WHERE id IN (SELECT schedule FROM schedule_threads WHERE thread=?1)",[id])?;
            let ids: Vec<_> = messages.iter().filter(|m| m.id == id || m.parent == Some(id)).map(|m| m.id).collect();
            let ids = serde_json::to_string(&ids)?;
            // Do not leave historical upserts that can resurrect deleted content on replay.
            // Existing cursors remain valid; AUTOINCREMENT and the tombstone preserve ordering.
            tx.execute("DELETE FROM events WHERE
                (json_extract(body,'$.type')='message' AND json_extract(body,'$.message.id') IN (SELECT value FROM json_each(?1))) OR
                (json_extract(body,'$.type')='reaction' AND json_extract(body,'$.id') IN (SELECT value FROM json_each(?1)))", [&ids])?;
            tx.execute("DELETE FROM reactions WHERE message IN (SELECT value FROM json_each(?1))", [&ids])?;
            // Foreign keys cascade attachments, task blocks, placements and acknowledgements.
            tx.execute("DELETE FROM messages WHERE id IN (SELECT value FROM json_each(?1))", [&ids])?;
            emit(tx,&who.actor,&audience,EventKind::ThreadDeleted { id,channel:root.channel })?;
            Ok(Some(id))
        }
        SetLabels { id,labels } => {
            validate_labels(&labels)?;
            let mut m = message(tx,id,who)?;
            require_creator(tx,&m,who)?;
            if labels.contains_key("area") && is_global(tx,id)? { return Err(invalid("global tasks cannot have an area")); }
            m.labels=labels; save(tx,&who.actor,&m)?; Ok(Some(id))
        }
        EditTask { id, text, labels, needs, assignee } => {
            message(tx,id,who)?;
            let p = placement(tx,id)?;
            apply(tx,who,author,sender,UpdateTask { id,text,labels,needs,assignee,area:p.area,parent:p.parent })
        }
        UpdateTaskFields { id, text, labels, needs, assignee, parent } => {
            message(tx,id,who)?;
            let area = placement(tx,id)?.area;
            apply(tx,who,author,sender,UpdateTask { id,text,labels,needs,assignee,area,parent })
        }
        UpdateTask { id, text, labels, needs, assignee, area, parent } => {
            let mut m = message(tx,id,who)?;
            require_task_manager(tx,&m,who)?;
            validate_text(&text)?;
            validate_labels(&labels)?;
            let p = TaskPlacement { area,parent,global:is_global(tx,id)?,done_by:Vec::new() };
            validate_global_fields(&labels,&needs,assignee.as_deref(),&p)?;
            validate_placement(tx,who,&m.channel,Some(id),&p)?;
            if needs.len() > 128 { return Err(invalid("at most 128 dependencies")); }
            let needs: Vec<_> = needs.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
            let task = m.task.as_mut().unwrap();
            if let Some(a) = assignee.as_ref().filter(|_| task.assignee != assignee) { validate_assignee(tx,a,&m.channel)?; }
            if task.needs != needs {
                let mut pending = needs.clone();
                let mut seen = BTreeSet::new();
                while let Some(dep) = pending.pop() {
                    if dep == id { return Err(invalid("task dependencies cannot form a cycle")); }
                    if !seen.insert(dep) { continue; }
                    pending.extend(dependency(tx,who,&m.channel,dep)?.needs);
                }
            }
            if task.owner.is_some() && (task.assignee != assignee || task.needs != needs) {
                task.state = TaskState::Waiting; task.owner = None; task.expires_at = None;
            }
            task.assignee = assignee;
            task.needs = needs;
            m.text = text;
            m.labels = labels;
            save_placement(tx,id,&p)?;
            save(tx,&who.actor,&m)?;
            Ok(Some(id))
        }
        AcknowledgeTask { id, done } => {
            let m = message(tx,id,who)?;
            if !is_global(tx,id)? { return Err(invalid("only global tasks support acknowledgements")); }
            mutable_task(&m)?;
            let changed = if done {
                tx.execute("INSERT OR IGNORE INTO task_acknowledgements(message,actor) VALUES (?1,?2)", params![id,who.actor])?
            } else {
                tx.execute("DELETE FROM task_acknowledgements WHERE message=?1 AND actor=?2", params![id,who.actor])?
            };
            if changed != 0 { emit_message(tx,&who.actor,&m)?; }
            Ok(Some(id))
        }
        Assign { id,assignee } => {
            let mut m=message(tx,id,who)?; require_task_manager(tx,&m,who)?;
            if is_global(tx,id)? { return Err(invalid("global tasks cannot be assigned")); }
            if let Some(a)=&assignee { validate_assignee(tx,a,&m.channel)?; }
            let t=m.task.as_mut().unwrap();
            if t.assignee != assignee {
                t.assignee=assignee;
                // Handoff releases, rather than transfers, a lease. The recipient must claim.
                if t.owner.is_some() { t.state=TaskState::Waiting; t.owner=None; t.expires_at=None; }
                save(tx,&who.actor,&m)?;
            }
            Ok(Some(id))
        }
        Block { id,text } => {
            validate_text(&text)?;
            let mut m=message(tx,id,who)?; require_task_manager(tx,&m,who)?;
            if is_global(tx,id)? { return Err(invalid("global tasks cannot be blocked")); }
            tx.execute("INSERT INTO task_blocks(message,actor,reason) VALUES (?1,?2,?3) ON CONFLICT(message) DO UPDATE SET actor=excluded.actor,reason=excluded.reason",params![id,who.actor,text])?;
            let t=m.task.as_mut().unwrap(); t.state=TaskState::Blocked; t.owner=None; t.expires_at=None;
            save(tx,&who.actor,&m)?;
            insert(tx,&who.actor,sender,NewMessage { channel:m.channel,parent:Some(id),author:author.into(),text,labels:BTreeMap::new(),task:None })?;
            Ok(Some(id))
        }
        Unblock { id } => {
            let m=message(tx,id,who)?; mutable_task(&m)?;
            let blocker: Option<String>=tx.query_row("SELECT actor FROM task_blocks WHERE message=?1",[id],|r|r.get(0)).optional()?;
            // The worker who released their lease by blocking may also clear that block.
            if blocker.as_deref()!=Some(&who.actor) { require_task_manager(tx,&m,who)?; }
            if blocker.is_none() { return Err(conflict("task has no manual block")); }
            tx.execute("DELETE FROM task_blocks WHERE message=?1",[id])?;
            save(tx,&who.actor,&m)?;
            Ok(Some(id))
        }
        Start { id } => {
            let m=message(tx,id,who)?;
            let t=mutable_task(&m)?;
            let at=now();
            if t.owner.as_deref()!=Some(&who.actor) || !t.expires_at.is_some_and(|end| end>at) {
                return Err(conflict("only the active lease owner can start"));
            }
            record_start(tx,&who.actor,m,at)?;
            if let Some(parent)=placement(tx,id)?.parent {
                record_start(tx,&who.actor,message(tx,parent,who)?,at)?;
            }
            Ok(Some(id))
        }
        Claim { lease_secs,.. } | ClaimNext { lease_secs,.. } | Renew { lease_secs,.. } => {
            if !(1..=86400).contains(&lease_secs) { return Err(invalid("lease_secs must be 1..86400")); }
            let renew = matches!(action, Renew { .. });
            let id = match action {
                ClaimNext { channel, labels, assignment, .. } => {
                    validate_channel(&channel)?;
                    who.check(&channel)?;
                    validate_labels(&labels)?;
                    // Selection and the ordinary lease transition share the action transaction.
                    let candidate = tx.query_row("SELECT m.id FROM messages m
                        LEFT JOIN task_placements p ON p.message=m.id
                        WHERE m.channel=?1 AND COALESCE(p.global,0)=0
                        AND json_extract(m.body,'$.task.state') IN ('ready','in_progress') AND json_extract(m.body,'$.task.owner') IS NULL
                        AND ((?2 AND json_extract(m.body,'$.task.assignee') IS NULL)
                          OR (?3 AND json_extract(m.body,'$.task.assignee')=?4))
                        AND NOT EXISTS (SELECT 1 FROM json_each(?5) wanted WHERE NOT EXISTS
                          (SELECT 1 FROM json_each(m.body,'$.labels') actual WHERE actual.key=wanted.key AND actual.value=wanted.value))
                        ORDER BY m.id LIMIT 1", params![channel, assignment != ClaimAssignment::Mine,
                            assignment != ClaimAssignment::Unassigned, who.actor, serde_json::to_string(&labels)?], |r| r.get(0)).optional()?;
                    let Some(id) = candidate else { return Ok(None) };
                    id
                }
                Claim { id,.. } | Renew { id,.. } => id,
                _ => unreachable!(),
            };
            let mut m = message(tx,id,who)?;
            if is_global(tx,id)? { return Err(conflict("global tasks cannot be claimed or renewed")); }
            let t=m.task.as_mut().ok_or_else(|| invalid("not a task"))?;
            if renew { if t.owner.as_deref()!=Some(&who.actor) { return Err(conflict("only the active lease owner can renew")); } }
            else {
                if !matches!(t.state,TaskState::Ready|TaskState::InProgress) || t.owner.is_some() { return Err(conflict("task is not ready")); }
                if t.assignee.as_ref().is_some_and(|a| a!=&who.actor) { return Err(forbidden()); }
            }
            t.state=if t.started_at.is_some() { TaskState::InProgress } else { TaskState::Claimed }; t.owner=Some(who.actor.clone()); t.expires_at=Some(now()+lease_secs);
            save(tx,&who.actor,&m)?; Ok(Some(id))
        }
        Complete { id,ref text } | CompleteAnyway { id,ref text } | Fail { id,ref text } => {
            let completed=!matches!(action,Fail { .. });
            if !text.is_empty() { validate_text(text)?; }
            let mut m=message(tx,id,who)?; let t=m.task.as_mut().ok_or_else(|| invalid("not a task"))?;
            if t.owner.as_deref()!=Some(&who.actor) { return Err(conflict("only the active lease owner can finish")); }
            if matches!(action,Complete { .. }) && tx.query_row("SELECT EXISTS(SELECT 1 FROM task_placements p JOIN messages m ON m.id=p.message
                WHERE p.parent=?1 AND json_extract(m.body,'$.task.state') IS NOT 'completed')", [id], |r| r.get::<_,bool>(0))? {
                return Err(conflict("all subtasks must be completed before completing the parent"));
            }
            t.state=if completed { TaskState::Completed } else { TaskState::Failed }; t.finished_by=t.owner.take(); t.expires_at=None;
            t.result=(!text.is_empty()).then(||text.clone());
            save(tx,&who.actor,&m)?;
            if !text.is_empty() { insert(tx,&who.actor,sender,NewMessage { channel:m.channel,parent:Some(id),author:author.into(),text:text.clone(),labels:BTreeMap::new(),task:None })?; }
            Ok(Some(id))
        }
        Cancel { id } => {
            let mut m=message(tx,id,who)?; require_creator(tx,&m,who)?;
            let t=m.task.as_mut().ok_or_else(|| invalid("not a task"))?;
            if matches!(t.state,TaskState::Completed|TaskState::Failed|TaskState::Canceled) { return Err(conflict("task is terminal")); }
            t.state=TaskState::Canceled; t.owner=None; t.expires_at=None; save(tx,&who.actor,&m)?; Ok(Some(id))
        }
        React { id,reaction,remove } => {
            let m=message(tx,id,who)?;
            if reaction.trim().is_empty() || reaction.len()>64 || reaction.chars().any(char::is_control) { return Err(invalid("reaction must be 1..64 bytes without control characters")); }
            let changed=if remove { tx.execute("DELETE FROM reactions WHERE message=?1 AND actor=?2 AND reaction=?3",params![id,who.actor,reaction])? } else { tx.execute("INSERT OR IGNORE INTO reactions VALUES (?1,?2,?3)",params![id,who.actor,reaction])? };
            if changed>0 { emit(tx,&who.actor,&message_audience(tx,&m)?,EventKind::Reaction { id,channel:m.channel,reaction,remove })?; } Ok(Some(id))
        }
        a @ (Schedule { .. } | ScheduleFollowup { .. }) => {
            let (channel,text,publish_at,labels,thread,internal) = match a {
                Schedule { channel,text,publish_at,labels } => (channel,text,publish_at,labels,None,false),
                ScheduleFollowup { thread,text,publish_at,labels,internal_only } => {
                    let root = message(tx,thread,who)?;
                    if root.parent.is_some() { return Err(invalid("follow-up requires a root thread")); }
                    (root.channel,text,publish_at,labels,Some(thread),internal_only)
                }
                _ => unreachable!(),
            };
            validate_text(&text)?; validate_labels(&labels)?;
            if publish_at<=now() || publish_at>i64::MAX as u64 { return Err(invalid("publish_at must be in the future")); }
            if thread.is_some() { who.check(&channel)?; } else { ensure_channel(tx,who,&channel)?; }
            tx.execute("INSERT INTO schedules(credential,actor,channel,text,labels,publish_at) VALUES (?1,?2,?3,?4,?5,?6)",params![who.id,author,channel,text,serde_json::to_string(&labels)?,publish_at])?;
            let id=tx.last_insert_rowid() as u64;
            if let Some(sender) = sender { tx.execute("INSERT INTO schedule_senders(schedule,sender) VALUES (?1,?2)", params![id,sender])?; }
            if let Some(thread) = thread { tx.execute("INSERT INTO schedule_threads(schedule,thread,internal) VALUES (?1,?2,?3)",params![id,thread,internal])?; }
            if !internal { emit(tx,&who.actor,&channel,EventKind::Scheduled { id,channel:channel.clone(),publish_at })?; } Ok(Some(id))
        }
        CancelSchedule { id } => {
            let row: Option<(u64,String)> = tx.query_row("SELECT credential,channel FROM schedules WHERE id=?1",[sql_id(id)?],|r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            let (credential,channel)=row.ok_or_else(|| invalid("unknown schedule"))?; who.check(&channel)?;
            if credential!=who.id { return Err(forbidden()); }
            let internal: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM schedule_threads WHERE schedule=?1 AND internal=1)",[id],|r|r.get(0))?;
            tx.execute("DELETE FROM schedules WHERE id=?1",[id])?;
            if !internal { emit(tx,&who.actor,&channel,EventKind::ScheduleCanceled { id,channel:channel.clone() })?; } Ok(Some(id))
        }
    }
}

fn dependency(tx: &Transaction<'_>, who: &Identity, channel: &str, id: u64) -> Result<Task> {
    let m = message(tx,id,who)?;
    if m.channel != channel { return Err(invalid("dependencies must be in the same channel")); }
    if is_global(tx,id)? { return Err(invalid("global tasks cannot be dependencies")); }
    m.task.ok_or_else(|| invalid("dependency must be a visible task"))
}

fn record_start(tx: &Transaction<'_>, actor: &str, mut m: Message, at: u64) -> Result<()> {
    let t=m.task.as_mut().ok_or_else(|| invalid("not a task"))?;
    if t.started_at.is_none() {
        t.started_at=Some(at);
        if matches!(t.state,TaskState::Ready|TaskState::Claimed) { t.state=TaskState::InProgress; }
        save(tx,actor,&m)?;
    }
    Ok(())
}

fn settle_tasks(tx: &Transaction<'_>) -> Result<()> {
    let messages=all_messages(tx)?;
    let mut states: BTreeMap<_,_>=messages.iter().filter_map(|m| m.task.as_ref().map(|t|(m.id,t.state))).collect();
    let manual: BTreeSet<u64>=tx.prepare("SELECT message FROM task_blocks")?.query_map([],|r|r.get(0))?.collect::<std::result::Result<_,_>>()?;
    for mut m in messages {
        let Some(t)=m.task.as_mut() else { continue };
        let expired=t.owner.is_some() && t.expires_at.is_none_or(|v|v<=now());
        if !expired && (t.owner.is_some() || !matches!(t.state,TaskState::Ready|TaskState::Waiting|TaskState::Blocked|TaskState::InProgress)) { continue; }
        let next=if manual.contains(&m.id) || t.needs.iter().any(|id|matches!(states.get(id),Some(TaskState::Failed|TaskState::Canceled|TaskState::Blocked))) { TaskState::Blocked }
            else if t.needs.iter().all(|id|states.get(id)==Some(&TaskState::Completed)) { if t.started_at.is_some() { TaskState::InProgress } else { TaskState::Ready } } else { TaskState::Waiting };
        states.insert(m.id,next);
        if expired || next!=t.state { t.state=next; t.owner=None; t.expires_at=None; save(tx,"system",&m)?; }
    }
    // Dependencies point backwards; updating states during the ordered pass handles
    // both blocking and unblocking the entire descendant closure atomically.
    Ok(())
}
fn maintain(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("DELETE FROM schedules WHERE id IN (SELECT schedule FROM schedule_threads WHERE internal=1) AND credential IN (SELECT id FROM credentials WHERE revoked=1 OR expires<=?1)",[now()])?;
    let expired=tx.prepare("SELECT s.id,s.channel FROM schedules s JOIN credentials c ON c.id=s.credential WHERE c.revoked=1 OR (c.expires IS NOT NULL AND c.expires<=?1)")?.query_map([now()],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    for (id,channel) in expired { tx.execute("DELETE FROM schedules WHERE id=?1",[id])?; emit(tx,"system",&channel,EventKind::ScheduleCanceled { id,channel:channel.clone() })?; }
    tx.execute("UPDATE schedule_threads SET fired=1 WHERE internal=1 AND fired=0 AND schedule IN (SELECT id FROM schedules WHERE publish_at<=?1)",[now()])?;
    let rows=tx.prepare("SELECT s.id,s.actor,s.channel,s.text,s.labels,c.actor,t.thread FROM schedules s JOIN credentials c ON c.id=s.credential LEFT JOIN schedule_threads t ON t.schedule=s.id WHERE s.publish_at<=?1 AND coalesce(t.internal,0)=0 AND c.revoked=0 AND (c.expires IS NULL OR c.expires>?1) ORDER BY s.publish_at,s.id")?.query_map([now()],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get(6)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    for (id,author,channel,text,labels,actor,parent) in rows {
        let sender: Option<String> = tx.query_row("SELECT sender FROM schedule_senders WHERE schedule=?1", [id], |r| r.get(0)).optional()?;
        let message_id=insert(tx,&actor,sender.as_deref(),NewMessage { channel:channel.clone(),parent,author,text,labels:serde_json::from_str(&labels)?,task:None })?;
        tx.execute("DELETE FROM schedules WHERE id=?1",[id])?;
        emit(tx,"system",&channel,EventKind::SchedulePublished { id,channel:channel.clone(),message_id })?;
    }
    settle_tasks(tx)
}

fn bearer(headers: &HeaderMap) -> Result<String> {
    headers.get("authorization").and_then(|h|h.to_str().ok()).and_then(|s|s.strip_prefix("Bearer ")).filter(|s|s.len()<=256).map(str::to_owned).ok_or_else(unauthorized)
}
async fn blocking<T: Send+'static>(f: impl FnOnce()->Result<T>+Send+'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|_|BoardError(StatusCode::INTERNAL_SERVER_ERROR,"board worker failed".into()))?
}
pub fn router(board: Board) -> Router {
    let uploads = Router::new().route("/v1/messages/{id}/attachments", post(http_attach).get(http_attachments))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES));
    Router::new().route("/v1/snapshot",get(http_snapshot)).route("/v1/events",get(http_events)).route("/v1/actions",post(http_action))
        .route("/v1/reactions",get(http_reactions)).route("/v1/schedules",get(http_schedules))
        .route("/v1/organization",get(http_organization))
        .route("/v1/owned-areas",get(http_owned_areas))
        .route("/v1/members",get(http_members)).route("/v1/display-name",post(http_display_name)).route("/v1/me",get(http_me)).route("/v1/join",post(http_join))
        .route("/v1/admin/invites",post(http_invite)).route("/v1/admin/restricted-invites",post(http_restricted_invite))
        .route("/v1/admin/invites/{id}",delete(http_revoke).get(http_explain))
        .route("/v1/admin/invites/{id}/grants/{grant}",delete(http_revoke_grant))
        .route("/v1/attachments/{id}",get(http_download_attachment))
        .layer(axum::extract::DefaultBodyLimit::max(128*1024)).merge(uploads).with_state(board)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentQuery { name: String }
async fn http_attach(State(board):State<Board>, headers:HeaderMap, Path(id):Path<u64>, Query(q):Query<AttachmentQuery>, bytes:axum::body::Bytes)->Result<Json<Attachment>> {
    let token = bearer(&headers)?;
    Ok(Json(blocking(move || board.attach(&token,id,&q.name,bytes.to_vec())).await?))
}
async fn http_attachments(State(board):State<Board>, headers:HeaderMap, Path(id):Path<u64>)->Result<Json<Vec<Attachment>>> {
    let token = bearer(&headers)?;
    Ok(Json(blocking(move || board.attachments(&token,id)).await?))
}
async fn http_download_attachment(State(board):State<Board>, headers:HeaderMap, Path(id):Path<u64>)->Result<Response> {
    let token = bearer(&headers)?;
    let bytes = blocking(move || board.download_attachment(&token,id)).await?;
    Ok(([("content-type","application/octet-stream"),("content-disposition","attachment"),("x-content-type-options","nosniff"),("cache-control","no-store"),("content-security-policy","sandbox; default-src 'none'")],bytes).into_response())
}
async fn http_snapshot(State(board):State<Board>,headers:HeaderMap)->Result<Json<Snapshot>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.snapshot(&token)).await?))
}
#[derive(Deserialize)]
struct OwnedAreasQuery { owner: String }
async fn http_owned_areas(State(board):State<Board>,headers:HeaderMap,Query(q):Query<OwnedAreasQuery>)->Result<Json<BTreeMap<String,Vec<String>>>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.owned_areas(&token,&q.owner)).await?))
}
async fn http_organization(State(board):State<Board>,headers:HeaderMap)->Result<Json<Organization>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.organization(&token)).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembersQuery { channel: String }
async fn http_me(State(board):State<Board>,headers:HeaderMap)->Result<Json<Member>> {
    let token=bearer(&headers)?;Ok(Json(blocking(move||{
        let db=board.lock()?;let who=Board::identity(&db,&token,Some(Grant::Read))?;
        let name=db.query_row("SELECT name FROM actor_display_names WHERE credential=?1",[who.id],|r|r.get(0)).optional()?;
        Ok(Member{actor:who.actor,display_name:name})
    }).await?))
}
async fn http_join(State(board):State<Board>,headers:HeaderMap)->Result<Json<Invite>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.join(&token)).await?))
}
async fn http_members(State(board):State<Board>,headers:HeaderMap,Query(q):Query<MembersQuery>)->Result<Json<Vec<Member>>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.members(&token,&q.channel)).await?))
}
async fn http_display_name(State(board):State<Board>,headers:HeaderMap,Json(request):Json<DisplayNameRequest>)->Result<StatusCode> {
    let token=bearer(&headers)?; blocking(move||board.set_display_name(&token,&request.name)).await?; Ok(StatusCode::NO_CONTENT)
}
async fn http_reactions(State(board):State<Board>,headers:HeaderMap)->Result<Json<Vec<Reaction>>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.reactions(&token)).await?))
}
async fn http_schedules(State(board):State<Board>,headers:HeaderMap)->Result<Json<Vec<ScheduledPost>>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.schedules(&token)).await?))
}
#[derive(Deserialize)]
struct EventsQuery { #[serde(default)] after:u64, #[serde(default)] wait_ms:u64 }
async fn http_events(State(board):State<Board>,headers:HeaderMap,Query(q):Query<EventsQuery>)->Result<Json<Vec<Event>>> {
    let token=bearer(&headers)?;
    if q.wait_ms>30000 { return Err(invalid("wait_ms must be at most 30000")); }
    let deadline=tokio::time::Instant::now()+Duration::from_millis(q.wait_ms);
    loop {
        let b=board.clone(); let t=token.clone();
        let events=blocking(move||b.events(&t,q.after)).await?;
        if !events.is_empty() || tokio::time::Instant::now()>=deadline { return Ok(Json(events)); }
        tokio::time::sleep_until(std::cmp::min(deadline,tokio::time::Instant::now()+Duration::from_millis(200))).await;
    }
}
async fn http_action(State(board):State<Board>,headers:HeaderMap,Json(action):Json<Action>)->Result<Json<Outcome>> {
    let token=bearer(&headers)?;
    let header=|name| -> Result<Option<String>> { headers.get(name).map(|v|v.to_str().map(str::to_owned).map_err(|_|invalid("invalid header encoding"))).transpose() };
    let profile=header("x-neochat-profile")?; let key=header("idempotency-key")?;
    let sender=header("x-neochat-sender")?;
    Ok(Json(blocking(move||board.action_with_sender(&token,profile.as_deref(),key.as_deref(),sender.as_deref(),action)).await?))
}
async fn http_invite(State(board):State<Board>,headers:HeaderMap,Json(request):Json<InviteRequest>)->Result<Json<Invite>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.create_invite(&token,request)).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestrictedInvite { invite: InviteRequest, grants: BTreeSet<Grant> }
async fn http_restricted_invite(State(board):State<Board>,headers:HeaderMap,Json(request):Json<RestrictedInvite>)->Result<Json<Invite>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.issue(&token,request.invite,Some(request.grants))).await?))
}
async fn http_explain(State(board):State<Board>,headers:HeaderMap,Path(id):Path<u64>)->Result<Json<serde_json::Value>> {
    let token=bearer(&headers)?; Ok(Json(blocking(move||board.explain(&token,id)).await?))
}
async fn http_revoke_grant(State(board):State<Board>,headers:HeaderMap,Path((id,grant)):Path<(u64,Grant)>)->Result<StatusCode> {
    let token=bearer(&headers)?; blocking(move||board.revoke_grant(&token,id,grant)).await?; Ok(StatusCode::NO_CONTENT)
}
async fn http_revoke(State(board):State<Board>,headers:HeaderMap,Path(id):Path<u64>)->Result<StatusCode> {
    let token=bearer(&headers)?; blocking(move||board.revoke_invite(&token,id)).await?; Ok(StatusCode::NO_CONTENT)
}