//! Детерминированные генераторы реалистичных корпусов ровно заданного размера
//! (по умолчанию 10 МиБ = 10 * 1024 * 1024 байт) для бенчмарка.
//!
//! Корпуса собираются из шаблонов предложений со слотами и частотных списков
//! слов; выбор значений — LCG без внешних зависимостей. Хвост выравнивается
//! по границе символа UTF-8, итоговый размер точно равен целевому.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ru,
    En,
    Es,
    De,
    TrKz,
    ArFa,
    Zh,
    Ja,
    Math,
    Code,
}

pub const KINDS: [Kind; 10] = [
    Kind::Ru,
    Kind::En,
    Kind::Es,
    Kind::De,
    Kind::TrKz,
    Kind::ArFa,
    Kind::Zh,
    Kind::Ja,
    Kind::Math,
    Kind::Code,
];

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Ru => "RU",
            Kind::En => "EN",
            Kind::Es => "ES",
            Kind::De => "DE",
            Kind::TrKz => "TR/KZ",
            Kind::ArFa => "AR/FA",
            Kind::Zh => "ZH",
            Kind::Ja => "JA",
            Kind::Math => "MATH",
            Kind::Code => "CODE",
        }
    }
}

/// Детерминированный LCG (xoshiro-подобный период достаточен для корпусов).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    pub fn pick<'a>(&mut self, list: &[&'a str]) -> &'a str {
        list[(self.next() % list.len() as u64) as usize]
    }
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next() % (hi - lo + 1) as u64) as usize
    }
}

struct LangData {
    words: &'static [&'static str],
    years: &'static [&'static str],
    templates: &'static [&'static str],
    paragraph_sep: &'static str,
}

static RU: LangData = LangData {
    words: &[
        "история", "данные", "система", "сервер", "программа", "время", "работа",
        "город", "страна", "народ", "жизнь", "год", "дело", "вопрос", "пример",
        "задача", "метод", "текст", "код", "файл", "проект", "компьютер",
        "язык", "мир", "наука", "школа", "книга", "письмо", "слово", "мысль",
        "данные.", "система,", "сервер;", "история", "развитие", "информация",
        "экономика", "культура", "образование", "исследование", "технология",
        "советский", "советского", "программирование", "алгоритм", "массив",
        "память", "процессор", "архив", "каталог", "отчёт", "таблица", "график",
    ],
    years: &["1926", "1961", "1991", "2001", "2010", "2026"],
    templates: &[
        "История советского программирования начинается с {}.",
        "В настоящее время данные передаются между серверами системы {}.",
        "Таким образом, развитие {} стало важным направлением.",
        "В том числе рассматривается вопрос о {} и {}.",
        "С помощью алгоритма {} обработка данных ускоряется.",
        "Несмотря на то что {} продолжается, результаты уже видны.",
        "Например, в {y} году объем данных достиг рекордных значений.",
        "Является одним из наиболее важных примеров {}.",
        "В связи с этим исследование {} получило поддержку.",
        "Более того, метод {} применяется в образовании и науке.",
        "Именно поэтому программа {} была принята в {y} году.",
        "Кроме того, система содержит данные о {} и {}.",
        "Как правило, работа с файлами начинается с каталога {}.",
        "В результате тестирование сервера показало: {} работает.",
        "В частности, алгоритм сжатия данных {} превосходит ожидания.",
        "С одной стороны, время работы растет, с другой — {} падает.",
    ],
    paragraph_sep: "\n\n",
};

static EN: LangData = LangData {
    words: &[
        "information", "system", "research", "development", "technology",
        "language", "history", "example", "number", "world", "people",
        "computer", "memory", "storage", "compression", "encoding",
        "dictionary", "algorithm", "problem", "question", "service",
        "business", "results", "science", "student", "university", "company",
        "government", "process", "program", "function", "variable", "value",
        "data", "code", "file", "server", "project", "time", "work", "life",
        "city", "country", "school", "letter", "writing", "thinking",
    ],
    years: &["1949", "1969", "1984", "1997", "2008", "2026"],
    templates: &[
        "The development of {} was an important step forward.",
        "Information systems process data between servers and {}.",
        "This algorithm provides compression of {} in real time.",
        "In other words, the {} follows from the following example.",
        "For example, the dictionary contains {} and {} entries.",
        "As a result, the system reported that {} was complete.",
        "In particular, research on {} received international support.",
        "The history of computing includes {} and its consequences.",
        "According to the report, the number of {} reached a record in {y}.",
        "It is considered one of the most important examples of {}.",
        "Moreover, the technology behind {} has become standard practice.",
        "That is to say, memory usage drops when {} is applied.",
    ],
    paragraph_sep: "\n\n",
};

