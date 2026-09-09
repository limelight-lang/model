# Критический аудит collector/mutator memory protocol

Аудитор: Critic. Код: `/home/edmond/limelight/model`, `HEAD 16fc741`.
Документ: `model/dev/COLLECTOR-MUTATOR-MEMORY-PROTOCOL.md`.
Статус: аудит редакции с утверждениями Q1–Q9, F1–F5, S1–S3, R1–R2,
C1–C6, P1, V1–V3 (2026-09-09). Runtime не менялся.
Независимо повторён queue suite: 41 passed. Miri/TSan не запускались.

## Обозначения

- **implemented** — непосредственно исполняемый механизм в текущем коде.
- **RFC** — нормативное намерение; это не свидетельство реализации.
- **proposed** — новое предложение документа, для которого не предъявлен код.
- **unproven** — нужная гарантия заявлена, но протокол/доказательство отсутствует.
- **wrong** — утверждение о текущем механизме неверно либо приведён конкретный
  контрпример без дополнительных предусловий.

Кодовые ссылки ниже относительны к `model/`; RFC-ссылки относительны к корню
`/home/edmond/limelight`. Предлагаемый протокол не становится реализованным
оттого, что в вводной части документа стоит слово «проект».

## Проверенные факты, определяющие вывод

1. Синхронный detach существует: `src/cycle/queue.rs:815` забирает
   `write_segment` и `write_len` посредством двух последовательных `Cell::replace`.
   `OwnerCycleState` содержит обычные `Cell`, `queue.rs:198`; регистрация пишет
   обычный pointer и затем fill (`queue.rs:317`). Это **не** concurrent exchange.
2. Detached batch исключает overflow (`queue.rs:800`). Drop активной трассировки
   сначала очищает строки, затем вызывает `merge_candidates`, потом закрывает
   окно/возвращает withheld slots, потом reset scratch
   (`src/cycle/deferred_slot_reuse.rs:585`).
3. `merge_candidates` (`queue.rs:871`) сохраняет active lane, splice полных
   сегментов, partial head копирует через `append_entry`. Это может вызвать
   allocation/growth, spare/reserve/overflow; full overflow завершает процесс
   (`queue.rs:417`). No-allocation merge ещё не реализован.
4. Нынешний trace-window — TLS `DEFERRED_RETURNS`, а `ActiveTrace` намеренно
   non-Send (`deferred_slot_reuse.rs:147,405`). При освобождении удерживаются
   только сущности в **уже отмеченных** блоках, отдельная inline-row проверка
   больших сущностей (`deferred_slot_reuse.rs:681`). Это не owner-wide fence.
5. `ll_free` ставит `DEAD_IN_PLACE`, затем обрабатывает reset, затем candidate,
   затем trace-window, потом физический allocator free
   (`src/memory/stdapi.rs:386,405,419,455,464`). Dead candidate сейчас остаётся
   удержанным; production retirement отсутствует
   (`src/refcount.rs:1127`, `src/memory/stdapi.rs:444`).
6. `CollectingThread::take` проверяет только `COLLECTING` и
   `reset_window::is_open` (`src/cycle/collect.rs:109`). Общего
   `TEARDOWN_DEPTH` в `model/src` нет. S38.4/RFC требуют его в будущем.
7. Ordinary commit использует `Membership::Rows` до закрытия ActiveTrace
   (`collect.rs:178–199`). Pressure harvest закрывает trace до commit
   (`collect.rs:356–387`), список ограничен 1024
   (`src/cycle/members.rs:62`). При overflow берётся меньше корней и трассировка
   повторяется; при одном слишком большом графе pressure сдаётся
   (`collect.rs:274–295`). Частичный список для sever не используется.
8. `reclaim` резервирует все external drops до первого sever
   (`src/cycle/reclamation.rs:162`), sever всех членов (`:179`), release всех
   guards (`:222`), external drain (`:224`), discharge component (`:231`).
   Раннего retirement между этими последними фазами нет.
