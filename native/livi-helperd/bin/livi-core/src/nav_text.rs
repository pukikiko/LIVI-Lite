//! LIVI's one guidance dictionary, the UI and the car bridge both show what core writes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    De,
    Fr,
    Ua,
}

impl Language {
    pub fn of(setting: &str) -> Self {
        match setting {
            "de" => Self::De,
            "fr" => Self::Fr,
            "ua" | "uk" | "uk-UA" => Self::Ua,
            _ => Self::En,
        }
    }

    fn words(self) -> &'static Words {
        match self {
            Self::En => &EN,
            Self::De => &DE,
            Self::Fr => &FR,
            Self::Ua => &UA,
        }
    }
}

struct Words {
    unknown: &'static str,
    no_route: &'static str,
    roundabout_exit: &'static str,
    /// By the phone's maneuver code. 28 to 46 are the roundabout exits 1 to 19.
    maneuvers: [&'static str; 54],
}

const ROUNDABOUT_EXITS: std::ops::RangeInclusive<u32> = 28..=46;

pub fn maneuver(code: u32, language: Language) -> Option<String> {
    let words = language.words();
    if ROUNDABOUT_EXITS.contains(&code) {
        return Some(format!("{} {}", words.roundabout_exit, code - ROUNDABOUT_EXITS.start() + 1));
    }
    words.maneuvers.get(code as usize).map(|text| (*text).to_string())
}

pub fn unknown_maneuver(language: Language) -> &'static str {
    language.words().unknown
}

pub fn no_route(language: Language) -> &'static str {
    language.words().no_route
}

pub fn distance(meters: f64) -> Option<String> {
    if !meters.is_finite() || meters < 0.0 {
        return None;
    }
    if meters < 1000.0 {
        return Some(format!("{} m", meters.round()));
    }
    let km = meters / 1000.0;
    if km < 10.0 {
        Some(format!("{} km", one_decimal(km)))
    } else {
        Some(format!("{} km", km.round()))
    }
}

fn one_decimal(v: f64) -> String {
    let exact = format!("{v:.30}");
    let (whole, fraction) = exact.split_once('.').unwrap_or((&exact, "0"));
    let mut tenths: u64 = whole.parse::<u64>().unwrap_or(0) * 10;
    let mut digits = fraction.bytes();
    tenths += u64::from(digits.next().unwrap_or(b'0') - b'0');
    if digits.next().unwrap_or(b'0') >= b'5' {
        tenths += 1;
    }
    format!("{}.{}", tenths / 10, tenths % 10)
}

pub fn time_left(seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    let total = seconds.floor() as u64;
    let (hours, minutes) = (total / 3600, total % 3600 / 60);
    if hours > 0 { Some(format!("{hours}:{minutes:02} h")) } else { Some(format!("{minutes} min")) }
}

const EN: Words = Words {
    unknown: "Unknown",
    no_route: "No Route",
    roundabout_exit: "Roundabout exit",
    maneuvers: [
        "No turn",
        "Turn left",
        "Turn right",
        "Go straight",
        "Make a U-turn",
        "Continue on the current road",
        "Enter roundabout",
        "Exit roundabout",
        "Exit highway",
        "Merge onto highway",
        "End of navigation",
        "Proceed to the route",
        "Arrived",
        "Keep left",
        "Keep right",
        "Enter ferry",
        "Exit ferry",
        "Change ferry",
        "Make a U-turn to rejoin the route",
        "Use the roundabout to make a U-turn",
        "At the end of the road, turn left",
        "At the end of the road, turn right",
        "Exit highway on the left",
        "Exit highway on the right",
        "Arrived (left)",
        "Arrived (right)",
        "Make a U-turn when possible",
        "End of directions",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "Sharp left",
        "Sharp right",
        "Slight left",
        "Slight right",
        "Change highway",
        "Change highway (left)",
        "Change highway (right)",
    ],
};

const DE: Words = Words {
    unknown: "Unbekannt",
    no_route: "Zielführung aus",
    roundabout_exit: "Ausfahrt",
    maneuvers: [
        "Keine Abbiegung",
        "Links abbiegen",
        "Rechts abbiegen",
        "Geradeaus",
        "Wenden",
        "Der Straße folgen",
        "In den Kreisverkehr einfahren",
        "Kreisverkehr verlassen",
        "Autobahn verlassen",
        "Auf die Autobahn auffahren",
        "Navigation beendet",
        "Zur Route fahren",
        "Angekommen",
        "Links halten",
        "Rechts halten",
        "Auf die Fähre fahren",
        "Fähre verlassen",
        "Fähre wechseln",
        "Wenden und zur Route zurück",
        "Im Kreisverkehr wenden",
        "Am Ende der Straße links abbiegen",
        "Am Ende der Straße rechts abbiegen",
        "Autobahn links verlassen",
        "Autobahn rechts verlassen",
        "Angekommen (links)",
        "Angekommen (rechts)",
        "Bei Gelegenheit wenden",
        "Zielführung beendet",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "Scharf links",
        "Scharf rechts",
        "Leicht links",
        "Leicht rechts",
        "Autobahnwechsel",
        "Autobahnwechsel (links)",
        "Autobahnwechsel (rechts)",
    ],
};

