//! The proxy's settings, kept in %APPDATA%\rust-dispatch-proxy\gui-settings.json.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub port: u16,
    /// -auto: find the connections automatically.
    pub auto: bool,
    /// Used when `auto` is off: "IP" or "IP@weight", separated by spaces.
    pub addresses: String,
    /// -system-proxy: manage Windows' proxy setting.
    pub system_proxy: bool,
    pub min_links: u32,
    /// -race, in milliseconds (0 = off).
    pub race_ms: u64,
    pub verbose: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            port: 8080,
            auto: true,
            addresses: String::new(),
            system_proxy: true,
            min_links: 2,
            race_ms: 300,
            verbose: false,
        }
    }
}

/// %APPDATA%\rust-dispatch-proxy (falls back to the program's folder).
pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(|d| PathBuf::from(d).join("rust-dispatch-proxy"))
        .or_else(|| std::env::current_exe().ok()?.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn log_path() -> PathBuf {
    data_dir().join("proxy.log")
}

fn settings_path() -> PathBuf {
    data_dir().join("gui-settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir())?;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(settings_path(), json)
    }

    /// The proxy's command line for these settings.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec!["-lport".to_string(), self.port.to_string()];
        if self.auto {
            args.push("-auto".into());
        } else {
            args.extend(self.addresses.split([' ', ',', ';', '\n']).filter(|a| !a.is_empty()).map(String::from));
        }
        if self.system_proxy {
            args.extend(["-system-proxy".into(), "-min-links".into(), self.min_links.to_string()]);
        }
        args.extend(["-race".into(), self.race_ms.to_string()]);
        if self.verbose {
            args.push("-verbose".into());
        }
        args.extend(["-log".into(), log_path().to_string_lossy().into_owned()]);
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_command_line() {
        let args = Settings::default().args();
        assert_eq!(
            &args[..8],
            ["-lport", "8080", "-auto", "-system-proxy", "-min-links", "2", "-race", "300"]
        );
        assert_eq!(args[8], "-log");
    }

    #[test]
    fn manual_addresses() {
        let s = Settings {
            auto: false,
            addresses: "192.168.1.10@3, 10.0.0.5  ".into(),
            system_proxy: false,
            ..Settings::default()
        };
        let args = s.args();
        assert_eq!(&args[..4], ["-lport", "8080", "192.168.1.10@3", "10.0.0.5"]);
        assert!(!args.iter().any(|a| a == "-auto" || a == "-system-proxy"));
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"port": 9000}"#).unwrap();
        assert_eq!(s.port, 9000);
        assert!(s.auto);
        assert_eq!(s.race_ms, 300);
    }
}
