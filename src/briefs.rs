//! Слои 4–5: «брифы» — устойчивые конструкции, перекрывающие длинные
//! последовательности байтов одним токеном 4 или 5 байт.
//!
//! Слой 4 (4 Б, ≤ 134 млн ID): брифы длиной 5–64 байта. Слой 5 (5 Б,
//! ≤ 17 млрд ID): длинные брифы более 64 байт, включая фразы CJK из 10+
//! иероглифов (20+ байт UTF-8) — ровно тот случай, ради которого в схеме
//! появилась пятая ширина.

pub static STRUCT_STATIC: &[&str] = &[
    // Русские устойчивые связки
    " в настоящее время", " в том числе", " таким образом", " в связи с этим",
    " является одним из", " с одной стороны", " с другой стороны",
    " на сегодняшний день", " по отношению к", " в частности",
    " а также", " как правило", " в результате", " в дальнейшем",
    " в ближайшее время", " во время", " с помощью", " благодаря этому",
    " в первую очередь", " несмотря на то", " в течение нескольких",
    " большого количества", " является одним из наиболее",
    " в соответствии с", " в зависимости от", " большое количество",
    " существенное значение", " имеет большое значение",
    // Конструкции кода (Rust/Python/JS/C)
    "fn main() {\n    ", "    }\n}\n\n", "pub fn ", "pub struct ",
    "#[derive(Debug)]", "String>::new()",  ".clone();\n",
    ".push_str(&", "std::collections::", "use std::", "impl Default for ",
    "-> Result<(), ", "Vec<String>", "let mut buf = ", "if let Some(",
    "match self {", "(a, b) in ", "console.log(", "});\n    ",
    "return null;", "=== undefined", "null !== ", "for (let i = 0; ",
    "function(", " => {\n        ", "return self.", "const { ",
    "import { ", "from \"react\";", "async function ", "await fetch(",
    "\"use strict\";\n\n", "export default ", "module.exports = ",
    "#include <stdio.h>\n\n", "int main(void) {\n    ", "    return 0;\n}\n",
    "def __init__(self", "if __name__ == \"__main__\":", "    print(f\"",
    "self._", "range(len(", " except Exception as e:", "    else:\n        ",
    "    if not ", ": str) -> ", ": int = ", " | None = None",
    "Result<Vec<u8>, ", "&mut Vec<u8>", "as usize);",
    "unwrap_or_default()", "to_string().unwrap()", "String::from_utf8(",
    // Математические конструкции
    "\\int_{0}^{\\infty}", "\\int_{-\\infty}^{\\infty}",
    "\\frac{1}{2}", "\\frac{\\pi}{2}", "\\frac{d}{dx}", "\\sqrt{\\pi}",
    "\\begin{equation}", "\\end{equation}", "\\begin{aligned}",
    "\\sum_{i=1}^{n}", "\\sum_{k=1}^{n}", "\\prod_{i=1}^{n}",
    "e^{-x^2}", "\\,dx = ", "\\mathrm{d}x", "\\begin{cases}",
    "\\end{cases}", "\\left(\\frac", "\\right)\\,", "\\quad\\quad",
    "\\int_{0}^{1}", "\\lim_{n \\to \\infty}", "\\mathbb{R}^{n}",
    "\\varphi(n)", "\\equiv 1 \\pmod", "\\bmod p", "x^{2} + y^{2}",
    // Фразы CJK/японский/корейский/тайский (многознаковые слова)
    "在目前的基础上", "数据压缩算法", "我们需要保存信息",
    "人工智能技术发展", "数据结构与算法", "随着经济的不断发展",
    "在数据处理和存储方面", "系统性能的显著提高",
    "データ圧縮アルゴリズム", "情報を保存します", "こんにちは世界",
    "人工知能技術の発展", "効率的に保存するため",
    "데이터 압축 알고리즘", "인공지능 기술의 발전",
    "การบีบอัดข้อมูล", "การจัดเก็บข้อมูล",
    // Идиоматика EN
    " public static void ", " in accordance with the ",
    " as soon as possible", " in the middle of ",
];

pub static EXTENDED_STATIC: &[&str] = &[
    // Длинные формулы (44–50 байт → один токен 5 байт)
    "\\int_{-\\infty}^{\\infty} e^{-x^2}\\,dx = \\sqrt{\\pi}",
    "\\frac{d}{dx}\\left[\\int_{a}^{x} f(t)\\,dt\\right] = f(x)",
    "\\lim_{n\\to\\infty}\\left(1+\\frac{1}{n}\\right)^n = e",
    "\\sum_{n=1}^{\\infty}\\frac{1}{n^2} = \\frac{\\pi^2}{6}",
    "\\begin{equation}\\label{eq:main}\\end{equation}",
    "\\frac{\\partial^2 u}{\\partial x^2} + \\frac{\\partial^2 u}{\\partial y^2} = 0",
    // Длинные конструкции кода
    "public static void main(String[] args) {\n        ",
    "console.log(JSON.stringify(data, null, 2));",
    "for (int i = 0; i < n; i++) {\n            ",
    "if err != nil {\n\t\treturn nil, err\n\t}",
    "std::vector<std::string>::const_iterator",
    "fn main() {\n    println!(\"Hello, world!\");\n}",
    "if __name__ == \"__main__\":\n    main()",
    "return jsonify({\"status\": \"ok\", \"data\": data})",
    // Русские длинные связки
    " в связи с вышеизложенным",
    " в соответствии с законодательством",
    " является одним из наиболее важных",
    " в кратчайшие сроки",
    " с целью повышения эффективности",
    " дальнейшего развития и совершенствования",
    // CJK: фразы из 10+ иероглифов (20+ байт) — целевой случай слоя 5
    "中华人民共和国",
    "在数据处理和存储方面发挥着重要的作用",
    "通过算法优化提高系统性能",
    "我们需要更加高效的数据压缩算法",
    "人工智能技术在各领域的应用",
    "随着经济的不断发展和进步",
    "为了解决这个问题我们需要进一步研究",
    "数据结构和算法是计算机科学的基础",
    "在这个基础上我们进行了大量的实验",
    "为用户提供了更加便捷的服务体验",
    // Японские длинные фразы
    "データ圧縮アルゴリズムの研究",
    "人工知能技術の発展により",
    "情報を効率的に保存するため",
    "この問題を解決する必要があります",
    // Корейская длинная фраза
    "데이터를 효율적으로 저장하기 위하여",
    // Тайская длинная фраза
    "การบีบอัดข้อมูลและการจัดเก็บข้อมูล",
];
