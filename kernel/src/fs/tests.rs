use super::*;
use std::{boxed::Box, vec, vec::Vec};

fn setup() -> (Box<FileSystem>, ProcessFs) {
    let mut fs = Box::new(FileSystem::new());
    fs.initialize().unwrap();
    let mut process = ProcessFs::new();
    fs.attach_process(&mut process).unwrap();
    (fs, process)
}

fn writable() -> OpenFlags {
    OpenFlags::READ
        .union(OpenFlags::WRITE)
        .union(OpenFlags::CREATE)
}

#[test]
fn ram_file_offsets_holes_partial_capacity_and_eof() {
    let (mut fs, mut process) = setup();
    let fd = fs.open(&mut process, b"/tmp/a", writable()).unwrap();
    assert_eq!(fs.write(&mut process, fd, b"abc"), Ok(3));
    assert_eq!(fs.seek(&mut process, fd, 6, SeekWhence::Start), Ok(6));
    assert_eq!(fs.write(&mut process, fd, b"z"), Ok(1));
    fs.seek(&mut process, fd, 0, SeekWhence::Start).unwrap();
    let mut bytes = [0xff; 10];
    assert_eq!(fs.read(&mut process, fd, &mut bytes), Ok(7));
    assert_eq!(&bytes[..7], b"abc\0\0\0z");
    assert_eq!(fs.read(&mut process, fd, &mut bytes), Ok(0));
    fs.seek(
        &mut process,
        fd,
        (MAX_FILE_BYTES - 2) as i64,
        SeekWhence::Start,
    )
    .unwrap();
    assert_eq!(fs.write(&mut process, fd, b"abcd"), Ok(2));
    assert_eq!(fs.write(&mut process, fd, b"x"), Err(FsError::NoSpace));
    assert_eq!(fs.fstat(&process, fd).unwrap().size, MAX_FILE_BYTES as u64);
    assert_eq!(
        fs.seek(&mut process, fd, i64::MIN, SeekWhence::Current),
        Err(FsError::InvalidArgument)
    );
    assert_eq!(
        fs.seek(&mut process, fd, 0, SeekWhence::Current),
        Ok(MAX_FILE_BYTES as u64)
    );
}

#[test]
fn duplicate_descriptors_share_offset_while_independent_opens_do_not() {
    let (mut fs, mut process) = setup();
    let fd = fs.open(&mut process, b"/tmp/a", writable()).unwrap();
    fs.write(&mut process, fd, b"abcd").unwrap();
    fs.seek(&mut process, fd, 0, SeekWhence::Start).unwrap();
    let duplicate = fs.dup(&mut process, fd).unwrap();
    let separate = fs.open(&mut process, b"/tmp/a", OpenFlags::READ).unwrap();
    let mut byte = [0];
    fs.read(&mut process, fd, &mut byte).unwrap();
    assert_eq!(byte, [b'a']);
    fs.read(&mut process, duplicate, &mut byte).unwrap();
    assert_eq!(byte, [b'b']);
    fs.read(&mut process, separate, &mut byte).unwrap();
    assert_eq!(byte, [b'a']);
    assert_eq!(fs.dup2(&mut process, duplicate, fd), Ok(fd));
    fs.close(&mut process, duplicate).unwrap();
    fs.read(&mut process, fd, &mut byte).unwrap();
    assert_eq!(byte, [b'c']);
    assert_eq!(fs.dup2(&mut process, 15, fd), Err(FsError::BadDescriptor));
    assert_eq!(fs.seek(&mut process, fd, 0, SeekWhence::Current), Ok(3));
}

#[test]
fn cwd_and_descriptors_are_process_private_and_inherited_open_descriptions_survive_parent_exit() {
    let (mut fs, mut parent) = setup();
    fs.mkdir(&mut parent, b"/tmp/work").unwrap();
    fs.chdir(&mut parent, b"/tmp/work").unwrap();
    let fd = fs.open(&mut parent, b"a", writable()).unwrap();
    fs.write(&mut parent, fd, b"abc").unwrap();
    fs.seek(&mut parent, fd, 0, SeekWhence::Start).unwrap();
    let mut child = ProcessFs::new();
    fs.inherit_process(&parent, &mut child).unwrap();
    let mut independent = ProcessFs::new();
    fs.attach_process(&mut independent).unwrap();
    assert_eq!(
        fs.descriptor_kind(&independent, fd),
        Err(FsError::BadDescriptor)
    );
    assert_eq!(fs.stat(&independent, b"a"), Err(FsError::NotFound));
    fs.chdir(&mut child, b"..").unwrap();
    let mut path = [0; MAX_PATH];
    let count = fs.getcwd(&parent, &mut path).unwrap();
    assert_eq!(&path[..count], b"/tmp/work");
    let mut byte = [0];
    fs.read(&mut parent, fd, &mut byte).unwrap();
    fs.cleanup_process(&mut parent);
    fs.read(&mut child, fd, &mut byte).unwrap();
    assert_eq!(byte, [b'b']);
    fs.cleanup_process(&mut child);
    fs.cleanup_process(&mut independent);
    fs.cleanup_process(&mut parent);
    assert!(
        fs.opens
            .iter()
            .all(|open| matches!(open.target, OpenTarget::Free))
    );
    assert!(fs.nodes.iter().all(|node| node.references == 0));
    assert_eq!(fs.processes, 0);
}

