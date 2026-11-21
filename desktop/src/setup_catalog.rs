//! Install the shared plugin and its device-local board registry. Never echo catalog contents.
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, fs, io::{ErrorKind, Read, Write}, path::{Component, Path, PathBuf}};

const INVALID: &str = "No changes: expected a strict JSON server map with unique keys (bare or under mcp).";
const UNSAFE: &str = "No changes: unsafe or read-only Neoism catalog path. Check ownership, links and permissions.";
const BUSY: &str = "No changes: catalog is busy or changed. Close other editors and retry.";
const IO: &str = "Could not read or save Neoism’s MCP catalog.";
const LIMIT: usize = 4 * 1024 * 1024;
pub(super) const BOARDS: &str = "neoboard-boards.json";
const BAD_BOARDS: &str = "No changes: invalid board registry. Check the board IDs, labels and absolute connection paths.";

#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum Board {
    Hosted { label: String, invite: PathBuf },
    Local { label: String, data_dir: PathBuf },
}

impl Board {
    fn valid(&self) -> bool {
        let (label, path) = match self { Self::Hosted { label, invite } => (label, invite), Self::Local { label, data_dir } => (label, data_dir) };
        !label.trim().is_empty() && label.len() <= 256 && !label.contains(['/', '\\']) && !label.chars().any(char::is_control) && path.is_absolute()
    }

    fn same_connection(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Hosted { invite: a, .. }, Self::Hosted { invite: b, .. }) => a == b,
            (Self::Local { data_dir: a, .. }, Self::Local { data_dir: b, .. }) => a == b,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Replacement {
    board_id: String,
    dir: PathBuf,
    previous: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome { Done(&'static str), Confirm(Replacement) }

// Borrow the original values so adding a key doesn't round large numbers, rewrite existing
// definitions, or drop wrapper metadata. Refuse duplicate keys in either map we traverse.
fn object(s: &str) -> Result<BTreeMap<String, &RawValue>, &'static str> {
    struct Object;
    impl<'de> Visitor<'de> for Object {
        type Value = BTreeMap<String, &'de RawValue>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("an object") }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut entries = BTreeMap::new();
            while let Some((k, v)) = map.next_entry()? {
                if entries.insert(k, v).is_some() { return Err(serde::de::Error::custom("duplicate")); }
            }
            Ok(entries)
        }
    }
    let mut de = serde_json::Deserializer::from_str(s);
    let out = de.deserialize_map(Object).map_err(|_| INVALID)?;
    de.end().map_err(|_| INVALID)?;
    Ok(out)
}

fn insert(source: &str, map: &str, empty: bool, key: &str, value: &Value) -> String {
    let end = map.as_ptr() as usize - source.as_ptr() as usize + map.len() - 1;
    let mut out = source.to_owned();
    out.insert_str(end, &format!("{}\n  {}: {}\n", if empty { "" } else { "," }, serde_json::json!(key), value));
    out
}

fn add_board(source: &str, id: &str, board: &Board) -> Result<Option<String>, &'static str> {
    let valid_id = |s: &str| !s.is_empty() && s.len() <= 128 && !matches!(s, "." | "..") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !valid_id(id) || !board.valid() { return Err(BAD_BOARDS); }
    let entries = object(source).map_err(|_| BAD_BOARDS)?;
    for (key, raw) in &entries {
        let existing: Board = serde_json::from_str(raw.get()).map_err(|_| BAD_BOARDS)?;
        if !valid_id(key) || !existing.valid() { return Err(BAD_BOARDS); }
        if key == id && !existing.same_connection(board) { return Err("No changes: this board ID already has a different registration."); }
    }
    if entries.contains_key(id) { return Ok(None); }
    let value = serde_json::to_value(board).map_err(|_| BAD_BOARDS)?;
    Ok(Some(insert(source, source.trim_end(), entries.is_empty(), id, &value)))
}

fn linked(m: &fs::Metadata) -> bool {
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 { return true; } // any reparse point
    }
    m.file_type().is_symlink()
}

fn prepare_directory(dir: &Path) -> Result<(), &'static str> {
    if !dir.is_absolute() || dir.parent().is_none() || dir.components().any(|c| c == Component::ParentDir) { return Err(UNSAFE); }
    for p in dir.ancestors().collect::<Vec<_>>().into_iter().rev() {
        #[cfg(windows)]
        if p.components().count() == 1 && p.has_root() == false { continue; }
        let meta = match fs::symlink_metadata(p) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
                builder.create(p).map_err(|_| IO)?;
                fs::symlink_metadata(p).map_err(|_| IO)?
            }
            Err(_) => return Err(IO),
        };
        if linked(&meta) || !meta.is_dir() { return Err(UNSAFE); }
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            let mine = meta.uid() == rustix::process::geteuid().as_raw();
            let sticky_tmp = p != dir && meta.uid() == 0 && meta.mode() & 0o1000 != 0;
            if (!mine && (p == dir || meta.uid() != 0)) || (meta.mode() & 0o022 != 0 && !sticky_tmp) { return Err(UNSAFE); }
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<Option<String>, &'static str> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IO),
    };
    if !m.is_file() || linked(&m) || m.permissions().readonly() { return Err(UNSAFE); }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if m.nlink() != 1 || m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o022 != 0 { return Err(UNSAFE); }
    }
    let mut bytes = vec![];
    fs::File::open(path).map_err(|_| IO)?.take(LIMIT as u64 + 1).read_to_end(&mut bytes).map_err(|_| IO)?;
    if bytes.len() > LIMIT { return Err("No changes: catalog exceeds the size limit."); }
    String::from_utf8(bytes).map(Some).map_err(|_| INVALID)
}

