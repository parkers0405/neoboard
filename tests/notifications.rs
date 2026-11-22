//! HTTP only, against a private tempdir and an ephemeral loopback daemon.
use neochat::protocol::Discovery;
use reqwest::blocking::{Client, Response};
use serde_json::{json, Value};
use std::{fs, io::{BufRead,BufReader}, path::Path, process::{Child,Command,Stdio}, time::Duration};

struct Server { process:Child, endpoint:Discovery, admin:String, client:Client }
impl Drop for Server { fn drop(&mut self) { let _=self.process.kill(); let _=self.process.wait(); } }
impl Server {
    fn start(dir:&Path)->Self {
        let mut process=Command::new(env!("CARGO_BIN_EXE_neochatd")).args(["--portable-v1","--bind","127.0.0.1:0","--data-dir"]).arg(dir)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut line=String::new(); BufReader::new(process.stdout.take().unwrap()).read_line(&mut line).unwrap(); assert_eq!(line.trim(),"READY");
        Self {process,endpoint:serde_json::from_slice(&fs::read(dir.join("endpoint.json")).unwrap()).unwrap(),admin:fs::read_to_string(dir.join("admin-token")).unwrap(),
            client:Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap()}
    }
    fn req(&self,method:&str,path:&str,token:&str,body:Value)->Response {
        let mut req=self.client.request(method.parse().unwrap(),format!("{}/v1/{path}",self.endpoint.url)).bearer_auth(token);
        if !body.is_null() { req=req.json(&body); }
        req.send().unwrap()
    }
    fn api(&self,method:&str,path:&str,token:&str,body:Value)->Value {
        let r=self.req(method,path,token,body); let status=r.status(); let text=r.text().unwrap(); assert!(status.is_success(),"{method} {path}: {status}: {text}");
        if text.is_empty() {Value::Null} else {serde_json::from_str(&text).unwrap()}
    }
    fn get(&self,path:&str,token:&str)->Value {self.api("GET",path,token,Value::Null)}
    fn act(&self,token:&str,body:Value)->Value {self.api("POST","actions",token,body)}
    fn invite(&self,name:&str,channels:&[&str],grants:&[&str])->Value {
        self.api("POST","admin/restricted-invites",&self.admin,json!({"invite":{"actor":name,"channels":channels},"grants":grants}))
    }
}
fn token(invite:&Value)->&str {invite["token"].as_str().unwrap()}
fn items(page:&Value)->&Vec<Value> {page["items"].as_array().unwrap()}

