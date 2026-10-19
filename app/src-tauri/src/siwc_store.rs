use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use uuid::Uuid;
use zeroize::Zeroize;

const MAX_RECORD_BYTES: usize = 256 * 1024;
const DIRECT_PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct RegistrationMetadata {
    pub(crate) issuer: String,
    pub(crate) subject: String,
    pub(crate) client_id: String,
    pub(crate) ext_agent_host_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct TokenCredentials {
    pub(crate) id_token: String,
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) expires_at_unix: i64,
    pub(crate) token_type: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) saved_at: String,
}

impl Drop for TokenCredentials {
    fn drop(&mut self) {
        self.id_token.zeroize();
        self.access_token.zeroize();
        self.refresh_token.zeroize();
        self.token_type.zeroize();
        self.scopes.zeroize();
        self.saved_at.zeroize();
    }
}

impl TokenCredentials {
    pub(crate) fn allows_plan_usage(&self) -> bool {
        self.scopes.iter().any(|scope| scope == DIRECT_PLAN_SCOPE)
    }
}

#[derive(Deserialize, Serialize)]
struct HostIdentity {
    ext_agent_host_id: String,
}

pub(crate) struct CredentialStore {
    root: PathBuf,
}

pub(crate) struct CredentialFileLock {
    path: PathBuf,
    owner_id: String,
}

impl Drop for CredentialFileLock {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path).is_ok_and(|owner| owner == self.owner_id) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl CredentialStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn registration_path(&self) -> PathBuf {
        self.root.join("chatgpt-registration.dpapi")
    }

    pub(crate) fn tokens_path(&self) -> PathBuf {
        self.root.join("chatgpt-tokens.dpapi")
    }

    fn host_path(&self) -> PathBuf {
        self.root.join("chatgpt-host.dpapi")
    }

    fn host_lock_path(&self) -> PathBuf {
        self.root.join("chatgpt-host.lock")
    }

    fn account_lock_path(&self) -> PathBuf {
        self.root.join("chatgpt-account.lock")
    }

    pub(crate) async fn acquire_refresh_lock(&self) -> Result<CredentialFileLock, String> {
        acquire_file_lock(
            self.root.join("chatgpt-refresh.lock"),
            Duration::from_secs(45),
            Duration::from_secs(120),
        )
        .await
    }

    pub(crate) fn load_or_create_host_id(&self) -> Result<String, String> {
        fs::create_dir_all(&self.root)
            .map_err(|error| format!("无法创建AI账号本地数据目录：{error}"))?;
        let _lock = acquire_file_lock_blocking(
            self.host_lock_path(),
            Duration::from_secs(10),
            Duration::from_secs(30),
        )?;
        if let Some(identity) = self.read_record::<HostIdentity>(&self.host_path())? {
            validate_host_id(&identity.ext_agent_host_id)?;
            return Ok(identity.ext_agent_host_id);
        }

        let host_id = format!("urn:uuid:{}", Uuid::new_v4().hyphenated());
        self.write_record(
            &self.host_path(),
            &HostIdentity {
                ext_agent_host_id: host_id.clone(),
            },
        )?;
        Ok(host_id)
    }

    pub(crate) fn load_registration(&self) -> Result<Option<RegistrationMetadata>, String> {
        self.read_record(&self.registration_path())
    }

    pub(crate) fn save_registration(
        &self,
        registration: &RegistrationMetadata,
    ) -> Result<(), String> {
        let host_id = self.load_or_create_host_id()?;
        validate_registration(registration, &host_id)?;
        let _lock = self.acquire_account_lock()?;
        self.write_record(&self.registration_path(), registration)
    }

    pub(crate) fn save_account(
        &self,
        registration: &RegistrationMetadata,
        tokens: &TokenCredentials,
    ) -> Result<(), String> {
        let host_id = self.load_or_create_host_id()?;
        validate_registration(registration, &host_id)?;
        validate_tokens(tokens)?;
        let _lock = self.acquire_account_lock()?;
        self.write_record(&self.registration_path(), registration)?;
        self.write_record(&self.tokens_path(), tokens)
    }

    pub(crate) fn load_tokens(&self) -> Result<Option<TokenCredentials>, String> {
        self.read_record(&self.tokens_path())
    }

    pub(crate) fn save_tokens(&self, tokens: &TokenCredentials) -> Result<(), String> {
        validate_tokens(tokens)?;
        let _lock = self.acquire_account_lock()?;
        self.write_record(&self.tokens_path(), tokens)
    }

    /// Disconnecting revokes the credential set while preserving the encrypted
    /// registration identity so a later authorization can reuse its issued id.
    pub(crate) fn disconnect(&self) -> Result<(), String> {
        let _lock = self.acquire_account_lock()?;
        remove_if_present(&self.tokens_path())
    }

    /// Forgetting the account removes the token set, registration metadata and
    /// stable host identity. The next registration therefore receives a new id.
    pub(crate) fn forget_account(&self) -> Result<(), String> {
        let _lock = self.acquire_account_lock()?;
        remove_if_present(&self.tokens_path())?;
        remove_if_present(&self.registration_path())?;
        remove_if_present(&self.host_path())
    }

    fn acquire_account_lock(&self) -> Result<CredentialFileLock, String> {
        acquire_file_lock_blocking(
            self.account_lock_path(),
            Duration::from_secs(10),
            Duration::from_secs(30),
        )
    }

    fn write_record<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), String> {
        let mut plaintext = serde_json::to_vec(value)
            .map_err(|error| format!("无法序列化AI账号本地数据：{error}"))?;
        if plaintext.len() > MAX_RECORD_BYTES {
            plaintext.fill(0);
            return Err("AI账号本地数据超过安全存储大小限制。".into());
        }
        let encrypted = protect_current_user(&plaintext);
        plaintext.fill(0);
        let encrypted = encrypted?;
        atomic_write(path, &encrypted)
    }

    fn read_record<T: DeserializeOwned>(&self, path: &Path) -> Result<Option<T>, String> {
        let encrypted = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("无法读取加密AI账号数据：{error}")),
        };
        if encrypted.len() > MAX_RECORD_BYTES + 64 * 1024 {
            return Err("加密AI账号数据超过安全存储大小限制，已拒绝读取。".into());
        }
        let mut plaintext = unprotect_current_user(&encrypted)?;
        if plaintext.len() > MAX_RECORD_BYTES {
            plaintext.fill(0);
            return Err("解密后的AI账号数据超过安全存储大小限制，已拒绝读取。".into());
        }
        let result = serde_json::from_slice(&plaintext)
            .map_err(|error| format!("加密AI账号数据格式无效或已损坏：{error}"));
        plaintext.fill(0);
        result.map(Some)
    }
}

