//! Private pull inbox. The public event is the immutable source; the ledger stores
//! only references and private read state. Deleting source history cascades.
use super::*;
use serde_json::{json, Value};

pub(super) fn init(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE INDEX IF NOT EXISTS notification_message_history ON events(json_extract(body,'$.message.id'),cursor);
        CREATE INDEX IF NOT EXISTS notification_thread_history ON events(coalesce(json_extract(body,'$.message.parent'),json_extract(body,'$.message.id')),cursor);
        CREATE TABLE IF NOT EXISTS subscriptions(id INTEGER PRIMARY KEY AUTOINCREMENT,
            credential INTEGER NOT NULL REFERENCES credentials(id), thread INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
            kind TEXT NOT NULL, author TEXT NOT NULL, after_cursor INTEGER NOT NULL, registered_cursor INTEGER NOT NULL,
            UNIQUE(credential,thread,kind,author));
        CREATE INDEX IF NOT EXISTS subscriptions_by_thread ON subscriptions(thread);
        CREATE TABLE IF NOT EXISTS inbox(id INTEGER PRIMARY KEY AUTOINCREMENT, credential INTEGER NOT NULL REFERENCES credentials(id),
            event INTEGER NOT NULL REFERENCES events(cursor) ON DELETE CASCADE, reason TEXT NOT NULL, is_read INTEGER NOT NULL DEFAULT 0,
            UNIQUE(credential,event));
        CREATE INDEX IF NOT EXISTS inbox_by_owner ON inbox(credential,is_read,id);
        CREATE INDEX IF NOT EXISTS inbox_by_event ON inbox(event);")?;
    Ok(())
}

// Bare handles use the actor alphabet (plus Unicode letters for display names).
// Quoting makes spaces/punctuation in exact display names unambiguous.
fn mentions(text: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    let mut fence = None;
    let mut inline = 0;
    for line in text.lines() {
        let start = line.trim_start();
        let marker = start.chars().next().unwrap_or(' ');
        let run = start.chars().take_while(|&c| c == marker).count();
        if let Some((ch, n)) = fence {
            if marker == ch && run >= n && start[run..].trim().is_empty() { fence = None; }
            continue;
        }
        if line.starts_with("    ") || line.starts_with('\t') { continue; }
        if matches!(marker, '`' | '~') && run >= 3 { fence = Some((marker,run)); inline = 0; continue; }
        let mut chars = line.char_indices().peekable();
        while let Some((i,c)) = chars.next() {
            if c == '`' {
                let mut n = 1;
                while chars.next_if(|&(_,c)| c == '`').is_some() { n += 1; }
                if inline == n { inline = 0; } else if inline == 0 { inline = n; }
            }
            if inline != 0 || c != '@' { continue; }
            if line[..i].chars().next_back().is_some_and(|p| !p.is_whitespace() && !"([,;:".contains(p)) { continue; }
            if let Some(quoted) = line[i+1..].strip_prefix('"') {
                if let Some((name,_)) = quoted.split_once('"') {
                    names.insert(name);
                    while chars.next_if(|&(j,_)| j <= i+2+name.len()).is_some() {}
                }
            } else {
                let mut end = i+1;
                while let Some((j,c)) = chars.next_if(|&(_,c)| c.is_alphanumeric() || "_.-".contains(c)) { end = j+c.len_utf8(); }
                if chars.peek().is_some_and(|&(_,c)| matches!(c,'/'|'@'|'+')) { continue; }
                let name = line[i+1..end].trim_end_matches('.');
                if !name.is_empty() { names.insert(name); }
            }
        }
    }
    names
}

fn previous(tx: &Transaction<'_>, id: u64, cursor: u64) -> Result<Option<Message>> {
    let body: Option<String> = tx.query_row("SELECT json_extract(body,'$.message') FROM events WHERE json_extract(body,'$.message.id')=?1 AND cursor<?2 ORDER BY cursor DESC LIMIT 1",params![id,cursor],|r|r.get(0)).optional()?;
    Ok(body.map(|s|serde_json::from_str(&s)).transpose()?)
}

