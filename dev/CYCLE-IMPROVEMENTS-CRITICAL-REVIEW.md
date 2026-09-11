# Cycle: возможности улучшения и критическая проверка предложений

Дата: 2026-09-11.

> **К предложениям отнеслись критически.** Это повторный анализ исходников
> с контраргументами и условиями принятия, а не перечень доказанных ускорений.
> Статус: исследовательская записка; изменения алгоритма не утверждены.
> Независимый внешний review и новые замеры не проводились. На этапе анализа
> runtime-тесты не запускались; проверки перед публикацией перечислены ниже.
> Чтение кода не доказывает отсутствие ошибок.

Рассмотрены `model` на `b33df9a245b4d9a42e6d1a74f1233f034e7409df` и
`rfc` на `5fcc2722e4e0be431c317b50c0a3ae6e52904c79`. Сравнение с Cangjie
опирается на статью и открытый mirror runtime на
`18cd0af893b06bfd0aedcef82aaa9eaf31cc40d2`; это конкретный снимок,
а не утверждение о последней версии официальной ветки.

## Что уже делает Cycle

Источники: [регистрация](../src/refcount.rs), [точки запуска](../src/gc.rs),
[driver](../src/cycle/collect.rs), [trace](../src/cycle/trace.rs),
[mark](../src/cycle/mark.rs), [scan](../src/cycle/scan.rs),
[validation](../src/cycle/validation.rs), [finalization](../src/cycle/finalization.rs),
[reclamation](../src/cycle/reclamation.rs).

1. Non-final decrement регистрирует допустимый объект кандидатом.
   Нулевой RC запускает обычное разрушение. Регистрация не запускает trace
   посреди изменения графа.
2. В согласованной точке owner проверяет eligibility, получает trace token
   и отделяет batch кандидатов. `mark` обходит их граф и вычитает внутренние
   рёбра из временных shadow counts, не меняя настоящие RC.
3. Только после mark всех выбранных корней запускается scan. Для точного
   shadow count положительный остаток означает удержание вне вычтенных рёбер;
   живыми становятся и достижимые от него объекты. Saturated rows сохраняются
   консервативно. Оставшиеся строки получают `PotentiallyUnreachable`.
4. Обычный путь сохраняет rows через teardown. Pressure-путь собирает
   ограниченный список members и возвращает trace scratch до деструкторов.
   При переполнении списка он повторяет trace с меньшим числом корней.
5. Owner независимо перечитывает RC и рёбра members. Проверка:
   `sum(RC) == internal_edges + guard_refs`.
   Она опирается на то, что каждое внутреннее ребро действительно учтено RC,
   references в корнях учтены, а арифметика остаётся точной.
6. Подтверждённый набор получает guards; weak-ссылки инвалидируются;
   исполняются деструкторы. После вызова member-деструкторов нужна повторная
   exact validation: resurrection могла изменить результат. Если ни один
   такой деструктор не исполнялся, этот повтор уже пропускается.
7. Резервируется место для внешних children, затем выполняются sever,
   освобождение members и снятие guards. Внешние children отпускаются после
   завершения member frees. Возврат candidate slots требует также retirement
   записей после окончания читающей их membership.

**Это уже работающий owner-side путь в runtime model.** Worker остаётся
отдельной незавершённой частью: [token](../src/cycle/token.rs),
[план](../PLAN.md), раздел «S38». Наличие token не разрешает конкурентное
чтение `PlainCells`, публикацию результатов или повторное использование rows.

Две оговорки к слову «компонента»:

- Driver передаёт весь найденный unreachable set одной membership;
  разделения этого набора на независимые компоненты сейчас нет.
- [Maturation](../src/cycle/maturation.rs) уже ищет SCC живого подграфа
  алгоритмом Pearce. Это отдельная работа для ageing, а не готовое
  разбиение мусора для независимых commits.

В начале [RFC rc-cycle](../../rfc/model/gc/rc-cycle.md) всё ещё написано
«not implemented»; похожий старый текст о нулевых ABI-ответах остался в
[memory-manager](../docs/memory-manager.md). Для состояния реализации здесь
использованы реальные вызовы из `gc.rs` и `collect.rs`. Нормативные
инварианты RFC при этом не отменяются.

## Поправки к исходному сравнению с Cangjie