9. Outside buffers обходят entity fence: retained payload может вернуть блок,
   buffer chunk — free_chunk; код прямо называет S38.3 незавершённым
   (`src/memory/buffer_arena.rs:952–980`). `PlainCells` делает plain `.read()`
   (`src/cells.rs:265–283`). Атомарный lifetime-флаг не исправляет A1.
10. Thread exit сейчас assert-ит отсутствие trace/standing-members, а не ждёт
    worker (`src/memory/heap.rs:1682–1687`), затем теряет queue records без
    clearing candidate bits (`src/cycle/queue.rs:1018,1034`). S39.1 открыт.

## Конкретные граничные случаи

### F1. Atomic free-флаг без handshake

Мутатор прочёл `fence=false` → коллектор записал `true` и начал чтение →
мутатор завершил физический free, разрешённый ранее. Sequential consistency
одного флага не убирает этот interleaving. Нужен указанный handshake либо
доказанная синхронизация со всеми free-operation critical sections.

### F2. Удержание только посещённых блоков

Worker читает `A.child=B` → мутатор убирает ссылку и уничтожает B → блок B ещё
не stamped, текущий `classify` разрешает reuse → worker читает B. Поэтому
нынешний touched-only window нельзя включить удалённым вызовом и считать
конкурентным fence.

### F3. Slot удержан, данные обхода уже освобождены

Worker получил table pointer из Array → мутатор освобождает/меняет table →
`buffer_free_longlived_payload` возвращает chunk/retained block → worker читает
таблицу. Сохранённый заголовок Array ситуацию не исправляет. Нужны lifetime
внешнего хранилища и законные concurrent reads его структуры.

### F4. Intrusive link портит содержимое читаемого объекта

Worker решил читать class pointer Object+8 → мутатор довёл объект до free →
нынешний deferred-return stack записал next по Object+8 → worker получил next
как class. Нужен иной учёт или специальный reader/death protocol; простое
расширение current stack на background не годится.

### F5. Два слова очереди не дают concurrent detach

Worker заменяет head → мутатор начинает запись в старый/новый segment,
используя ранее прочитанные head/fill → worker заменяет fill → publication
попадает не в переданную границу. Более того, обычные Cell/read/write уже data
race. Atomic только head не исправляет второй mutable word. A2/S8.7 открыт.

### F6. Merge с частичным head при полном отказе памяти

У active head нет места, detached head partial. Нынешний merge пытается
append_entry каждого элемента до отдачи detached head в spare. При пустых
spares/reserve/pool идёт в overflow; при заполненном overflow abort.
Следовательно, «merge/finish без allocations» нельзя отнести к current code.

### F7. refcount=0 ещё не завершённая смерть

У default Object собственный user destructor защищён временным +1
(`object.rs:580–600`), поэтому утверждение «он всегда выполняется при нуле»
было бы неверным. Точный сценарий: родитель закончил user destructor, снял
guard и стоит при нуле в phase 2; освобождение ребёнка запускает **ребёнков**
destructor (`object.rs:626–633`), тот аллоцирует и может войти в inline GC.
Retirement родителя только по zero вернул бы его память до возврата в
незавершённый dispose родителя. `DEAD_IN_PLACE` ставится на входе `ll_free`,
после dispose (`object.rs:689–706`). Проверка нужна вместе с доказанным
владением удержанным адресом и отсутствием других readers.

### F8. Повторный free с DEAD_IN_PLACE

Снять candidate и вызвать ll_free недостаточно: `take_slot_for_free` отвергнет
собственный уже установленный DEAD_IN_PLACE (`refcount.rs:1058`). Снять mark
нужно в hand-back той же отложенной операции; нельзя повторно занести слот в
несколько deferred chains или читать его после возврата.

### F9. Rows остаются, а память блока возвращена

Обычная membership перечисляет retained members через survivor list и large
members через block header (`membership.rs:172`, `row.rs:261`). Последний free
может вернуть block и holder survivor list (`retained.rs:259`). UAF возникает
на продолжении перечисления/очистки даже без новой allocation. Сначала последний
row reader и unstamping всех соответствующих pointers, затем physical returns.

### F10. Early pressure cleanup только на успешном sever

