//! Launch options: command line first, then environment, then defaults.
//!
//! | Option | Env | Default |
//! |---|---|---|
//! | `--config-dir DIR` | `MUSAIC_CONFIG_DIR` | `~/.musaic` (no `MEDIA_MANAGER_CONFIG_DIR`, no `~/.media_manager` move) |
//! | `--port N` | `MUSAIC_PORT` | `0`: an ephemeral loopback port |
//! | `--network live\|offline\|replay` | `MUSAIC_NETWORK` | `live`; `replay` when only a replay dir is given |
//! | `--replay-dir DIR` | `MUSAIC_REPLAY_DIR` | none |
//! | `--frontend-dir DIR` | `MUSAIC_FRONTEND_DIR` | the bundle's `frontend/` resource, else (debug builds) the repo's `frontend/dist` |
//! | `--dev-memory-secrets` | none | off: secrets go to the OS keyring |
//! | `--selftest-report FILE` | none | off (needs `--dev-memory-secrets`) |
//! | `--hidden` | none | off: the window is shown |
//!
//! The bind host is always `127.0.0.1`: the desktop shell never listens on
//! another interface (`MUSAIC_HOST` is ignored on purpose).
//!
//! `--dev-memory-secrets` keeps API keys in memory instead of the OS
//! keyring. It is refused unless the config dir is under the system temp
//! dir or under a directory named `work` (a repository's scratch
//! area), the rule `musaic-api --verification-memory-secrets` uses, so a
//! dev run can never be pointed at a real `~/.musaic` and lose its keys.

use std::path::{Path, PathBuf};

/// Outbound network mode for the resolver, AcoustID and cover downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkMode {
    Live,
    Offline,
    Replay(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub config_dir: PathBuf,
    pub port: u16,
    pub network: NetworkMode,
    pub frontend_dir: Option<PathBuf>,
    pub dev_memory_secrets: bool,
    pub selftest_report: Option<PathBuf>,
    pub hidden: bool,
}

/// Where the values come from (separated so tests need no process env).
pub trait Source {
    fn arg(&self, name: &str) -> Option<String>;
    fn flag(&self, name: &str) -> bool;
    fn env(&self, name: &str) -> Option<String>;
    fn cwd(&self) -> PathBuf;
}

/// The real process: `std::env::args()` and `std::env::var`.
pub struct Process;

impl Source for Process {
    fn arg(&self, name: &str) -> Option<String> {
        arg_in(std::env::args().skip(1), name)
    }
    fn flag(&self, name: &str) -> bool {
        std::env::args().skip(1).any(|a| a == name)
    }
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
    fn cwd(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_default()
    }
}