fn transition(m: &Message, previous: Option<&Message>) -> Option<&'static str> {
    if m.parent.is_some() { return previous.is_none().then_some("reply"); }
    let state = m.task.as_ref()?.state;
    if previous.and_then(|p|p.task.as_ref()).is_some_and(|t|t.state == state) { return None; }
    match state { TaskState::Completed => Some("completed"), TaskState::Blocked => Some("blocked"), TaskState::Failed => Some("failed"), _ => None }
}

fn deliver(tx: &Transaction<'_>, owner: u64, cursor: u64, reason: &str) -> Result<()> {
    tx.execute("INSERT INTO inbox(credential,event,reason) VALUES (?1,?2,?3) ON CONFLICT(credential,event) DO NOTHING",params![owner,cursor,reason])?;
    Ok(())
}

pub(super) fn on_message(tx: &Transaction<'_>, cursor: u64, actor: &str, m: &Message) -> Result<()> {
    let prev = previous(tx,m.id,cursor)?;
    let mut recipients = BTreeMap::new();
    if m.text.contains('@') && prev.as_ref().is_none_or(|p|p.text != m.text) {
        let old = prev.as_ref().map(|p|mentions(&p.text)).unwrap_or_default();
        let names = mentions(&m.text);
        if !names.is_subset(&old) {
            let members = Board::directory(tx,&m.channel,is_global(tx,m.parent.unwrap_or(m.id))?)?;
            let actors: BTreeSet<_> = members.iter().map(|m|m.actor.as_str()).collect();
            let mut aliases = BTreeMap::new();
            for member in &members {
                if let Some(name) = member.display_name.as_deref() {
                    aliases.entry(name).and_modify(|a|*a=None).or_insert(Some(member.actor.as_str()));
                }
            }
            for name in names.difference(&old) {
                if let Some(recipient) = actors.get(*name).copied().or_else(||aliases.get(*name).copied().flatten()) {
                    if recipient != actor { recipients.insert(recipient.to_owned(),"mention"); }
                }
            }
        }
    }
    let assignee = m.task.as_ref().and_then(|t| t.assignee.as_deref());
    if let Some(to) = assignee.filter(|to| *to != actor && Some(*to) != prev.as_ref().and_then(|p|p.task.as_ref()?.assignee.as_deref())) {
        recipients.insert(to.to_owned(), "assigned");
    }
    if let Some(reason) = transition(m,prev.as_ref()) {
        let rows = tx.prepare("SELECT c.actor FROM subscriptions s JOIN credentials c ON c.id=s.credential WHERE thread=?1 AND kind=?2 AND (author='' OR author=?3) AND after_cursor<?4")?.query_map(params![m.parent.unwrap_or(m.id),reason,m.author,cursor],|r|r.get::<_,String>(0))?;
        for row in rows { recipients.entry(row?).or_insert(reason); }
    }
    for (recipient,reason) in recipients {
        if let Some(who) = active_actor(tx,&recipient)? {
            if permits_message(&who,&m.channel,is_global(tx,m.parent.unwrap_or(m.id))?) && can_read(tx,who.id)? {
                deliver(tx,who.id,cursor,reason)?;
            }
        }
    }
    Ok(())
}

fn can_read(tx: &Transaction<'_>, id: u64) -> Result<bool> {
    Ok(tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM credential_grants WHERE credential=?1 AND NOT EXISTS(SELECT 1 FROM json_each(grants) WHERE value='read'))",[id],|r|r.get(0))?)
}

