use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use zeroize::Zeroizing;

const SERVICE: &str = "lazyterm.project-links";
const PREFIX: &str = "web-";
const CREDENTIAL_PREFIX: &str = "credential-";
const CLIPBOARD_LIFETIME: Duration = Duration::from_secs(30);

trait PasswordStore: Send + Sync {
    fn set(&self, reference: &str, password: &str) -> Result<(), String>;
    fn get(&self, reference: &str) -> Result<Zeroizing<String>, String>;
    fn delete(&self, reference: &str) -> Result<(), String>;
}

struct SystemStore;

fn store() -> Arc<dyn PasswordStore> {
    #[cfg(not(test))]
    {
        Arc::new(SystemStore)
    }
    #[cfg(test)]
    {
        static STORE: OnceLock<Arc<tests::Store>> = OnceLock::new();
        STORE
            .get_or_init(|| Arc::new(tests::Store::default()))
            .clone()
    }
}

fn entry(reference: &str) -> Result<keyring::Entry, String> {
    if !cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )) {
        return Err("System keychain is not supported on this platform.".into());
    }
    if !valid_reference(reference) {
        return Err("Invalid password reference.".into());
    }
    let service = if reference.starts_with(CREDENTIAL_PREFIX) {
        "lazyterm.credentials"
    } else {
        SERVICE
    };
    keyring::Entry::new(service, reference).map_err(|_| "Could not access system keychain.".into())
}

fn valid_reference(reference: &str) -> bool {
    reference
        .strip_prefix(PREFIX)
        .or_else(|| reference.strip_prefix(CREDENTIAL_PREFIX))
        .is_some_and(|token| {
            token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

impl PasswordStore for SystemStore {
    fn set(&self, reference: &str, password: &str) -> Result<(), String> {
        entry(reference)?
            .set_password(password)
            .map_err(|_| "Could not save password in system keychain; unlock it and retry.".into())
    }

    fn get(&self, reference: &str) -> Result<Zeroizing<String>, String> {
        entry(reference)?
            .get_password()
            .map(Zeroizing::new)
            .map_err(|_| "Could not read saved password; unlock the keychain or replace it.".into())
    }

    fn delete(&self, reference: &str) -> Result<(), String> {
        match entry(reference)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("Could not remove unused password from system keychain.".into()),
        }
    }
}

// A new password gets its own key. A failed metadata save never replaces the old password.
pub struct PreparedPassword {
    pub reference: String,
    committed: bool,
}

impl PreparedPassword {
    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PreparedPassword {
    fn drop(&mut self) {
        if !self.committed {
            let reference = self.reference.clone();
            std::thread::spawn(move || {
                let _ = store().delete(&reference);
            });
        }
    }
}

#[cfg(test)]
fn save_new(store: &dyn PasswordStore, password: &str) -> Result<String, String> {
    save_with_prefix(store, password, PREFIX)
}

fn save_with_prefix(
    store: &dyn PasswordStore,
    password: &str,
    prefix: &str,
) -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| "Could not generate password reference.".to_string())?;
    let reference = format!(
        "{prefix}{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    store.set(&reference, password)?;
    Ok(reference)
}

pub fn prepare(password: Zeroizing<String>) -> Receiver<Result<PreparedPassword, String>> {
    prepare_with_prefix(password, PREFIX)
}

pub fn prepare_credential(
    password: Zeroizing<String>,
) -> Receiver<Result<PreparedPassword, String>> {
    prepare_with_prefix(password, CREDENTIAL_PREFIX)
}

fn prepare_with_prefix(
    password: Zeroizing<String>,
    prefix: &'static str,
) -> Receiver<Result<PreparedPassword, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = save_with_prefix(store().as_ref(), &password, prefix).map(|reference| {
            PreparedPassword {
                reference,
                committed: false,
            }
        });
        let _ = sender.send(result);
    });
    receiver
}

pub fn remove_unused(references: Vec<String>) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut result = Ok(String::new());
        for reference in references {
            if let Err(error) = store().delete(&reference) {
                result = Err(error);
            }
        }
        let _ = sender.send(result);
    });
    receiver
}

pub fn copy_username(username: String) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = copy(Zeroizing::new(username), false).map(|()| "Username copied.".into());
        let _ = sender.send(result);
    });
    receiver
}

pub fn copy_public_key(public_key: String) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = copy(Zeroizing::new(public_key), false).map(|()| "Public key copied.".into());
        let _ = sender.send(result);
    });
    receiver
}

