use super::*;
use std::io::Write;
use zip::{write::SimpleFileOptions, ZipWriter};

fn fixture(ext: &str, last: &str) -> File {
    let mut zip = ZipWriter::new(tempfile::tempfile().unwrap());
    for name in BINS.iter().map(|s| format!("{s}{ext}")).chain(
        [
            "MCP.md",
            "OFL.GeistMono.txt",
            "LICENSE-Rune",
            "LICENSE-winit",
            last,
        ]
        .map(str::to_owned),
    ) {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(b"fixture").unwrap();
    }
    zip.finish().unwrap()
}

#[test]
fn release_contract() {
    assert!(asset("windows", "x86_64").unwrap_err().to_string().contains("NeoBoard-x86_64.msi"));
    for (os, arch, expected, ext) in [
        ("linux", "x86_64", "linux-x86_64", ""),
        ("macos", "aarch64", "darwin-aarch64", ""),
    ] {
        assert_eq!(asset(os, arch).unwrap(), format!("neoboard-{expected}.zip"));
        let dir = tempfile::tempdir().unwrap();
        extract(&mut fixture(ext, "LICENSE"), dir.path(), ext).unwrap();
        check_install(dir.path(), ext).unwrap();
        assert_eq!(
            fs::read(dir.path().join(format!("neochat-share{ext}"))).unwrap(),
            b"fixture"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join(format!("neoboard{ext}")))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
    }
    assert!(asset("linux", "aarch64").is_err());
}

#[test]
fn checksum_and_limits() {
    let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"abc").unwrap();
    verify(
        &mut file,
        &digest(&format!("{hash}  release.zip\n"), "release.zip").unwrap(),
    )
    .unwrap();
    assert!(digest(&format!("{hash}  other.zip"), "release.zip").is_err());
    assert!(digest("1234", "release.zip").is_err());
    file.write_all(b"!").unwrap();
    assert!(verify(&mut file, hash).is_err());
    assert!(bounded(&b"1234"[..], &mut Vec::new(), 3).is_err());
}

#[test]
fn rejects_unsafe_names_and_incomplete_bundles_before_activation() {
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("live");
    fs::create_dir(&live).unwrap();
    fs::write(live.join("neoboard"), "old").unwrap();
    for last in [
        "../escape",
        "C:\\escape",
        "neochat:stream",
        "README.md",
        "CON",
        "NEOCHAT",
    ] {
        let stage = tempfile::tempdir_in(root.path()).unwrap();
        assert!(extract(&mut fixture("", last), stage.path(), "").is_err());
        assert!(!root.path().join("escape").exists());
        assert_eq!(fs::read(live.join("neoboard")).unwrap(), b"old");
    }
}

#[test]
fn refuse_data_and_development_trees() {
    let root = tempfile::tempdir().unwrap();
    let install = tempfile::tempdir_in(root.path()).unwrap();
    extract(&mut fixture("", "LICENSE"), install.path(), "").unwrap();
    fs::write(root.path().join("Cargo.toml"), "").unwrap();
    assert!(check_install(install.path(), "").is_err());
    fs::remove_file(root.path().join("Cargo.toml")).unwrap();
    fs::write(install.path().join("board.sqlite"), "data").unwrap();
    assert!(check_install(install.path(), "").is_err());
    assert_eq!(
        fs::read(install.path().join("board.sqlite")).unwrap(),
        b"data"
    );
}

#[test]
fn refuses_oversized_directory_and_link_entries() {
    let stage = tempfile::tempdir().unwrap();
    let mut file = fixture("", "LICENSE");
    file.seek(SeekFrom::End(-14)).unwrap(); // both EOCD counts
    file.write_all(&[17, 0, 17, 0]).unwrap();
    assert!(extract(&mut file, stage.path(), "").is_err());
    assert_eq!(stage.path().read_dir().unwrap().count(), 0);

    let mut zip = ZipWriter::new(tempfile::tempfile().unwrap());
    for name in BINS {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(b"fixture").unwrap();
    }
    zip.add_symlink("LICENSE", "../private", SimpleFileOptions::default())
        .unwrap();
    assert!(extract(&mut zip.finish().unwrap(), stage.path(), "").is_err());
    assert!(fs::symlink_metadata(stage.path().join("LICENSE")).is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn swap_retains_whole_previous_bundle() {
    let root = tempfile::tempdir().unwrap();
    let live = tempfile::tempdir_in(root.path()).unwrap();
    let stage = tempfile::tempdir_in(root.path()).unwrap();
    for name in BINS {
        fs::write(live.path().join(name), "old").unwrap();
        fs::write(stage.path().join(name), "new").unwrap();
    }
    exchange(stage.path(), live.path()).unwrap();
    assert!(exchange(&root.path().join("missing"), live.path()).is_err());
    for name in BINS {
        assert_eq!(fs::read(live.path().join(name)).unwrap(), b"new");
        assert_eq!(fs::read(stage.path().join(name)).unwrap(), b"old");
    }
}
