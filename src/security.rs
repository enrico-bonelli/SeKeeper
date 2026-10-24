use std::io;
use std::time::{Duration, Instant};

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Must run before GTK or any secrets are loaded. This only changes this process
/// and its descendants, never the machine's global coredump configuration.
pub fn protect_process() -> io::Result<()> {
    let limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid rlimit pointer; prctl arguments are scalar values for these
    // Linux operations. No allocation or locks, so this is also pre_exec-safe.
    unsafe {
        if libc::setrlimit(libc::RLIMIT_CORE, &limits) != 0
            || libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
        {
            return Err(io::Error::last_os_error());
        }
        libc::umask(0o077);
    }
    Ok(())
}

pub fn validate_new_password(password: &str) -> Result<(), &'static str> {
    let characters = zeroize::Zeroizing::new(password.chars().collect::<Vec<char>>());
    if characters.len() < 15 || password.trim().is_empty() {
        return Err(
            "Usa almeno 15 caratteri. Preferisci 5–6 parole scelte a caso e una passphrase unica.",
        );
    }
    // Reject short repeating patterns without imposing character-class rules.
    let repetitive = (1..=4).any(|period| {
        characters
            .iter()
            .enumerate()
            .all(|(i, c)| *c == characters[i % period])
    });
    let common = [
        "passwordpassword",
        "password123456789",
        "123456789012345",
        "qwertyuiopasdfgh",
        "correct horse battery staple",
    ]
    .iter()
    .any(|common| password.eq_ignore_ascii_case(common));
    if repetitive || common {
        return Err(
            "Questa password è troppo prevedibile. Scegli una passphrase unica con parole casuali.",
        );
    }
    Ok(())
}

pub struct IdleLock {
    last_activity: Instant,
}

impl IdleLock {
    pub fn new(now: Instant) -> Self {
        Self { last_activity: now }
    }
    pub fn touch(&mut self, now: Instant) {
        self.last_activity = now;
    }
    pub fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_activity) >= IDLE_TIMEOUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_protection_is_enforced_in_an_isolated_child() {
        const MARKER: &str = "SEKEEPER_TEST_PROCESS_PROTECTION";
        if std::env::var_os(MARKER).is_some() {
            protect_process().unwrap();
            let mut limits = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            unsafe {
                assert_eq!(libc::getrlimit(libc::RLIMIT_CORE, &mut limits), 0);
                assert_eq!(libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0), 0);
                assert_eq!(libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0), 1);
                let mask = libc::umask(0o077);
                assert_eq!(mask, 0o077);
            }
            assert_eq!(limits.rlim_cur, 0);
            assert_eq!(limits.rlim_max, 0);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "security::tests::process_protection_is_enforced_in_an_isolated_child",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn new_password_policy_allows_passphrases_and_unicode() {
        for weak in [
            "",
            "a",
            "              ",
            "aaaaaaaaaaaaaaa",
            "abcabcabcabcabc",
            "Password123456789",
            "123456789012345",
        ] {
            assert!(validate_new_password(weak).is_err());
        }
        for good in [
            "laguna cedro nuvola marmo sentiero",
            "é🗝 alberi e stelle lontane",
            "a long unique password",
        ] {
            assert!(validate_new_password(good).is_ok());
        }
    }

    #[test]
    fn idle_lock_expires_at_deadline_and_resets_with_activity() {
        let start = Instant::now();
        let mut idle = IdleLock::new(start);
        assert!(!idle.expired(start + IDLE_TIMEOUT - Duration::from_millis(1)));
        assert!(idle.expired(start + IDLE_TIMEOUT));
        idle.touch(start + Duration::from_secs(200));
        assert!(!idle.expired(start + IDLE_TIMEOUT));
        assert!(idle.expired(start + Duration::from_secs(200) + IDLE_TIMEOUT));
    }
}