static ES: LangData = LangData {
    words: &[
        "información", "sistema", "investigación", "desarrollo", "tecnología",
        "lenguaje", "historia", "ejemplo", "número", "mundo", "personas",
        "computadora", "memoria", "compresión", "algoritmo", "problema",
        "pregunta", "servicio", "empresa", "proceso", "programa", "datos",
        "código", "archivo", "servidor", "proyecto", "tiempo", "ciudad",
        "país", "escuela", "cultura", "educación", "ciencia", "trabajo",
    ],
    years: &["1956", "1975", "1992", "2005", "2018", "2026"],
    templates: &[
        "El desarrollo de {} fue un paso importante para la ciencia.",
        "El sistema procesa información entre servidores y {}.",
        "Por ejemplo, el algoritmo comprime datos de {} con éxito.",
        "En otras palabras, la tecnología de {} se vuelve estándar.",
        "Según el informe, el número de {} alcanzó un récord en {y}.",
        "Es considerado uno de los ejemplos más importantes de {}.",
        "Además, la investigación sobre {} recibió apoyo internacional.",
        "Como resultado, el servicio informó que {} está completo.",
    ],
    paragraph_sep: "\n\n",
};

static DE: LangData = LangData {
    words: &[
        "Information", "System", "Forschung", "Entwicklung", "Technologie",
        "Sprache", "Geschichte", "Beispiel", "Zahl", "Welt", "Menschen",
        "Computer", "Speicher", "Kompression", "Algorithmus", "Problem",
        "Frage", "Dienst", "Unternehmen", "Prozess", "Programm", "Daten",
        "Code", "Datei", "Server", "Projekt", "Zeit", "Stadt", "Land",
        "Schule", "Kultur", "Bildung", "Wissenschaft", "Arbeit",
    ],
    years: &["1953", "1972", "1989", "2003", "2016", "2026"],
    templates: &[
        "Die Entwicklung von {} war ein wichtiger Schritt für die Wissenschaft.",
        "Das System verarbeitet Daten zwischen Servern und {}.",
        "Zum Beispiel komprimiert der Algorithmus Daten von {}.",
        "Mit anderen Worten, die Technologie von {} wird zum Standard.",
        "Laut dem Bericht erreichte die Zahl der {} im Jahr {y} einen Rekord.",
        "Es gilt als eines der wichtigsten Beispiele für {}.",
        "Darüber hinaus erhielt die Forschung zu {} internationale Unterstützung.",
        "Infolgedessen meldete das System, dass {} abgeschlossen ist.",
    ],
    paragraph_sep: "\n\n",
};

static TRKZ: LangData = LangData {
    words: &[
        "bilgi", "sistem", "araştırma", "gelişme", "teknoloji", "dil",
        "tarih", "örnek", "sayı", "dünya", "insan", "bilgisayar", "bellek",
        "sıkıştırma", "algoritma", "problem", "soru", "hizmet", "şirket",
        "süreç", "program", "veri", "kod", "dosya", "sunucu", "proje",
        "zaman", "şehir", "ülke", "okul", "kültür", "eğitim", "bilim",
        "деректер", "жүйе", "зерттеу", "технология", "деректерді",
        "алгоритм", "мәселе", "сұрақ", "қызмет", "бағдарлама", "код",
        "файл", "сервер", "жоба", "уақыт", "қала", "ел", "мектеп",
        "мәдениет", "білім", "ғылым", "жұмыс", "тарих", "өрнек", "саны",
    ],
    years: &["1946", "1970", "1993", "2006", "2019", "2026"],
    templates: &[
        "Bu sistem, verilerini sunucular arasında {} ile işler.",
        "Örneğin, algoritma {} verilerini başarıyla sıkıştırır.",
        "Rapora göre, {} sayısı {y} yılında rekor seviyeye ulaştı.",
        "Бұл жүйе деректерді серверлер арасында {} арқылы өңдейді.",
        "Мысалға, алгоритм {} деректерін сәтті сығады.",
        "Есеп бойынша, {} саны {y} жылы рекордтық деңгейге жетті.",
        "Бұл технология {} саласында стандартқа айналды.",
        "Сондықтан зерттеушілер {} мәселесін қарастырды.",
    ],
    paragraph_sep: "\n\n",
};