В успешной ветке все external children сохраняют displaced counted references
до `drain_drops`; поэтому после ALL sever/ALL guard release можно закончить
membership, retire queue, затем запустить external destructors. Однако при
`reserve_drops` failure или resurrection `release_guards` само может запускать
внешние destructors (`finalization.rs:769`). Там обещание раннего cleanup до
первого external destructor неверно; требуется обычная завершающая очистка.

### F11. Reset из деструктора собственного OWNER_COMMIT

Если правило «reset сначала ждёт закрытия задания» применяется буквально:
OWNER_COMMIT → destructor → reset → wait OWNER_COMMIT → тот же destructor не
возвращается. Нужна отдельная не ожидающая собственного задания ветка и
определённая семантика запрета/отсрочки/допустимого независимого reset.

### F12. Ожидание token внутри собственной сборки

OOM внутри GC destructor не вправе ждать завершения той же сборки. Нынешний
COLLECTING закрывает рекурсивный collect, reset_window закрывает collect из
reset. Общий запрет ordinary teardown ещё не реализован. Проверка eligibility
должна предшествовать ожиданию, а не находиться после него.

### F13. Finished worker, незавершённый owner

Worker завершил SCAN и отпустил старый trace token → новый worker перезаписал
block row pointers → owner читает старую membership. Поэтому отдельное состояние
busy/result lifetime должно переживать token release и запрещать новый trace.
То же относится к exit/adoption в READY: readers worker закончились, owner rows
ещё именуют старые блоки.

### F14. Передача чужого TLS workspace

Worker положил raw `ActiveTrace` в inbox, owner drop-нул его: `LentWorkspace`
возвращает блок в **текущую** thread queue (`arena.rs:175`), returns window
читает текущий TLS, accounting и critical reserve также имеют thread state.
Нужен самостоятельный owning payload и маршрут возврата. `gc_metadata.rs:46`
отдельно предупреждает о тестовом per-thread ledger при cross-thread return;
global counters атомарны, это не утверждение об underflow production ledger.

### F15. Отмена теряет живые/непросмотренные roots

Пакет содержит только найденный garbage → остальные detached candidates
больше нигде не записаны → их candidate bit мешает новой регистрации →
перманентный miss. Cancellation/ACK обязан сохранить весь detached batch,
включая partial/unprocessed записи; активная очередь и overflow не заменяются.

### F16. Pressure membership overflow

Один root достигает >1024 unreachable members. Копирование первых 1024 и sever
части не лицензировано: это может быть не замкнутый набор. Current code очищает
overflowed harvest, уменьшает root bound, сдаётся при одном root. Документ обязан
сохранять этот отказ, а не обещать progress каждым pressure round.

### F17. Завершение worker не может требовать owner-progress

Owner уже остановил программу и ждёт ACK. Worker пытается выделить result
buffer, дождаться свободного inbox/owner pickup/user lock → deadlock/отказ
finish. Заранее выделенный capacity-one packet снимает этот конкретный цикл,
но это proposed resource contract, не current implementation. Work budget не
гарантирует wall-clock wait без планирования worker ОС.

## Проверка осуществимости уплотнения

Нельзя объявлять allocation-free linear compaction невозможным только потому,
что current merge его не делает. Проверена **абстрактная** схема:

1. Читать существующие сегменты по их старым заполненным диапазонам.
2. Плотно писать живые записи в существующие segment slots, сохраняя каждый
   source pointer перед записью. Destination physical cursor не обгоняет source.
3. Затем прочитать overflow и заполнить остаток segment capacity; только
   избыток сохранить в overflow. Он не превышает исходный overflow length.
4. Последний занятый output segment сделать head с соответствующим fill;
   предшествующие output segments полные и идут за ним. Пустые segments вернуть.

Exhaustive Python simulation в этой сессии: **29 748 случаев**, capacity 1–4,
0–3 segments, overflow 0–3, keep/drop masks до 11 исходных записей. Проверены
отсутствие потери/дубликатов, read-before-overwrite, число записей и сохранение
условия head-partial/rest-full. Ошибок не найдено.

