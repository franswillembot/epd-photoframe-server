use serde::Deserialize;

/// Snapshot of sensor readings forwarded by the device, captured per request.
/// Each field is `Option` because the device may report any subset of these on
/// any given request. Some fields are currently only forwarded to MQTT, while
/// overlays consume the values they need.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SensorState {
    pub battery_mv: Option<u32>,
    pub battery_pct: Option<u8>,
    pub temperature_c: Option<f32>,
    pub humidity_pct: Option<f32>,
    pub power: Option<PowerState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    Battery,
    Charging,
    Full,
    Fault,
}

impl PowerState {
    /// Every variant, in declaration order. Used by MQTT discovery to
    /// advertise the permitted enum values.
    pub const ALL: [Self; 4] = [Self::Battery, Self::Charging, Self::Full, Self::Fault];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Battery => "battery",
            Self::Charging => "charging",
            Self::Full => "full",
            Self::Fault => "fault",
        }
    }
}

impl std::fmt::Display for PowerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
