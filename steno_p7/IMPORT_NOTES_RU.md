# Импорт STENO-P8 Stage19/20 в steno-universal — заметки обзора

Источник: upload/STENO-P8_Stage19_Joint_Optimizer.zip и
STENO-P8_Stage20_Benchmark_Integrity.zip (крейт steno_p7, ~2.7K строк).
Stage20 = Stage19 + integrity.rs (CRC-64/ECMA-182 конверт «S20» вокруг
SP8-кадра) + stage20.rs (бенч-примитивы) + bin/stage20_bench.rs.

## Архитектура

- `lib.rs` (SP7): словарный текстовый кодек + rANS/адаптивное
  диапазонное кодирование. Используется только бинарями `steno-p7`
  (compress/decompress/roundtrip/bench) и тестами jtco.
- `economic.rs` (SP8): словарь слов/знаковых серий (валидный UTF-8),
  префиксные коды 1–5 Б, жадный разбор через trie, точная экономика,
  fail-open в литеральный кадр. Работает с произвольными байтами.
- `joint.rs` (Stage19): линейный классификатор профилей (advisory) +
  маршрутизация ВСЕГО через SP8. Классификация не влияет на байты.
- `integrity.rs` (Stage20): S20-конверт = MAGIC + версия + длина +
  CRC-64 + SP8-кадр; проверка лимитов вывода, длины и контрольной
  суммы до возврата данных.
- `stage20.rs` + `bin/stage20_bench.rs`: повторяемый бенч против
  zstd/xz/gzip на тех же байтах с побайтовой верификацией.

## Исправления сборки (обязательные, смысл сохранён)

Крейт в поставке НЕ компилировался (3 ошибки):

1. `src/lib.rs:221` — `let local_rounds=...` → `let local_rounds:usize=...`
   (неоднозначный целочисленный тип, E0689).
2. `src/lib.rs:1014` — `model.totals=[256;CONTEXT_COUNT]` (21) в поле
   типа `[u32;ADAPTIVE_CONTEXT_COUNT]` (32) → `[256u32;ADAPTIVE_CONTEXT_COUNT]`
   (E0308; намерение — по 256 на контекст после инициализации).
3. `src/lib.rs:430` — `dict.into_iter()` перемещал `dict`, нужный в
   `Ok(dict)` (E0382) → `dict.clone().into_iter()`.
4. `src/lib.rs:811` — `max_n=6` для CJK при `window.len()<6` давал
   underflow-панику `window[len-n..]` → `window.len().min(6)`.

## Состояние тестов после исправлений (56 тестов lib)

- ВСЕ тесты Stage19/20 проходят: economic (6), joint (5), integrity (4),
  включая roundtrip всех 256 значений байта, детерминизм, лимиты вывода,
  детекцию порчи конверта.
- Тесты SP7-этапов: 31 ok, 2 FAILED, 2 ВИСЯТ (>100 c, CPU-цикл):
  - `math::tokenizes_formula_components_and_preserves_spans` — токенайзер
    не порождает MathTokenKind::Superscript;
  - `stage7::phrase_candidates_preserve_exact_substrings` — расхождение
    ожиданий;
  - `jtco_tests::stage10_frequent_tokens_receive_short_ids` и
    `stage14_predictive_parse_roundtrips_multilingual_text` — зависают в
    оптимизаторе словаря (подозрение на бесконечный цикл переказначения
    порядка в `optimize_token_order`).
- Ветку PS-серии эти проблемы не задевают: joint/economic/integrity не
  вызывают SP7-путь lib.rs.

## Бенч на синтетике Stage20 (zstd не установлен — SKIP)

SP8+S20 на повторяющихся корпусах сильно проигрывает xz (у SP8 нет
back-reference/LZ-модели и энтропийного кодера):
- synthetic-text: S20 54.8% против xz 0.46%;
- synthetic-math: S20 97.3% против xz 0.59%;
- synthetic-binary: S20 100.06% (кадр РАСТЁТ), xz 1.0%.
На бинарных (не-UTF8) входах SP8 переходит в литеральный режим
(fail-open) — сжатия нет по построению.

Результаты на реальных файлах пользователя (fb2 677647, m10 mixed 10МБ)
— в PS20_BENCH_RU.md и our_files_bench CSV.