#[test]
fn readonly_init_files_and_nested_directory_paths_are_enforced() {
    let mut fs = Box::new(FileSystem::new());
    fs.initialize().unwrap();
    fs.add_init_file(b"bin/hello", b"immutable").unwrap();
    let mut process = ProcessFs::new();
    fs.attach_process(&mut process).unwrap();
    assert_eq!(
        fs.file_bytes(&process, b"/init/bin/hello"),
        Ok(b"immutable".as_slice())
    );
    assert_eq!(
        fs.open(&mut process, b"/init/bin/hello", writable()),
        Err(FsError::ReadOnly)
    );
    assert_eq!(
        fs.open(&mut process, b"/init/new", writable()),
        Err(FsError::ReadOnly)
    );
    assert_eq!(fs.mkdir(&mut process, b"/init/x"), Err(FsError::ReadOnly));
    assert_eq!(
        fs.unlink(&mut process, b"/init/bin/hello"),
        Err(FsError::ReadOnly)
    );
    assert_eq!(
        fs.rename(&mut process, b"/init/bin/hello", b"/tmp/hello"),
        Err(FsError::ReadOnly)
    );
    assert_eq!(fs.add_init_file(b"later", b""), Err(FsError::Busy));
    let fd = fs
        .open(&mut process, b"/init//bin/../bin/hello", OpenFlags::READ)
        .unwrap();
    let mut bytes = [0; 32];
    assert_eq!(fs.read(&mut process, fd, &mut bytes), Ok(9));
    assert_eq!(&bytes[..9], b"immutable");
    for path in [
        b"/init/bin/hello/".as_slice(),
        b"/init/bin/hello/.",
        b"/init/bin/hello/..",
    ] {
        assert_eq!(fs.stat(&process, path), Err(FsError::NotDirectory));
    }
    assert_eq!(
        fs.stat(&process, b"/init\0/bin"),
        Err(FsError::InvalidArgument)
    );
}

#[test]
fn append_and_truncate_follow_shared_flags_and_zero_old_storage() {
    let (mut fs, mut process) = setup();
    let fd = fs.open(&mut process, b"/tmp/a", writable()).unwrap();
    fs.write(&mut process, fd, b"secret").unwrap();
    let append = fs
        .open(
            &mut process,
            b"/tmp/a",
            OpenFlags::WRITE.union(OpenFlags::APPEND),
        )
        .unwrap();
    fs.seek(&mut process, append, 0, SeekWhence::Start).unwrap();
    fs.write(&mut process, append, b"!").unwrap();
    assert_eq!(
        fs.file_bytes(&process, b"/tmp/a"),
        Ok(b"secret!".as_slice())
    );
    let truncated = fs
        .open(
            &mut process,
            b"/tmp/a",
            OpenFlags::WRITE.union(OpenFlags::TRUNCATE),
        )
        .unwrap();
    fs.seek(&mut process, truncated, 3, SeekWhence::Start)
        .unwrap();
    fs.write(&mut process, truncated, b"x").unwrap();
    assert_eq!(
        fs.file_bytes(&process, b"/tmp/a"),
        Ok(b"\0\0\0x".as_slice())
    );
    assert_eq!(
        fs.open(
            &mut process,
            b"/tmp/a",
            writable().union(OpenFlags::EXCLUSIVE)
        ),
        Err(FsError::AlreadyExists)
    );
    assert_eq!(
        OpenFlags::from_bits(OpenFlags::READ.union(OpenFlags::TRUNCATE).bits()),
        Err(FsError::InvalidArgument)
    );
}

