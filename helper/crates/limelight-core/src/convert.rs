//! Unit conversions shared by the daemon and the GUI.

/// Coolest colour temperature an Elgato light accepts.
pub const KELVIN_MAX: u16 = 7000;
/// Warmest colour temperature an Elgato light accepts.
pub const KELVIN_MIN: u16 = 2900;
/// Mired value for `KELVIN_MAX` (the device speaks mired).
pub const MIRED_MIN: u16 = (1_000_000u32 / KELVIN_MAX as u32) as u16;
/// Mired value for `KELVIN_MIN`.
pub const MIRED_MAX: u16 = (1_000_000u32 / KELVIN_MIN as u32) as u16;

pub fn clamp_mired(mired: u16) -> u16 {
    mired.clamp(MIRED_MIN, MIRED_MAX)
}

pub fn kelvin_to_mired(kelvin: u16) -> u16 {
    let clamped = kelvin.clamp(KELVIN_MIN, KELVIN_MAX) as u32;
    clamp_mired(((1_000_000u32 + clamped / 2) / clamped) as u16)
}

pub fn mired_to_kelvin(mired: u16) -> u16 {
    let clamped = clamp_mired(mired) as u32;
    ((1_000_000u32 + clamped / 2) / clamped) as u16
}

/// GUI slider "warmth" 0.0 (cool, 7000 K) .. 1.0 (warm, 2900 K) → kelvin.
pub fn warmth_to_kelvin(warmth: f32) -> u16 {
    let range = (KELVIN_MAX - KELVIN_MIN) as f32;
    (KELVIN_MAX as f32 - warmth.clamp(0.0, 1.0) * range).round() as u16
}

/// Kelvin → GUI slider "warmth" 0.0 .. 1.0.
pub fn kelvin_to_warmth(kelvin: u16) -> f32 {
    let range = (KELVIN_MAX - KELVIN_MIN) as f32;
    ((KELVIN_MAX as f32 - kelvin as f32) / range).clamp(0.0, 1.0)
}

/// GUI slider 0.0 .. 1.0 → device brightness percent 0 .. 100.
pub fn slider_to_brightness(slider: f32) -> u8 {
    (slider.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// Device brightness percent 0 .. 100 → GUI slider 0.0 .. 1.0.
pub fn brightness_to_slider(brightness: u8) -> f32 {
    brightness.min(100) as f32 / 100.0
}
