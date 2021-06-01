use super::*;

pub fn do_openat(fs_path: &FsPath, flags: u32, mode: FileMode) -> Result<FileDesc> {
    debug!(
        "openat: fs_path: {:?}, flags: {:#o}, mode: {:#o}",
        fs_path, flags, mode
    );

    let current = current!();
    let fs = current.fs().read().unwrap();
    let masked_mode = mode & !current.process().umask();

    let inode_file = fs.open_file(&fs_path, flags, masked_mode)?;
    let file_ref = FileRef::new_inode(inode_file);

    let fd = {
        let creation_flags = CreationFlags::from_bits_truncate(flags);
        current.add_file(file_ref, creation_flags.must_close_on_spawn())
    };
    Ok(fd)
}