fn validate_host_id(host_id: &str) -> Result<(), String> {
    let value = host_id.strip_prefix("urn:uuid:").ok_or_else(|| {
        "已保存的本机账号标识格式无效，已拒绝覆盖。请使用“忘记账号”清除后重连。".to_string()
    })?;
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| "已保存的本机账号标识损坏，已拒绝覆盖。请使用“忘记账号”清除后重连。".into())
}

pub(crate) fn valid_issued_client_id(client_id: &str) -> bool {
    !client_id.trim().is_empty()
        && client_id.len() <= 512
        && client_id != "dynamic_agent_client"
        && client_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn validate_registration(registration: &RegistrationMetadata, host_id: &str) -> Result<(), String> {
    if registration.issuer != "https://auth.openai.com" {
        return Err("注册信息的签发方不是受支持的OpenAI账号服务。".into());
    }
    if registration.subject.trim().is_empty() || registration.subject.len() > 1024 {
        return Err("注册信息缺少有效的账号主体标识，无法安全关联账号。".into());
    }
    if !valid_issued_client_id(&registration.client_id) {
        return Err("注册信息缺少服务端签发的客户端ID。".into());
    }
    if registration.ext_agent_host_id != host_id {
        return Err("注册信息与本机稳定标识不匹配，请重新连接账号。".into());
    }
    Ok(())
}

fn validate_tokens(tokens: &TokenCredentials) -> Result<(), String> {
    if tokens.id_token.trim().is_empty()
        || tokens.access_token.trim().is_empty()
        || tokens.refresh_token.trim().is_empty()
    {
        return Err("OpenAI授权响应缺少必要令牌，未保存账号凭据。".into());
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("无法删除本地AI账号数据：{error}")),
    }
}