#[test]
fn unlinked_and_replaced_open_files_keep_original_bytes_until_last_close() {
    let (mut fs, mut process) = setup();
    let old = fs.open(&mut process, b"/tmp/a", writable()).unwrap();
    fs.write(&mut process, old, b"old").unwrap();
    fs.unlink(&mut process, b"/tmp/a").unwrap();
    assert_eq!(fs.stat(&process, b"/tmp/a"), Err(FsError::NotFound));
    assert!(!fs.fstat(&process, old).unwrap().linked);
    let replacement = fs.open(&mut process, b"/tmp/a", writable()).unwrap();
    fs.write(&mut process, replacement, b"new").unwrap();
    fs.seek(&mut process, old, 0, SeekWhence::Start).unwrap();
    let mut bytes = [0; 3];
    fs.read(&mut process, old, &mut bytes).unwrap();
    assert_eq!(&bytes, b"old");
    let source = fs.open(&mut process, b"/tmp/b", writable()).unwrap();
    fs.write(&mut process, source, b"src").unwrap();
    fs.rename(&mut process, b"/tmp/b", b"/tmp/a").unwrap();
    fs.seek(&mut process, replacement, 0, SeekWhence::Start)
        .unwrap();
    fs.read(&mut process, replacement, &mut bytes).unwrap();
    assert_eq!(&bytes, b"new");
    assert_eq!(fs.file_bytes(&process, b"/tmp/a"), Ok(b"src".as_slice()));
    fs.cleanup_process(&mut process);
    assert_eq!(fs.ram_used.iter().filter(|&&used| used).count(), 1);
}

#[test]
fn directory_enumeration_rewinds_and_rename_preserves_cwd_without_cycles() {
    let (mut fs, mut process) = setup();
    fs.mkdir(&mut process, b"/tmp/one").unwrap();
    fs.mkdir(&mut process, b"/tmp/one/two").unwrap();
    fs.chdir(&mut process, b"/tmp/one/two").unwrap();
    assert_eq!(
        fs.rename(&mut process, b"/tmp/one", b"/tmp/one/two/cycle"),
        Err(FsError::InvalidArgument)
    );
    fs.rename(&mut process, b"/tmp/one", b"/tmp/moved").unwrap();
    let mut path = [0; MAX_PATH];
    let count = fs.getcwd(&process, &mut path).unwrap();
    assert_eq!(&path[..count], b"/tmp/moved/two");
    assert_eq!(fs.unlink(&mut process, b"."), Err(FsError::InvalidArgument));
    assert_eq!(
        fs.unlink(&mut process, b"/tmp/moved/two"),
        Err(FsError::Busy)
    );
    assert_eq!(
        fs.unlink(&mut process, b"/tmp/moved"),
        Err(FsError::NotEmpty)
    );
    let directory = fs
        .open(
            &mut process,
            b"/tmp",
            OpenFlags::READ.union(OpenFlags::DIRECTORY),
        )
        .unwrap();
    let duplicate = fs.dup(&mut process, directory).unwrap();
    let mut entry = [DirEntry::EMPTY];
    fs.getdents(&mut process, directory, &mut entry).unwrap();
    assert_eq!(&entry[0].name[..entry[0].name_length], b".");
    fs.getdents(&mut process, duplicate, &mut entry).unwrap();
    assert_eq!(&entry[0].name[..entry[0].name_length], b"..");
    fs.getdents(&mut process, directory, &mut entry).unwrap();
    assert_eq!(&entry[0].name[..entry[0].name_length], b"moved");
    assert_eq!(fs.getdents(&mut process, directory, &mut entry), Ok(0));
    fs.seek(&mut process, duplicate, 0, SeekWhence::Start)
        .unwrap();
    assert_eq!(fs.getdents(&mut process, directory, &mut entry), Ok(1));
    assert_eq!(
        fs.seek(&mut process, directory, 1, SeekWhence::Start),
        Err(FsError::InvalidArgument)
    );
}

#[test]
fn failure_to_reserve_descriptors_leaves_no_created_or_truncated_files() {
    let (mut fs, mut process) = setup();
    let fd = fs.open(&mut process, b"/tmp/keep", writable()).unwrap();
    fs.write(&mut process, fd, b"keep").unwrap();
    for _ in 4..MAX_FDS {
        fs.dup(&mut process, fd).unwrap();
    }
    assert_eq!(
        fs.open(&mut process, b"/tmp/new", writable()),
        Err(FsError::TooManyFiles)
    );
    assert_eq!(fs.stat(&process, b"/tmp/new"), Err(FsError::NotFound));
    assert_eq!(
        fs.open(
            &mut process,
            b"/tmp/keep",
            writable().union(OpenFlags::TRUNCATE)
        ),
        Err(FsError::TooManyFiles)
    );
    assert_eq!(
        fs.file_bytes(&process, b"/tmp/keep"),
        Ok(b"keep".as_slice())
    );
    fs.cleanup_process(&mut process);
    assert!(
        fs.opens
            .iter()
            .all(|open| matches!(open.target, OpenTarget::Free))
    );
}

