//! Unpack a verified download into a fresh directory, refusing anything that
//! could land outside it.
//!
//! ```text
//! entry path ─▶ relative, no `..`? ─▶ parents are real dirs (no symlink)? ─▶ write
//! symlink    ─▶ relative target that stays inside the directory?        ─▶ create
//! anything else (devices, fifos) ─▶ skipped
//! ```
//! Blocking: call from `spawn_blocking`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use theodore_ports::code_intel::{CodeIntelError, CodeIntelResult};

use crate::manifest::ArchiveFormat;

/// Unpacked bytes past this abort the install (a zip bomb, not a server).
const MAX_UNPACKED_BYTES: u64 = 4 << 30;

/// Unpack `archive` (of `format`) into the existing empty dir `dest`, then
/// check that `bin` is a regular file there and make it executable. `Gz`
/// and `Binary` write their single file to `bin`.
pub(crate) fn unpack(
    format: ArchiveFormat,
    archive: &Path,
    dest: &Path,
    bin: &str,
) -> CodeIntelResult<PathBuf> {
    let err = |e: io::Error| CodeIntelError::Download(format!("unpack {}: {e}", archive.display()));
    let mut sink = Sink {
        root: dest,
        written: 0,
    };
    let open = || File::open(archive);
    match format {
        ArchiveFormat::Binary => sink.file(Path::new(bin), &mut open().map_err(err)?, Some(0o755)),
        ArchiveFormat::Gz => sink.file(
            Path::new(bin),
            &mut GzDecoder::new(open().map_err(err)?),
            Some(0o755),
        ),
        ArchiveFormat::TarGz => untar(&mut sink, open().map_err(err)?, None),
        ArchiveFormat::NpmTgz => untar(&mut sink, open().map_err(err)?, Some("package")),
        ArchiveFormat::Zip | ArchiveFormat::Wheel => unzip(&mut sink, open().map_err(err)?),
    }
    .map_err(err)?;

    let rel = Path::new(bin);
    check_no_symlinks(dest, rel).map_err(err)?;
    let path = dest.join(rel);
    let meta = fs::symlink_metadata(&path)
        .map_err(|e| err(io::Error::new(e.kind(), format!("bin {bin}: {e}"))))?;
    if !meta.is_file() {
        return Err(err(unsafe_entry(format!("bin {bin} is not a file"))));
    }
    set_mode(&path, 0o755).map_err(err)?;
    Ok(path)
}

fn untar(sink: &mut Sink<'_>, file: File, strip: Option<&str>) -> io::Result<()> {
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw = entry.path()?.into_owned();
        let Some(rel) = entry_path(&raw, strip)? else {
            continue;
        };
        let kind = entry.header().entry_type();
        match kind {
            tar::EntryType::Directory => sink.dir(&rel)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = entry.header().mode().ok();
                sink.file(&rel, &mut entry, mode)?;
            }
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| {
                        unsafe_entry(format!("{}: symlink without target", raw.display()))
                    })?
                    .into_owned();
                sink.symlink(&rel, &target)?;
            }
            tar::EntryType::Link => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| {
                        unsafe_entry(format!("{}: hard link without target", raw.display()))
                    })?
                    .into_owned();
                let Some(target) = entry_path(&target, strip)? else {
                    return Err(unsafe_entry(format!(
                        "{}: hard link to the root",
                        raw.display()
                    )));
                };
                sink.hardlink(&rel, &target)?;
            }
            other => {
                tracing::debug!(target: "code_intel", path = %raw.display(), ?other, "archive entry skipped");
            }
        }
    }
    Ok(())
}

fn unzip(sink: &mut Sink<'_>, file: File) -> io::Result<()> {
    let mut zip = zip::ZipArchive::new(file).map_err(io::Error::other)?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(io::Error::other)?;
        let name = f.name().to_string();
        if name.contains('\\') {
            return Err(unsafe_entry(format!("{name}: backslash in path")));
        }
        let Some(rel) = entry_path(Path::new(&name), None)? else {
            continue;
        };
        if f.is_dir() {
            sink.dir(&rel)?;
        } else if f.is_symlink() {
            let mut target = String::new();
            (&mut f).take(4096).read_to_string(&mut target)?;
            sink.symlink(&rel, Path::new(&target))?;
        } else {
            let mode = f.unix_mode();
            sink.file(&rel, &mut f, mode)?;
        }
    }
    Ok(())
}