/// `--name value` or `--name=value`.
pub fn arg_in(args: impl IntoIterator<Item = String>, name: &str) -> Option<String> {
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn absolute(cwd: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

impl Options {
    pub fn parse(src: &dyn Source) -> Result<Options, String> {
        let cwd = src.cwd();
        let config_dir = match src.arg("--config-dir").filter(|d| !d.is_empty()) {
            Some(d) => absolute(&cwd, &d),
            None => musaic_core::config::config_dir()
                .ok_or("no home directory and no MUSAIC_CONFIG_DIR; cannot start")?,
        };
        let port = match src.arg("--port").or_else(|| src.env("MUSAIC_PORT")) {
            None => 0,
            Some(p) => p
                .trim()
                .parse::<u16>()
                .map_err(|_| format!("bad port {p:?}"))?,
        };
        let mode = src.arg("--network").or_else(|| src.env("MUSAIC_NETWORK"));
        let replay_dir = src
            .arg("--replay-dir")
            .or_else(|| src.env("MUSAIC_REPLAY_DIR"))
            .map(|d| absolute(&cwd, &d));
        let network = match (mode.as_deref().map(str::to_ascii_lowercase), replay_dir) {
            (None, None) => NetworkMode::Live,
            (None, Some(d)) => NetworkMode::Replay(d),
            (Some(m), d) => match (m.as_str(), d) {
                ("live", None) => NetworkMode::Live,
                ("offline", None) => NetworkMode::Offline,
                ("replay", Some(d)) => NetworkMode::Replay(d),
                ("replay", None) => {
                    return Err("--network replay needs --replay-dir DIR".into());
                }
                ("live" | "offline", Some(_)) => {
                    return Err("--replay-dir is only valid with --network replay".into());
                }
                (other, _) => {
                    return Err(format!("unknown --network {other:?} (live|offline|replay)"));
                }
            },
        };
        let frontend_dir = src
            .arg("--frontend-dir")
            .or_else(|| src.env("MUSAIC_FRONTEND_DIR"))
            .map(|d| absolute(&cwd, &d));
        let dev_memory_secrets = src.flag("--dev-memory-secrets");
        if dev_memory_secrets && !scratch_dir_ok(&config_dir) {
            return Err(format!(
                "--dev-memory-secrets refused: config dir {} is not under the temp dir or a `work` directory",
                config_dir.display()
            ));
        }
        let selftest_report = src.arg("--selftest-report").map(|d| absolute(&cwd, &d));
        if selftest_report.is_some() && !dev_memory_secrets {
            // The report carries the launch token: test runs only.
            return Err("--selftest-report needs --dev-memory-secrets".into());
        }
        Ok(Options {
            config_dir,
            port,
            network,
            frontend_dir,
            dev_memory_secrets,
            selftest_report,
            hidden: src.flag("--hidden"),
        })
    }
}

/// Whether a config dir is a scratch location (system temp dir, or under a
/// directory named `work`): musaic-api's rule for
/// `--verification-memory-secrets`, shared.
pub use musaic_api::verification::scratch_dir_ok;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Fake {
        args: Vec<String>,
        env: HashMap<String, String>,
    }

    impl Source for Fake {
        fn arg(&self, name: &str) -> Option<String> {
            arg_in(self.args.clone(), name)
        }
        fn flag(&self, name: &str) -> bool {
            self.args.iter().any(|a| a == name)
        }
        fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn cwd(&self) -> PathBuf {
            std::env::temp_dir()
        }
    }

    fn parse(args: &[&str], env: &[(&str, &str)]) -> Result<Options, String> {
        Options::parse(&Fake {
            args: args.iter().map(|s| s.to_string()).collect(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        })
    }

    fn tmp_cfg() -> String {
        std::env::temp_dir()
            .join("musaic-desktop-opts")
            .display()
            .to_string()
    }

    // Ephemeral port by default; MUSAIC_PORT / --port honoured.
    #[test]
    fn port_defaults_to_ephemeral_and_honours_flag_and_env() {
        let c = tmp_cfg();
        assert_eq!(parse(&["--config-dir", &c], &[]).unwrap().port, 0);
        assert_eq!(
            parse(&["--config-dir", &c], &[("MUSAIC_PORT", "9001")])
                .unwrap()
                .port,
            9001
        );
        assert_eq!(
            parse(
                &["--config-dir", &c, "--port=9002"],
                &[("MUSAIC_PORT", "9001")]
            )
            .unwrap()
            .port,
            9002
        );
        assert!(parse(&["--config-dir", &c, "--port", "x"], &[]).is_err());
    }

    // Live by default; offline / replay selectable for tests.
    #[test]
    fn network_modes() {
        let c = tmp_cfg();
        assert_eq!(
            parse(&["--config-dir", &c], &[]).unwrap().network,
            NetworkMode::Live
        );
        assert_eq!(
            parse(&["--config-dir", &c, "--network", "offline"], &[])
                .unwrap()
                .network,
            NetworkMode::Offline
        );
        assert!(matches!(
            parse(&["--config-dir", &c, "--replay-dir", "/r"], &[])
                .unwrap()
                .network,
            NetworkMode::Replay(_)
        ));
        assert!(parse(&["--config-dir", &c, "--network", "replay"], &[]).is_err());
        assert!(
            parse(
                &[
                    "--config-dir",
                    &c,
                    "--network",
                    "offline",
                    "--replay-dir",
                    "/r"
                ],
                &[]
            )
            .is_err()
        );
        assert!(parse(&["--config-dir", &c, "--network", "record"], &[]).is_err());
    }

    // Dev secrets only in a scratch config dir; never in a home dir.
    #[test]
    fn memory_secrets_need_a_scratch_config_dir() {
        let c = tmp_cfg();
        assert!(
            parse(&["--config-dir", &c, "--dev-memory-secrets"], &[])
                .unwrap()
                .dev_memory_secrets
        );
        let home_like = "/Users/nobody-musaic-test/.musaic";
        assert!(parse(&["--config-dir", home_like, "--dev-memory-secrets"], &[]).is_err());
        // The selftest report carries the launch token: dev runs only.
        assert!(parse(&["--config-dir", &c, "--selftest-report", "r.jsonl"], &[]).is_err());
    }
}