#[test]
fn ram_and_global_open_description_exhaustion_roll_back_and_recover() {
    let (mut fs, mut process) = setup();
    for number in 0..MAX_RAM_FILES {
        let name = std::format!("/tmp/{number}");
        let fd = fs.open(&mut process, name.as_bytes(), writable()).unwrap();
        fs.close(&mut process, fd).unwrap();
    }
    assert_eq!(
        fs.open(&mut process, b"/tmp/full", writable()),
        Err(FsError::NoSpace)
    );
    assert_eq!(fs.stat(&process, b"/tmp/full"), Err(FsError::NotFound));
    fs.unlink(&mut process, b"/tmp/0").unwrap();
    let fd = fs
        .open(&mut process, b"/tmp/recovered", writable())
        .unwrap();
    fs.close(&mut process, fd).unwrap();
    let mut others: Vec<ProcessFs> = vec![];
    for _ in 0..9 {
        let mut p = ProcessFs::new();
        fs.attach_process(&mut p).unwrap();
        others.push(p);
    }
    assert_eq!(
        fs.opens
            .iter()
            .filter(|open| matches!(open.target, OpenTarget::Free))
            .count(),
        2
    );
    let mut rejected = ProcessFs::new();
    assert_eq!(fs.attach_process(&mut rejected), Err(FsError::TooManyFiles));
    assert!(!rejected.is_attached());
    assert_eq!(
        fs.opens
            .iter()
            .filter(|open| matches!(open.target, OpenTarget::Free))
            .count(),
        2
    );
    fs.cleanup_process(&mut others[0]);
    fs.attach_process(&mut rejected).unwrap();
}

#[test]
fn permission_preflight_zero_buffers_and_cwd_failure_preserve_state() {
    let (mut fs, mut process) = setup();
    assert_eq!(fs.validate_read(&process, 0), Ok(DescriptorKind::Stdin));
    assert_eq!(fs.validate_write(&process, 1), Ok(DescriptorKind::Stdout));
    assert_eq!(fs.validate_read(&process, 1), Err(FsError::BadDescriptor));
    assert_eq!(fs.validate_write(&process, 0), Err(FsError::BadDescriptor));
    let dir = fs.open(&mut process, b"/tmp", OpenFlags::READ).unwrap();
    assert_eq!(fs.validate_read(&process, dir), Err(FsError::IsDirectory));
    let fd = fs.open(&mut process, b"/tmp/a", writable()).unwrap();
    assert_eq!(fs.read(&mut process, fd, &mut []), Ok(0));
    assert_eq!(fs.write(&mut process, fd, &[]), Ok(0));
    fs.chdir(&mut process, b"/tmp").unwrap();
    let mut tiny = [0xaa; 3];
    assert_eq!(fs.getcwd(&process, &mut tiny), Err(FsError::Overflow));
    assert_eq!(tiny, [0xaa; 3]);
    assert_eq!(fs.chdir(&mut process, b"a"), Err(FsError::NotDirectory));
    assert_eq!(fs.stat(&process, b"a").unwrap().kind, DescriptorKind::File);
}

fn archive(entries: &[(&[u8], &[u8], u8)]) -> &'static [u8] {
    let mut bytes = Vec::new();
    for &(path, payload, kind) in entries {
        let mut header = [0u8; 512];
        header[..path.len()].copy_from_slice(path);
        let size = std::format!("{:011o}\0", payload.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[148..156].fill(b' ');
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: usize = header.iter().map(|&byte| byte as usize).sum();
        let checksum = std::format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(payload);
        bytes.resize(bytes.len().div_ceil(512) * 512, 0);
    }
    bytes.resize(bytes.len() + 1024, 0);
    Box::leak(bytes.into_boxed_slice())
}

