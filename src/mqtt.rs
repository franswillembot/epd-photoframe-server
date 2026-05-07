//! MQTT publisher for per-screen sensor values, with Home Assistant
//! discovery emitted on startup. The eventloop runs in a background tokio
//! task so request handlers never block on broker availability — state
//! publishes use `try_publish` and silently drop if the send buffer is full.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rumqttc::{AsyncClient, MqttOptions, QoS};

use crate::PowerState;
use crate::config::{MqttConfig, Publish, ScreenConfig};
use crate::overlays::SensorState;

pub struct Publisher {
    client: AsyncClient,
    state_prefix: String,
}

#[derive(Clone)]
struct Sensor {
    /// Topic suffix and discovery `object_id`.
    key: &'static str,
    /// Human-readable name shown in Home Assistant.
    name: &'static str,
    /// Home Assistant `device_class`. For `enum` sensors this is the literal
    /// `"enum"`.
    device_class: &'static str,
    /// Numeric sensors set this to `Some(unit)`; the Home Assistant discovery
    /// payload then also gets `state_class: "measurement"`.
    unit: Option<&'static str>,
    /// Enum sensors (`device_class = "enum"`) list their permitted values
    /// here so Home Assistant can validate states and show a chooser.
    options: Option<Vec<String>>,
}

impl Sensor {
    fn battery_pct() -> Self {
        Self {
            key: "battery_pct",
            name: "Battery",
            device_class: "battery",
            unit: Some("%"),
            options: None,
        }
    }
    fn battery_mv() -> Self {
        Self {
            key: "battery_mv",
            name: "Battery voltage",
            device_class: "voltage",
            unit: Some("mV"),
            options: None,
        }
    }
    fn temperature() -> Self {
        Self {
            key: "temperature",
            name: "Temperature",
            device_class: "temperature",
            unit: Some("°C"),
            options: None,
        }
    }
    fn humidity() -> Self {
        Self {
            key: "humidity",
            name: "Humidity",
            device_class: "humidity",
            unit: Some("%"),
            options: None,
        }
    }
    fn power() -> Self {
        Self {
            key: "power",
            name: "Power",
            device_class: "enum",
            unit: None,
            options: Some(
                PowerState::ALL
                    .iter()
                    .map(|p| p.as_str().to_string())
                    .collect(),
            ),
        }
    }
    fn last_seen() -> Self {
        Self {
            key: "last_seen",
            name: "Last seen",
            device_class: "timestamp",
            unit: None,
            options: None,
        }
    }
}

fn enabled_sensors(cfg: &ScreenConfig) -> Vec<Sensor> {
    cfg.publish
        .iter()
        .flat_map(|p| match p {
            Publish::Battery => vec![Sensor::battery_pct(), Sensor::battery_mv()],
            Publish::Temperature => vec![Sensor::temperature()],
            Publish::Humidity => vec![Sensor::humidity()],
            Publish::Power => vec![Sensor::power()],
            Publish::LastSeen => vec![Sensor::last_seen()],
        })
        .collect()
}

/// Home Assistant's discovery `node_id` and entity `unique_id` only allow
/// `[a-z0-9_]`, so any other character in a screen name (e.g. the hyphen in
/// `living-room`) is mapped to `_`. Topics tolerate hyphens, so the
/// state-topic uses the original screen name verbatim.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

impl Publisher {
    /// Connects to the broker, spawns the eventloop in the background, and
    /// publishes a Home Assistant discovery config for every enabled sensor
    /// on every screen. Discovery messages queue up if the broker isn't
    /// reachable yet — they'll be sent once the eventloop connects.
    pub fn connect(cfg: &MqttConfig, screens: &[ScreenConfig]) -> Self {
        let mut opts = MqttOptions::new(&cfg.client_id, &cfg.broker, cfg.port);
        if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
            opts.set_credentials(u, p);
        }
        opts.set_keep_alive(Duration::from_mins(1));