#[test]
fn durable_replay_transitions_read_state_and_cancel() {
    let dir=tempfile::tempdir().unwrap(); let s=Server::start(dir.path()); let host=s.endpoint.token.clone();
    let invite=s.invite("watch",&["general"],&["read","notifications"]); let w=token(&invite);
    let root=s.act(&host,json!({"type":"post","channel":"general","task":true,"text":"delegate"})); let id=&root["id"];
    let filter=json!({"thread":id,"kind":"blocked","after_cursor":root["cursor"]});
    s.act(&host,json!({"type":"block","id":id,"text":"first"}));
    s.act(&host,json!({"type":"edit","id":id,"text":"still blocked"}));
    let sub=s.api("POST","subscriptions",w,filter.clone()); assert_eq!(sub["after_cursor"],root["cursor"]);
    assert_eq!(s.api("POST","subscriptions",w,filter.clone()),sub);
    let sub_path=format!("subscriptions/{}",sub["id"]);
    s.api("DELETE",&sub_path,w,Value::Null);
    let sub=s.api("POST","subscriptions",w,filter); // historical event already delivered
    let reply=json!({"thread":id,"kind":"reply","author":"local/worker"});
    let reply_sub=s.api("POST","subscriptions",w,reply); assert_eq!(reply_sub["after_cursor"],reply_sub["registered_cursor"]);
    let completed=s.api("POST","subscriptions",w,json!({"thread":id,"kind":"completed"}));
    s.act(&host,json!({"type":"reply","thread":id,"text":"wrong author"}));
    let act=json!({"type":"reply","thread":id,"text":"@watch check"});
    for _ in 0..2 { s.client.post(format!("{}/v1/actions",s.endpoint.url)).bearer_auth(&host).header("x-neochat-profile","worker").header("idempotency-key","reply").json(&act).send().unwrap().error_for_status().unwrap(); }
    let page=s.get("inbox",w); assert_eq!(page["actor"],"watch"); assert_eq!(items(&page).len(),2); assert_eq!(page,s.get("inbox",w));
    assert_eq!(items(&page)[0]["kind"],"blocked"); assert_eq!(items(&page)[1]["kind"],"mention");
    assert_eq!(items(&s.get(&format!("inbox?after={}&limit=1",items(&page)[0]["id"]),w)),&vec![items(&page)[1].clone()]);
    let mark=format!("inbox/{}/read",items(&page)[0]["id"]);
    assert_eq!(s.req("POST",&mark,&host,json!({"read":true})).status(),404);
    s.api("POST",&mark,w,json!({"read":true}));
    drop(s); // abrupt process exit; no graceful flush or in-memory watcher survives
    let s=Server::start(dir.path()); assert_eq!(s.get("subscriptions",w),json!([sub,reply_sub,completed]));
    assert_eq!(items(&s.get("inbox",w)).len(),1); assert_eq!(items(&s.get("inbox?read=true",w))[0]["id"],items(&page)[0]["id"]);
    s.api("POST",&mark,w,json!({"read":false}));
    s.act(&host,json!({"type":"unblock","id":id})); s.act(&host,json!({"type":"block","id":id,"text":"second"}));
    s.api("DELETE",&format!("subscriptions/{}",sub["id"]),w,Value::Null);
    s.act(&host,json!({"type":"unblock","id":id})); s.act(&host,json!({"type":"block","id":id,"text":"ignored"}));
    s.act(&host,json!({"type":"unblock","id":id})); s.act(&host,json!({"type":"claim","id":id,"lease_secs":60})); s.act(&host,json!({"type":"complete","id":id,"text":""}));
    let page=s.get("inbox",w); assert_eq!(items(&page).iter().map(|i|i["kind"].as_str().unwrap()).collect::<Vec<_>>(),["blocked","mention","blocked","completed"]);
    // A late subscription recognizes only the original reply by the exact author,
    // not edits. Overlap with the prior mention leaves its delivery ID untouched.
    s.api("DELETE",&format!("subscriptions/{}",reply_sub["id"]),w,Value::Null);
    s.api("POST","subscriptions",w,json!({"thread":id,"kind":"reply","author":"local/worker","after_cursor":0}));
    assert_eq!(s.get("inbox",w),page);
    let failed=s.act(&host,json!({"type":"post","channel":"general","task":true,"text":"another task"}))["id"].clone();
    let dependent=s.act(&host,json!({"type":"post","channel":"general","task":true,"text":"waiting","needs":[failed]}))["id"].clone();
    s.api("POST","subscriptions",w,json!({"thread":dependent,"kind":"blocked"}));
    s.api("POST","subscriptions",w,json!({"thread":failed,"kind":"failed"}));
    s.api("POST","subscriptions",w,json!({"thread":failed,"kind":"reply","author":"local"}));
    s.act(&host,json!({"type":"claim","id":failed,"lease_secs":60})); s.act(&host,json!({"type":"fail","id":failed,"text":"reason"}));
    let page=s.get("inbox",w); assert_eq!(items(&page).len(),7); assert_eq!(items(&page)[4]["kind"],"failed"); assert_eq!(items(&page)[5]["kind"],"reply");
    assert_eq!(items(&page)[6]["kind"],"blocked"); assert_eq!(items(&page)[6]["thread"],dependent); assert_eq!(items(&page)[6]["actor"],"system");
}

#[test]
fn retained_history_cannot_bypass_a_narrowed_scope() {
    let dir=tempfile::tempdir().unwrap(); let s=Server::start(dir.path());
    let reader=s.invite("reader",&["secret","general"],&["read","notifications"]); let t=token(&reader);
    let root=s.act(&s.endpoint.token,json!({"type":"post","channel":"secret","text":"@reader"}));
    let sub=s.api("POST","subscriptions",t,json!({"thread":root["id"],"kind":"reply","author":"local"}));
    let mark=format!("inbox/{}/read",items(&s.get("inbox",t))[0]["id"]); drop(s);
    // Narrow the stored credential *without* pruning any of its source rows,
    // so a cascade cannot mask a missing read-time permission check.
    let db=neochat::storage::Connection::open(dir.path().join("board.sqlite3")).unwrap();
    db.execute("UPDATE credentials SET channels='[\"general\"]' WHERE actor='reader'",[]).unwrap(); drop(db);
    let s=Server::start(dir.path()); s.act(&s.endpoint.token,json!({"type":"reply","thread":root["id"],"text":"still secret"}));
    assert!(items(&s.get("inbox",t)).is_empty()); assert_eq!(s.get("subscriptions",t),json!([]));
    assert_eq!(s.req("POST",&mark,t,json!({"read":true})).status(),404);
    assert_eq!(s.req("DELETE",&format!("subscriptions/{}",sub["id"]),t,Value::Null).status(),404);
    assert_eq!(s.req("POST","subscriptions",t,json!({"thread":root["id"],"kind":"reply","author":"local","after_cursor":0})).status(),404);
}