static ARFA: LangData = LangData {
    words: &[
        "المعلومات", "النظام", "البحث", "التطوير", "التقنية", "اللغة",
        "التاريخ", "مثال", "الرقم", "العالم", "الناس", "الحاسوب", "الذاكرة",
        "الضغط", "الخوارزمية", "مشكلة", "سؤال", "خدمة", "شركة", "عملية",
        "برنامج", "البيانات", "ملف", "خادم", "مشروع", "وقت", "مدينة",
        "دولة", "مدرسة", "ثقافة", "تعليم", "علم", "عمل",
        "اطلاعات", "سیستم", "تحقیق", "توسعه", "فناوری", "زبان", "تاریخ",
        "مثال", "عدد", "دنیا", "مردم", "رایانه", "حافظه", "فشرده‌سازی",
        "الگوریتم", "مسئله", "پرسش", "خدمت", "شرکت", "داده‌ها", "پرونده",
        "سرور", "پروژه", "زمان", "شهر", "کشور", "مدرسه", "فرهنگ",
        "آموزش", "علم", "کار",
    ],
    years: &["1948", "1967", "1987", "2004", "2015", "2026"],
    templates: &[
        "يعالج النظام البيانات بين الخوادم و{} بنجاح.",
        "على سبيل المثال، تضغط الخوارزمية بيانات {} بكفاءة.",
        "وفقاً للتقرير، بلغ عدد {} مستوى قياسياً في عام {y}.",
        "يُعتبر هذا أحد الأمثلة الأكثر أهمية في مجال {}.",
        "سیستم، داده‌ها را بین سرورها و {} پردازش می‌کند.",
        "به عنوان مثال، الگوریتم داده‌های {} را فشرده می‌سازد.",
        "بر اساس گزارش، شمار {} در سال {y} به رکورد رسید.",
        "این یکی از مهم‌ترین نمونه‌ها در زمینه {} است.",
    ],
    paragraph_sep: "\n\n",
};

static ZH: LangData = LangData {
    words: &[
        "数据", "系统", "研究", "发展", "技术", "语言", "历史", "例子",
        "数字", "世界", "人们", "计算机", "存储器", "压缩", "算法",
        "问题", "问题,", "服务", "公司", "过程", "程序", "代码", "文件",
        "服务器", "项目", "时间", "城市", "国家", "学校", "文化", "教育",
        "科学", "工作", "信息", "经济", "历史.", "方法", "结果", "市场",
        "在目前的基础上", "通过算法优化", "人工智能技术",
    ],
    years: &["一九五七", "一九七八", "一九九四", "二〇〇七", "二〇一九", "二〇二六"],
    templates: &[
        "这个系统在服务器之间处理{}。",
        "例如，算法成功地压缩了{}。",
        "根据报告，{}的数量在{y}年达到创纪录水平。",
        "这被认为是{}领域最重要的例子之一。",
        "此外，关于{}的研究得到了国际支持。",
        "因此，服务报告指出{}已经完成。",
        "通过算法优化，{}的性能显著提高。",
        "在数据处理和存储方面，{}发挥着重要的作用。",
    ],
    paragraph_sep: "\n\n",
};

static JA: LangData = LangData {
    words: &[
        "情報", "システム", "研究", "発展", "技術", "言語", "歴史", "例",
        "数字", "世界", "人々", "コンピュータ", "記憶装置", "圧縮",
        "アルゴリズム", "問題", "質問", "サービス", "会社", "過程",
        "プログラム", "コード", "ファイル", "サーバー", "プロジェクト",
        "時間", "都市", "国家", "学校", "文化", "教育", "科学", "仕事",
        "データ圧縮", "人工知能", "効率的",
    ],
    years: &["一九五五", "一九七六", "一九九〇", "二〇〇五", "二〇一七", "二〇二六"],
    templates: &[
        "このシステムは、サーバー間で{}を処理します。",
        "例えば、アルゴリズムは{}を効率的に圧縮します。",
        "報告によると、{}の数は{y}年に記録的な水準に達しました。",
        "これは{}分野における最も重要な例の一つと考えられています。",
        "さらに、{}に関する研究は国際的な支持を得ました。",
        "したがって、サービスは{}が完了したと報告しました。",
        "データ圧縮アルゴリズムは{}の性能を大幅に向上させます。",
        "人工知能技術の発展により、{}が可能になりました。",
    ],
    paragraph_sep: "\n\n",
};

