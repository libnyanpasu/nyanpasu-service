#![allow(dead_code)]
#[cfg(not(windows))]
use nyanpasu_ipc::utils::os::NYANPASU_USER_GROUP;
use tracing_attributes::instrument;

#[instrument]
pub fn is_nyanpasu_group_exists() -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let output = Command::new("getent")
            .arg("group")
            .arg(NYANPASU_USER_GROUP)
            .output()
            .expect("failed to execute process");
        tracing::debug!("output: {:?}", output);
        output.status.success()
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let group = format!("/Groups/{}", NYANPASU_USER_GROUP);
        let output = Command::new("dscl")
            .arg(".")
            .arg("-read")
            .arg(&group)
            .output()
            .expect("failed to execute process");
        tracing::debug!("output: {:?}", output);
        output.status.success()
    }
}

#[instrument]
pub fn create_nyanpasu_group() -> Result<(), anyhow::Error> {
    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let output = Command::new("groupadd")
            .arg(NYANPASU_USER_GROUP)
            .output()
            .expect("failed to execute process");
        tracing::debug!("output: {:?}", output);
        if !output.status.success() {
            anyhow::bail!("failed to create nyanpasu group");
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("dscl")
            .arg(".")
            .arg("-create")
            .arg(format!("/Groups/{}", NYANPASU_USER_GROUP))
            .output()
            .expect("failed to execute process");
        tracing::debug!("output: {:?}", output);
        if !output.status.success() {
            anyhow::bail!("failed to create nyanpasu group");
        }
        Ok(())
    }
}

#[instrument]
pub fn is_user_in_nyanpasu_group(username: &str) -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let output = Command::new("id")
            .arg("-nG")
            .arg(username)
            .output()
            .expect("failed to execute process");
        let output = String::from_utf8_lossy(&output.stdout);
        tracing::debug!("output: {:?}", output);
        output.contains(NYANPASU_USER_GROUP)
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("dseditgroup")
            .arg("-o")
            .arg("checkmember")
            .arg("-m")
            .arg(username)
            .arg(NYANPASU_USER_GROUP)
            .output()
            .expect("failed to execute process");
        tracing::debug!("output: {:?}", output);
        output.status.success()
    }
}

#[instrument]
pub fn add_user_to_nyanpasu_group(username: &str) -> Result<(), anyhow::Error> {
    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let output = Command::new("usermod")
            .arg("-aG")
            .arg(NYANPASU_USER_GROUP)
            .arg(username)
            .output()
            .expect("failed to execute process");
        tracing::debug!("output: {:?}", output);
        if !output.status.success() {
            anyhow::bail!("failed to add user to nyanpasu group");
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("dseditgroup")
            .arg("-o")
            .arg("edit")
            .arg("-a")
            .arg(username)
            .arg("-t")
            .arg("user")
            .arg(NYANPASU_USER_GROUP)
            .output()
            .expect("failed to execute process");
        tracing::debug!("output: {:?}", output);
        if !output.status.success() {
            anyhow::bail!("failed to add user to nyanpasu group");
        }
        Ok(())
    }
}
