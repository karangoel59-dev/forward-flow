use std::sync::Mutex;

// Protect entry mutations and Android checkouts with the same vault lock.
pub(crate) static VAULT_WRITES: Mutex<()> = Mutex::new(());
