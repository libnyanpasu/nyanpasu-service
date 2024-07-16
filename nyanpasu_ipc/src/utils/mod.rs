use std::path::PathBuf;

use interprocess::local_socket::{GenericFilePath, Name, ToFsName};

pub mod os;

pub(crate) fn get_name<'n>(placeholder: &str) -> Result<Name<'n>, std::io::Error> {
    // TODO: support generic namespaced while I have clear understanding how to change the user group
    // if GenericNamespaced::is_supported() {
    //     return if cfg!(windows) {
    //         Ok(placeholder.to_string().to_ns_name::<GenericNamespaced>()?)
    //     } else {
    //         Ok(format!("{placeholder}.sock").to_ns_name::<GenericNamespaced>()?)
    //     };
    // }
    let name = if cfg!(windows) {
        format!("\\\\.\\pipe\\{placeholder}")
    } else {
        format!("/var/run/{placeholder}.sock")
    };
    name.to_fs_name::<GenericFilePath>()
}

#[cfg(unix)]
pub(crate) async fn remove_socket_if_exists(placeholder: &str) -> Result<(), std::io::Error> {
    let path: PathBuf = PathBuf::from(format!("/var/run/{placeholder}.sock"));
    if tokio::fs::metadata(&path).await.is_ok() {
        tokio::fs::remove_file(&path).await?;
    }
    Ok(())
}

/// Get the current millisecond timestamp
pub fn get_current_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
