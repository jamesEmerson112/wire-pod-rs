//! Translation of `pkg/wirepod/localization/localization.go`.

pub const VALID_VOSK_MODELS: [&str; 14] = [
    "en-US", "it-IT", "es-ES", "fr-FR", "de-DE", "pt-BR", "pl-PL", "zh-CN", "tr-TR", "ru-RU",
    "nt-NL", "uk-UA", "vi-VN", "ko-KR",
];

pub const STR_WEATHER_IN: &str = "str_weather_in";
pub const STR_WEATHER_FORECAST: &str = "str_weather_forecast";
pub const STR_WEATHER_TOMORROW: &str = "str_weather_tomorrow";
pub const STR_WEATHER_THE_DAY_AFTER_TOMORROW: &str = "str_weather_the_day_after_tomorrow";
pub const STR_WEATHER_TONIGHT: &str = "str_weather_tonight";
pub const STR_WEATHER_THIS_AFTERNOON: &str = "str_weather_this_afternoon";
pub const STR_EYE_COLOR_PURPLE: &str = "str_eye_color_purple";
pub const STR_EYE_COLOR_BLUE: &str = "str_eye_color_blue";
pub const STR_EYE_COLOR_SAPPHIRE: &str = "str_eye_color_sapphire";
pub const STR_EYE_COLOR_YELLOW: &str = "str_eye_color_yellow";
pub const STR_EYE_COLOR_TEAL: &str = "str_eye_color_teal";
pub const STR_EYE_COLOR_TEAL2: &str = "str_eye_color_teal2";
pub const STR_EYE_COLOR_GREEN: &str = "str_eye_color_green";
pub const STR_EYE_COLOR_ORANGE: &str = "str_eye_color_orange";
pub const STR_ME: &str = "str_me";
pub const STR_SELF: &str = "str_self";
pub const STR_VOLUME_LOW: &str = "str_volume_low";
pub const STR_VOLUME_QUIET: &str = "str_volume_quiet";
pub const STR_VOLUME_MEDIUM_LOW: &str = "str_volume_medium_low";
pub const STR_VOLUME_MEDIUM: &str = "str_volume_medium";
pub const STR_VOLUME_NORMAL: &str = "str_volume_normal";
pub const STR_VOLUME_REGULAR: &str = "str_volume_regular";
pub const STR_VOLUME_MEDIUM_HIGH: &str = "str_volume_medium_high";
pub const STR_VOLUME_HIGH: &str = "str_volume_high";
pub const STR_VOLUME_LOUD: &str = "str_volume_loud";
pub const STR_VOLUME_MUTE: &str = "str_volume_mute";
pub const STR_VOLUME_NOTHING: &str = "str_volume_nothing";
pub const STR_VOLUME_SILENT: &str = "str_volume_silent";
pub const STR_VOLUME_OFF: &str = "str_volume_off";
pub const STR_VOLUME_ZERO: &str = "str_volume_zero";
pub const STR_NAME_IS: &str = "str_name_is";
pub const STR_NAME_IS2: &str = "str_name_is1";
pub const STR_NAME_IS3: &str = "str_name_is2";
pub const STR_FOR: &str = "str_for";
pub const STR_ZERO: &str = "str_zero";
pub const STR_ONE: &str = "str_one";
pub const STR_TWO: &str = "str_two";
pub const STR_THREE: &str = "str_three";
pub const STR_FOUR: &str = "str_four";
pub const STR_FIVE: &str = "str_five";
pub const STR_SIX: &str = "str_six";
pub const STR_SEVEN: &str = "str_seven";
pub const STR_EIGHT: &str = "str_eight";
pub const STR_NINE: &str = "str_nine";
pub const STR_TEN: &str = "str_ten";
pub const STR_ELEVEN: &str = "str_eleven";
pub const STR_TWELVE: &str = "str_twelve";
pub const STR_THIRTEEN: &str = "str_thirteen";
pub const STR_FOURTEEN: &str = "str_fourteen";
pub const STR_FIFTEEN: &str = "str_fifteen";
pub const STR_SIXTEEN: &str = "str_sixteen";
pub const STR_SEVENTEEN: &str = "str_seventeen";
pub const STR_EIGHTEEN: &str = "str_eighteen";
pub const STR_NINETEEN: &str = "str_nineteen";
pub const STR_TWENTY: &str = "str_twenty";
pub const STR_THIRTY: &str = "str_thirty";
pub const STR_FOURTY: &str = "str_fourty";
pub const STR_FIFTY: &str = "str_fifty";
pub const STR_SIXTY: &str = "str_sixty";
pub const STR_SEVENTY: &str = "str_seventy";
pub const STR_EIGHTY: &str = "str_eighty";
pub const STR_NINETY: &str = "str_ninety";
pub const STR_ONE_HUNDRED: &str = "str_one_hundred";
pub const STR_ONE_HOUR: &str = "str_one_hour";
pub const STR_ONE_HOUR_ALT: &str = "str_one_hour_alt";
pub const STR_HOUR: &str = "str_hour";
pub const STR_MINUTE: &str = "str_minute";
pub const STR_SECOND: &str = "str_second";

