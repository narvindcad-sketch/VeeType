use anyhow::Context;
use keyring::Entry;

const CREDENTIAL_SERVICE: &str = "VeeType";

pub struct KeyVault;

impl KeyVault {
    pub fn get_key(account: &str) -> anyhow::Result<Option<String>> {
        let entry = Entry::new(CREDENTIAL_SERVICE, account)
            .with_context(|| format!("Opening Windows Credential Manager entry for {account}"))?;
        match entry.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error).with_context(|| format!("Reading credential {account}")),
        }
    }

    pub fn save_key(account: &str, key: &str) -> anyhow::Result<()> {
        let entry = Entry::new(CREDENTIAL_SERVICE, account)
            .with_context(|| format!("Opening Windows Credential Manager entry for {account}"))?;
        entry
            .set_password(key)
            .with_context(|| format!("Saving credential {account}"))
    }

    pub fn delete_key(account: &str) -> anyhow::Result<()> {
        let entry = Entry::new(CREDENTIAL_SERVICE, account)
            .with_context(|| format!("Opening Windows Credential Manager entry for {account}"))?;
        match entry.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).with_context(|| format!("Removing credential {account}")),
        }
    }
}
