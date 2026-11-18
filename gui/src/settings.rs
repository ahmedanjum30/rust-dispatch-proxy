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
    /// Connections switched off in the list (adapter names, e.g. "Wi-Fi").
    pub excluded: Vec<String>,
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
            excluded: Vec::new(),
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

/// The proxy's -exclude-file: the switched-off connections, which the
/// running proxy re-reads every second.
pub fn exclude_path() -> PathBuf {
    data_dir().join("excluded-connections.txt")
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
        std::fs::write(settings_path(), json)?;
        self.write_exclude_file()
    }

    /// Writes the switched-off connections where the proxy looks for them.
    pub fn write_exclude_file(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir())?;
        let mut text = String::from("# Connections switched off in proxy-gui (adapter names)\n");
        for name in &self.excluded {
            text.push_str(name);
            text.push('\n');
        }
        // Only when it changed, so the proxy doesn't log a change that isn't one.
        if std::fs::read_to_string(exclude_path()).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(exclude_path(), text)?;
        }
        Ok(())
    }

    /// Whether the connection on this adapter is switched on.
    pub fn uses(&self, adapter: &str) -> bool {
        !self.excluded.iter().any(|e| e.eq_ignore_ascii_case(adapter))
    }

    /// Switches the connection on this adapter on or off.
    pub fn set_used(&mut self, adapter: &str, on: bool) {
        self.excluded.retain(|e| !e.eq_ignore_ascii_case(adapter));
        if !on {
            self.excluded.push(adapter.to_string());
        }
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
        args.extend(["-exclude-file".into(), exclude_path().to_string_lossy().into_owned()]);
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
        assert_eq!(args[8], "-exclude-file");
        assert!(args[9].ends_with("excluded-connections.txt"));
        assert_eq!(args[10], "-log");
    }

    #[test]
    fn switching_connections_on_and_off() {
        let mut s = Settings::default();
        assert!(s.uses("Wi-Fi"));
        s.set_used("Wi-Fi", false);
        s.set_used("Wi-Fi", false); // listed once
        assert!(!s.uses("wi-fi"));
        assert_eq!(s.excluded, ["Wi-Fi"]);
        s.set_used("WI-FI", true);
        assert!(s.uses("Wi-Fi"));
        assert!(s.excluded.is_empty());
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