/// The entry's path relative to the target dir: only plain components.
/// `None` for the archive's root itself (`./`). With `strip`, the first
/// component must be exactly that and is dropped.
fn entry_path(raw: &Path, strip: Option<&str>) -> io::Result<Option<PathBuf>> {
    let mut out = PathBuf::new();
    let mut components = raw.components().filter(|c| !matches!(c, Component::CurDir));
    if let Some(prefix) = strip {
        match components.next() {
            Some(Component::Normal(c)) if c == prefix => {}
            _ => {
                return Err(unsafe_entry(format!(
                    "{}: outside `{prefix}/`",
                    raw.display()
                )));
            }
        }
    }
    for c in components {
        match c {
            Component::Normal(c) => out.push(c),
            _ => return Err(unsafe_entry(format!("{}: unsafe path", raw.display()))),
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// Writes under `root`, never through a symlink.
struct Sink<'a> {
    root: &'a Path,
    written: u64,
}

impl Sink<'_> {
    fn dir(&mut self, rel: &Path) -> io::Result<()> {
        self.real_parents(rel)?;
        let path = self.root.join(rel);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(unsafe_entry(format!(
                "{}: exists, not a directory",
                rel.display()
            ))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&path),
            Err(e) => Err(e),
        }
    }

    fn file(&mut self, rel: &Path, reader: &mut dyn Read, mode: Option<u32>) -> io::Result<()> {
        self.real_parents(rel)?;
        let path = self.root.join(rel);
        match fs::symlink_metadata(&path) {
            // Archives may repeat a path; the last one wins.
            Ok(m) if m.is_file() => fs::remove_file(&path)?,
            Ok(_) => {
                return Err(unsafe_entry(format!(
                    "{}: exists, not a file",
                    rel.display()
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let budget = MAX_UNPACKED_BYTES - self.written;
        let n = io::copy(&mut Read::take(&mut *reader, budget + 1), &mut out)?;
        if n > budget {
            return Err(unsafe_entry(format!(
                "more than {MAX_UNPACKED_BYTES} bytes unpacked"
            )));
        }
        self.written += n;
        let exec = mode.is_some_and(|m| m & 0o111 != 0);
        set_mode(&path, if exec { 0o755 } else { 0o644 })
    }

    fn symlink(&mut self, rel: &Path, target: &Path) -> io::Result<()> {
        // Depth of the link's directory below the root; each `..` climbs one.
        let mut depth = rel.components().count() - 1;
        for c in target.components() {
            match c {
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::ParentDir if depth > 0 => depth -= 1,
                _ => {
                    return Err(unsafe_entry(format!(
                        "{} -> {}: link leaves the directory",
                        rel.display(),
                        target.display()
                    )));
                }
            }
        }
        self.real_parents(rel)?;
        let path = self.root.join(rel);
        if fs::symlink_metadata(&path).is_ok() {
            return Err(unsafe_entry(format!("{}: exists", rel.display())));
        }
        make_symlink(target, &path)
    }

    fn hardlink(&mut self, rel: &Path, target: &Path) -> io::Result<()> {
        check_no_symlinks(self.root, target)?;
        let source = self.root.join(target);
        if !fs::symlink_metadata(&source)?.is_file() {
            return Err(unsafe_entry(format!(
                "{}: hard link to a non-file",
                rel.display()
            )));
        }
        let mode = file_mode(&source);
        self.file(rel, &mut File::open(&source)?, mode)
    }

    /// Create `rel`'s parent dirs; refuse if one is a symlink or a file.
    fn real_parents(&self, rel: &Path) -> io::Result<()> {
        let mut cur = self.root.to_path_buf();
        let Some(parent) = rel.parent() else {
            return Ok(());
        };
        for c in parent.components() {
            cur.push(c);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    return Err(unsafe_entry(format!(
                        "{}: parent is a link or a file",
                        rel.display()
                    )));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&cur)?,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Every existing component of `rel` under `root` is a real dir or file.
fn check_no_symlinks(root: &Path, rel: &Path) -> io::Result<()> {
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(unsafe_entry(format!("{}: unsafe path", rel.display())));
        }
        cur.push(c);
        if fs::symlink_metadata(&cur)?.file_type().is_symlink() {
            return Err(unsafe_entry(format!(
                "{}: goes through a symlink",
                rel.display()
            )));
        }
    }
    Ok(())
}

fn unsafe_entry(msg: String) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unsafe archive entry: {msg}"),
    )
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn file_mode(_: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn make_symlink(target: &Path, link: &Path) -> io::Result<()> {
    tracing::debug!(target: "code_intel", link = %link.display(), target = %target.display(), "symlink skipped");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    pub(crate) enum E<'a> {
        File(&'a str, &'a [u8], u32),
        Dir(&'a str),
        Link(&'a str, &'a str),
        Hard(&'a str, &'a str),
    }

    /// A tar.gz with raw entry names: `tar::Header::set_path` refuses `..`
    /// and absolute paths, which are exactly what the tests need.
    pub(crate) fn tar_gz(entries: &[E<'_>]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for e in entries {
            let mut h = tar::Header::new_gnu();
            let (name, data, kind, mode, link): (&str, &[u8], _, u32, Option<&str>) = match e {
                E::File(n, d, m) => (n, d, tar::EntryType::Regular, *m, None),
                E::Dir(n) => (n, b"", tar::EntryType::Directory, 0o755, None),
                E::Link(n, t) => (n, b"", tar::EntryType::Symlink, 0o777, Some(t)),
                E::Hard(n, t) => (n, b"", tar::EntryType::Link, 0o644, Some(t)),
            };
            let gnu = h.as_gnu_mut().unwrap();
            gnu.name[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(t) = link {
                gnu.linkname[..t.len()].copy_from_slice(t.as_bytes());
            }
            h.set_entry_type(kind);
            h.set_mode(mode);
            h.set_size(data.len() as u64);
            h.set_cksum();
            b.append(&h, data).unwrap();
        }
        let tar = b.into_inner().unwrap();
        gzip(&tar)
    }

    pub(crate) fn gzip(data: &[u8]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(data).unwrap();
        gz.finish().unwrap()
    }

    pub(crate) fn zip_of(entries: &[E<'_>]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for e in entries {
            match e {
                E::File(n, d, m) => {
                    w.start_file(*n, SimpleFileOptions::default().unix_permissions(*m))
                        .unwrap();
                    w.write_all(d).unwrap();
                }
                E::Dir(n) => w.add_directory(*n, SimpleFileOptions::default()).unwrap(),
                E::Link(n, t) => w.add_symlink(*n, *t, SimpleFileOptions::default()).unwrap(),
                E::Hard(..) => unreachable!("zip has no hard links"),
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn run(
        format: ArchiveFormat,
        bytes: &[u8],
        bin: &str,
    ) -> (tempfile::TempDir, CodeIntelResult<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        fs::write(&archive, bytes).unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let r = unpack(format, &archive, &dest, bin);
        (dir, r)
    }

    fn is_exec(p: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).unwrap().permissions().mode() & 0o111 != 0
    }

    #[test]
    fn tar_gz_with_dirs_links_and_modes() {
        let bytes = tar_gz(&[
            E::Dir("ls-1.0/"),
            E::File("ls-1.0/bin/ls", b"#!/bin/sh\n", 0o755),
            E::File("ls-1.0/README", b"hi", 0o644),
            E::Link("ls-1.0/bin/ls-link", "ls"),
            E::Link("ls-1.0/lib/up", "../bin/ls"),
            E::Hard("ls-1.0/README2", "ls-1.0/README"),
        ]);
        let (dir, r) = run(ArchiveFormat::TarGz, &bytes, "ls-1.0/bin/ls");
        let bin = r.unwrap();
        assert!(is_exec(&bin));
        let out = dir.path().join("out/ls-1.0");
        assert_eq!(fs::read(out.join("README2")).unwrap(), b"hi");
        assert!(!is_exec(&out.join("README")));
        assert_eq!(
            fs::read_link(out.join("lib/up")).unwrap(),
            Path::new("../bin/ls")
        );
    }

    #[test]
    fn traversal_and_escaping_links_are_rejected() {
        let cases: Vec<Vec<E<'_>>> = vec![
            vec![E::File("../evil", b"x", 0o644)],
            vec![E::File("a/../../evil", b"x", 0o644)],
            vec![E::File("/tmp/evil", b"x", 0o644)],
            vec![E::Link("a", "/etc/passwd")],
            vec![E::Link("a", "../outside")],
            vec![E::Link("d/a", "../../outside")],
            // A link to a dir inside, then a write through it.
            vec![
                E::Dir("real/"),
                E::Link("l", "real"),
                E::File("l/x", b"x", 0o644),
            ],
            vec![E::Hard("h", "../outside")],
        ];
        for entries in cases {
            let (dir, r) = run(ArchiveFormat::TarGz, &tar_gz(&entries), "bin");
            let e = r.unwrap_err().to_string();
            assert!(e.contains("unsafe archive entry"), "{e}");
            assert!(!dir.path().join("evil").exists());
        }
    }

    #[test]
    fn npm_tgz_strips_package_and_requires_it() {
        let ok = tar_gz(&[E::File("package/bin/server.js", b"js", 0o644)]);
        let (dir, r) = run(ArchiveFormat::NpmTgz, &ok, "bin/server.js");
        r.unwrap();
        assert!(dir.path().join("out/bin/server.js").is_file());

        let bad = tar_gz(&[E::File("other/bin/server.js", b"js", 0o644)]);
        let (_dir, r) = run(ArchiveFormat::NpmTgz, &bad, "bin/server.js");
        assert!(r.unwrap_err().to_string().contains("outside `package/`"));
    }

    #[test]
    fn zip_and_wheel() {
        let bytes = zip_of(&[
            E::Dir("srv/"),
            E::File("srv/bin/srv", b"\x7fELF", 0o755),
            E::Link("srv/current", "bin/srv"),
        ]);
        for format in [ArchiveFormat::Zip, ArchiveFormat::Wheel] {
            let (_dir, r) = run(format, &bytes, "srv/bin/srv");
            assert!(is_exec(&r.unwrap()));
        }
        for bad in [
            zip_of(&[E::File("../evil", b"x", 0o644)]),
            zip_of(&[E::Link("l", "../../etc")]),
        ] {
            let (_dir, r) = run(ArchiveFormat::Zip, &bad, "x");
            assert!(r.unwrap_err().to_string().contains("unsafe archive entry"));
        }
    }

    #[test]
    fn gz_and_binary_write_bin() {
        let (_d, r) = run(ArchiveFormat::Gz, &gzip(b"payload"), "srv");
        let bin = r.unwrap();
        assert_eq!(fs::read(&bin).unwrap(), b"payload");
        assert!(is_exec(&bin));

        let (_d, r) = run(ArchiveFormat::Binary, b"raw", "bin/srv");
        assert_eq!(fs::read(r.unwrap()).unwrap(), b"raw");
    }

    #[test]
    fn bin_must_be_a_regular_file() {
        let bytes = tar_gz(&[E::File("real", b"x", 0o755), E::Link("bin", "real")]);
        let (_d, r) = run(ArchiveFormat::TarGz, &bytes, "bin");
        assert!(r.unwrap_err().to_string().contains("symlink"));
        let (_d, r) = run(ArchiveFormat::TarGz, &bytes, "missing");
        assert!(r.is_err());
    }
}