struct Lock(PathBuf);
impl Drop for Lock { fn drop(&mut self) { let _ = fs::remove_dir(&self.0); } }

pub(super) fn server(adapter: &str, registry: &str) -> Value {
    serde_json::json!({"type":"local","command":[adapter,"--boards-file",registry],"enabled":true})
}

pub(super) fn install(dir: &Path, id: &str, adapter: &str, board: &Board, approved: Option<&Replacement>) -> Result<Outcome, &'static str> {
    if dir.ancestors().any(|p| fs::symlink_metadata(p.join(".git")).is_ok()) { return Err("No changes: the Neoism config directory must be outside any repository."); }
    let registry = dir.join(BOARDS);
    let arg = registry.to_str().filter(|s| !s.contains(['{','}']) && !s.chars().any(char::is_control)).ok_or(UNSAFE)?;
    let server = server(adapter, arg);
    prepare_directory(dir)?;
    let lock = dir.join(".neoboard-mcp.lock");
    fs::create_dir(&lock).map_err(|_| BUSY)?;
    let _lock = Lock(lock);
    let path = dir.join("mcp.json");
    let before = read(&path)?;
    let profiles = read(&registry)?;
    let profile_out = add_board(profiles.as_deref().unwrap_or("{}\n"), id, board)?;
    let source = before.as_deref().unwrap_or("{}\n");
    let root = object(source)?;
    let (map, entries) = match root.get("mcp") {
        Some(raw) => (raw.get(), object(raw.get())?),
        None => (source.trim_end(), root),
    };
    if entries.values().any(|v| !v.get().starts_with('{')) { return Err(INVALID); }
    let catalog_out = if let Some(old) = entries.get("neoboard") {
        object(old.get())?;
        if serde_json::from_str::<Value>(old.get()).ok().as_ref() == Some(&server) { None } else {
            let ticket = Replacement { dir: dir.to_owned(), board_id: id.into(), previous: Sha256::digest(old.get().as_bytes()).into() };
            if approved != Some(&ticket) { return Ok(Outcome::Confirm(ticket)); }
            let start = old.get().as_ptr() as usize - source.as_ptr() as usize;
            let mut out = source.to_owned();
            out.replace_range(start..start + old.get().len(), &server.to_string());
            Some(out)
        }
    } else {
        if approved.is_some() { return Err(BUSY); } // Don't revive a binding removed during confirmation.
        Some(insert(source, map, entries.is_empty(), "neoboard", &server))
    };
    if catalog_out.is_none() && profile_out.is_none() { return Ok(Outcome::Done("Already added to Neoism.")); }
    if [&catalog_out,&profile_out].into_iter().flatten().any(|s| s.len() > LIMIT) { return Err("No changes: configuration exceeds the size limit."); }
    if read(&path)? != before || read(&registry)? != profiles { return Err(BUSY); }
    // Publish the registry first so the command never points to a missing file.
    if let Some(out) = &profile_out { save(&registry, &profiles, out)?; }
    if let Some(out) = &catalog_out {
        if let Err(e) = save(&path, &before, out) {
            return Err(if profile_out.is_some() { "Board saved, but plugin registration failed. Check the catalog and retry." } else { e });
        }
    }
    Ok(Outcome::Done("Board added. Reconnect neoboard in Neoism."))
}