#[test]
fn valid_tar_mount_creates_readonly_nested_files_and_preserves_payload() {
    let mut fs = Box::new(FileSystem::new());
    fs.initialize().unwrap();
    fs.mount_tar(archive(&[
        (b"bin/", b"", b'5'),
        (b"bin/hello", b"ELF bytes", b'0'),
        (b"motd", b"hello\n", b'0'),
    ]))
    .unwrap();
    let mut process = ProcessFs::new();
    fs.attach_process(&mut process).unwrap();
    assert_eq!(
        fs.boot_file(&process, b"/init/bin/hello"),
        Ok(b"ELF bytes".as_slice())
    );
    assert_eq!(
        fs.stat(&process, b"/init/bin").unwrap().kind,
        DescriptorKind::Directory
    );
    assert_eq!(
        fs.open(&mut process, b"/init/motd", OpenFlags::WRITE),
        Err(FsError::ReadOnly)
    );
}

#[test]
fn malformed_tar_headers_paths_truncation_and_duplicate_entries_never_leave_partial_mounts() {
    for input in [
        archive(&[(b"valid", b"ok", b'0'), (b"../escape", b"bad", b'0')]),
        archive(&[(b"valid", b"ok", b'0'), (b"valid", b"again", b'0')]),
        archive(&[(b"valid", b"ok", b'0'), (b"symlink", b"", b'2')]),
    ] {
        let mut fs = Box::new(FileSystem::new());
        fs.initialize().unwrap();
        assert!(fs.mount_tar(input).is_err());
        assert_eq!(fs.nodes.iter().filter(|node| node.linked).count(), 3);
    }
    let valid = archive(&[(b"one", b"payload", b'0')]);
    let mut corrupted = valid.to_vec();
    corrupted[1] ^= 1;
    let mut fs = Box::new(FileSystem::new());
    fs.initialize().unwrap();
    assert_eq!(
        fs.mount_tar(Box::leak(corrupted.into_boxed_slice())),
        Err(TarError::InvalidChecksum)
    );
    assert_eq!(
        fs.mount_tar(&valid[..valid.len() - 1]),
        Err(TarError::Truncated)
    );
    assert_eq!(
        fs.mount_tar(&valid[..valid.len() - 512]),
        Err(TarError::Truncated)
    );
}

#[test]
fn initramfs_node_capacity_failure_rolls_back_and_per_file_parent_rollback_is_scoped() {
    let mut fs = Box::new(FileSystem::new());
    fs.initialize().unwrap();
    fs.add_init_file(b"keep", b"keep").unwrap();
    let long_name = [b'x'; MAX_NAME + 1];
    let mut path = b"new/".to_vec();
    path.extend_from_slice(&long_name);
    assert_eq!(fs.add_init_file(&path, b""), Err(FsError::NameTooLong));
    assert!(fs.child(INIT, b"new").is_none());
    assert!(fs.child(INIT, b"keep").is_some());
    let mut empty = Box::new(FileSystem::new());
    empty.initialize().unwrap();
    let names: Vec<Vec<u8>> = (0..MAX_NODES)
        .map(|i| std::format!("file{i}").into_bytes())
        .collect();
    let entries: Vec<(&[u8], &[u8], u8)> = names
        .iter()
        .map(|name| (name.as_slice(), b"".as_slice(), b'0'))
        .collect();
    assert_eq!(
        empty.mount_tar(archive(&entries)),
        Err(TarError::Filesystem(FsError::NoSpace))
    );
    assert_eq!(empty.nodes.iter().filter(|node| node.linked).count(), 3);
    empty
        .mount_tar(archive(&[(b"recovered", b"ok", b'0')]))
        .unwrap();
}

#[test]
fn malformed_tar_numeric_lengths_and_every_truncated_block_leave_mount_empty() {
    let original = archive(&[(b"first", b"ok", b'0'), (b"second", b"payload", b'0')]);
    for length in (0..original.len()).step_by(512) {
        let mut fs = Box::new(FileSystem::new());
        fs.initialize().unwrap();
        assert_eq!(fs.mount_tar(&original[..length]), Err(TarError::Truncated));
        assert_eq!(fs.nodes.iter().filter(|node| node.linked).count(), 3);
    }
    for value in [
        b"77777777777\0".as_slice(),
        b"00000000008\0",
        b"\x8000000000000",
    ] {
        let mut bytes = original.to_vec();
        bytes[124..136].copy_from_slice(value);
        bytes[148..156].fill(b' ');
        let checksum: usize = bytes[..512].iter().map(|&byte| byte as usize).sum();
        bytes[148..156].copy_from_slice(std::format!("{checksum:06o}\0 ").as_bytes());
        let mut fs = Box::new(FileSystem::new());
        fs.initialize().unwrap();
        assert!(fs.mount_tar(Box::leak(bytes.into_boxed_slice())).is_err());
        assert_eq!(fs.nodes.iter().filter(|node| node.linked).count(), 3);
    }
}