Это НЕ production test, не доказательство raw-pointer provenance/memory
ordering и не background SPSC test. Схема меняет порядок candidates; это нужно
разрешить явно, а root-bound fairness проверить отдельно. Ещё нужны корректные
gc_metadata charges, spare management, обработка нескольких исходных partial
heads, unwind, lifecycle физически возвращаемых сущностей. Поэтому линейная
очистка осуществима как предложение; существующий merge ею не является.

## Расхождения источников

- `rfc/model/gc/rc-cycle.md:599` и Y12 требуют worker detach, но одновременно
  `rc-cycle.md:614`/Y12 S8.7 признают concurrent linearization незавершённой.
  Это не запрещает обсуждать альтернативу owner detach, но не делает её уже
  согласованной с пользователем или нормой RFC.
- Y12 clause 2 (`cycle/questions.md:843`) ещё содержит dispose-rather-than-restore;
  последующая поправка (`:974`) и актуальный `rc-cycle.md` описывают merge.
  Реальный `queue.rs:871` делает merge. Старую фразу нельзя использовать против
  существующего кода как действующую спецификацию.
- `rc-cycle.md` Zero-count section всё ещё описывает fixed 1024 deferred records
  и overflow marks. Текущий `deferred_slot_reuse.rs` использует intrusive stack
  всех withheld entity slots; 1024 осталось в **pressure membership**, не в этом
  стеке.
- `model/PLAN.md:3264,3304,3331,3364` содержит открытые S38.0/A1, claim, gate и
  free deferral. Их будущие acceptance clauses не являются выполненными тестами.

## Матрица каждого утверждения исправленного документа

Строка таблицы относится ко всему помеченному пункту, включая следующие за
ним абзацы до следующего ID. Составные пункты разделены на факты кода и новые
обязанности. Обозначения совпадают с легендой выше.