// for grammer
pub const ALL_STR: [&str; 68] = [
    "str_weather_in",
    "str_weather_forecast",
    "str_weather_tomorrow",
    "str_weather_the_day_after_tomorrow",
    "str_weather_tonight",
    "str_weather_this_afternoon",
    "str_eye_color_purple",
    "str_eye_color_blue",
    "str_eye_color_sapphire",
    "str_eye_color_yellow",
    "str_eye_color_teal",
    "str_eye_color_teal2",
    "str_eye_color_green",
    "str_eye_color_orange",
    "str_me",
    "str_self",
    "str_volume_low",
    "str_volume_quiet",
    "str_volume_medium_low",
    "str_volume_medium",
    "str_volume_normal",
    "str_volume_regular",
    "str_volume_medium_high",
    "str_volume_high",
    "str_volume_loud",
    "str_volume_mute",
    "str_volume_nothing",
    "str_volume_silent",
    "str_volume_off",
    "str_volume_zero",
    "str_name_is",
    "str_name_is1",
    "str_name_is2",
    "str_for",
    "str_zero",
    "str_one",
    "str_two",
    "str_three",
    "str_four",
    "str_five",
    "str_six",
    "str_seven",
    "str_eight",
    "str_nine",
    "str_ten",
    "str_eleven",
    "str_twelve",
    "str_thirteen",
    "str_fourteen",
    "str_fifteen",
    "str_sixteen",
    "str_seventeen",
    "str_eighteen",
    "str_nineteen",
    "str_twenty",
    "str_thirty",
    "str_fourty",
    "str_fifty",
    "str_sixty",
    "str_seventy",
    "str_eighty",
    "str_ninety",
    "str_one_hundred",
    "str_one_hour",
    "str_one_hour_alt",
    "str_hour",
    "str_minute",
    "str_second",
];