fn save(path: &Path, before: &Option<String>, out: &str) -> Result<(), &'static str> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().ok_or(UNSAFE)?).map_err(|_| IO)?;
    tmp.write_all(out.as_bytes()).and_then(|_| tmp.as_file().sync_all()).map_err(|_| IO)?;
    if &read(path)? != before { return Err(BUSY); }
    if before.is_some() { tmp.persist(path).map_err(|_| IO)?; }
    else { tmp.persist_noclobber(path).map_err(|_| BUSY)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_replacement_requires_current_confirmation_and_preserves_neighbors_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let path = dir.join("mcp.json");
        let board = Board::Local { label: "Team".into(), data_dir: dir.join("board") };
        let run = |approved: Option<&Replacement>| install(&dir, "team", "adapter", &board, approved);
        let other = r#""unrelated":{"opaque":123456789012345678901234567890}"#;
        for wrapped in [false, true] {
            let entries = format!("{other},\"neoboard\":{{\"command\":[\"legacy\"]}}");
            let existing = if wrapped { format!("{{\"note\":true,\"mcp\":{{{entries}}}}}\n") } else { format!("{{{entries}}}\n") };
            fs::write(&path, &existing).unwrap();
            let Outcome::Confirm(ticket) = run(None).unwrap() else { panic!() };
            assert_eq!(fs::read_to_string(&path).unwrap(), existing);
            assert!(!dir.join(BOARDS).exists());
            fs::write(&path, existing.replace("legacy", "changed")).unwrap();
            let Outcome::Confirm(new) = run(Some(&ticket)).unwrap() else { panic!() };
            assert_ne!(new, ticket);
            assert!(!dir.join(BOARDS).exists());
            assert!(matches!(run(Some(&new)), Ok(Outcome::Done(_))));
            let after = fs::read_to_string(&path).unwrap();
            assert!(after.contains(other));
            let root: Value = serde_json::from_str(&after).unwrap();
            let map = if wrapped { assert_eq!(root["note"], true); &root["mcp"] } else { &root };
            assert_eq!(map["neoboard"]["command"], json!(["adapter","--boards-file",dir.join(BOARDS)]));
            assert_eq!(map.as_object().unwrap().len(), 2);
            run(None).unwrap();
            assert!(fs::read_to_string(&path).unwrap() == after);
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
                assert_eq!(fs::metadata(dir.join(BOARDS)).unwrap().permissions().mode() & 0o077, 0);
            }
            fs::remove_file(dir.join(BOARDS)).unwrap();
        }
    }

    #[test]
    fn refuses_conflicts_invalid_catalogs_and_contention_without_rewriting() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let path = dir.join("mcp.json");
        let board = Board::Local { label: "Team".into(), data_dir: dir.join("board") };
        let run = |config: &Path| install(config, "team", "adapter", &board, None);
        for invalid in ["", "[]", "{", "{ } garbage", r#"{"mcp":false}"#, r#"{"entry":null}"#, r#"{"old":{},"old":{}}"#,
            r#"{"neoboard":{"command":1,"command":2}}"#] {
            fs::write(&path, invalid).unwrap();
            assert!(run(&dir).is_err());
            assert!(fs::read_to_string(&path).unwrap() == invalid);
            assert!(!dir.join(BOARDS).exists());
        }
        fs::write(&path, "{}").unwrap();
        fs::create_dir(dir.join(".neoboard-mcp.lock")).unwrap();
        assert_eq!(run(&dir), Err(BUSY));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        assert_eq!(run(Path::new("relative")), Err(UNSAFE));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_destinations_remain_untouched() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let dir = base.join("config");
        fs::create_dir(&dir).unwrap();
        let path = dir.join("mcp.json");
        let other = base.join("other");
        fs::write(&other, "{}").unwrap();
        let board = Board::Hosted { label: "Team".into(), invite: other.clone() };
        let run = |config: &Path| install(config, "team", "adapter", &board, None);
        symlink(&other, &path).unwrap();
        assert_eq!(run(&dir), Err(UNSAFE));
        fs::remove_file(&path).unwrap();
        fs::hard_link(&other, &path).unwrap();
        assert_eq!(run(&dir), Err(UNSAFE));
        assert_eq!(fs::read_to_string(&other).unwrap(), "{}");
        fs::remove_file(&path).unwrap();
        symlink(&dir, base.join("linked-dir")).unwrap();
        assert_eq!(run(&base.join("linked-dir/sub")), Err(UNSAFE));
        assert!(!dir.join("sub").exists());
        symlink(&other, dir.join(BOARDS)).unwrap();
        assert_eq!(run(&dir), Err(UNSAFE));
        fs::remove_file(dir.join(BOARDS)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(run(&dir), Err(UNSAFE));
        assert!(!path.exists());
    }

    #[test]
    fn refuses_invalid_profiles_and_repository_config_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let config = base.join("config"); fs::create_dir(&config).unwrap();
        let board = Board::Local { label: "Team".into(), data_dir: base.clone() };
        let run = |dir: &Path| install(dir, "team", "adapter", &board, None);
        let path = json!(base);
        for bad in [r#"{"other":{"label":"Team","invite":"relative"}}"#.into(), r#"{"other":{},"other":{}}"#.into(),
            format!(r#"{{"other":{{"label":"Team","invite":{path},"data_dir":{path}}}}}"#),
            format!(r#"{{"other":{{"label":"Team","invite":{path},"invite":{path}}}}}"#),
            format!(r#"{{"other":{{"invite":{path}}}}}"#), format!(r#"{{"other":{{"label":"Team","token":"secret","invite":{path}}}}}"#), "[]".into()] {
            fs::write(config.join(BOARDS), &bad).unwrap();
            assert_eq!(run(&config), Err(BAD_BOARDS));
            assert_eq!(fs::read_to_string(config.join(BOARDS)).unwrap(), bad);
            assert!(!config.join("mcp.json").exists());
        }
        let conflict = json!({"team":{"label":"Team","invite":base.join("other")}}).to_string();
        fs::write(config.join(BOARDS), &conflict).unwrap();
        assert!(run(&config).is_err());
        assert_eq!(fs::read_to_string(config.join(BOARDS)).unwrap(), conflict);
        fs::write(base.join(".git"), "gitdir: example").unwrap();
        assert!(run(&base.join("repo-config")).is_err());
        assert!(!base.join("repo-config").exists());
    }
}