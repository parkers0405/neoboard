use neochat::{client::Client, invite_file::InviteFile};
use serde::{de::{self, MapAccess, Visitor}, Deserialize, Deserializer};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fmt, fs::File, io::BufReader, path::{Path, PathBuf}};

// Value maps keep only the last occurrence. Before routing, reject ambiguous keys
// anywhere along the selector path; never let JSON field order choose a board.
pub(super) fn unambiguous_call(frame: &[u8]) -> bool {
    use serde::de::IgnoredAny;
    #[derive(Deserialize)]
    struct Call { #[serde(rename = "method")] _method: IgnoredAny, #[serde(rename = "params")] _params: Params }
    #[derive(Deserialize)]
    struct Params { #[serde(rename = "name")] _name: IgnoredAny, #[serde(rename = "arguments")] _args: Option<Args> }
    #[derive(Deserialize)]
    struct Args { #[serde(rename = "board_id")] _id: Option<IgnoredAny> }
    serde_json::from_slice::<Call>(frame).is_ok()
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Entry {
    Invite { label: String, invite: PathBuf },
    Local { label: String, data_dir: PathBuf },
}

struct Entries(BTreeMap<String, Entry>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Deserializing directly to a map would silently accept duplicate IDs.
        struct EntriesVisitor;
        impl<'de> Visitor<'de> for EntriesVisitor {
            type Value = Entries;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("a board registry") }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Entries, M::Error> {
                let mut entries = BTreeMap::new();
                while let Some((id, entry)) = map.next_entry::<String, Entry>()? {
                    if id.is_empty() || id.len() > 128 || matches!(id.as_str(), "." | "..")
                        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)) {
                        return Err(de::Error::custom("invalid board ID"));
                    }
                    if entries.insert(id, entry).is_some() { return Err(de::Error::custom("duplicate board ID")); }
                }
                if entries.is_empty() { return Err(de::Error::custom("empty board registry")); }
                Ok(Entries(entries))
            }
        }
        d.deserialize_map(EntriesVisitor)
    }
}

pub(super) enum Connections {
    Single(Client),
    Registry(BTreeMap<String, (String, Client)>),
}

impl Connections {
    pub(super) fn load(path: &Path) -> Option<Self> {
        if !path.is_absolute() || !path.is_file() { return None; }
        let entries: Entries = serde_json::from_reader(BufReader::new(File::open(path).ok()?)).ok()?;
        let mut boards = BTreeMap::new();
        for (id, entry) in entries.0 {
            let (label, client) = match entry {
                Entry::Invite { label, invite } => {
                    let invite = InviteFile::read(&invite).ok()?;
                    (label, Client::new(&invite.endpoint, &invite.token, None).ok()?)
                }
                Entry::Local { label, data_dir } if data_dir.is_absolute() =>
                    (label, Client::discover(Some(&data_dir), None).ok()?),
                _ => return None,
            };
            if label.trim().is_empty() || label.len() > 256 || label.chars().any(|c| c.is_control() || matches!(c, '/' | '\\')) { return None; }
            boards.insert(id, (label, client));
        }
        Some(Self::Registry(boards))
    }

    pub(super) fn set_sender(&mut self, sender: &str) {
        let stamp = |c: &mut Client| *c = c.clone().with_sender(sender);
        match self {
            Self::Single(c) => stamp(c),
            Self::Registry(boards) => boards.values_mut().for_each(|(_, c)| stamp(c)),
        }
    }

    pub(super) fn list(&self) -> Value {
        let Self::Registry(boards) = self else { unreachable!("boards_list is registry-only") };
        json!({"boards":boards.iter().map(|(id, (label, _))| json!({"board_id":id,"label":label})).collect::<Vec<_>>()})
    }

    pub(super) fn select(&self, args: &mut Value) -> Option<&Client> {
        match self {
            Self::Single(c) => Some(c),
            Self::Registry(boards) => {
                let id = args.as_object_mut()?.remove("board_id")?;
                boards.get(id.as_str()?).map(|(_, c)| c)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_dispatch_consumes_only_the_routing_argument() {
        let client = Client::new("http://127.0.0.1:1", "unused", None).unwrap();
        let connections = Connections::Registry(BTreeMap::from([("selected".into(), ("Team".into(), client))]));
        let mut args = json!({"board_id":"selected", "board":"sprint-scope", "labels":{"board_id":"user-data"}});
        assert!(connections.select(&mut args).is_some());
        assert_eq!(args, json!({"board":"sprint-scope", "labels":{"board_id":"user-data"}}));
    }
}