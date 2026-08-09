//! Adversarial checks on theme-archive extraction.
//!
//! These are deliberately independent of `assets.rs`'s own unit tests. Theme
//! tarballs are downloaded from third-party repositories and extracted into the
//! user's home directory, so "a test named `path_traversal_is_rejected` passes"
//! is not sufficient evidence — these assert on the *filesystem* afterwards,
//! proving nothing escaped rather than trusting a returned error.

use std::fs;
use std::path::{Path, PathBuf};

use cosmic_conf::assets::Installer;
use flate2::write::GzEncoder;
use flate2::Compression;
use tempfile::TempDir;

/// Build a `.tar.gz` containing arbitrary entries, including hostile ones a
/// well-behaved archiver would refuse to produce.
fn hostile_tarball(path: &Path, entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) {
    let file = fs::File::create(path).unwrap();
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));

    for (name, kind, data, link_target) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*kind);
        header.set_mode(0o644);
        header.set_size(if link_target.is_some() {
            0
        } else {
            data.len() as u64
        });

        // `append_data`/`set_path` reject `..` and absolute paths, so a hostile
        // archive cannot be produced through the safe API. Write the raw name
        // bytes into the GNU header directly — this is precisely what a
        // malicious archiver does, and the only way to test the guard honestly.
        write_raw_name(&mut header, name);
        if let Some(target) = link_target {
            write_raw_link(&mut header, target);
        }
        header.set_cksum();

        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

/// Overwrite the GNU header's `name` field with arbitrary bytes, bypassing the
/// validation `Header::set_path` performs.
fn write_raw_name(header: &mut tar::Header, name: &str) {
    let gnu = header.as_gnu_mut().expect("new_gnu produces a GNU header");
    gnu.name = [0u8; 100];
    let bytes = name.as_bytes();
    assert!(bytes.len() < 100, "fixture name too long for a GNU header");
    gnu.name[..bytes.len()].copy_from_slice(bytes);
}

/// Same, for the `linkname` field.
fn write_raw_link(header: &mut tar::Header, target: &str) {
    let gnu = header.as_gnu_mut().expect("new_gnu produces a GNU header");
    gnu.linkname = [0u8; 100];
    let bytes = target.as_bytes();
    assert!(bytes.len() < 100, "fixture link target too long");
    gnu.linkname[..bytes.len()].copy_from_slice(bytes);
}

/// A theme directory just complete enough for `plan` to consider the archive.
fn theme_with_archive(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let theme_dir = tmp.path().join("Configs/.config/hyde/themes/Evil");
    let source_dir = tmp.path().join("Source");
    fs::create_dir_all(&theme_dir).unwrap();
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(theme_dir.join("hypr.theme"), "general {\n    gaps_in = 3\n}\n").unwrap();

    hostile_tarball(&source_dir.join("Gtk_Evil.tar.gz"), entries);
    (tmp, theme_dir, source_dir)
}

/// Anything created outside the sandbox root is an escape.
fn assert_nothing_outside(canary: &Path) {
    assert!(
        !canary.exists(),
        "archive extraction escaped its destination and wrote {}",
        canary.display()
    );
}

#[test]
fn parent_dir_traversal_never_writes_outside_destination() {
    let (tmp, theme_dir, source_dir) = theme_with_archive(&[(
        "../../../../../../tmp/cosmic_conf_escape_canary",
        tar::EntryType::Regular,
        b"pwned",
        None,
    )]);

    let home = tmp.path().join("home");
    let data = home.join(".local/share");
    let installer = Installer::with_paths(&data, &home);

    let result = installer.plan(&theme_dir, Some(&source_dir), "Evil", true);

    // Whether it is rejected at plan time or apply time, the invariant is the
    // same: nothing lands outside the destination.
    if let Ok(plan) = result {
        let _ = installer.apply(&plan);
    }
    assert_nothing_outside(Path::new("/tmp/cosmic_conf_escape_canary"));
}

#[test]
fn absolute_path_entry_never_writes_outside_destination() {
    let (tmp, theme_dir, source_dir) = theme_with_archive(&[(
        "/tmp/cosmic_conf_abs_canary",
        tar::EntryType::Regular,
        b"pwned",
        None,
    )]);

    let home = tmp.path().join("home");
    let data = home.join(".local/share");
    let installer = Installer::with_paths(&data, &home);

    if let Ok(plan) = installer.plan(&theme_dir, Some(&source_dir), "Evil", true) {
        let _ = installer.apply(&plan);
    }
    assert_nothing_outside(Path::new("/tmp/cosmic_conf_abs_canary"));
}

/// The subtle one: neither entry path contains `..`, so a naive check passes.
/// The symlink redirects a later, innocent-looking write outside the tree.
#[test]
fn symlink_indirection_never_writes_outside_destination() {
    let (tmp, theme_dir, source_dir) = theme_with_archive(&[
        ("escape", tar::EntryType::Symlink, b"", Some("/tmp")),
        (
            "escape/cosmic_conf_symlink_canary",
            tar::EntryType::Regular,
            b"pwned",
            None,
        ),
    ]);

    let home = tmp.path().join("home");
    let data = home.join(".local/share");
    let installer = Installer::with_paths(&data, &home);

    if let Ok(plan) = installer.plan(&theme_dir, Some(&source_dir), "Evil", true) {
        let _ = installer.apply(&plan);
    }
    assert_nothing_outside(Path::new("/tmp/cosmic_conf_symlink_canary"));
}

/// A benign archive must still install, or the guard is uselessly strict.
#[test]
fn well_formed_archive_still_installs() {
    let (tmp, theme_dir, source_dir) = theme_with_archive(&[(
        "Evil-Theme/index.theme",
        tar::EntryType::Regular,
        b"[Desktop Entry]\n",
        None,
    )]);

    let home = tmp.path().join("home");
    let data = home.join(".local/share");
    let installer = Installer::with_paths(&data, &home);

    let plan = installer
        .plan(&theme_dir, Some(&source_dir), "Evil", true)
        .expect("a well-formed archive must plan cleanly");
    installer.apply(&plan).expect("and must apply");

    assert!(
        home.join(".themes/Evil-Theme/index.theme").exists(),
        "benign archive did not install; guard is too strict"
    );
}