// All text must be lowercase!
// Columns: en-US it-IT es-ES fr-FR de-DE pl-PL zh-CN tr-TR ru-RU nt-NL uk-UA vi-VN ko-KR
#[rustfmt::skip]
static TEXTS: &[(&str, &[&str])] = &[
    (STR_WEATHER_IN, &[" in ", " a ", " en ", " en ", " in ", " w ", " 的 ", " içinde ", " в ", " in ", " в ", " ở ", "의 "]),
    (STR_WEATHER_FORECAST, &["forecast", "previsioni", "pronóstico", "prévisions", "wettervorhersage", "prognoza", "预报", "tahmin", "прогноз", "voorspelling", "прогноз", "dự báo", "일기 예보"]),
    (STR_WEATHER_TOMORROW, &["tomorrow", "domani", "mañana", "demain", "morgen", "jutro", "明天", "yarın", "завтра", "morgen", "завтра", "ngày mai", "내일"]),
    (STR_WEATHER_THE_DAY_AFTER_TOMORROW, &["day after tomorrow", "dopodomani", "el día después de mañana", "lendemain de demain", "am tag nach morgen", "pojutrze", "后天", "yarından sonra", "послезавтра", "overmorgen", "післязавтра", "ngày mốt", "모레"]),
    (STR_WEATHER_TONIGHT, &["tonight", "stasera", "esta noche", "ce soir", "heute abend", "dziś wieczorem", "今晚", "bu gece", "сегодня вечером", "vanavond", "сьогодні ввечері", "tối nay", "오늘 밤"]),
    (STR_WEATHER_THIS_AFTERNOON, &["afternoon", "pomeriggio", "esta tarde", "après-midi", "heute nachmittag", "popołudniu", "下午", "bu öğleden sonra", "после полудня", "middag", "після полудня", "chiều nay", "오후"]),
    (STR_EYE_COLOR_PURPLE, &["purple", "lilla", "violeta", "violet", "violett", "fioletowy", "紫色", "mor", "фиолетовый", "paars", "фіолетовий", "màu tím", "보라색"]),
    (STR_EYE_COLOR_BLUE, &["blue", "blu", "azul", "bleu", "blau", "niebieski", "蓝色", "mavi", "голубой", "blauw", "голубий", "màu xanh", "파란색"]),
    (STR_EYE_COLOR_SAPPHIRE, &["sapphire", "zaffiro", "zafiro", "saphir", "saphir", "szafir", "天蓝", "safir", "синий", "saffier", "синій", "màu ngọc bích", "사파이어색"]),
    (STR_EYE_COLOR_YELLOW, &["yellow", "giallo", "amarillo", "jaune", "gelb", "żółty", "黄色", "sarı", "жёлтый", "geel", "жовтий", "màu vàng", "노란색"]),
    (STR_EYE_COLOR_TEAL, &["teal", "verde acqua", "verde azulado", "sarcelle", "blaugrün", "morski", "浅绿", "teal", "бирюзовый", "wintertaling", "бірюзовий", "xanh lá cây", "청록색"]),
    (STR_EYE_COLOR_TEAL2, &["tell", "acquamarina", "aguamarina", "acquamarine", "acquamarina", "akwamaryn", "蓝绿", "turkuaz", "аквамарин", "vertellen", "аквамариновий", "màu xanh ngọc", "아쿠아마린색"]),
    (STR_EYE_COLOR_GREEN, &["green", "verde", "verde", "vert", "grün", "zielony", "绿色", "yeşil", "зелёный", "groente", "зелений", "màu xanh lá", "초록색"]),
    (STR_EYE_COLOR_ORANGE, &["orange", "arancio", "naranja", "orange", "orange", "pomarańczowy", "橙色", "turuncu", "оранжевый", "oranje", "оранжевий", "màu cam", "주황색"]),
    (STR_ME, &["me", "me", "me", "moi", "mir", "mnie", "我", "ben", "меня", "mij", "мене", "tôi", "나", "내"]),
    (STR_SELF, &["self", "mi", "mía", "moi", "mein", "ja", "自己", "kendim", "себя", "zelf", "себе", "bản thân", "본인", "자신"]),
    (STR_VOLUME_LOW, &["low", "minimo", "bajo", "bas", "niedrig", "niski", "低", "düşük", "низкий", "laag", "на мінімум", "thấp", "아주 작게"]),
    (STR_VOLUME_QUIET, &["quiet", "basso", "tranquilo", "silencieux", "ruhig", "cichy", "安静", "sessiz", "тихо", "rustig", "тихо", "yên tĩnh", "작게"]),
    (STR_VOLUME_MEDIUM_LOW, &["medium low", "medio basso", "medio-bajo", "moyen bas", "mittelschwer", "średnio niski", "中低", "orta düşük", "ниже среднего", "middel laag", "нижче середнього", "vừa thấp", "조금 작게"]),
    (STR_VOLUME_MEDIUM, &["medium", "medio", "medio", "moyen", "mittel", "średni", "中档", "orta", "средний", "medium", "середню", "vừa", "중간"]),
    (STR_VOLUME_NORMAL, &["normal", "normale", "normal", "normal", "normal", "normalny", "正常", "normal", "нормальный", "normaal", "нормальна", "bình thường", "보통"]),
    (STR_VOLUME_REGULAR, &["regular", "regolare", "regular", "standard", "regulär", "zwyczajny", "标准", "düzenli", "обычный", "normaal", "звичайна", "thông thường", "보통"]),
    (STR_VOLUME_MEDIUM_HIGH, &["medium high", "medio alto", "medio-alto", "moyen-élevé", "mittelhoch", "średno wysoki", "中高", "orta yüksek", "выше среднего", "gemiddeld hoog", "вище середнього", "vừa cao", "조금 크게"]),
    (STR_VOLUME_HIGH, &["high", "alto", "alto", "élevé", "hoch", "wysoki", "高档", "yüksek", "высокий", "hoog", "висока", "cao", "크게"]),
    (STR_VOLUME_LOUD, &["loud", "massimo", "fuerte", "fort", "laut", "głośny", "高", "gürültülü", "громкий", "luidruchtig", "гучний", "to", "아주 크게"]),
    (STR_VOLUME_MUTE, &["mute", "muto", "mudo", "muet", "stumm", "wyciszony", "静音", "sessiz", "немой", "stom", "німий", "im lặng", "음소거"]),
    (STR_VOLUME_NOTHING, &["nothing", "nessuno", "nada", "rien", "nichts", "nic", "无声", "hiçbir şey", "", "Niets", "нічого", "không có gì", "음소거"]),
    (STR_VOLUME_SILENT, &["silent", "silenzioso", "silencio", "silencieux", "still", "cichy", "悄声", "sessiz", "тихий", "stil", "тихий", "yên lặng", "조용"]),
    (STR_VOLUME_OFF, &["off", "spento", "apagado", "éteindre", "aus", "wyłączony", "关闭", "kapalı", "выключить", "uit", "вимкнути", "tắt", "꺼"]),
    (STR_VOLUME_ZERO, &["zero", "zero", "cero", "zéro", "null", "zero", "零", "sıfır", "ноль", "nul", "нуль", "không", "영"]),
    (STR_NAME_IS, &[" is ", " è ", " es ", " est ", " ist ", " to ", "到", " olan ", "", " is ", "", " là ", "은 "]),
    (STR_NAME_IS2, &["'s", "sono ", "soy ", "suis ", "bin ", " się ", "的", "'nin", "", "", "", "của", "의 "]),
    (STR_NAME_IS3, &["names", " chiamo ", " llamo ", "appelle ", "werde", "imię", "名字", "adlar", "имена", "namen", "імена", "tên", "이름은"]),
    (STR_FOR, &[" for ", " per ", " para ", " pour ", " für ", " dla ", "给", " için ", "для", " voor ", " для ", " cho ", " 위해 "]),
    (STR_ZERO, &["zero", "zero", "zero", "zéro", "zero", "zero", "zero", "zero", "zero", "zero", "zero", "zero", "영"]),
    (STR_ONE, &["one", "uno", "one", "un", "one", "one", "one", "one", "one", "one", "one", "one", "일"]),
    (STR_TWO, &["two", "due", "two", "deux", "two", "two", "two", "two", "two", "two", "two", "two", "이"]),
    (STR_THREE, &["three", "tre", "three", "trois", "three", "three", "three", "three", "three", "three", "three", "three", "삼"]),
    (STR_FOUR, &["four", "quattro", "four", "quatre", "four", "four", "four", "four", "four", "four", "four", "four", "사"]),
    (STR_FIVE, &["five", "cinque", "five", "cinq", "five", "five", "five", "five", "five", "five", "five", "five", "오"]),
    (STR_SIX, &["six", "sei", "six", "six", "six", "six", "six", "six", "six", "six", "six", "six", "육"]),
    (STR_SEVEN, &["seven", "sette", "seven", "sept", "seven", "seven", "seven", "seven", "seven", "seven", "seven", "seven", "칠"]),
    (STR_EIGHT, &["eight", "otto", "eight", "huit", "eight", "eight", "eight", "eight", "eight", "eight", "eight", "eight", "팔"]),
    (STR_NINE, &["nine", "nove", "nine", "neuf", "nine", "nine", "nine", "nine", "nine", "nine", "nine", "nine", "구"]),
    (STR_TEN, &["ten", "dieci", "ten", "dix", "ten", "ten", "ten", "ten", "ten", "ten", "ten", "ten", "십"]),
    (STR_ELEVEN, &["eleven", "undici", "eleven", "onze", "eleven", "eleven", "eleven", "eleven", "eleven", "eleven", "eleven", "eleven", "십일"]),
    (STR_TWELVE, &["twelve", "dodici", "twelve", "douze", "twelve", "twelve", "twelve", "twelve", "twelve", "twelve", "twelve", "twelve", "십이"]),
    (STR_THIRTEEN, &["thirteen", "tredici", "thirteen", "treize", "thirteen", "thirteen", "thirteen", "thirteen", "thirteen", "thirteen", "thirteen", "thirteen", "십삼"]),
    (STR_FOURTEEN, &["fourteen", "quattordici", "fourteen", "quatorze", "fourteen", "fourteen", "fourteen", "fourteen", "fourteen", "fourteen", "fourteen", "fourteen", "십사"]),
    (STR_FIFTEEN, &["fifteen", "quindici", "fifteen", "quinze", "fifteen", "fifteen", "fifteen", "fifteen", "fifteen", "fifteen", "fifteen", "fifteen", "십오"]),
    (STR_SIXTEEN, &["sixteen", "sedici", "sixteen", "seize", "sixteen", "sixteen", "sixteen", "sixteen", "sixteen", "sixteen", "sixteen", "sixteen", "십육"]),
    (STR_SEVENTEEN, &["seventeen", "diciassette", "seventeen", "dix-sept", "seventeen", "seventeen", "seventeen", "seventeen", "seventeen", "seventeen", "seventeen", "seventeen", "십칠"]),
    (STR_EIGHTEEN, &["eighteen", "diciotto", "eighteen", "dix-huit", "eighteen", "eighteen", "eighteen", "eighteen", "eighteen", "eighteen", "eighteen", "eighteen", "십팔"]),
    (STR_NINETEEN, &["nineteen", "diciannove", "nineteen", "dix-neuf", "nineteen", "nineteen", "nineteen", "nineteen", "nineteen", "nineteen", "nineteen", "nineteen", "십구"]),
    (STR_TWENTY, &["twenty", "venti", "twenty", "vingt", "twenty", "twenty", "twenty", "twenty", "twenty", "twenty", "twenty", "twenty", "이십"]),
    (STR_THIRTY, &["thirty", "trenta", "thirty", "trente", "thirty", "thirty", "thirty", "thirty", "thirty", "thirty", "thirty", "thirty", "삼십"]),
    (STR_FOURTY, &["fourty", "quaranta", "fourty", "quarante", "fourty", "fourty", "fourty", "fourty", "fourty", "fourty", "fourty", "fourty", "사십"]),
    (STR_FIFTY, &["fifty", "cinquanta", "fifty", "cinquante", "fifty", "fifty", "fifty", "fifty", "fifty", "fifty", "fifty", "fifty", "오십"]),
    (STR_SIXTY, &["sixty", "sessanta", "sixty", "soixante", "sixty", "sixty", "sixty", "sixty", "sixty", "sixty", "sixty", "sixty", "육십"]),
    (STR_SEVENTY, &["seventy", "settantta", "seventy", "soixante-dix", "seventy", "seventy", "seventy", "seventy", "seventy", "seventy", "seventy", "seventy", "칠십"]),
    (STR_EIGHTY, &["eighty", "ottanta", "eighty", "quatre-vingt", "eighty", "eighty", "eighty", "eighty", "eighty", "eighty", "eighty", "eighty", "팔십"]),
    (STR_NINETY, &["ninety", "novanta", "ninety", "quatre vingt dix", "ninety", "ninety", "ninety", "ninety", "ninety", "ninety", "ninety", "ninety", "구십"]),
    (STR_ONE_HUNDRED, &["one hundred", "cento", "one hundred", "cent", "one hundred", "one hundred", "one hundred", "one hundred", "one hundred", "one hundred", "one hundred", "one hundred", "백"]),
    (STR_ONE_HOUR, &["one hour", "un'ora", "one hour", "une heure", "one hour", "one hour", "one hour", "one hour", "one hour", "one hour", "one hour", "one hour", "한 시간"]),
    (STR_ONE_HOUR_ALT, &["an hour", "un ora", "an hour", "une heure", "an hour", "an hour", "an hour", "an hour", "an hour", "an hour", "an hour", "an hour", "한 시간"]),
    (STR_HOUR, &["hour", "ore", "hour", "heure", "hour", "hour", "hour", "hour", "hour", "hour", "hour", "hour", "시간"]),
    (STR_MINUTE, &["minute", "minuti", "minute", "minute", "minute", "minute", "minute", "minute", "minute", "minute", "minute", "minute", "분"]),
    (STR_SECOND, &["second", "secondi", "second", "seconde", "second", "second", "second", "second", "second", "second", "second", "second", "초"]),
];