#[test]
fn mentions_grants_and_visibility() {
    let dir=tempfile::tempdir().unwrap(); let s=Server::start(dir.path()); let host=&s.endpoint.token;
    let alice=s.invite("alice",&["general","secret"],&["read","notifications","post","display_name"]); let a=token(&alice);
    let other=s.invite("other",&["general"],&["read","display_name"]); let o=token(&other);
    let hidden=s.invite("hidden",&["elsewhere"],&["read","display_name"]); let h=token(&hidden);
    for t in [a,o,h] { s.api("POST","display-name",t,json!({"name":"Blue Jay"})); }
    let denied=s.invite("write-only",&["general"],&["notifications"]);
    let post=|channel:&str,text:&str|s.act(host,json!({"type":"post","channel":channel,"text":text}));
    let root=post("general","@\"Blue Jay\" prefix @alice-suffix x@alice.com `@alice`\n```rust\n@alice\n```\n    @alice\n@hidden");
    assert!(items(&s.get("inbox",a)).is_empty()); assert!(items(&s.get("inbox",o)).is_empty()); assert!(items(&s.get("inbox",h)).is_empty());
    s.act(a,json!({"type":"post","channel":"general","text":"@alice"}));
    assert!(items(&s.get("inbox",a)).is_empty());
    s.api("POST","display-name",o,json!({"name":"alice"})); // exact actor wins
    post("general","@alice, @\"Blue Jay\" and @alice!");
    assert_eq!(items(&s.get("inbox",a)).len(),1); assert!(items(&s.get("inbox",o)).is_empty());
    post("general","@\"Blue Jay\""); assert_eq!(items(&s.get("inbox",a)).len(),2); // hidden alias not ambiguous
    let filter=json!({"thread":root["id"],"kind":"reply","author":"local"});
    assert_eq!(s.req("POST","subscriptions",o,filter.clone()).status(),403);
    assert_eq!(s.req("POST","inbox/1/read",o,json!({"read":true})).status(),403);
    assert_eq!(s.req("GET","inbox",token(&denied),Value::Null).status(),403);
    assert_eq!(s.req("POST","subscriptions",token(&denied),filter.clone()).status(),403);
    assert_eq!(s.req("GET","inbox","bad",Value::Null).status(),401);
    let secret=post("secret","@alice secret text"); let id=&secret["id"];
    let filter=json!({"thread":id,"kind":"reply","author":"local"});
    assert_eq!(s.req("POST","subscriptions",host,json!({"thread":id,"kind":"reply","author":"local","after_cursor":99999})).status(),400);
    let sub=s.api("POST","subscriptions",a,filter.clone());
    assert_eq!(s.req("DELETE",&format!("subscriptions/{}",sub["id"]),host,Value::Null).status(),404);
    assert_eq!(s.get("subscriptions",host),json!([]));
    let unseen=s.invite("outsider",&["general"],&["read","notifications"]);
    assert_eq!(s.req("POST","subscriptions",token(&unseen),filter).status(),404);
    s.api("DELETE","admin/boards/secret",&s.admin,Value::Null);
    let page=s.get("inbox",a); assert_eq!(items(&page).len(),2); assert!(!page.to_string().contains("secret")); assert_eq!(s.get("subscriptions",a),json!([]));
    s.api("DELETE",&format!("admin/invites/{}/grants/read",alice["id"]),&s.admin,Value::Null);
    assert_eq!(s.req("GET","inbox",a,Value::Null).status(),403); assert_eq!(s.req("GET","subscriptions",a,Value::Null).status(),403);
    s.api("DELETE",&format!("admin/invites/{}",alice["id"]),&s.admin,Value::Null); assert_eq!(s.req("GET","inbox",a,Value::Null).status(),401);
}