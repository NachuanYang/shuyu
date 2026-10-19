use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReaderPreferences {
    pub theme: String,
    pub font_family: String,
    pub font_size_px: f64,
    pub line_height: f64,
    pub content_width_px: f64,
    pub paragraph_spacing_em: f64,
    pub enable_online_discovery: bool,
}

impl Default for ReaderPreferences {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            font_family: "system-serif".into(),
            font_size_px: 18.0,
            line_height: 1.85,
            content_width_px: 720.0,
            paragraph_spacing_em: 0.9,
            enable_online_discovery: false,
        }
    }
}

impl ReaderPreferences {
    pub fn normalized(mut self) -> Self {
        if !matches!(self.theme.as_str(), "paper" | "light" | "dark") {
            self.theme = "dark".into();
        }
        if !matches!(self.font_family.as_str(), "system-serif" | "system-sans") {
            self.font_family = "system-serif".into();
        }
        self.font_size_px = clamp(self.font_size_px, 18.0, 14.0, 28.0, 0);
        self.line_height = clamp(self.line_height, 1.85, 1.4, 2.2, 2);
        self.content_width_px = clamp(self.content_width_px, 720.0, 540.0, 860.0, 0);
        self.paragraph_spacing_em = clamp(self.paragraph_spacing_em, 0.9, 0.4, 1.6, 1);
        self
    }
}

fn clamp(value: f64, fallback: f64, min: f64, max: f64, decimals: i32) -> f64 {
    if !value.is_finite() {
        return fallback;
    }
    let factor = 10_f64.powi(decimals);
    (value.clamp(min, max) * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use super::ReaderPreferences;

    #[test]
    fn default_theme_is_dark_and_online_discovery_starts_off() {
        let prefs = ReaderPreferences::default();
        assert_eq!(prefs.theme, "dark");
        assert_eq!(prefs.font_size_px, 18.0);
        assert!(!prefs.enable_online_discovery);
    }

    #[test]
    fn corrupted_values_fall_back_and_numeric_values_are_clamped() {
        let prefs = ReaderPreferences {
            theme: "neon".into(),
            font_family: "unknown".into(),
            font_size_px: 99.0,
            line_height: f64::NAN,
            content_width_px: 300.0,
            paragraph_spacing_em: 99.0,
            ..ReaderPreferences::default()
        }
        .normalized();

        assert_eq!(prefs.theme, "dark");
        assert_eq!(prefs.font_family, "system-serif");
        assert_eq!(prefs.font_size_px, 28.0);
        assert_eq!(prefs.line_height, 1.85);
        assert_eq!(prefs.content_width_px, 540.0);
        assert_eq!(prefs.paragraph_spacing_em, 1.6);
    }
}