static MATH: LangData = LangData {
    words: &[
        "x", "y", "z", "n", "k", "i", "a", "b", "c", "t", "u", "v", "m",
        "\\alpha", "\\beta", "\\gamma", "\\delta", "\\theta", "\\lambda",
        "\\mu", "\\sigma", "\\omega", "\\varphi", "\\epsilon", "\\pi",
        "f(x)", "g(x)", "\\frac{1}{2}", "\\frac{n(n+1)}{2}", "\\sqrt{2}",
        "\\sqrt{\\pi}", "e^{-x^2}", "\\infty", "\\mathbb{R}", "\\mathbb{N}",
    ],
    years: &["1", "2", "3", "5", "10", "100"],
    templates: &[
        "\\int_{0}^{\\infty} {} \\,dx = \\frac{\\pi}{2} \\quad (n \\geq 1).",
        "\\sum_{i=1}^{n} {} = \\frac{n(n+1)(2n+1)}{6} \\leq \\infty.",
        "\\frac{d}{dx}\\left[ {} \\right] = \\frac{\\partial f}{\\partial x} \\cdot {}.",
        "Если {} \\to 0 при n \\to \\infty, то {} \\approx {}.",
        "\\lim_{n \\to \\infty} \\left(1 + \\frac{1}{n}\\right)^{n} = e \\approx 2{,}718.",
        "Для всех \\varepsilon > 0 существует \\delta > 0: |x - a| < \\delta \\Rightarrow |f(x) - f(a)| < \\varepsilon.",
        "\\sum_{k=1}^{\\infty} \\frac{1}{k^2} = \\frac{\\pi^2}{6} \\in \\mathbb{R}.",
        "Пусть f: \\mathbb{R} \\to \\mathbb{R}, где {} и {} непрерывны.",
        "\\forall x \\in \\mathbb{N} \\; \\exists y \\in \\mathbb{N}: y > x^2 + {}.",
        "\\begin{equation}\\label{eq:main} {} \\end{equation}",
    ],
    paragraph_sep: "\n\n",
};

static CODE: LangData = LangData {
    words: &[
        "data", "buffer", "result", "config", "parser", "reader", "writer",
        "cache", "index", "record", "packet", "stream", "token", "lexer",
        "cursor", "offset", "length", "capacity", "threshold", "context",
        "self", "this", "item", "node", "tree", "graph", "table", "row",
    ],
    years: &["0", "1", "2", "4", "8", "16", "32", "64", "128", "256"],
    templates: &[
        "fn process_{s}(input: &[u8]) -> Result<Vec<u8>, Error> {\n    let mut {s} = Vec::with_capacity(input.len());\n    for &b in input {\n        {s}.push(b ^ 0x20);\n    }\n    Ok({s})\n}\n",
        "def handle_{s}(request):\n    data = request.get_json()\n    if not data or \"{s}\" not in data:\n        return jsonify({\"error\": \"missing field\"}), 400\n    return process(data[\"{s}\"])\n",
        "async function fetch_{s}(url) {\n    const response = await fetch(url);\n    if (!response.ok) throw new Error(`HTTP ${response.status}`);\n    const {s} = await response.json();\n    return {s}.map((item) => item.id);\n}\n",
        "pub struct {S} {\n    pub id: u64,\n    pub name: String,\n    pub {s}: Option<Vec<u8>>,\n}\n\nimpl {S} {\n    pub fn new(id: u64, name: &str) -> Self {\n        Self { id, name: name.to_string(), {s}: None }\n    }\n}\n",
        "for i in 0..{y} {\n    let {s} = buffer[i].clone();\n    if {s}.len() > {y} {\n        continue;\n    }\n    total += {s}.len();\n}\n",
        "class {S}:\n    def __init__(self, {s}_id):\n        self.{s}_id = {s}_id\n        self._cache = dict()\n\n    def lookup(self, key):\n        if key not in self._cache:\n            self._cache[key] = len(key) * {y}\n        return self._cache[key]\n",
        "int main(void) {\n    for (int i = 0; i < {y}; i++) {\n        printf(\"{s} %d\\n\", i);\n    }\n    return 0;\n}\n",
        "const {S} = {\n    name: \"{s}\",\n    version: 2,\n    limits: { max: {y}, timeout: 30 },\n    handler: (req, res) => res.json({ ok: true, {s}: req.body }),\n};\n",
    ],
    paragraph_sep: "\n",
};