// Go reads `vars.APIConfig.STT.Language`; here the caller passes the language in.
pub fn get_text(language: &str, key: &str) -> &'static str {
    let Some(data) = TEXTS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v) else {
        // Go indexes a nil slice here and panics.
        return "";
    };
    match language {
        "it-IT" => data[1],
        "es-ES" => data[2],
        "fr-FR" => data[3],
        "de-DE" => data[4],
        "pl-PL" => data[5],
        "zh-CN" => data[6],
        "tr-TR" => data[7],
        "ru-RU" => data[8],
        "nt-NL" => data[9],
        "uk-UA" => data[10],
        "vi-VN" => data[11],
        "ko-KR" => data[12],
        _ => data[0],
    }
}

// TODO(M2): ReloadVosk, which needs vars.LoadIntents and vars.SttInitFunc.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_column_for_every_language() {
        assert_eq!(TEXTS.len(), ALL_STR.len());
        for (key, data) in TEXTS {
            assert!(ALL_STR.contains(key));
            assert!(data.len() >= 13, "{key}");
        }
    }

    #[test]
    fn get_text_picks_the_language_column_and_defaults_to_english() {
        assert_eq!(get_text("en-US", STR_MINUTE), "minute");
        assert_eq!(get_text("it-IT", STR_MINUTE), "minuti");
        assert_eq!(get_text("xx-XX", STR_WEATHER_TOMORROW), "tomorrow");
        assert_eq!(get_text("en-US", "str_missing"), "");
    }
}
