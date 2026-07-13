use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(alias = "retention-days", deserialize_with = "deserialize_retention")]
    pub retention: String,
    #[serde(default = "default_fuzzy")]
    pub fuzzy: bool,
    #[serde(default = "default_auto_snapshot", alias = "auto-snapshot")]
    pub auto_snapshot: bool,
    #[serde(
        default = "default_auto_snapshot_interval",
        alias = "auto-snapshot-interval"
    )]
    pub auto_snapshot_interval: String,
}

fn default_fuzzy() -> bool {
    true
}

fn default_auto_snapshot() -> bool {
    false
}

fn default_auto_snapshot_interval() -> String {
    "24h".to_string()
}

fn deserialize_retention<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct RetentionVisitor;

    impl<'de> Visitor<'de> for RetentionVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or an integer representing retention period")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value.to_owned())
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(format!("{}d", value))
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value < 0 {
                return Err(de::Error::invalid_value(
                    de::Unexpected::Signed(value),
                    &self,
                ));
            }
            Ok(format!("{}d", value))
        }
    }

    deserializer.deserialize_any(RetentionVisitor)
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            retention: "30d".to_string(),
            fuzzy: true,
            auto_snapshot: false,
            auto_snapshot_interval: "24h".to_string(),
        }
    }
}

pub fn get_config_path() -> Result<PathBuf, String> {
    let mut dir = storage::get_db_dir().map_err(|e| e.to_string())?;
    dir.push("config.toml");
    Ok(dir)
}

pub fn load_config() -> AppConfig {
    let path = match get_config_path() {
        Ok(p) => p,
        Err(_) => return AppConfig::default(),
    };

    if !path.exists() {
        let default_cfg = AppConfig::default();
        let _ = save_config(&default_cfg);
        return default_cfg;
    }

    let builder = config::Config::builder()
        .add_source(config::File::from(path))
        .build();

    match builder {
        Ok(c) => match c.try_deserialize::<AppConfig>() {
            Ok(cfg) => cfg,
            Err(_) => AppConfig::default(),
        },
        Err(_) => AppConfig::default(),
    }
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = get_config_path()?;
    let toml_str = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(path, toml_str).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn parse_any_duration(s: &str) -> Result<chrono::Duration, String> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return Err("Duration string cannot be empty".to_string());
    }

    let mut num_str = String::new();
    let mut unit_str = String::new();

    for c in s.chars() {
        if c.is_digit(10) {
            if !unit_str.is_empty() {
                return Err(format!(
                    "Invalid duration format: digit found after unit in '{}'",
                    s
                ));
            }
            num_str.push(c);
        } else {
            unit_str.push(c);
        }
    }

    if num_str.is_empty() {
        return Err(format!(
            "Invalid duration: missing numeric value in '{}'",
            s
        ));
    }

    let num = num_str
        .parse::<i64>()
        .map_err(|_| format!("Invalid duration number in '{}'", s))?;
    let unit_str = unit_str.trim();

    let duration = match unit_str {
        "s" | "sec" | "second" | "seconds" => chrono::Duration::seconds(num),
        "min" | "minute" | "minutes" => chrono::Duration::minutes(num),
        "h" | "hr" | "hour" | "hours" => chrono::Duration::hours(num),
        "" | "d" | "day" | "days" => chrono::Duration::days(num),
        "w" | "week" | "weeks" => chrono::Duration::weeks(num),
        "m" | "mo" | "month" | "months" => chrono::Duration::days(num * 30),
        "y" | "year" | "years" => chrono::Duration::days(num * 365),
        _ => {
            return Err(format!(
                "Unknown duration unit: '{}' (supported: s, min, h, d, w, m, y)",
                unit_str
            ))
        }
    };

    if duration.num_seconds() <= 0 {
        return Err("Duration must be positive.".to_string());
    }

    Ok(duration)
}

pub fn parse_duration(s: &str) -> Result<chrono::Duration, String> {
    let duration = parse_any_duration(s)?;
    let min_dur = chrono::Duration::hours(1);
    let max_dur = chrono::Duration::days(3650);

    if duration < min_dur || duration > max_dur {
        return Err("Retention duration must be between 1 hour and 3650 days.".to_string());
    }

    Ok(duration)
}
