use std::result::Result as StdResult;

use axum::Router;
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions,
    tokio::{Listener, Stream as InterProcessStream, prelude::*},
};
#[cfg(unix)]
use interprocess::os::unix::local_socket::ListenerOptionsExt;
#[cfg(windows)]
use interprocess::os::windows::{
    local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
};
use thiserror::Error;
use tracing_attributes::instrument;

type Result<T> = StdResult<T, ServerError>;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

pub struct InterProcessListener(Listener, String);

/// copy from axum::serve::listener.rs
async fn handle_accept_error(e: std::io::Error) {
    if is_connection_error(&e) {
        return;
    }

    // [From `hyper::Server` in 0.14](https://github.com/hyperium/hyper/blob/v0.14.27/src/server/tcp.rs#L186)
    //
    // > A possible scenario is that the process has hit the max open files
    // > allowed, and so trying to accept a new connection will fail with
    // > `EMFILE`. In some cases, it's preferable to just wait for some time, if
    // > the application will likely close some files (or connections), and try
    // > to accept the connection again. If this option is `true`, the error
    // > will be logged at the `error` level, since it is still a big deal,
    // > and then the listener will sleep for 1 second.
    //
    // hyper allowed customizing this but axum does not.
    tracing::error!("accept error: {e}");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
}

fn is_connection_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

impl axum::serve::Listener for InterProcessListener {
    type Io = InterProcessStream;
    // FIXME: it should be supported by upstream, or waiting for upstream got supported listener trait
    type Addr = String;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok(stream) => return (stream, self.1.clone()),
                Err(e) => handle_accept_error(e).await,
            }
        }
    }

    #[inline]
    fn local_addr(&self) -> tokio::io::Result<Self::Addr> {
        Ok(self.1.clone())
    }
}

#[instrument(skip(with_graceful_shutdown))]
pub async fn create_server(
    placeholder: &str,
    app: Router,
    with_graceful_shutdown: Option<impl Future<Output = ()> + Send + 'static>,
) -> Result<()> {
    let name_str = crate::utils::get_name_string(placeholder);
    let name = name_str.as_str().to_fs_name::<GenericFilePath>()?;
    #[cfg(unix)]
    {
        crate::utils::remove_socket_if_exists(placeholder).await?;
    }
    tracing::debug!("socket name: {:?}", name);
    let options = ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Both);
    #[cfg(windows)]
    let options = {
        use widestring::u16cstr;
        let sdsf = u16cstr!("D:(A;;GA;;;WD)"); // TODO: allow only the permitted users to connect
        let sw = SecurityDescriptor::deserialize(sdsf)?;
        options.security_descriptor(sw)
    };
    // allow owner and group to read and write
    #[cfg(unix)]
    let options = options.mode({
        #[cfg(target_os = "linux")]
        {
            0o664 as u32
        }
        #[cfg(not(target_os = "linux"))]
        {
            0o664 as u16
        }
    });

    let listener = options.create_tokio()?;
    let listener = InterProcessListener(listener, name_str);
    // change the socket group
    tracing::debug!("changing socket group and permissions...");
    crate::utils::os::change_socket_group(placeholder)?;
    crate::utils::os::change_socket_mode(placeholder)?;

    tracing::debug!("mounting service...");
    let server = axum::serve(listener, app);
    match with_graceful_shutdown {
        Some(graceful_shutdown) => server.with_graceful_shutdown(graceful_shutdown).await?,
        None => server.await?,
    };
    Ok(())
}
