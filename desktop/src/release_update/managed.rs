//! Managed installs never enter the portable bundle swap. The release API supplies
//! a semver tag, not a URL or command. Both downloads are pinned to that tag.
use super::{Release, RELEASES};
use reqwest::{Url, blocking::Client, redirect::Policy};
use sha2::{Digest, Sha256};
use std::{error::Error, fs::File, io::{self, Read, Seek}, path::Path, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Installer { Msi, Dmg }

impl Installer {
    fn filename(self) -> &'static str {
        match self {
            Self::Msi => "NeoBoard-x86_64.msi",
            Self::Dmg => "neoboard-darwin-aarch64.dmg",
        }
    }

    pub(super) fn available(self, release: &Release) -> bool {
        let name = self.filename();
        let sidecar = format!("{name}.sha256");
        release.assets.iter().any(|a| a == name) && release.assets.contains(&sidecar)
    }

    fn url(self, release: &Release) -> String {
        format!("{RELEASES}/download/{}/{}", release.tag(), self.filename())
    }

    pub(super) fn start(self, release: &Release) -> Result<String> {
        let client = Client::builder().https_only(true).timeout(Duration::from_secs(300))
            .user_agent("neoboard-desktop-update")
            .redirect(Policy::custom(|a| {
                if a.previous().len() < 4 && asset_host(a.url()) { a.follow() }
                else { a.error("refused untrusted release redirect") }
            })).build()?;
        self.handoff(release, |url| {
            let response = client.get(url).send()?.error_for_status()?;
            if response.status() != reqwest::StatusCode::OK { return Err("unexpected release response".into()); }
            Ok(response)
        }, |path| self.launch(path))
    }

    fn handoff<R: Read>(self, release: &Release, mut fetch: impl FnMut(String) -> Result<R>, launch: impl FnOnce(&Path) -> Result<()>) -> Result<String> {
        let url = self.url(release);
        let mut sidecar = String::new();
        fetch(format!("{url}.sha256"))?.take(1025).read_to_string(&mut sidecar)?;
        let expected = checksum(&sidecar, self.filename())?;
        let mut staging = tempfile::Builder::new();
        staging.prefix("neoboard-installer-");
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            staging.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let dir = staging.tempdir()?;
        let path = dir.path().join(self.filename());
        stage_verified(fetch(url)?, &path, expected, MAX_BYTES)?;
        launch(&path)?;
        // The external installer may outlive this desktop and still need the file.
        let _ = dir.keep();
        Ok(format!("Verified installer: {}. You can delete it after the upgrade.", path.display()))
    }

    fn launch(self, path: &Path) -> Result<()> {
        match self {
            #[cfg(windows)]
            Self::Msi => {
                use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
                use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
                let mut buf = [0; 32768];
                // Do not search PATH or the working directory for msiexec.exe.
                let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
                if n == 0 || n >= buf.len() { return Err("cannot locate Windows Installer".into()); }
                let system = PathBuf::from(OsString::from_wide(&buf[..n]));
                std::process::Command::new(system.join("msiexec.exe")).arg("/i").arg(path).arg("/norestart").spawn()?;
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Dmg => {
                if !std::process::Command::new("/usr/bin/hdiutil").args(["attach", "-readonly", "-autoopen"]).arg(path).status()?.success() {
                    return Err("could not open the verified disk image".into());
                }
                Ok(())
            }
            _ => Err(format!("no installer handoff for {} on this platform", path.display()).into()),
        }
    }
}

fn asset_host(url: &Url) -> bool {
    url.scheme() == "https" && url.username().is_empty() && url.password().is_none() && url.port().is_none()
        && matches!(url.host_str(), Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com"))
}

fn checksum<'a>(text: &'a str, name: &str) -> Result<&'a str> {
    let mut fields = text.split_ascii_whitespace();
    let hash = fields.next().unwrap_or_default();
    if text.len() > 1024 || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        || !fields.next().is_some_and(|f| f.strip_prefix('*').unwrap_or(f) == name) || fields.next().is_some() {
        return Err("invalid installer checksum or filename".into());
    }
    Ok(hash)
}

fn stage_verified(body: impl Read, path: &Path, expected: &str, limit: u64) -> Result<()> {
    let mut file = File::create_new(path)?;
    let n = io::copy(&mut body.take(limit + 1), &mut file)?;
    if n == 0 || n > limit { return Err("invalid installer size".into()); }
    file.rewind()?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected) { return Err("installer checksum mismatch".into()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_update::stable_release;
    use std::fs;

    #[test]
    fn verified_versioned_asset_only_reaches_launcher() {
        for (installer, tag, name) in [(Installer::Msi, "v1.2.3", "NeoBoard-x86_64.msi"), (Installer::Dmg, "1.2.3", "neoboard-darwin-aarch64.dmg")] {
            let json = serde_json::json!({"tag_name":tag,"draft":false,"prerelease":false,"html_url":"file:///never", "assets":[{"name":name,"browser_download_url":"https://example.org/never"},{"name":format!("{name}.sha256")}]});
            let release = stable_release(json.to_string().as_bytes()).unwrap();
            assert!(installer.available(&release));
            let base = format!("https://github.com/parkers0405/neoboard/releases/download/{tag}/{name}");
            let sample = b"inert fixture";
            let good = format!("{:x}  {name}\n", Sha256::digest(sample));
            for (sidecar, body, valid) in [(good.as_str(),sample.as_slice(),true), (&good,b"unexpected contents",false), ("bad",sample,false)] {
                let mut urls = Vec::new();
                let mut launched = None;
                let result = installer.handoff(&release, |url| {
                    urls.push(url.clone());
                    if url == format!("{base}.sha256") { Ok(sidecar.as_bytes()) }
                    else { assert_eq!(url, base); Ok(body) }
                }, |path| {
                    assert_eq!(path.file_name().unwrap(), name);
                    assert_eq!(fs::read(path)?, sample);
                    launched = Some(path.to_owned());
                    Ok(())
                });
                assert_eq!(result.is_ok(), valid);
                assert_eq!(launched.is_some(), valid);
                assert_eq!(urls.len(), if sidecar == "bad" { 1 } else { 2 });
                if let Some(path) = launched { fs::remove_dir_all(path.parent().unwrap()).unwrap(); }
            }
        }
    }

    #[test]
    fn reject_untrusted_redirects_misnamed_sidecars_and_oversize_files() {
        for url in ["https://evil.example/file", "https://release-assets.githubusercontent.com.evil.example/file", "http://objects.githubusercontent.com/f", "https://github.com@evil.example/f", "https://release-assets.githubusercontent.com:1234/f"] {
            assert!(!asset_host(&Url::parse(url).unwrap()));
        }
        assert!(asset_host(&Url::parse("https://release-assets.githubusercontent.com/a").unwrap()));
        assert!(checksum(&format!("{}  other.msi", "a".repeat(64)), Installer::Msi.filename()).is_err());
        let dir = tempfile::tempdir().unwrap();
        let sample = b"too large";
        assert!(stage_verified(&sample[..], &dir.path().join("test"), &format!("{:x}", Sha256::digest(sample)), 8).is_err());
    }
}