/// Заглушки шаблонов: `{}` — слово, `{y}` — «год», `{s}` — идентификатор,
/// `{S}` — идентификатор с заглавной буквы.
fn expand(template: &str, rng: &mut Rng, data: &LangData) -> String {
    let mut out = String::with_capacity(template.len() + 64);
    let mut rest = template;
    while let Some(pos) = rest.find('{') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if tail.starts_with("{}") {
            out.push_str(rng.pick(data.words));
            rest = &tail[2..];
        } else if tail.starts_with("{y}") {
            out.push_str(rng.pick(data.years));
            rest = &tail[3..];
        } else if tail.starts_with("{s}") {
            out.push_str(&slug(rng, data.words));
            rest = &tail[3..];
        } else if tail.starts_with("{S}") {
            let mut s = slug(rng, data.words).into_bytes();
            if let Some(first) = s.first_mut() {
                if first.is_ascii_lowercase() {
                    first.make_ascii_uppercase();
                }
            }
            out.push_str(std::str::from_utf8(&s).unwrap_or("Item"));
            rest = &tail[3..];
        } else {
            out.push('{');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn slug(rng: &mut Rng, words: &[&str]) -> String {
    let w = rng.pick(words);
    let cleaned: String = w
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    let suffix = rng.next() % 1000;
    if cleaned.is_empty() {
        format!("item_{suffix}")
    } else {
        format!("{cleaned}_{suffix}")
    }
}

/// Генерация корпуса заданного вида ровно `target` байт.
pub fn generate(kind: Kind, target: usize, seed: u64) -> Vec<u8> {
    let data = match kind {
        Kind::Ru => &RU,
        Kind::En => &EN,
        Kind::Es => &ES,
        Kind::De => &DE,
        Kind::TrKz => &TRKZ,
        Kind::ArFa => &ARFA,
        Kind::Zh => &ZH,
        Kind::Ja => &JA,
        Kind::Math => &MATH,
        Kind::Code => &CODE,
    };
    let mut rng = Rng::new(seed);
    let mut buf: Vec<u8> = Vec::with_capacity(target + target / 8);
    let mut paragraph: Vec<String> = Vec::new();
    while buf.len() < target {
        let count = rng.range(2, 6);
        paragraph.clear();
        for _ in 0..count {
            let t = rng.pick(data.templates);
            paragraph.push(expand(t, &mut rng, data));
        }
        let sep = match kind {
            Kind::Zh => "",
            Kind::Ja => " ",
            Kind::Code | Kind::Math => "",
            _ => " ",
        };
        let joined = paragraph.join(sep);
        buf.extend_from_slice(joined.as_bytes());
        buf.extend_from_slice(data.paragraph_sep.as_bytes());
        // периодически вставляем структурные разделители разных типов
        match rng.next() % 8 {
            0 => buf.extend_from_slice(b"\n--- \n\n"),
            1 => buf.extend_from_slice(b"\n## \n\n"),
            _ => {}
        }
    }
    // Точное выравнивание: усечение по границе символа, добор пробелами
    if buf.len() > target {
        let mut cut = target;
        while cut > 0 && buf[cut] & 0xC0 == 0x80 {
            cut -= 1;
        }
        buf.truncate(cut);
    }
    while buf.len() < target {
        let pad = (target - buf.len()).min(4096);
        buf.extend(std::iter::repeat(b' ').take(pad));
    }
    debug_assert_eq!(buf.len(), target);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_size_and_valid_utf8() {
        let target = 10 * 1024 * 1024;
        for kind in KINDS {
            let raw = generate(kind, target, 42);
            assert_eq!(raw.len(), target, "{}: размер не равен целевому", kind.name());
            assert!(std::str::from_utf8(&raw).is_ok(), "{}: не UTF-8", kind.name());
        }
    }

    #[test]
    fn deterministic_generation() {
        let a = generate(Kind::Ru, 4096, 7);
        let b = generate(Kind::Ru, 4096, 7);
        assert_eq!(a, b);
        let c = generate(Kind::Ru, 4096, 8);
        assert_ne!(a, c);
    }
}
