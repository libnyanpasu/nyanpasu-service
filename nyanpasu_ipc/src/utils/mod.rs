use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Name, NameType, ToFsName, ToNsName,
};

pub(crate) fn get_name<'n>(placeholder: &str) -> Result<Name<'n>, std::io::Error> {
    if GenericNamespaced::is_supported() {
        return if cfg!(windows) {
            Ok(placeholder.to_string().to_ns_name::<GenericNamespaced>()?)
        } else {
            Ok(format!("{placeholder}.sock").to_ns_name::<GenericNamespaced>()?)
        };
    }
    let name = if cfg!(windows) {
        format!("\\\\.\\pipe\\{placeholder}")
    } else {
        format!("/var/run/{placeholder}.sock")
    };
    name.to_fs_name::<GenericFilePath>()
}

pub async fn is_service_installed() -> bool {
    true
}