async fn acquire_file_lock(
    path: PathBuf,
    timeout: Duration,
    stale_after: Duration,
) -> Result<CredentialFileLock, String> {
    let start = Instant::now();
    loop {
        match try_create_lock(&path) {
            Ok(Some(lock)) => return Ok(lock),
            Ok(None) => {
                remove_stale_lock(&path, stale_after);
                if start.elapsed() >= timeout {
                    return Err("另一个书隅进程正在刷新ChatGPT令牌，请稍后重试。".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn acquire_file_lock_blocking(
    path: PathBuf,
    timeout: Duration,
    stale_after: Duration,
) -> Result<CredentialFileLock, String> {
    let start = Instant::now();
    loop {
        match try_create_lock(&path) {
            Ok(Some(lock)) => return Ok(lock),
            Ok(None) => {
                remove_stale_lock(&path, stale_after);
                if start.elapsed() >= timeout {
                    return Err("另一个书隅进程正在初始化AI账号数据，请稍后重试。".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

fn try_create_lock(path: &Path) -> Result<Option<CredentialFileLock>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "AI账号锁文件路径无效。".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建AI账号数据目录：{error}"))?;
    let owner_id = format!("{}:{}", std::process::id(), Uuid::new_v4());
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(error) => return Err(format!("无法创建ChatGPT令牌同步锁：{error}")),
    };
    file.write_all(owner_id.as_bytes())
        .map_err(|error| format!("无法写入ChatGPT令牌同步锁：{error}"))?;
    file.sync_all()
        .map_err(|error| format!("无法持久化ChatGPT令牌同步锁：{error}"))?;
    Ok(Some(CredentialFileLock {
        path: path.to_path_buf(),
        owner_id,
    }))
}

fn remove_stale_lock(path: &Path, stale_after: Duration) {
    let stale = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > stale_after);
    if stale {
        let _ = fs::remove_file(path);
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "AI账号数据文件路径无效。".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建AI账号本地数据目录：{error}"))?;
    let temp_path = parent.join(format!(".{}.{}.tmp", Uuid::new_v4(), "siwc"));
    let write_result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| format!("无法创建AI账号加密临时文件：{error}"))?;
        file.write_all(contents)
            .map_err(|error| format!("无法写入AI账号加密数据：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("无法持久化AI账号加密数据：{error}"))?;
        drop(file);
        replace_file(&temp_path, path)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(format!(
            "无法原子更新AI账号加密数据：{}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(_source: &Path, _destination: &Path) -> Result<(), String> {
    Err("ChatGPT账号凭据仅支持Windows DPAPI安全存储。".into())
}

#[cfg(windows)]
fn protect_current_user(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB},
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len() as u32,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let protected = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if protected == 0 {
        return Err(format!(
            "Windows DPAPI加密AI账号数据失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let encrypted =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData as _) };
    Ok(encrypted)
}

#[cfg(not(windows))]
fn protect_current_user(_plaintext: &[u8]) -> Result<Vec<u8>, String> {
    Err("ChatGPT账号凭据仅支持Windows DPAPI安全存储。".into())
}

#[cfg(windows)]
fn unprotect_current_user(encrypted: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let unprotected = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if unprotected == 0 {
        return Err(format!(
            "Windows DPAPI无法解密AI账号数据；数据可能已损坏或属于其他Windows账户：{}",
            std::io::Error::last_os_error()
        ));
    }
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData as _) };
    Ok(plaintext)
}

#[cfg(not(windows))]
fn unprotect_current_user(_encrypted: &[u8]) -> Result<Vec<u8>, String> {
    Err("ChatGPT账号凭据仅支持Windows DPAPI安全存储。".into())
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_file_lock, CredentialStore, RegistrationMetadata, TokenCredentials,
        DIRECT_PLAN_SCOPE,
    };
    use std::time::Duration;

    fn registration(host_id: &str) -> RegistrationMetadata {
        RegistrationMetadata {
            issuer: "https://auth.openai.com".into(),
            subject: "test-subject".into(),
            client_id: "oaiapp_test".into(),
            ext_agent_host_id: host_id.into(),
        }
    }

    fn tokens() -> TokenCredentials {
        TokenCredentials {
            id_token: "test-id-token".into(),
            access_token: "secret-access-token".into(),
            refresh_token: "secret-refresh-token".into(),
            expires_at_unix: 1_900_000_000,
            token_type: "Bearer".into(),
            scopes: vec!["openid".into(), DIRECT_PLAN_SCOPE.into()],
            saved_at: "2026-10-09T00:00:00Z".into(),
        }
    }

    #[test]
    fn disconnect_deletes_tokens_but_keeps_registration_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let host_id = store.load_or_create_host_id().unwrap();
        let registration = registration(&host_id);
        store.save_registration(&registration).unwrap();
        store.save_tokens(&tokens()).unwrap();

        store.disconnect().unwrap();

        assert_eq!(store.load_registration().unwrap(), Some(registration));
        assert!(store.load_tokens().unwrap().is_none());
        assert_eq!(store.load_or_create_host_id().unwrap(), host_id);
        let encrypted = std::fs::read(store.tokens_path()).unwrap_err();
        assert_eq!(encrypted.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn forgetting_account_removes_registration_and_tokens() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let registration = registration(&store.load_or_create_host_id().unwrap());
        store.save_registration(&registration).unwrap();
        store.save_tokens(&tokens()).unwrap();

        store.forget_account().unwrap();

        assert_eq!(store.load_registration().unwrap(), None);
        assert!(store.load_tokens().unwrap().is_none());
        assert!(std::fs::read(store.host_path()).is_err());
    }

    #[test]
    fn protected_files_do_not_contain_plaintext_tokens() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let registration = registration(&store.load_or_create_host_id().unwrap());
        store.save_registration(&registration).unwrap();
        store.save_tokens(&tokens()).unwrap();

        let stored = std::fs::read(store.tokens_path()).unwrap();
        let stored = String::from_utf8_lossy(&stored);
        assert!(!stored.contains("secret-access-token"));
        assert!(!stored.contains("secret-refresh-token"));
        assert!(!stored.contains("test-id-token"));
    }

    #[test]
    fn host_id_is_persisted_and_reused_until_the_account_is_forgotten() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let first = store.load_or_create_host_id().unwrap();
        let second = store.load_or_create_host_id().unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("urn:uuid:"));

        store.forget_account().unwrap();

        let after_forget = store.load_or_create_host_id().unwrap();
        assert_ne!(first, after_forget);
    }

    #[test]
    fn invalid_dpapi_payload_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        std::fs::write(store.tokens_path(), b"not a DPAPI payload").unwrap();

        assert!(store.load_tokens().is_err());
    }

    #[test]
    fn identity_credentials_without_direct_usage_scope_can_be_saved_but_do_not_grant_usage() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let mut tokens = tokens();
        tokens.scopes = vec!["openid".into()];

        store.save_tokens(&tokens).unwrap();
        let saved = store.load_tokens().unwrap().unwrap();
        assert!(!saved.allows_plan_usage());
    }

    #[test]
    fn mismatched_registration_host_id_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let registration = registration("urn:uuid:00000000-0000-4000-8000-000000000000");

        assert!(store
            .save_registration(&registration)
            .unwrap_err()
            .contains("本机稳定标识不匹配"));
    }

    #[test]
    fn dynamic_registration_entrypoint_cannot_be_saved_as_issued_client_id() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let host_id = store.load_or_create_host_id().unwrap();
        let mut registration = registration(&host_id);
        registration.client_id = "dynamic_agent_client".into();

        assert!(store.save_registration(&registration).is_err());
    }

    #[tokio::test]
    async fn refresh_lock_serializes_other_processes_and_releases_after_scope() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("refresh.lock");
        let first = acquire_file_lock(
            path.clone(),
            Duration::from_secs(1),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(acquire_file_lock(
            path.clone(),
            Duration::from_millis(100),
            Duration::from_secs(30)
        )
        .await
        .is_err());
        drop(first);
        assert!(
            acquire_file_lock(path, Duration::from_secs(1), Duration::from_secs(30))
                .await
                .is_ok()
        );
    }
}