**Отдельный observer не измеряет паузу owner Cycle.** В статье Cangjie,
§6.2, observer регистрирует разрывы выполнения своего цикла при работе
allocation workload; это наблюдаемая задержка с влиянием GC и scheduler.
У нас другой owner может собирать свою кучу, пока observer продолжает
работать. Его хорошие цифры совместимы с длинной паузой собирающего owner.
[Статья, §6.2](https://jcst.ict.ac.cn/fileup/1000-9000/PDF/JCST-2603-OF-2509-15978.pdf).

**Dense-region exemption не равен pruning графа.** Cangjie принимает это
решение после маркировки: не перемещать регион с высокой долей живых bytes.
Наш pruning сокращает обход и может отложить обнаружение мусора. Общим
является только выбор между работой сейчас и удержанием памяти;
переносить критерий плотности как доказательство живости нельзя.
[Cangjie: ExemptFromRegions](https://github.com/causerp/cangjie_runtime/blob/18cd0af893b06bfd0aedcef82aaa9eaf31cc40d2/runtime/src/Heap/Allocator/RegionManager.cpp#L479).

**«Fully concurrent» не означает отсутствие ожиданий.** В проверенном коде
`StartLightSync` вызывает `WaitUntilAllMutatorStopped`, а `MarkSatbBuffer`
имеет STW fallback при превышении обоих порогов — числа итераций и времени.
Это аргумент измерять все задержки, а не основание объявлять один GC лучше.
[Синхронизация](https://github.com/causerp/cangjie_runtime/blob/18cd0af893b06bfd0aedcef82aaa9eaf31cc40d2/runtime/src/Mutator/MutatorManager.cpp#L328),
[remark fallback](https://github.com/causerp/cangjie_runtime/blob/18cd0af893b06bfd0aedcef82aaa9eaf31cc40d2/runtime/src/Heap/Collector/TracingCollector.cpp#L440).

Предыдущее утверждение «наши candidate bit и deferred reuse дешевле» не
подкреплено сопоставимым измерением. Они решают другую задачу: сохраняют
identity до безопасного возврата slot; forwarding сохраняет возможность
разрешить ссылки после перемещения. Аналогично, отсутствие relocation
barrier не делает обычную ссылку бесплатной: у Cycle остаются RC и store
protocol. Прямого performance-сравнения двух runtime здесь нет.

## Приоритет предложений

Приоритет означает порядок исследования, а не разрешение на реализацию.

| Порядок | Предложение | Текущий вывод |
|---|---|---|
| 1 | Измерять owner latency и стоимость фаз | Нужная основа следующих решений |
| 2 | Измерить полную цену maturation; ограничить её расход scratch | Конкретный кандидат, выигрыш не измерен |
| 3 | Проверить progress pruning и bounded pressure trace | Требуются сценарии отказа и отложенного сбора |
| 4 | Сопоставить flat rows и chunks | Уже открыто в плане; решать по workload |
| 5 | Исследовать изоляцию независимых garbage sets | Только если общий отказ заметен в workload |
| 6 | Исследовать aggregate-first discovery | Дорогой эксперимент с двойным обходом при неуспехе |
| — | Удалить exact check; добавить negative cache; перенести moving GC | Не принимать в предложенном простом виде |

## 1. Измерять паузу на собирающем owner и стоимость всех фаз

**Предложение.** Измерять длительность реального `ll_gc_maybe_collect` на
owner, а также внешнюю длительность allocation call, который может вызвать
pressure collection. Для пользовательской задержки дополнительно измерять
интервалы обслуживания работы на этом же owner. Отдельный observer оставить
контролем влияния на другие потоки и scheduling.

Записывать число samples, p50/p99/p99.9, максимум, throughput и memory
high-water. Максимум — наблюдение конкретного прогона, не верхняя граница.
Недостаточно отсортировать несколько десятков худших значений и назвать это
устойчивой оценкой p99.9; нужны полная выборка и повторяемость между запусками.

Фазы: poll maintenance, ожидание token, mark, scan, harvest, maturation,
initial validation, guards/weak invalidation, деструкторы, revalidation,
reservation/sever/free, external drops, close/retirement. Для вложенных
событий различать inclusive и exclusive time, иначе сумма посчитает часть
времени дважды. Возвраты через guards/Drop и отказные выходы тоже входят.

**Критика.** Таймер на каждом ребре, TLS first touch или растущий лог способны
создать измеряемую проблему сами. Буфер должен быть подготовлен до критического
участка; внутри него недопустимы форматирование и скрытые аллокации.
Нужны отдельные structural и timing runs, а затем сравнение instrumented и
обычной сборок. Wall time нельзя механически «очистить от scheduler» одним
observer; CPU time и scheduling events — дополнительные наблюдения.

**Принять, если:** известна погрешность инструмента, учтены все выходы и
различимы дешёвый невооружённый poll, poll с maintenance, реальная сборка,
refusal и pause от пользовательского деструктора. Основа счётчиков уже
предусмотрена в [плане](../PLAN.md), «S40», пункт «Count the workspace and the
cache traffic»; не следует заводить конкурирующую систему измерений.

## 2. Maturation должна окупаться и оставлять память для reclamation

**Факт.** `commit_before_drops` вызывает `stamp_live_components` до initial
validation. Это два прохода по touched list и обход живого подграфа; состояние
SCC и frames использует scratch arena. Та же arena затем обслуживает
`reserve_drops`. Комментарий «What the descent takes, it takes before the
teardown asks» уже прямо описывает возможность отказа teardown после
расходования памяти maturation.
[Код maturation](../src/cycle/maturation.rs), [commit](../src/cycle/collect.rs),
[reservation](../src/cycle/reclamation.rs).

**Предложение.** Сначала посчитать суммарную цену за весь интервал ageing:
дополнительные обходы, scratch peak, реально пропущенные edges, задержку
сбора и отказы reclamation. Затем сравнить полную maturation с вариантом,
который работает лишь в доступном бюджете и не делает новых manager draws
ради stamps. Отказ от ageing должен оставлять незавершённую SCC без нового
stamp, как и существующий отказ descent; завершённые SCC сохраняют результат.

**Критика.** Запрет новых draws может постоянно срывать maturation большой
живой SCC: мы заплатим за её префикс много раз и никогда не получим pruning.
Уже выделенные сегменты тоже могут быть нужны дальнейшей reservation.
Поэтому «без новых draws» не доказывает, что память для teardown защищена.
Если требуется освобождать scratch обратно после ageing, нужен отдельный
протокол lifetime; обнулить всю arena нельзя, membership ещё читает rows.

**Принять, если:** mixed live-core + garbage-ring workload под тем же pool
cap чаще доходит до возврата используемых slots, а выигрыш сохраняется по
всему интервалу ageing. Контроли: только live core, churn SCC, только мусор,
глубокий граф и высокий fan-out. Отдельно считать долю targets с
`CANDIDATE_BIT`: их pruning не отсекает, независимо от возраста.

Не предлагать «добавить producer живых stamps»: он уже построен. Старые
синтетические проценты до его появления были отозваны, что записано в плане.

## 3. Ограничение работы должно сопровождаться проверкой progress

**Факт.** Обычный trace получает `ALL_ROOTS`. Pressure-путь после overflow
уменьшает число корней, но один корень всё ещё может достигать огромного графа.
При отказе scratch весь trace прекращается. При overflow на одном корне
pressure-путь вооружает следующий poll. Лимит members не является лимитом
edges или времени. [Driver](../src/cycle/collect.rs), [trace](../src/cycle/trace.rs).

**Предложение.** Исследовать budget на работу до начала irreversible
finalization: edges, touched blocks и scratch draws. Первым вариантом считать
безопасный полный abort с сохранением candidates, а не commit частично
вычисленных shadow counts. При повторе предусмотреть progress: owner-side
смену выбранных roots либо согласованное увеличение бюджета.

**Критика.** Постоянно начинать один большой корень заново означает starvation.
Переставить корни недостаточно, если каждый достигает того же live core.
Просто сохранить DFS stack между polls тоже недостаточно: mutator уже изменил
RC и edges; такой continuation требует нового snapshot/validation протокола.
Проверка бюджета только между объектами не ограничивает обработку одного
контейнера с миллионом fields. Произвольный пользовательский деструктор
оставляет полный commit без строгого временного bound даже при bounded trace.

**Принять, если:** тесты показывают одновременно безопасный abort и достижение
собираемого хвоста за конечное число попыток при явно заданных условиях.
Нужны giant root, перекрывающиеся closures, большой live prefix перед малым
dead ring, очень широкий контейнер и отказ scratch на разных фазах.

**Для pruning отдельно:** довести до end-to-end сценарий из backlog
«The prune's own recall loss has no case»: настоящий mature target без
candidate registration, потеря последнего внешнего удержания, отложенный
результат, turnover и последующая сборка. Не подменять producer инъекцией
`ExternallyReferenced`. Источники: [план](../PLAN.md),
[prune](../src/cycle/mark.rs), [re-offer](../src/cycle/queue.rs),
[epoch](../src/cycle/epoch.rs).

64 commits на epoch — счётчик событий, а не обещание времени освобождения.
Проверять также редкие commits, долгую неактивность owner и полные обороты
короткого header stamp; отдельно формулировать предпосылку, что нужный
turnover и следующий eligible poll действительно наступят.

## 4. Sparse rows: различать reserved bytes, writes и cache misses

**Факт.** Flat array резервируется на touched block, а группы rows
инициализируются лениво. Поэтому «зарезервировано 16 KiB» не означает
«записано 16 KiB» или «прочитано 16 KiB из RAM».
[Shadow](../src/cycle/shadow.rs), [arena](../src/cycle/arena.rs).

**Предложение.** Продолжить существующее сравнение flat rows с chunks
из [плана](../PLAN.md), «S40». Основной шанс chunks — sparse tracing по многим
blocks: меньше scratch draws и меньше мест, где GC может получить отказ.
Считать также повторные membership walks и цену `contains`: обычный путь
пользуется rows, pressure-путь — отсортированным списком с binary search.
[Membership](../src/cycle/membership.rs).

**Критика.** Chunks добавляют индирекцию, metadata и возможный branch на edge;
на плотном графе могут проиграть по времени, что нужно измерить. Один процент density не описывает смешанные
size classes, retained и large populations. Автоматическая смена формы ещё
сложнее: старые row pointers не должны устареть, а отказ перехода обязан
оставлять всю collection в допустимом состоянии.

**Принять, если:** A/B на dense и one-entity-per-block размещениях показывает
измеримый выигрыш по manager draws/отказам или по времени при приемлемом
проигрыше противоположной нагрузки. Hardware counters должны измерять
cache/TLB эффект; арифметика reserved bytes его не доказывает.
Сначала сравнить две фиксированные формы, затем обсуждать adaptive выбор.

## 5. Отделять независимый мусор при отказе общего commit

**Факт.** Один resurrected member может сохранить весь общий набор.
То же относится к отказу reservation внешних children. Этот компромисс
уже описан в `collect.rs`, «The commit is one component».

**Предложение.** Если явление существенно, исследовать независимые группы
для reclamation. Начать с двух несвязанных rings, где один resurrected,
а второй должен освобождаться без ожидания следующей полной сборки.

**Критика.** SCC-разбиение само по себе не делает groups независимыми:
`SCC A -> SCC B` даёт B внешнее удержание относительно её membership.
Нужны порядок обработки, текущая exact validation и сохранение guards/weak
protocol. Сначала проще рассмотреть группы без рёбер между ними; это тоже
новое построение membership, которое расходует память.
Деструкторы могут изменить связи, а external drops запускают пользовательский
код. Предварительное разбиение не заменяет revalidation непосредственно перед
reclamation и не даёт права менять согласованное финализационное поведение.

**Принять, если:** частота общих отказов оправдывает partition overhead;
тесты покрывают несвязанные rings, однонаправленные рёбра между SCC,
resurrection, новые связи из деструктора и reservation failure.
Без этой частоты текущий union остаётся разумным простым решением.

## 6. Aggregate-first discovery: возможность, а не уже готовое ускорение

В [RFC](../../rfc/model/gc/rc-cycle.md), «Aggregate proof fast path»,
предложен первый обход с visited set и суммами, с выделением shadow rows
только при неуспехе агрегатного доказательства. В текущем driver порядок иной:
сначала полноценные mark/scan с rows, затем exact aggregate validation.
Наличие суммы в `validation.rs` не означает наличие aggregate-first discovery.

**Предложение.** Эксперимент оправдан, если реальные candidate closures часто
целиком состоят из garbage и visited/membership дешевле текущих rows.
Для замкнутого набора equality позволяет избежать поиска живого поднабора.

**Контрпример.** `A <-> B`, `B -> L`, `root -> L`. A и B — garbage, L — live.
Обход, включающий L, провалит общую equality, хотя A и B можно собрать.
При частом таком результате заплатим за предварительный обход, потом ещё за
mark/scan. Visited set и retained members тоже требуют памяти; слово
«без shadow» не означает «без scratch».

**Принять, если:** измерены hit rate и цена обоих исходов, соблюдены точность
RC, учёт внутренних edges, отсутствие переполнения и owner consistency.
Нужны mixed closures, общие descendants и границы GC heap/arenas.
Независимая проверка до пользовательских деструкторов сохраняется либо
заменяется отдельно обоснованной равноценной проверкой.

## Какие быстрые решения отклоняем

**Удалить initial exact validation.** Это не просто дубликат trace: она
пересчитывает edges по текущим cells до деструкторов. В плане пункт
«Elide the redundant exact test after an in-line owner trace» закрыт отказом
от elision. Не открывать его снова только из-за отсутствия конкурентного
mutator. Пропуск повторной проверки при отсутствии member-деструкторов уже
реализован — предлагать его как новую оптимизацию тоже неверно.

**Кешировать «предыдущая pressure collection ничего не освободила».**
Для `A <-> B`, `root -> B` потеря `root -> B` может быть non-final decrement
у уже зарегистрированного B: нового candidate entry нет, RC=0 нет, но мусор
появился. Invalidation только по enrolment неверна. Любой cache должен
покрывать релевантные RC/graph changes и completed deaths, а стоимость его
барьера должна окупаться. Текущий отказ от cache записан в
[BENCHMARKS](BENCHMARKS.md), «S40.4 a refused allocation repeats the whole
live candidate lane».

**Ускорить retirement, считая только freed objects.** Счётчик freed не равен
немедленно доступной памяти нужного size class. Уже измеренный early retirement
помог matching-class allocation, но не уменьшил sampled peak и добавил проход
по очереди. Новое изменение должно считать actual slots returned, blocks
returned, успешный allocation retry и удержанную память до retirement.
[BENCHMARKS](BENCHMARKS.md), «S39.4 early pressure retirement returns matching
slots at one extra queue pass». Эти числа исторические, не свежая оценка HEAD.

**Скопировать forwarding, pointer tags, SATB или глобальный handshake Cangjie.**
Они обслуживают concurrent tracing и relocation. Для Cycle это изменение
модели доступа и lifetime, а не локальная оптимизация. Нельзя также утверждать,
что любая форма compaction обязательно требует read barrier: STW relocation
или handles имеют другие затраты. Но для нашей модели стабильных адресов,
FFI и RC ни один из этих вариантов не является бесплатной заменой allocator.

**Сразу вынести trace в worker.** Это существующее направление плана, но
сохранение памяти не решает data race при чтении cells и не обеспечивает
linearized detach. Кроме того, owner сохраняет validation, деструкторы,
revalidation, reclamation и retirement. Максимально возможное сокращение
owner work ограничено измеренной долей выносимого trace; ожидание результата
и удержание памяти способны съесть выигрыш.
[Token](../src/cycle/token.rs),
[разбор протокола](COLLECTOR-MUTATOR-MEMORY-REVIEW.md),
[RFC audit](../../rfc/dev/ALGORITHM-AUDIT.md).

## Условия следующего решения

Начать с измерения полного пути и полной цены maturation. Далее выбрать
одну гипотезу по наблюдаемому ограничению: owner pause, scratch refusal,
задержка reclaim или повторение бесполезного trace. Сравнивать обе версии
на одинаковом графе, размещении, pool cap и объёме полезной работы.

Успех — изменение целевой метрики при сохранённых инвариантах и приемлемой
цене остальных метрик. Меньшее время одного trace ценой постоянно отложенного
garbage не является достаточным результатом. Новые испытания должны
использовать опубликованные объекты и реальные producer/consumer пути;
синтетический счётчик, самостоятельно назначивший нужные состояния, не
подтверждает production-оптимизацию. Инструментальный run, correctness run
и timing run следует различать. Эта записка не закрывает пункты плана.

## Проверки перед публикацией

2026-09-11: четыре обычных прогона `cargo test --lib` и один с
`hash-folding` (`LL_HASH_SEED=1`) — по 875 passed, 10 ignored; три прогона
с `debug-journal` — по 879 passed, 12 ignored. Все прогоны использовали
`-j 4` и `--test-threads=4`. Также прошли `cargo build --release`,
`cargo bench --no-run` (оба с `-j 4`) и `cargo +1.94 fmt --check`.
Проверены 34 локальные ссылки документа, запись индекса и whitespace diff.
Это проверка состояния репозитория перед публикацией документации;
она не измеряет и не доказывает эффективность предложенных оптимизаций.
