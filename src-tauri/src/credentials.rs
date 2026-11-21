use anyhow::Result;

const SERVICE: &str = "local.voxa.translate";

#[cfg(target_os = "macos")]
mod platform {
    use super::{Result, SERVICE};
    use anyhow::anyhow;
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    pub fn load(account: &str) -> Result<Option<String>> {
        match get_generic_password(SERVICE, account) {
            Ok(bytes) => Ok(Some(String::from_utf8(bytes).map_err(|_| {
                anyhow!("API key đã lưu trong macOS Keychain không hợp lệ.")
            })?)),
            Err(error) if error.code() == -25300 => Ok(None),
            Err(error) if matches!(error.code(), -128 | -25293 | -25308) => Err(anyhow!(
                "macOS chưa cho Voxa đọc API key. Mở Cài đặt và cho phép nếu Keychain hỏi."
            )),
            Err(error) => Err(anyhow!(
                "Không đọc được API key trong macOS Keychain ({}).",
                error.code()
            )),
        }
    }

    pub fn store(account: &str, key: &str) -> Result<()> {
        set_generic_password(SERVICE, account, key.as_bytes())
            .map_err(|_| anyhow!("Không lưu được API key vào macOS Keychain."))
    }

    pub fn forget(account: &str) -> Result<()> {
        match delete_generic_password(SERVICE, account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == -25300 => Ok(()),
            Err(_) => Err(anyhow!("Không xóa được API key khỏi macOS Keychain.")),
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{Result, SERVICE};
    use anyhow::anyhow;
    use keyring::{Entry, Error};

    fn entry(account: &str) -> Result<Entry> {
        Entry::new(SERVICE, account)
            .map_err(|_| anyhow!("Không mở được Windows Credential Manager."))
    }

    pub fn load(account: &str) -> Result<Option<String>> {
        match entry(account)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(Error::NoEntry) => Ok(None),
            Err(_) => Err(anyhow!(
                "Không đọc được API key trong Windows Credential Manager."
            )),
        }
    }

    pub fn store(account: &str, key: &str) -> Result<()> {
        entry(account)?
            .set_password(key)
            .map_err(|_| anyhow!("Không lưu được API key vào Windows Credential Manager."))
    }

    pub fn forget(account: &str) -> Result<()> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(_) => Err(anyhow!(
                "Không xóa được API key khỏi Windows Credential Manager."
            )),
        }
    }
}

pub use platform::{forget, load, store};