const FR: Words = Words {
    unknown: "Inconnu",
    no_route: "Aucun itinéraire",
    roundabout_exit: "Sortie",
    maneuvers: [
        "Pas de virage",
        "Tournez à gauche",
        "Tournez à droite",
        "Continuez tout droit",
        "Faites demi-tour",
        "Continuez sur la route actuelle",
        "Entrez dans le rond-point",
        "Sortez du rond-point",
        "Quittez l'autoroute",
        "Rejoignez l'autoroute",
        "Fin de la navigation",
        "Rejoignez l'itinéraire",
        "Arrivé",
        "Restez à gauche",
        "Restez à droite",
        "Embarquez sur le ferry",
        "Quittez le ferry",
        "Changez de ferry",
        "Faites demi-tour pour rejoindre l'itinéraire",
        "Faites demi-tour au rond-point",
        "Au bout de la route, tournez à gauche",
        "Au bout de la route, tournez à droite",
        "Quittez l'autoroute à gauche",
        "Quittez l'autoroute à droite",
        "Arrivé (à gauche)",
        "Arrivé (à droite)",
        "Faites demi-tour dès que possible",
        "Fin du guidage",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "Virage serré à gauche",
        "Virage serré à droite",
        "Légèrement à gauche",
        "Légèrement à droite",
        "Changement d'autoroute",
        "Changement d'autoroute (à gauche)",
        "Changement d'autoroute (à droite)",
    ],
};

const UA: Words = Words {
    unknown: "Невідомо",
    no_route: "Немає маршруту",
    roundabout_exit: "З’їзд",
    maneuvers: [
        "Без повороту",
        "Поверніть ліворуч",
        "Поверніть праворуч",
        "Рухайтесь прямо",
        "Розворот",
        "Продовжуйте цією дорогою",
        "В'їдьте на кільце",
        "З'їдьте з кільця",
        "З'їзд з автомагістралі",
        "В'їзд на автомагістраль",
        "Навігацію завершено",
        "Прямуйте до маршруту",
        "Прибули",
        "Тримайтесь ліворуч",
        "Тримайтесь праворуч",
        "Заїдьте на пором",
        "З'їдьте з порома",
        "Змініть пором",
        "Розворот, щоб повернутись на маршрут",
        "Виконайте розворот через кільце",
        "В кінці дороги поверніть ліворуч",
        "В кінці дороги поверніть праворуч",
        "З'їзд з автомагістралі ліворуч",
        "З'їзд з автомагістралі праворуч",
        "Прибули (ліворуч)",
        "Прибули (праворуч)",
        "Розверніться, коли буде можливо",
        "Маршрут завершено",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "Різко ліворуч",
        "Різко праворуч",
        "Плавно ліворуч",
        "Плавно праворуч",
        "Зміна автомагістралі",
        "Зміна автомагістралі (ліворуч)",
        "Зміна автомагістралі (праворуч)",
    ],
};

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn vectors() -> Value {
        serde_json::from_str(include_str!("nav_text_vectors.json")).unwrap()
    }

    #[test]
    fn every_maneuver_reads_as_the_old_dictionary_had_it() {
        let v = vectors();
        for (key, language) in
            [("en", Language::En), ("de", Language::De), ("fr", Language::Fr), ("ua", Language::Ua)]
        {
            for (code, text) in v[key]["maneuvers"].as_object().unwrap() {
                let code: i64 = code.parse().unwrap();
                let ours = u32::try_from(code)
                    .ok()
                    .and_then(|c| maneuver(c, language))
                    .unwrap_or_else(|| unknown_maneuver(language).to_string());
                assert_eq!(ours, text.as_str().unwrap(), "{key} {code}");
            }
            assert_eq!(unknown_maneuver(language), v[key]["none"]);
        }
    }

    #[test]
    fn distances_and_times_read_as_before() {
        let v = vectors();
        for (meters, text) in v["meters"].as_object().unwrap() {
            assert_eq!(distance(meters.parse().unwrap()).as_deref(), text.as_str(), "{meters} m");
        }
        for (seconds, text) in v["seconds"].as_object().unwrap() {
            assert_eq!(
                time_left(seconds.parse().unwrap()).as_deref(),
                text.as_str(),
                "{seconds} s"
            );
        }
        assert_eq!(distance(1250.0).as_deref(), Some("1.3 km"));
        assert_eq!(distance(f64::NAN), None);
    }

    #[test]
    fn the_language_setting_picks_the_words() {
        assert_eq!(Language::of("uk-UA"), Language::Ua);
        assert_eq!(Language::of("fr"), Language::Fr);
        assert_eq!(Language::of("xx"), Language::En);
        assert_eq!(no_route(Language::De), "Zielführung aus");
        assert_eq!(maneuver(30, Language::De).as_deref(), Some("Ausfahrt 3"));
        assert_eq!(maneuver(54, Language::En), None);
    }
}