| ID | Проверенные заявления | Статус и доказательство / ограничение |
|---|---|---|
| Q1 | Один writer, TLS locator, 64-byte OwnerCycleState, Cell head/fill, plain entry write | **implemented**: `queue.rs:198–240,248,317–330`. Header atomics не синхронизируют эти другие слова — верное ограничение; concurrent invocation не реализован. |
| Q2 | 64 KiB segment, 8160 slots x86-64, head fill/rest full, 8152 overflow | **implemented**: `queue.rs:154–166,767`; BLOCK_PAYLOAD=65280. Формулы соответствуют числам. |
| Q3 | decrement → nonzero gate → bit → queue; повтор без дубля; queue не strong | **implemented**: `refcount.rs:777–815`; candidate bit входит в gate mask. `append_entry` не делает retain. Уникальность предполагает корректное использование registration API; raw internal append сам не дедуплицирует. |
| Q4 | growth spares → critical → overflow; full overflow abort; overflow drain с конца | **implemented**: `queue.rs:338–395,417,666`; refill при init/poll — `heap.rs` thread init, `queue.rs:981`, `gc.rs`. Фраза «границы должны предотвращать исчерпание» — **RFC/unproven** как глобальная гарантия runtime; это обязательство производителя barriers, не доказательство current queue. |
| Q5 | detach двух слов, overflow остаётся, новый lane, Drop отвергает потерю | **implemented**: `queue.rs:815,736`; Drop намеренно не паникует повторно при уже идущем unwind. Он сигнализирует потерю, а не автоматически восстанавливает batch. |
| Q6 | walk не pop, MARK/SCAN одинаковый prefix, новые записи не расширяют batch | **implemented**: `queue.rs:730,767`; `trace.rs:80–127` фиксирует traced и сканирует ровно столько. При MARK failure SCAN не выполняется; при SCAN failure scan prefix прекращается — «дважды» относится к успешной трассировке. |
| Q7 | merge сохраняет active lane, full splice, partial copy может потребовать память | **implemented**: `queue.rs:871–949`. Обобщение no-allocation на partial merge было бы **wrong**, см. F6; исправленный текст этого не делает. |
| Q8 | RFC reader-detach; A2/S8.7 открыт; current Cells нельзя вызвать cross-thread; два atomics недостаточны | **RFC + unproven**, корректно помечено. `rfc/model/gc/cycle/questions.md:831–862`, `rfc/dev/ALGORITHM-AUDIT.md:56`, `model/PLAN.md:3304`. Пример head/fill логически демонстрирует отсутствие pair invariant; current Rust уже имел бы data race. Новая mandatory owner-detach схема не навязывается. |
| Q9 | Y12 старая disposal clause против новой merge clause; current merge | **implemented/RFC inconsistency**: Y12 `:843` vs `:974`, `rc-cycle.md:549`, `queue.rs:871`. Утверждение верно. |
| F1 | dispose → DEAD → reset → candidate → trace → allocator, occupancy при actual return | **implemented** для рассматриваемого object/free пути: `object.rs:689–706`, `stdapi.rs:386–468`, `heap.rs:1044–1048`. Remote free отдельно откладывает owner occupancy update; это не обещание immediate used decrement у foreign free. Порядок reset перед candidate действительно существенен. |
| F2 | intrusive byte8 stack, workspace Cell/TLS, ActiveTrace non-Send | **implemented**: `deferred_slot_reuse.rs:113–158,332–343,405–438`. Не является shared owner-wide protection — верное отрицание. |
| F3 | touched-only classify, inline row large, untouched returns | **implemented**: `deferred_slot_reuse.rs:681–753`. Синхронный механизм нельзя автоматически переносить на worker; см. F2/F4 edge cases выше. |
| F4 | buffer/retained отдельные return paths, stdapi-only fix недостаточен | **implemented gap + RFC S38.3**: `buffer_arena.rs:952–980,767` явно пишет intrusive remote chunk links вне entity gate. `PLAN.md:3364`. |
| F5 | zero root skipped without fields, запись остаётся, batch merge, retirement absent/exit gap | **implemented limitation**: `mark.rs` schedule_root_if_unvisited; `queue.rs:730,1018`; `deferred_slot_reuse.rs:585`; `refcount.rs:1127`. Нулевой header всё-таки читается; речь именно о полях тела. |
| S1 | normal Rows через commit, sweep before merge/returns | **implemented**: `collect.rs:178–199`, `membership.rs:172`, `deferred_slot_reuse.rs:585–621`. Вставка retirement после sweep — **proposed**, совместима с lifecycle при соблюдении R1 и allocation-free queue restoration. Ошибочный partial trace не даёт permission финализировать, но его завершённые смерти могут быть retired на clean boundary. |
| S2 | pressure harvest1024, rows/scratch gone before commit, overflow retrace smaller prefix | **implemented**: `members.rs:62`, `collect.rs:274–295,356–387`. При overflow список пуст; при одном чрезмерном root pressure прекращает попытки. Не обещает успех или бесконечное дробление. |
| S3 | ранний cleanup после ALL sever/ALL release, до external drain | **proposed**, фазово допустим на success path `reclamation.rs:162–231`: displaced children держат counted refs. Current API не делает retirement. Component discharge/StandingMembers lifetime потребуется изменить, COLLECTING сохранить. |
| S3 | нельзя reset/sweep arena с непустыми DeferredDrops; failure/resurrection отдельная ветка | **implemented prerequisite + proposed integration**: `arena.rs:643–666` проверяет/rewind drops; `finalization.rs:775` может вызывать user code при снятии guards. Исправленная success-only оговорка нужна и верна. |
| S3 | cached member count до consume, один набор без SCC | **implemented shape/proposed API**: `collect.rs:434–463` делает один commit. Нынешний `members.len()` после reclaim сам по себе безопасен: cached Rows count или slice length (`membership.rs:98`); замена мотивирована будущим consuming API, не найденным UAF. Участники отклонённого после resurrection набора тоже могут умереть при снятии guards. |
| R1 | exclusive queue ownership, no readers/rows/reset, live keep bit, dead zero+DEAD, publish live bounds before free | **proposed**, базовые операции есть (`refcount.rs:1058,1087,1145`; `stdapi.rs:455`), production retirement нет. Предусловие readable **withheld** candidate slot должно быть доказано отдельно; один DEAD на произвольном адресе недостаточен. Reset absorption caveat ниже. |
| R1 | временный byte8 dead list, candidate arm не дублирует WithheldReturns, без Vec | **proposed**, корректен при указанных clean-boundary условиях и candidate bit, сохраняемом до публикации новых queue ranges. `stdapi.rs:455` возвращает до stack arm. Требуется pop next-before-free, как `deferred_slot_reuse.rs:252`. Unwind/error ownership пока **unproven**, в тексте честно оставлен проверке. |
| R2 | reverse chain → two-cursor pack → reverse used prefix, head partial/rest full, zero/full cases | **proposed; abstract algorithm verified**: дополнительно выполнена точная linked-node simulation описанного R2, 5260 exhaustive случаев, `dev/tools/check_memory_protocol_compaction.py`. Read/write cursors не затирают непрочитанное; использованные и лишние nodes образуют partition. Это не runtime/provenance проверка. |
| R2 | O(entries+segments), no new segment for one-chain pack, overflow отдельно, order may change | **proposed; structurally supported** количеством линейных проходов и monotone writer. Два reversals меняют head-filling placement; не требуется сохранение исторического queue order. Несколько partial chains, gc_metadata и безаллокативный merge остаются **unproven**, как текст и указывает. |
| C1 | worker selects owner, token single reader, writer continues, capacity-one inbox, owner exact commit | **RFC**, не current code: `rc-cycle.md:504–619`; current S38 открыт. Диаграмма — обязанности, не concurrency implementation. Publication inbox pointer само не ACK; исправленный поясняющий абзац требует окончания worker accesses. |
| C2 | synchronized gate/inbox/claim, no second trace until commit end, token released before destructors | **proposed + RFC base**, совместимая необходимая гарантия, но atomic state transitions/linearization **unproven**. Текущий COLLECTING — local Cell; заменить им межпоточный gate нельзя (`collect.rs:68`). |
| C3 | coarse owner-domain fence, unvisited blocks/outside buffers, no strong refs, old free race | **proposed/unproven**: current `classify` уже, buffers bypass. Фраза о необходимых free/claim согласованиях верна; одно flag-store не устраняет F1. Конкретного механизма в тексте сознательно нет. |
| C3 | свободные до окна слоты usable, новые deaths held, независимые owners unaffected | **proposed**, непротиворечиво при корректной защите allocation identity и доказанной независимости domains. Это не current implementation и не доказательство disjointness. |
| C4 | READY/result holds beyond token; release only after queue/reset/reader obligations | **RFC A3 + proposed lifetime contract**; current window демонстрирует два локальных удержания, но межпоточного ACK нет. Reset — третья причина удержания в реальном stdapi; текст её учитывает. |
| C5 | intercept buffers before links/occupancy; no overwrite worker-readable byte8; metadata capacity before death | **proposed/unproven** с прямыми опасными местами `buffer_arena.rs:767`, `deferred_slot_reuse.rs:332`, `cells.rs:376`. Сам storage layout и безотказная ёмкость не предъявлены, поэтому рабочим free protocol это не считается. |
| C6 | all roots returned; tagged entries need decode; roots ≠ all members | **RFC/proposed**, факты current code подтверждены: `queue.rs:27,767` младшие биты только reserved, masking нет; `mark.rs` traverses descendants, `membership.rs` строится по rows/list. Tagged pointers прямо current walker передавать нельзя. |
| C6 | owning rows result independent of TLS and next worker tasks, failure/return manager open | **proposed/unproven**: `ActiveTrace` non-Send, `arena.rs:175` returns thread-local workspace, reserve/accounting routing needs explicit owner. Текст правильно не объявляет existing raw queue готовым result lifetime. |
| P1 | owner pressure waits worker then recover all entries; complete-list fast path or discard/retrace | **proposed**. Требуется gate closure перед ожиданием, no owner-needed ACK, rows cleared before inline trace. Full membership >1024 должна попасть в fallback/отказ; incomplete prefix нельзя без проверки назвать full result. Existing single-thread fallback `collect.rs:248`, worker/inbox отсутствуют. |
| P1 | entity alloc unconditional one retry; actual COLLECTING/reset eligibility; generic teardown gate future | **implemented + RFC future**, теперь корректно разделено: `heap.rs:2183`, `collect.rs:109`, `PLAN.md:3331`. Parent phase2 zero-before-DEAD — точный counterexample F7 выше. |
| P1 | no mandatory cancellation, worker trace no user code/wait pickup, no wall-time/success guarantee | **RFC/proposed contract** для будущего worker. Current PlainCells/OutsideCells предполагают tracing hooks без user side effects; worker implementation отсутствует, поэтому отсутствие waits пока не выполненный тест. CPU scheduling limitation верна. |
| V1 | 41 queue tests, fixture caveat, independent TLS not shared queue concurrency, no Miri/TSan | **verified** независимо: `cargo test --lib cycle::queue:: -- --test-threads=4 --quiet` → 41 passed, 0 failed, 0 ignored. `queue/tests/the_tokens_every_lane_holds.rs`, `what_gc_owns.rs` подтверждают per-thread/raw-fixture характер. Это не фоновые SPSC-тесты. |
| V2 | listed acceptance scenarios | **proposed tests**, не результаты. Все сценарии относятся к реальным границам кода; дополнить reset-before-retained-index сценарием ниже и default-parent phase2 вариантом zero+unfinished. |
| V3 | PlainCells, A1/torn/mut alias lifetime, reset/exit/adoption/sharing/leftover live candidates | **implemented gaps + RFC prerequisites**: `cells.rs:265–283`, `buffer_arena.rs:952`; A1/A4/B3/B4/C3 в `rfc/dev/ALGORITHM-AUDIT.md`; thread exit `heap.rs:1682`, `queue.rs:1018`. Correct conditional prerequisites, не solved. |
| V3 | reset inside own commit cannot wait self; current GC-from-reset refused, reset-from-GC possible | **implemented behavior + valid counterexample**: `CollectingThread::take` только проверяет открытый reset, обратного запрета не ставит; `promote.rs` reset routines не читают COLLECTING. Literal wait-own-commit deadlocks F11. Исправленный текст оставляет семантику будущего worker reset открытой. |