// Both grants are needed for a private write: the write must not become a
// message/visibility oracle after read access is revoked.
fn private<T>(board: Board, token: String, write: bool, f: impl FnOnce(&Transaction<'_>,&Identity)->Result<T>) -> Result<T> {
    let mut db = board.lock()?;
    let tx = db.transaction()?;
    let who = Board::identity(&tx,&token,Some(Grant::Read))?;
    if write { Board::identity(&tx,&token,Some(Grant::Notifications))?; }
    let result = f(&tx,&who)?;
    tx.commit()?;
    Ok(result)
}

pub(super) fn routes() -> Router<Board> {
    Router::new().route("/v1/inbox",get(inbox)).route("/v1/inbox/{id}/read",post(mark))
        .route("/v1/subscriptions",post(register).get(subscriptions)).route("/v1/subscriptions/{id}",delete(cancel))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page { read: Option<bool>, after: Option<u64>, limit: Option<usize> }

fn visible(who: &Identity, audience: &str) -> bool {
    audience.strip_prefix("@board/").map_or_else(||who.permits(audience),|b|who.permits_board(b))
}

async fn inbox(State(board):State<Board>, headers:HeaderMap, Query(page):Query<Page>) -> Result<impl IntoResponse> {
    let token = bearer(&headers)?;
    let body = blocking(move||private(board,token,false,|tx,who| {
        let limit = page.limit.unwrap_or(100);
        if !(1..=500).contains(&limit) { return Err(invalid("limit must be 1..500")); }
        let read = page.read.unwrap_or(false);
        let mut stmt = tx.prepare("SELECT i.id,e.cursor,e.channel,i.reason,e.body FROM inbox i JOIN events e ON e.cursor=i.event WHERE i.credential=?1 AND i.is_read=?2 AND i.id>?3 ORDER BY i.id")?;
        let mut rows = stmt.query(params![who.id,read,sql_id(page.after.unwrap_or(0))?])?;
        let mut items = Vec::new();
        while let Some(r) = rows.next()? {
            if !visible(who,&r.get::<_,String>(2)?) { continue; }
            let e:Event = serde_json::from_str(&r.get::<_,String>(4)?)?;
            let EventKind::Message { message:m } = e.kind else { return Err(BoardError(StatusCode::INTERNAL_SERVER_ERROR,"unexpected ledger event".into())); };
            items.push(json!({"id":r.get::<_,u64>(0)?,"cursor":r.get::<_,u64>(1)?,"kind":r.get::<_,String>(3)?,"read":read,
                "thread":m.parent.unwrap_or(m.id),"message_id":m.id,"channel":m.channel,"actor":e.actor,"author":m.author,"text":m.text,"created_at":e.created_at}));
            if items.len() == limit { break; }
        }
        Ok(json!({"actor":who.actor,"cursor":cursor(tx)?,"items":items}))
    })).await?;
    Ok(([("cache-control","no-store")],Json(body)))
}

fn missing() -> BoardError { BoardError(StatusCode::NOT_FOUND,"not found".into()) }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadState { read: bool }
async fn mark(State(board):State<Board>, headers:HeaderMap, Path(id):Path<u64>, Json(change):Json<ReadState>) -> Result<StatusCode> {
    let token = bearer(&headers)?;
    blocking(move||private(board,token,true,|tx,who| {
        let audience:Option<String> = tx.query_row("SELECT e.channel FROM inbox i JOIN events e ON e.cursor=i.event WHERE i.credential=?1 AND i.id=?2",params![who.id,sql_id(id)?],|r|r.get(0)).optional()?;
        if !audience.is_some_and(|a|visible(who,&a)) { return Err(missing()); }
        tx.execute("UPDATE inbox SET is_read=?3 WHERE credential=?1 AND id=?2",params![who.id,id,change.read])?;
        Ok(StatusCode::NO_CONTENT)
    })).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter { thread: u64, kind: String, author: Option<String>, after_cursor: Option<u64> }

fn subscription(id:u64, thread:u64, kind:&str, author:&str, after:u64, registered:u64) -> Value {
    json!({"id":id,"thread":thread,"kind":kind,"author":(!author.is_empty()).then_some(author),"after_cursor":after,"registered_cursor":registered})
}

async fn register(State(board):State<Board>, headers:HeaderMap, Json(filter):Json<Filter>) -> Result<Json<Value>> {
    let token = bearer(&headers)?;
    Ok(Json(blocking(move||private(board,token,true,|tx,who| {
        let root = message(tx,filter.thread,who)?;
        if root.parent.is_some() { return Err(invalid("thread must be a root")); }
        let author = filter.author.as_deref().unwrap_or("");
        match filter.kind.as_str() {
            "reply" => {
                if author.split('/').count()>2 { return Err(invalid("reply requires an exact actor or actor/profile")); }
                for segment in author.split('/') { validate_name(segment)?; }
            }
            "completed"|"failed"|"blocked" if root.task.is_some() && filter.author.is_none() => (),
            _ => return Err(invalid("expected a task state or a reply author")),
        }
        let high = cursor(tx)?;
        let after = filter.after_cursor.unwrap_or(high);
        if after > high { return Err(invalid("after_cursor is in the future")); }
        if tx.query_row("SELECT EXISTS(SELECT 1 FROM snapshot_floors WHERE credential=?1 AND cursor>?2)",params![who.id,after],|r|r.get::<_,bool>(0))? { return Err(conflict("board access changed; fetch a new snapshot")); }
        let existing:Option<(u64,u64,u64)> = tx.query_row("SELECT id,after_cursor,registered_cursor FROM subscriptions WHERE credential=?1 AND thread=?2 AND kind=?3 AND author=?4",params![who.id,root.id,filter.kind,author],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id,start,registered)) = existing {
            if filter.after_cursor.is_some_and(|a|a != start) { return Err(conflict("active filter has a different start cursor")); }
            return Ok(subscription(id,root.id,&filter.kind,author,start,registered));
        }
        tx.execute("INSERT INTO subscriptions(credential,thread,kind,author,after_cursor,registered_cursor) VALUES (?1,?2,?3,?4,?5,?6)",params![who.id,root.id,filter.kind,author,after,high])?;
        let id = tx.last_insert_rowid() as u64;
        if after < high {
            let mut stmt = tx.prepare("SELECT cursor,channel,body FROM events WHERE coalesce(json_extract(body,'$.message.parent'),json_extract(body,'$.message.id'))=?1 AND cursor>?2 ORDER BY cursor")?;
            let mut rows = stmt.query(params![root.id,after])?;
            while let Some(row) = rows.next()? {
                if !visible(who,&row.get::<_,String>(1)?) { continue; }
                let e:Event = serde_json::from_str(&row.get::<_,String>(2)?)?;
                if let EventKind::Message {message:m} = e.kind {
                    let c = row.get(0)?;
                    if (author.is_empty() || m.author == author) && transition(&m,previous(tx,m.id,c)?.as_ref()) == Some(filter.kind.as_str()) { deliver(tx,who.id,c,&filter.kind)?; }
                }
            }
        }
        Ok(subscription(id,root.id,&filter.kind,author,after,high))
    })).await?))
}

async fn subscriptions(State(board):State<Board>, headers:HeaderMap) -> Result<impl IntoResponse> {
    let token = bearer(&headers)?;
    let list = blocking(move||private(board,token,false,|tx,who| {
        let mut stmt = tx.prepare("SELECT s.id,thread,kind,author,after_cursor,registered_cursor,m.channel,coalesce(p.global,0) FROM subscriptions s JOIN messages m ON m.id=s.thread LEFT JOIN task_placements p ON p.message=m.id WHERE credential=?1 ORDER BY s.id")?;
        let mut rows = stmt.query([who.id])?;
        let mut list = Vec::new();
        while let Some(r) = rows.next()? {
            if permits_message(who,&r.get::<_,String>(6)?,r.get(7)?) {
                list.push(subscription(r.get(0)?,r.get(1)?,&r.get::<_,String>(2)?,&r.get::<_,String>(3)?,r.get(4)?,r.get(5)?));
            }
        }
        Ok(list)
    })).await?;
    Ok(([("cache-control","no-store")],Json(list)))
}

async fn cancel(State(board):State<Board>, headers:HeaderMap, Path(id):Path<u64>) -> Result<StatusCode> {
    let token = bearer(&headers)?;
    blocking(move||private(board,token,true,|tx,who| {
        let thread:Option<u64> = tx.query_row("SELECT thread FROM subscriptions WHERE credential=?1 AND id=?2",params![who.id,sql_id(id)?],|r|r.get(0)).optional()?;
        message(tx,thread.ok_or_else(missing)?,who).map_err(|e|if e.0==StatusCode::NOT_FOUND { missing() } else { e })?;
        tx.execute("DELETE FROM subscriptions WHERE credential=?1 AND id=?2",params![who.id,id])?;
        Ok(StatusCode::NO_CONTENT)
    })).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_notifies_only_the_new_actor_once() {
        let dir=tempfile::tempdir().unwrap(); let token=random_token(); let admin=random_token();
        let board=Board::open(dir.path().join("board.sqlite3"),&token,&admin).unwrap();
        for actor in ["one","two","other"] { board.create_invite(&admin,InviteRequest{actor:actor.into(),channels:vec![],expires_at:None}).unwrap(); }
        board.action(&token,None,None,Action::Post{channel:"general".into(),text:"Review @one".into(),task:true,labels:Default::default(),needs:vec![],assignee:Some("one".into())}).unwrap();
        let id=board.snapshot(&token).unwrap().messages[0].id;
        for to in [Some("two"),Some("two"),None] { board.action(&token,None,None,Action::Assign{id,assignee:to.map(str::to_owned)}).unwrap(); }
        let db=board.lock().unwrap(); let mut query=db.prepare("SELECT c.actor,i.reason FROM inbox i JOIN credentials c ON i.credential=c.id ORDER BY i.event").unwrap();
        let results=query.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap().collect::<std::result::Result<Vec<_>,_>>().unwrap();
        assert_eq!(results,vec![("one".into(),"assigned".into()),("two".into(),"assigned".into())]);
    }

    #[test]
    fn delivery_failure_rolls_back_source_and_partial_fanout() {
        let dir = tempfile::tempdir().unwrap();
        let token = random_token(); let admin = random_token();
        let board = Board::open(dir.path().join("board.sqlite3"),&token,&admin).unwrap();
        for actor in ["one","two"] {
            board.create_invite(&admin,InviteRequest { actor:actor.into(), channels:vec![], expires_at:None }).unwrap();
        }
        let post = Action::Post {channel:"general".into(),text:"@one @two".into(),task:false,labels:Default::default(),needs:vec![],assignee:None};
        // Deliberately reject the second recipient after the public event and
        // first delivery have already been inserted in the action transaction.
        board.lock().unwrap().execute("CREATE UNIQUE INDEX inject_failure ON inbox(event)",[]).unwrap();
        assert!(board.action(&token,None,Some("retry"),post.clone()).is_err());
        let snapshot = board.snapshot(&token).unwrap(); assert_eq!(snapshot.cursor,0); assert!(snapshot.messages.is_empty());
        assert_eq!(board.lock().unwrap().query_row("SELECT count(*) FROM inbox",[],|r|r.get::<_,u64>(0)).unwrap(),0);
        board.lock().unwrap().execute("DROP INDEX inject_failure",[]).unwrap();
        board.action(&token,None,Some("retry"),post).unwrap();
        assert_eq!(board.lock().unwrap().query_row("SELECT count(*) FROM inbox",[],|r|r.get::<_,u64>(0)).unwrap(),2);
    }
}