pub fn copy_password(reference: String) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = store()
            .get(&reference)
            .and_then(|password| copy(password, true));
        let _ = sender.send(result.map(|()| "Password copied; clipboard expires in 30s.".into()));
    });
    receiver
}

pub fn copy_password_value(password: String) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = copy(Zeroizing::new(password), true);
        let _ = sender.send(result.map(|()| "Password copied; clipboard expires in 30s.".into()));
    });
    receiver
}

pub fn read_password(reference: &str) -> Result<Zeroizing<String>, String> {
    store().get(reference)
}

struct CopyRequest {
    text: Zeroizing<String>,
    secret: bool,
    reply: Sender<Result<(), String>>,
}

enum ClipboardMessage {
    Copy(CopyRequest),
    Clear(Sender<()>),
}

static CLIPBOARD_WORKER: OnceLock<Sender<ClipboardMessage>> = OnceLock::new();
static CLIPBOARD_CLOSED: AtomicBool = AtomicBool::new(false);

pub fn clear_password_clipboard() {
    CLIPBOARD_CLOSED.store(true, Ordering::SeqCst);
    if let Some(worker) = CLIPBOARD_WORKER.get() {
        let (reply, receiver) = mpsc::channel();
        if worker.send(ClipboardMessage::Clear(reply)).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_millis(500));
        }
    }
}

trait Clipboard {
    fn set(&mut self, value: &str) -> Result<(), String>;
    fn set_secret(&mut self, value: &str) -> Result<(), String> {
        self.set(value)
    }
    fn get(&mut self) -> Result<Zeroizing<String>, String>;
    fn clear(&mut self) -> Result<(), String>;
}

impl Clipboard for arboard::Clipboard {
    fn set(&mut self, value: &str) -> Result<(), String> {
        self.set_text(value)
            .map_err(|_| "Could not copy to clipboard.".into())
    }

    fn set_secret(&mut self, value: &str) -> Result<(), String> {
        set_secret_clipboard(self, value)
    }

    fn get(&mut self) -> Result<Zeroizing<String>, String> {
        self.get_text()
            .map(Zeroizing::new)
            .map_err(|_| "Clipboard unavailable.".into())
    }

    fn clear(&mut self) -> Result<(), String> {
        arboard::Clipboard::clear(self).map_err(|_| "Could not clear clipboard.".into())
    }
}

fn system_clipboard() -> Option<Box<dyn Clipboard>> {
    #[cfg(not(test))]
    {
        arboard::Clipboard::new()
            .ok()
            .map(|clipboard| Box::new(clipboard) as Box<dyn Clipboard>)
    }
    #[cfg(test)]
    {
        Some(Box::new(tests::TestClipboard(String::new())))
    }
}

