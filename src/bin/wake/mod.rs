mod store;
mod inbox;
mod runtime;
mod service;
mod worker;
mod provision;
mod neoism_pair;
mod live;

use clap::{Args, Subcommand};
use rand::RngCore;
use std::{env,path::{Path,PathBuf}};
use store::{Binding,Source,transaction};

#[derive(Args)]
pub struct Options { #[command(subcommand)] command: Command }
#[derive(Args)]
struct Scope {
    #[arg(long)] board_id: String,
    #[arg(long, default_value=store::DEFAULT_CHAT)] chat_id: String,
    #[arg(long, value_parser=["neoism","claude","codex"])] harness: String,
}
impl Scope {
    fn key(&self)->Result<String,String>{
        store::binding_key(&self.board_id, &self.harness, &self.chat_id)
    }
}
#[derive(Subcommand)]
enum Command {
    /// Create a ten-minute, single-use pairing code. Does not enable wakes.
    Prepare { #[command(flatten)] scope:Scope, #[arg(long)] project:String, #[arg(long,required_unless_present="create_agent",conflicts_with="create_agent")] invite:Option<PathBuf>, #[arg(long,requires="create_agent")] local_data_dir:Option<PathBuf>, #[arg(long,requires="create_agent")] local_endpoint:Option<String>, #[arg(long,requires="local_data_dir")] create_agent:bool },
    /// Run in the dedicated agent conversation's trusted workspace. Never selects the latest chat.
    Pair { #[arg(long,required_unless_present="request",conflicts_with="request")] code:Option<String>, #[arg(long)] request:Option<String>, #[arg(long)] session:Option<String>, #[arg(long)] executable:Option<PathBuf>, #[arg(long)] workspace:Option<PathBuf>, #[arg(long)] origin:Option<String> },
    /// Enable inbox turns for the paired conversation (may use model credits and tools).
    On(Scope),
    /// Stop admitting new turns; leave an already reserved turn and all inbox messages alone.
    Off(Scope),
    /// Remove only this local chat pairing. Does not revoke board membership.
    Disconnect(Scope),
    Status(Scope),
    /// List this board's paired chats as JSON, without credentials or local paths.
    List { #[arg(long)] board_id: String },
    /// Explicitly request one diagnostic turn, without changing the automatic wake switch.
    Test(Scope),
    /// Acknowledge a single diagnostic nonce from the paired runtime's turn.
    Ack { #[arg(long)] code:String },
    /// After inspecting the session, skip a previously uncertain wake without resending it.
    Resolve(Scope),
    /// Run the consumer in the foreground (normally started by the per-user login service).
    Run { #[arg(long)] once:bool },
    /// Hook entry point installed in a paired Claude Code conversation. Waits for that
    /// conversation's next delivery, prints it and exits 2 so the open window starts a turn.
    #[command(hide=true)]
    Watch,
}
fn nonce()->String { let mut bytes=[0u8;16];rand::rngs::OsRng.fill_bytes(&mut bytes);bytes.iter().map(|b|format!("{b:02x}")).collect() }
fn now()->u64 { neochat::board::now() }
pub(super) fn quoted_exe()->Result<String,String>{
    let p=env::current_exe().map_err(|_|"Cannot locate neoboard")?;
    let s=p.to_str().filter(|x|!x.chars().any(char::is_control)).ok_or("Cannot quote this install path")?;
    #[cfg(not(windows))] return Ok(format!("'{}'",s.replace('\'',"'\\''")));
    #[cfg(windows)] return Ok(format!("& '{}'",s.replace('\'',"''")));
}
fn executable(name:&str)->Option<PathBuf>{
    let filename=format!("{}{}",name,env::consts::EXE_SUFFIX);
    env::var_os("PATH").into_iter().flat_map(|p|env::split_paths(&p).collect::<Vec<_>>()).filter(|p|p.is_absolute()).map(|p|p.join(&filename)).find(|p|p.is_file())
}
fn target(h:&str,session:Option<String>,workspace:Option<PathBuf>,bin:Option<PathBuf>,origin:Option<String>)->Result<runtime::Target,String>{
    let keys:&[&str]=match h {"neoism"=>&["NEOISM_SESSION_ID"],"codex"=>&["CODEX_THREAD_ID"],"claude"=>&["CLAUDE_CODE_SESSION_ID","CLAUDE_SESSION_ID"],_=>return Err("Unknown harness".into())};
    let mut session=session;
    for reported in keys.iter().filter_map(|k|env::var(k).ok().filter(|s|!s.is_empty())) {
        if session.as_ref().is_some_and(|id|id!=&reported){return Err("Conversation IDs disagree; nothing was paired.".into());}
        session=Some(reported);
    }
    let session=session.ok_or("This shell does not expose its conversation ID. Run the pairing command inside the dedicated conversation with --session <its exact ID>. No latest-session lookup is used.")?;
    let workspace=workspace.or_else(||env::current_dir().ok()).ok_or("Cannot locate workspace")?;
    let (bin,origin)=if h=="neoism" {(None,origin.or_else(||env::var("NEOISM_AGENT_SERVER").ok()))} else {(Some(bin.or_else(||executable(h)).ok_or("Runtime executable not found; use --executable with its absolute path")?),None)};
    let t=runtime::Target {harness:h.into(),session,workspace,executable:bin,origin};runtime::validate(&t)?;Ok(t)
}
fn duplicate(a:&Source,b:&Source)->bool {a.origin==b.origin && a.project==b.project && a.actor==b.actor}
fn pair(dir:&Path,code:String,session:Option<String>,bin:Option<PathBuf>,workspace:Option<PathBuf>,origin:Option<String>)->Result<(),String>{
    let source=transaction(dir,|s|s.pending.get(&code).filter(|p|p.expires>now()).map(|p|p.source.clone()).ok_or("Pairing code expired or already used".into()))?;
    let (t,bearer)=if source.harness=="neoism" {neoism_pair::resolve(session,workspace,bin,origin)?} else {(target(&source.harness,session,workspace,bin,origin)?,None)};
    runtime::probe(&t,bearer.as_deref())?;inbox::poll(&source,0)?;
    transaction(dir,|s|{let p=s.pending.remove(&code).ok_or("Pairing code is no longer valid")?;
        if p.expires<=now(){return Err("Pairing code expired".into());}
        let key=store::binding_key(&source.board,&source.harness,&source.chat_id)?;
        if s.bindings.contains_key(&key) || s.bindings.values().any(|b|duplicate(&b.source,&source)){return Err("This board actor is already paired. Give each dedicated conversation its own scoped actor.".into());}
        s.bindings.insert(key,Binding{source,target:t,bearer,enabled:false,watermark:0,uncertain:None,last:"Paired; no turn sent".into(),test:None,live:false,live_test:false});Ok(())})?;
    println!("Paired. Automatic wake is off. Choose Turn on when ready.");Ok(())
}
fn toggle(dir:&Path,key:&str,on:bool)->Result<(),String>{
    let _guard=store::lock(dir,"control.lock",true)?;
    let b=worker::binding(dir,key)?;
    if on {inbox::poll(&b.source,b.watermark)?;runtime::probe(&b.target,b.bearer.as_deref())?;if b.uncertain.is_some(){return Err("Inspect the previous uncertain turn and use wake resolve before enabling".into());}}
    let any=transaction(dir,|s|{s.bindings.get_mut(key).ok_or("Not paired")?.enabled=on;Ok(s.bindings.values().any(|x|x.enabled))})?;
    let manual=env::var_os("NEOBOARD_WAKE_HOME").is_some() && env::var("NEOBOARD_WAKE_MANUAL").as_deref()==Ok("1");
    if on && !manual { if let Err(e)=service::ensure_running(){transaction(dir,|s|{s.bindings.get_mut(key).unwrap().enabled=false;Ok(())})?;return Err(e);} }
    else if !any {service::uninstall()?;}
    // Best effort: without it, wakes still run in the background as before.
    let live=on && b.target.harness!="neoism" && {
        let installed=live::install(&b);
        transaction(dir,|s|{let cur=s.bindings.get_mut(key).ok_or("Not paired")?;cur.live=installed.is_ok();
            if let Err(e)=&installed{cur.last=format!("Open-window wake unavailable ({e}); wakes run in the background");} Ok(())})?;
        installed.is_ok() && !b.live
    };
    println!("{}{}",if on {"On. New and unread inbox notifications can wake this conversation."}else{"Off. Pairing and inbox preserved; a turn already in progress may finish."},
        if live {" Restart the paired conversation once so wakes appear in its open window."}else{""});Ok(())
}
pub fn run(options:Options)->Result<(),String>{
    run_in(options, store::home()?)
}
fn run_in(options:Options, dir:PathBuf)->Result<(),String>{
    match options.command {
        Command::Prepare{scope,project,invite,local_data_dir,local_endpoint,create_agent}=>{
            let key=scope.key()?;if !neochat::valid_project(&project){return Err("Invalid project".into());}
            let _guard=store::lock(&dir,"control.lock",true)?;
            if transaction(&dir,|s|Ok(s.bindings.contains_key(&key)))?{return Err("Already paired; use On or Off".into());}
            let invite=if create_agent{provision::local(&dir,local_data_dir.as_deref().ok_or("Missing local data directory")?,local_endpoint.as_deref(),&key,&project,&scope.harness)?}else{invite.ok_or("Missing scoped invite")?};
            let (origin,actor)=inbox::identify(&invite,&project)?;
            let source=Source{board:scope.board_id,chat_id:scope.chat_id,project,invite,origin,actor,harness:scope.harness,local:local_data_dir.filter(|_|create_agent)};let code=nonce();
            transaction(&dir,|s|s.prepare(code.clone(),source,now()))?;
            println!("{} wake pair --code {}",quoted_exe()?,code);Ok(())
        },
        Command::Pair{code,request,session,executable,workspace,origin}=>{
            use sha2::{Digest,Sha256};
            let code=match code { Some(code)=>code, None=>transaction(&dir,|s|s.pending.keys().find(|code|request.as_deref()==Some(&format!("{:x}",Sha256::digest(code.as_bytes())))).cloned().ok_or("Pairing request not found on this device. Generate a fresh prompt in NeoBoard.".into()))? };
            pair(&dir,code,session,executable,workspace,origin)
        },
        Command::List{board_id}=>{let rows=transaction(&dir,|s|s.list(&board_id))?;println!("{}",serde_json::Value::Array(rows));Ok(())},
        Command::Status(scope)=>{let key=scope.key()?;let _=worker::binding(&dir,&key);let text=transaction(&dir,|s|Ok(match s.bindings.get(&key){None=>"Not paired".to_string(),Some(b)=>format!("{} | @{} | {} | {}{}",if b.enabled{"On"}else{"Off"},b.source.actor,b.target.session,b.last,if b.uncertain.is_some(){" | Reserved; if interrupted, inspect session before wake resolve"}else{""})}))?;println!("{text}");Ok(())},
        Command::On(scope)=>toggle(&dir,&scope.key()?,true),Command::Off(scope)=>toggle(&dir,&scope.key()?,false),
        Command::Disconnect(scope)=>{
            let key=scope.key()?;
            let _control=store::lock(&dir,"control.lock",true)?;
            let binding=transaction(&dir,|s|s.bindings.get(&key).cloned().ok_or("Not paired".into()))?;
            let _session=worker::session_lock(&dir,&binding)?;
            let any=transaction(&dir,|s|{
                s.bindings.remove(&key);
                s.pending.retain(|_,p|!(p.source.board==scope.board_id && p.source.harness==scope.harness && p.source.chat_id==scope.chat_id));
                Ok(s.bindings.values().any(|b|b.enabled))
            })?;
            if !any && binding.enabled { service::uninstall()?; }
            println!("Not paired"); Ok(())
        },
        Command::Test(scope)=>worker::test(&dir,&scope.key()?),Command::Run{once}=>worker::run(&dir,once),
        Command::Watch=>std::process::exit(live::watch(&dir)),
        Command::Ack{code}=>transaction(&dir,|s|{let b=s.bindings.values_mut().find(|b|b.test.as_ref()==Some(&code)).ok_or("No pending test for this code")?;b.test=None;b.last="Test acknowledged by the conversation".into();println!("Acknowledged");Ok(())}),
        Command::Resolve(scope)=>{let key=scope.key()?;let b=transaction(&dir,|s|s.bindings.get(&key).cloned().ok_or("Not paired".into()))?;let _guard=worker::session_lock(&dir,&b)?;transaction(&dir,|s|{let b=s.bindings.get_mut(&key).ok_or("Not paired")?;if b.uncertain.take().is_none(){return Err("No uncertain delivery".into());}b.test=None;b.last="Operator skipped the uncertain delivery; not replayed".into();println!("Skipped without resending");Ok(())})}
    }
}

#[cfg(test)]
mod chat_tests;