        let (client, mut eventloop) = AsyncClient::new(opts, 256);
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "mqtt eventloop error, sleeping before retry");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });

        let publisher = Self {
            client,
            state_prefix: cfg.state_prefix.clone(),
        };
        for screen in screens {
            for sensor in enabled_sensors(screen) {
                publisher.publish_discovery(cfg, screen, sensor);
            }
        }
        publisher
    }

    fn publish_discovery(&self, cfg: &MqttConfig, screen: &ScreenConfig, sensor: Sensor) {
        let slug = slug(&screen.name);
        let device_name = screen.mqtt_name.as_deref().unwrap_or(&screen.name);
        let topic = format!(
            "{}/sensor/epd_photoframe_{}/{}/config",
            cfg.discovery_prefix, slug, sensor.key
        );
        let mut payload = serde_json::json!({
            "name": sensor.name,
            "unique_id": format!("epd_photoframe_{}_{}", slug, sensor.key),
            "state_topic": format!("{}/{}/{}", self.state_prefix, screen.name, sensor.key),
            "device_class": sensor.device_class,
            "device": {
                "identifiers": [format!("epd_photoframe_{}", slug)],
                "name": device_name,
                "manufacturer": "epd-photoframe-server",
                "model": "ePaper photo frame",
            },
        });
        if let Some(unit) = sensor.unit {
            payload["unit_of_measurement"] = unit.into();
            payload["state_class"] = "measurement".into();
        }
        if let Some(options) = sensor.options {
            payload["options"] = serde_json::json!(options);
        }
        if let Err(e) = self
            .client
            .try_publish(&topic, QoS::AtLeastOnce, true, payload.to_string())
        {
            tracing::warn!(topic = %topic, error = %e, "mqtt discovery publish failed");
        }
    }

    /// Publishes a single state value. Fire-and-forget; logs at warn if the
    /// outbound queue is full.
    pub fn publish(&self, screen: &str, key: &str, value: impl ToString) {
        let topic = format!("{}/{}/{}", self.state_prefix, screen, key);
        if let Err(e) = self
            .client
            .try_publish(&topic, QoS::AtMostOnce, true, value.to_string())
        {
            tracing::warn!(topic = %topic, error = %e, "mqtt state publish failed");
        }
    }

    /// Publishes all enabled sensor readings for one screen. Missing readings
    /// are skipped, except `last_seen`, which is server-side and always
    /// available.
    pub fn publish_screen_state(
        &self,
        screen: &str,
        enabled: &HashSet<Publish>,
        sensors: &SensorState,
        now: DateTime<Utc>,
    ) {
        if enabled.contains(&Publish::Battery) {
            if let Some(v) = sensors.battery_mv {
                self.publish(screen, "battery_mv", v);
            }
            if let Some(v) = sensors.battery_pct {
                self.publish(screen, "battery_pct", v);
            }
        }
        if enabled.contains(&Publish::Temperature)
            && let Some(v) = sensors.temperature_c
        {
            self.publish(screen, "temperature", v);
        }
        if enabled.contains(&Publish::Humidity)
            && let Some(v) = sensors.humidity_pct
        {
            self.publish(screen, "humidity", v);
        }
        if enabled.contains(&Publish::Power)
            && let Some(v) = sensors.power
        {
            self.publish(screen, "power", v);
        }
        if enabled.contains(&Publish::LastSeen) {
            self.publish(screen, "last_seen", now.to_rfc3339());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_lowercases_and_replaces_punctuation() {
        assert_eq!(slug("living-room"), "living_room");
        assert_eq!(slug("E1002-Landscape"), "e1002_landscape");
        assert_eq!(slug("foo.bar"), "foo_bar");
        assert_eq!(slug("plain"), "plain");
    }

    fn screen_with(publish: &str) -> ScreenConfig {
        toml::from_str(&format!(
            r#"
            name = "x"
            width = 800
            height = 480
            share_url = "https://example.com"
            publish = {publish}
            "#
        ))
        .unwrap()
    }

    fn keys(s: &[Sensor]) -> std::collections::BTreeSet<&str> {
        s.iter().map(|s| s.key).collect()
    }

    #[test]
    fn enabled_sensors_battery_includes_both_mv_and_pct() {
        let s = enabled_sensors(&screen_with(r#"["battery"]"#));
        assert_eq!(
            keys(&s),
            ["battery_pct", "battery_mv"].into_iter().collect()
        );
    }

    #[test]
    fn enabled_sensors_empty_publish_produces_nothing() {
        let s = enabled_sensors(&screen_with("[]"));
        assert!(s.is_empty());
    }

    #[test]
    fn enabled_sensors_all_on_produces_six() {
        let s = enabled_sensors(&screen_with(
            r#"["battery", "temperature", "humidity", "power", "last_seen"]"#,
        ));
        assert_eq!(
            keys(&s),
            [
                "battery_pct",
                "battery_mv",
                "temperature",
                "humidity",
                "power",
                "last_seen"
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn duplicate_publish_entries_collapse() {
        let s = enabled_sensors(&screen_with(r#"["battery", "battery", "last_seen"]"#));
        assert_eq!(
            keys(&s),
            ["battery_pct", "battery_mv", "last_seen"]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn power_sensor_is_an_enum_with_four_options() {
        let p = Sensor::power();
        assert_eq!(p.device_class, "enum");
        assert!(p.unit.is_none());
        assert_eq!(
            p.options.as_deref(),
            Some(
                &[
                    "battery".to_string(),
                    "charging".to_string(),
                    "full".to_string(),
                    "fault".to_string()
                ][..]
            )
        );
    }

    #[test]
    fn last_seen_sensor_is_a_timestamp_with_no_unit() {
        let s = Sensor::last_seen();
        assert_eq!(s.device_class, "timestamp");
        assert!(s.unit.is_none());
        assert!(s.options.is_none());
    }
}