fn set_secret_clipboard(clipboard: &mut arboard::Clipboard, value: &str) -> Result<(), String> {
    let builder = clipboard.set();
    #[cfg(target_os = "macos")]
    let builder = {
        use arboard::SetExtApple;
        builder.exclude_from_history()
    };
    #[cfg(target_os = "windows")]
    let builder = {
        use arboard::SetExtWindows;
        builder.exclude_from_monitoring()
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let builder = {
        use arboard::SetExtLinux;
        builder.exclude_from_history()
    };
    builder
        .text(value)
        .map_err(|_| "Could not copy to clipboard.".into())
}

fn clear_if_unchanged(clipboard: &mut dyn Clipboard, copied: &str) {
    if clipboard.get().is_ok_and(|current| *current == copied) {
        let _ = clipboard.clear();
    }
}

fn copy(text: Zeroizing<String>, secret: bool) -> Result<(), String> {
    if CLIPBOARD_CLOSED.load(Ordering::SeqCst) {
        return Err("Application is closing; copy canceled.".into());
    }
    let worker = CLIPBOARD_WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<ClipboardMessage>();
        std::thread::spawn(move || {
            let mut clipboard = None;
            let mut expires: Option<(Instant, Zeroizing<String>)> = None;
            loop {
                let request = if let Some((deadline, _)) = &expires {
                    receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                } else {
                    receiver
                        .recv()
                        .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                };
                match request {
                    Ok(ClipboardMessage::Copy(request)) => {
                        if CLIPBOARD_CLOSED.load(Ordering::SeqCst) {
                            let _ = request
                                .reply
                                .send(Err("Application is closing; copy canceled.".into()));
                            continue;
                        }
                        if clipboard.is_none() {
                            clipboard = system_clipboard();
                        }
                        let result = clipboard
                            .as_deref_mut()
                            .ok_or_else(|| "System clipboard is unavailable.".to_string())
                            .and_then(|clipboard| {
                                if request.secret {
                                    clipboard.set_secret(&request.text)
                                } else {
                                    Clipboard::set(clipboard, &request.text)
                                }
                            });
                        if result.is_ok() {
                            expires = request
                                .secret
                                .then(|| (Instant::now() + CLIPBOARD_LIFETIME, request.text));
                        }
                        let _ = request.reply.send(result);
                    }
                    Ok(ClipboardMessage::Clear(reply)) => {
                        if let Some((_, text)) = expires.take()
                            && let Some(clipboard) = &mut clipboard
                        {
                            clear_if_unchanged(clipboard.as_mut(), &text);
                        }
                        let _ = reply.send(());
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((_, text)) = expires.take()
                            && let Some(clipboard) = &mut clipboard
                        {
                            clear_if_unchanged(clipboard.as_mut(), &text);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        sender
    });
    let (reply, receiver) = mpsc::channel();
    worker
        .send(ClipboardMessage::Copy(CopyRequest {
            text,
            secret,
            reply,
        }))
        .map_err(|_| "Clipboard worker unavailable.".to_string())?;
    receiver
        .recv()
        .map_err(|_| "Clipboard worker stopped.".to_string())?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    #[derive(Default)]
    pub(super) struct Store(Mutex<HashMap<String, String>>);
    impl PasswordStore for Store {
        fn set(&self, reference: &str, password: &str) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .insert(reference.into(), password.into());
            Ok(())
        }
        fn get(&self, reference: &str) -> Result<Zeroizing<String>, String> {
            self.0
                .lock()
                .unwrap()
                .get(reference)
                .cloned()
                .map(Zeroizing::new)
                .ok_or("missing".into())
        }
        fn delete(&self, reference: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(reference);
            Ok(())
        }
    }

    pub(super) struct TestClipboard(pub(super) String);
    impl Clipboard for TestClipboard {
        fn set(&mut self, value: &str) -> Result<(), String> {
            self.0 = value.into();
            Ok(())
        }
        fn get(&mut self) -> Result<Zeroizing<String>, String> {
            Ok(Zeroizing::new(self.0.clone()))
        }
        fn clear(&mut self) -> Result<(), String> {
            self.0.clear();
            Ok(())
        }
    }

    pub(crate) fn password_exists(reference: &str) -> bool {
        store().get(reference).is_ok()
    }

    #[test]
    fn abandoned_preparations_remove_only_their_new_password() {
        let old = prepare(Zeroizing::new("old".into()))
            .recv()
            .unwrap()
            .unwrap();
        let old_ref = old.reference.clone();
        let mut old = old;
        old.commit();
        drop(old);
        let new = prepare(Zeroizing::new("new".into()))
            .recv()
            .unwrap()
            .unwrap();
        let new_ref = new.reference.clone();
        drop(new);
        let deadline = Instant::now() + Duration::from_secs(2);
        while password_exists(&new_ref) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!password_exists(&new_ref));
        assert!(password_exists(&old_ref));
        store().delete(&old_ref).unwrap();
    }

    #[test]
    #[ignore = "opt-in native keychain smoke; may ask to unlock the system keychain"]
    fn native_keychain_round_trip() {
        let store = SystemStore;
        let reference = save_new(&store, "lazyterm-disposable-test-password").unwrap();
        let value = store.get(&reference);
        let removed = store.delete(&reference);
        assert_eq!(&**value.unwrap(), "lazyterm-disposable-test-password");
        removed.unwrap();
        assert!(store.get(&reference).is_err());
    }

    #[test]
    fn replacements_are_isolated_and_references_contain_no_password() {
        let store = Store::default();
        let old = save_new(&store, "old secret").unwrap();
        let new = save_new(&store, "new secret").unwrap();
        assert!(valid_reference(&old));
        assert!(valid_reference(&new));
        assert_ne!(old, new);
        assert_eq!(&**store.get(&old).unwrap(), "old secret");
        store.delete(&new).unwrap();
        assert_eq!(&**store.get(&old).unwrap(), "old secret");
        assert!(store.get(&new).is_err());
        assert!(!valid_reference("ssh-password"));
    }

    #[test]
    fn expiry_preserves_new_clipboard_contents() {
        let mut clipboard = TestClipboard("password".into());
        clear_if_unchanged(&mut clipboard, "password");
        assert!(clipboard.0.is_empty());
        clipboard.set("new contents").unwrap();
        clear_if_unchanged(&mut clipboard, "password");
        assert_eq!(clipboard.0, "new contents");
    }
}
