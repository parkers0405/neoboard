//! Admin namespace deletion. All data, scope, and replay changes share a writer commit.
use super::*;

// Expensive admin operations run outside the writer lock. Revalidate the
// host's snapshot at commit; a same-name recreation is not the previous board.
pub(super) fn check_view(tx: &Transaction<'_>, project: &str, cursor: u64) -> Result<()> {
    let stale = tx.query_row("SELECT EXISTS(SELECT 1 FROM board_deletions WHERE project=?1 AND cursor>?2)", params![project,cursor], |r| r.get::<_,bool>(0))?;
    if stale { return Err(conflict("board was deleted while request was pending; refresh and retry")); }
    if !password::project_exists(tx, project)? { return Err(BoardError(StatusCode::NOT_FOUND, "board not found".into())); }
    Ok(())
}

// Even a still-authorized host with a stale composing window must not recreate a
// deleted namespace by posting/scheduling. Explicit host channel creation or admin
// initialization reopens it, without changing the generation or restoring any grant.
pub(super) fn require_open(tx: &Transaction<'_>, project: &str) -> Result<()> {
    if tx.query_row("SELECT EXISTS(SELECT 1 FROM board_deletions WHERE project=?1 AND closed=1)", [project], |r| r.get::<_,bool>(0))? {
        return Err(conflict("board was deleted; explicitly recreate it first"));
    }
    Ok(())
}

pub(super) fn reopen(tx: &Transaction<'_>, project: &str) -> Result<()> {
    tx.execute("UPDATE board_deletions SET closed=0 WHERE project=?1 AND closed=1", [project])?;
    Ok(())
}

pub(super) async fn http_delete(State(board): State<Board>, headers: HeaderMap, Path(project): Path<String>) -> Result<StatusCode> {
    let token = bearer(&headers)?;
    blocking(move || board.delete_board(&token, &project)).await?;
    Ok(StatusCode::NO_CONTENT)
}

impl Board {
    pub fn delete_board(&self, admin: &str, project: &str) -> Result<()> {
        if digest(admin) != self.admin_hash { return Err(unauthorized()); }
        if !crate::valid_project(project) { return Err(invalid("invalid board project")); }
        if project == "general" { return Err(conflict("cannot delete the bootstrap namespace")); }
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let exists = tx.query_row("SELECT EXISTS(SELECT 1 FROM channels WHERE name=?1 OR (name>=?2 AND name<?3))
            OR EXISTS(SELECT 1 FROM areas WHERE board=?1) OR EXISTS(SELECT 1 FROM sprints WHERE board=?1)
            OR EXISTS(SELECT 1 FROM events WHERE channel=?1 OR (channel>=?2 AND channel<?3) OR channel='@board/'||?1)
            OR EXISTS(SELECT 1 FROM board_passwords WHERE project=?1)",
            params![project,format!("{project}/"),format!("{project}0")], |r| r.get::<_,bool>(0))?;
        if !exists { return Err(BoardError(StatusCode::NOT_FOUND, "board not found".into())); }

        // Reserve a strictly newer snapshot cursor even if deletion prunes the entire log.
        // Consume one AUTOINCREMENT value without publishing a namespace or content.
        tx.execute("INSERT INTO events(channel,body) VALUES ('@refresh','{}')", [])?;
        let floor = tx.last_insert_rowid();
        tx.execute("DELETE FROM events WHERE cursor=?1", [floor])?;
        tx.execute("INSERT INTO board_deletions VALUES (?1,?2,1) ON CONFLICT(project) DO UPDATE SET cursor=excluded.cursor, closed=1", params![project,floor])?;
        retire_scopes(&tx, project, floor)?;
        retire_requests(&tx, project)?;

        // The binary range is exactly project/ plus any suffix, including deep paths.
        // Remove RESTRICT parents and scheduled thread references before their messages.
        let in_board = "(channel=?1 OR (channel>=?2 AND channel<?3))";
        let ids = format!("SELECT id FROM messages WHERE {in_board}");
        for sql in [format!("DELETE FROM task_placements WHERE message IN ({ids})"),
            format!("DELETE FROM reactions WHERE message IN ({ids})"), format!("DELETE FROM schedules WHERE {in_board}"),
            format!("DELETE FROM messages WHERE {in_board}"), format!("DELETE FROM events WHERE {in_board} OR channel='@board/' || ?1"),
            "DELETE FROM channels WHERE name=?1 OR (name>=?2 AND name<?3)".into()] {
            tx.execute(&sql, params![project,format!("{project}/"),format!("{project}0")])?;
        }
        tx.execute("DELETE FROM board_passwords WHERE project=?1", [project])?;
        tx.execute("DELETE FROM areas WHERE board=?1", [project])?;
        tx.execute("DELETE FROM sprints WHERE board=?1", [project])?;
        tx.commit()?;
        Ok(())
    }
}

