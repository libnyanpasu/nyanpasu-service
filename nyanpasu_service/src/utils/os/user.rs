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
            .arg("nyanpasu")
            .output()
            .expect("failed to execute process");
        output.status.success();
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("dscl")
            .arg(".")
            .arg("-read")
            .arg("/Groups/nyanpasu")
            .output()
            .expect("failed to execute process");
        output.status.success()
    }
}

pub fn create_nyanpasu_group() -> Result<(), anyhow::Error> {
    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let output = Command::new("groupadd")
            .arg("nyanpasu")
            .output()
            .expect("failed to execute process");
        if !output.status.success() {
            anyhow::bail!("failed to create nyanpasu group");
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("dseditgroup")
            .arg("-o")
            .arg("create")
            .arg("-r")
            .arg("nyanpasu")
            .output()
            .expect("failed to execute process");
        if !output.status.success() {
            anyhow::bail!("failed to create nyanpasu group");
        }
        Ok(())
    }
}

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
            .arg("nyanpasu")
            .arg(username)
            .output()
            .expect("failed to execute process");
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
            .arg("nyanpasu")
            .output()
            .expect("failed to execute process");
        if !output.status.success() {
            anyhow::bail!("failed to add user to nyanpasu group");
        }
        Ok(())
    }
}
