use super::*;

pub async fn do_close(fd: FileDesc) -> Result<()> {
    debug!("close: fd: {}", fd);
    let current = current!();

    let file = current.file(fd)?;
    current.close_file(fd)?;
    file.close().await?;
    Ok(())
}