fn surviving_boards(tx: &Transaction<'_>, project: &str) -> Result<BTreeSet<String>> {
    // [] means ALL (including future projects). Represent surviving unrestricted guest
    // access positively, so every existing authorizer, shared join, and older daemon
    // honors the reduction. Include metadata-only namespaces as well as bare roots.
    let mut neighbors = BTreeSet::new();
    for name in tx.prepare("SELECT name FROM channels UNION SELECT board FROM areas UNION SELECT board FROM sprints
        UNION SELECT channel FROM messages UNION SELECT channel FROM events UNION SELECT project FROM board_passwords")?
        .query_map([], |r| r.get::<_,String>(0))? {
        let name = name?;
        let board = name.strip_prefix("@board/").unwrap_or_else(|| board_namespace(&name));
        if board != project { neighbors.insert(format!("{board}/*")); }
    }
    Ok(neighbors)
}

fn retire_scopes(tx: &Transaction<'_>, project: &str, floor: i64) -> Result<()> {
    let mut neighbors = None;
    let credentials = tx.prepare("SELECT id,actor,channels FROM credentials WHERE revoked=0")?
        .query_map([], |r| Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?
        .collect::<std::result::Result<Vec<_>,_>>()?;
    for (id, actor, raw) in credentials {
        let mut scopes: Vec<String> = serde_json::from_str(&raw)?;
        if !scopes.is_empty() && !scopes.iter().any(|s| board_namespace(s) == project) { continue; }
        if actor != "local" {
            if scopes.is_empty() {
                if neighbors.is_none() { neighbors = Some(surviving_boards(tx, project)?); }
                scopes.extend(neighbors.iter().flatten().cloned());
            }
            else { scopes.retain(|s| board_namespace(s) != project); }
            if scopes.is_empty() {
                tx.execute("UPDATE credentials SET revoked=1 WHERE id=?1", [id])?;
                continue;
            }
            tx.execute("UPDATE credentials SET channels=?2 WHERE id=?1", params![id,serde_json::to_string(&scopes)?])?;
        }
        tx.execute("INSERT INTO snapshot_floors VALUES (?1,?2) ON CONFLICT(credential) DO UPDATE SET cursor=excluded.cursor", params![id,floor])?;
    }
    Ok(())
}

// The scope and fingerprint suffice for an exact retry, even after its target disappears.
pub(super) fn fingerprint(tx: &Transaction<'_>, action: &Action, request: &str) -> Result<Option<String>> {
    use Action::*;
    let (sql,id) = match action {
        Post {channel,..} | PostTask {channel,..} | PostSprintTask {channel,..} | ClaimNext {channel,..}
        | Schedule {channel,..} | CreateChannel {channel} | DeleteChannel {channel}
        | CreateArea {board:channel,..} | SetAreaOwner {board:channel,..} | CreateSprint {board:channel,..} => {
            return Ok(Some(format!("{}:{}", board_namespace(channel), digest(request))));
        }
        FinishSprint {id,..} => ("SELECT board FROM sprints WHERE id=?1",id),
        CancelSchedule {id} => ("SELECT channel FROM schedules WHERE id=?1",id),
        Reply {thread:id,..} | ReplyWithLabels {thread:id,..} | ScheduleFollowup {thread:id,..}
        | SetTaskSprint {id,..} | UpdateTask {id,..} | UpdateTaskFields {id,..} | AcknowledgeTask {id,..}
        | Claim {id,..} | Start {id} | Renew {id,..} | Assign {id,..} | EditTask {id,..} | Block {id,..}
        | Unblock {id} | Complete {id,..} | CompleteAnyway {id,..} | Fail {id,..} | Cancel {id} | Edit {id,..}
        | DeleteThread {id} | SetLabels {id,..} | React {id,..} => ("SELECT channel FROM messages WHERE id=?1",id),
    };
    Ok(tx.query_row(sql,[sql_id(*id)?],|r|r.get::<_,String>(0)).optional()?
        .map(|c| format!("{}:{}", board_namespace(&c), digest(request))))
}

// Legacy idempotency records contain original post/edit/reply payloads. Attribute
// them before deleting their targets. Bound memory while migrating in the transaction.
fn retire_requests(tx: &Transaction<'_>, project: &str) -> Result<()> {
    let mut after = 0;
    loop {
        let rows = tx.prepare("SELECT rowid,request FROM idempotency WHERE rowid>?1 AND substr(request,1,1)='[' ORDER BY rowid LIMIT 32")?
            .query_map([after], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        if rows.is_empty() { break; }
        for (row, request) in rows {
            after = row;
            let (_,action): (Option<String>,Action) = serde_json::from_str(&request)?;
            // The original namespace of an already removed ID cannot be recovered.
            let fingerprint = fingerprint(tx, &action, &request)?.unwrap_or_else(|| "deleted".into());
            tx.execute("UPDATE idempotency SET request=?2 WHERE rowid=?1", params![row,fingerprint])?;
        }
    }
    tx.execute("UPDATE idempotency SET request='deleted',outcome='{}' WHERE substr(request,1,length(?1)+1)=?1||':'", [project])?;
    Ok(())
}