## Дополнительная незакрытая проверка: candidate, умерший внутри reset

Это **не подтверждённая воспроизведением ошибка** и не готовое исправление.
Но перед реализацией retirement требуется доказать не только отсутствие
активного reset в момент sweep, но и readability всех записей, переживших reset.

Потенциально существенная последовательность видна в разных участках кода:

1. `promote.rs:237–248` меняет survivor category на GcHeap до deferred-release
   drain (`:286–304`).
2. Ненулевой `ll_release` подходящей сущности может зарегистрировать candidate
   (`refcount.rs:801–815`); следующий release может довести её до нуля.
3. `stdapi.rs:419` даёт reset absorption приоритет перед candidate arm.
4. `retained::register`, `retained.rs:128–145`, не считает DEAD_IN_PLACE
   survivor занятым (`:302–305`); пустой блок может возвращаться по reset пути.

Нужно проверить, достижима ли такая комбинация ссылок в поддерживаемом reset,
и чем сохраняется адрес candidate до queue retirement. Одного R1-предусловия
«reset сейчас не открыт» недостаточно как доказательства сохранности после
его окончания. Ни подтверждённый UAF, ни исправление в этом аудите не заявляются.

## Итог аудита

В финальной редакции основного документа учтены найденные неточности:
существующий gate не приписывается всему ordinary teardown; early pressure
ограничен success path; смерть участника отклонённого набора при снятии guard
не исключена; сохранённый len не объявлен исправлением существующего UAF;
reset self-wait назван нерешённой границей; readability/identity удержанного
candidate является явным предусловием, а promoted-candidate reset case —
обязательной проверкой, не установленной ошибкой.

Исправленный текст в основном честно различает existing code/RFC/proposal.
Single-thread normal/pressure phase split опирается на существующий код;
candidate retirement и ранний возврат ещё proposed. Конкретный R2 не отвергнут
по предположению: его абстрактная форма проверена и ошибок не показала.
Concurrent схема остаётся набором контрактов с открытыми A1/A2/A3/A4,
handshake linearization и resource layout. Ни CAS-флаг, ни final validation,
ни 41 зелёный queue test не доказывают concurrent lifetime/read protocol.
