use std::collections::HashMap;

const EN: &str = include_str!("../../desktop/src/locales/en.ts");
const TR: &str = include_str!("../../desktop/src/locales/tr.ts");
const ES: &str = include_str!("../../desktop/src/locales/es.ts");
const FR: &str = include_str!("../../desktop/src/locales/fr.ts");
const DE: &str = include_str!("../../desktop/src/locales/de.ts");
const RU: &str = include_str!("../../desktop/src/locales/ru.ts");
const AR: &str = include_str!("../../desktop/src/locales/ar.ts");
const ZH: &str = include_str!("../../desktop/src/locales/zh.ts");
const JA: &str = include_str!("../../desktop/src/locales/ja.ts");
const KO: &str = include_str!("../../desktop/src/locales/ko.ts");

pub fn normalize(raw: &str) -> &'static str {
    match raw
        .to_ascii_lowercase()
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
    {
        "tr" => "tr",
        "es" => "es",
        "fr" => "fr",
        "de" => "de",
        "ru" => "ru",
        "ar" => "ar",
        "zh" => "zh",
        "ja" => "ja",
        "ko" => "ko",
        _ => "en",
    }
}

pub fn selected(setting: Option<&str>) -> &'static str {
    match setting.filter(|value| !value.is_empty() && *value != "system") {
        Some(locale) => normalize(locale),
        None => normalize(&sys_locale::get_locale().unwrap_or_default()),
    }
}

fn source(locale: &str) -> &'static str {
    match normalize(locale) {
        "tr" => TR,
        "es" => ES,
        "fr" => FR,
        "de" => DE,
        "ru" => RU,
        "ar" => AR,
        "zh" => ZH,
        "ja" => JA,
        "ko" => KO,
        _ => EN,
    }
}

fn parse(source: &str) -> HashMap<String, String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with("//") || line.starts_with("export ") || line == "};" {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            let key = key.trim().trim_matches('"');
            let value = value.trim().strip_suffix(',').unwrap_or(value.trim());
            if key.is_empty() || !value.starts_with('"') {
                return None;
            }
            serde_json::from_str::<String>(value)
                .ok()
                .map(|value| (key.to_owned(), value))
        })
        .collect()
}

pub struct Catalog {
    locale: &'static str,
    localized: HashMap<String, String>,
    english: HashMap<String, String>,
}

impl Catalog {
    pub fn new(locale: &str) -> Self {
        let locale = normalize(locale);
        Self {
            locale,
            localized: parse(source(locale)),
            english: parse(EN),
        }
    }

    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.localized
            .get(key)
            .or_else(|| self.english.get(key))
            .map(String::as_str)
            .unwrap_or(key)
    }

    pub fn is_rtl(&self) -> bool {
        self.locale == "ar"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn reuses_the_existing_catalogs() {
        assert_eq!(Catalog::new("tr-TR").get("settings"), "Ayarlar");
        assert_eq!(Catalog::new("en").get("pauseAll"), "Pause all");
    }

    #[test]
    fn every_locale_matches_the_canonical_keys_and_placeholders() {
        let english = parse(EN);
        let english_keys = english.keys().cloned().collect::<HashSet<_>>();
        for code in ["tr", "es", "fr", "de", "ru", "ar", "zh", "ja", "ko"] {
            let localized = parse(source(code));
            assert_eq!(
                localized.keys().cloned().collect::<HashSet<_>>(),
                english_keys,
                "locale {code} must mirror the English catalog"
            );
            for (key, english_value) in &english {
                let localized_value = &localized[key];
                assert_eq!(
                    placeholders(localized_value),
                    placeholders(english_value),
                    "locale {code}, key {key} must preserve placeholders"
                );
            }
        }
    }

    fn placeholders(value: &str) -> Vec<&str> {
        let mut result = Vec::new();
        let mut rest = value;
        while let Some(start) = rest.find('{') {
            rest = &rest[start..];
            let Some(end) = rest.find('}') else { break };
            result.push(&rest[..=end]);
            rest = &rest[end + 1..];
        }
        result.sort_unstable();
        result
    }
}
