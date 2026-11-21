# Lean proofs for the K0 partition scheduler

These are machine-checked proofs about **models** of the K0 scheduler. They are
proofs about the models, not about the Rust code, and they make no claim of
certification or compliance. One host test ties the Rust code to one of the
models on recorded inputs. It is a test, not a proof.

## Files

| File | What it is |
|---|---|
| `K0Part.lean` | A model of `Core` of the `k0-part` crate (`tick`, `poll`, `block`, `yield_now`, `set_table`, with the tick rule of `k0-slice`) across several domains |
| `K0Share.lean` | A model of one domain on its own timeline under the tick rule, with a bound on the CPU share of a task that never gives up the CPU |
| `K0ShareWide.lean` | The same results for a wider model of that domain. It imports `K0Share.lean` |
| `K0Bridge.lean` | Links the two models. It imports `K0Part.lean` and `K0Share.lean` and changes neither |
| `K0Close.lean` | Statements that pin down definitions the other files leave free. It imports `K0Part.lean` and `K0Bridge.lean` and changes neither |
| `ModelVectors.lean` | The executable `vectors`, which computes what `K0Part.lean` gives for recorded event scripts |
| `../../crates/k0-part/src/model_vectors.txt` | The recorded scripts with the model's result for each |
| `../../crates/k0-part/src/model_tests.rs` | The host test that runs those scripts through the Rust `Core` |

The first five files hold the proofs. All Lean files use core Lean only (no
Mathlib, no Std, no other dependency).

## What `K0Part.lean` proves

State: a table with a pending table and an epoch, per-domain ready queues,
per-domain waiting lists with optional deadlines in global ticks, the running
task with its tick-rule exemption bit. Events: `tick n` (a jump is one step),
`poll n`, `yield`, `block`, `wake t` (own domain), `wakeX t` (a cross-domain
wake, a separately labelled event), `setTable t`. The four task events
(`yield`, `block`, `wake`, `wakeX`) do nothing while no task runs, so only a
tick or an idle `poll` can queue a task of an idle domain.

- `pinv_pstep`, `inv_boot`, `inv_step`, `inv_exec`: the invariant `Inv` holds
  at boot and is kept by every event. Part of it is `PInv`: a pending table
  always starts after the last processed tick, and the epoch is never after
  it. The Rust code relies on both without stating them (`Core::set_table`
  does not look at the pending table it replaces, `Core::tick` computes
  `n - epoch` without a check). `setTable_now_needs_pendAfter` shows what
  would go wrong without the first.
- `guard_sound`, `guard_sound_event`: `set_table` after tick `n` leaves the
  domain of tick `n + 1` unchanged, provided a pending table starts after `n`
  (`PendAfter`, part of `Inv`) and none starts exactly at `n + 1`. Without the
  second hypothesis the statement is false: `guard_unsound_without_hyp` is a
  counterexample on a state reached from boot (`exP_reachable`: tick 1, a
  `set_table` call and tick 2 reach the state; a second call in tick 2 then
  discards the pending table the first one made), and there the decision
  itself flips (`guard_decision_flips`). Anything that arms a guard for tick
  `n + 1` before the second call must be recomputed after it.
- `plan_setTable_ge`, `plan_setTable_lt`, `plan_setTable_now`,
  `table_effective_at`: a table set after tick `n` governs ticks `>= n + 2` at
  position `(k - (n + 2)) mod cycle`, until a later call
  (`first_table_never_governs`); ticks `n` and `n + 1` keep the old plan (same
  hypothesis as above for `n + 1`).
- `cycle_eq_sum`, `domAtPos_window`, `window_cover`, `domAt_window`,
  `plan_setTable_window`: the layout of a table. Entry `i` governs exactly its
  window, the windows follow in the order of the entries, and the table
  repeats.
- `running_domain_function`, `running_domain_same`, `tick_jump_domain`: the
  running domain is the planned domain of the last processed tick, a function
  of the public events (new ticks and `set_table` calls) and the initial clock
  part only. Queues, waits and task actions do not enter.
- `no_slack_donation`: after any events from an `Inv` state, the running task
  belongs to the running domain, and if nothing runs the running domain's
  ready queue is empty. The model never lets one domain use another's window.
- `serve_wakes_due`, `serve_due_order`, `tick_serves`, `tick_frame`,
  `tick_old_domain`, `poll_serves`, `poll_frame`, `block_keeps_deadline`,
  `waiter_leaves_only`: timed waits do fire, and only for the domain that
  runs. A new tick takes every waiter of the domain that runs after it whose
  deadline has passed out of the waiting list: it is then in that domain's
  queue or is the task that runs. `serve` queues the last waiter first. No
  theorem here states the order of the queue after a whole tick. A new tick
  touches no domain that neither ran before nor runs after. A waiter leaves
  its list only when its domain is served at or after its deadline, or by a
  wake. Each of these is about one step and about tick indexes: none says when
  a tick comes, or that one comes.
- `step_tasks_perm`, `exec_tasks_perm`, `nodup_step`, `nodup_exec`,
  `step_tasks_mem`, `exec_tasks_mem`: no task is lost and none is duplicated.
  From a state with `Inv` and without duplicates (`NoDup`), the tasks of every
  domain (running, ready, waiting) after a run are a permutation of the tasks
  before, and `NoDup` is kept. `wake_needs_nodup` shows the hypothesis cannot
  be dropped. `wake_effect`, `wakeX_effect`: a woken task goes to the back of
  its own domain's queue.
- `noninterference`, `noninterference_final`: for any domain `D`, two `Inv`
  states related by `R D` (same clock part and running domain, same queue and
  waiting list of `D`, same running task and slice bit when `D` runs, all
  other domains arbitrary) and two event lists related by `Sync` give `D` the
  same view (its queue, its waiting list, which of its tasks runs with the
  slice bit) after every step, and `R` is preserved. `Sync` compares the two
  lists place by place:
  - A clock event (a tick index not seen before, or a `set_table` call) is the
    same public event in both runs. A new tick may reach one run through
    `tick` and the other through the idle loop's `poll` (`poll_eq_tick`), so
    the runs may differ in whether another domain idles.
  - Every other event is local to the running domain: an action of its task,
    its idle loop taking an interrupt without a new tick, or a repeated tick
    index. While `D` runs the local events are the same in both runs. While
    another domain runs they are unrelated, and `tick 0`, which does nothing,
    pads the run that has fewer (`tick_zero_noop`).
  - No `wakeX` into `D` in either run. This is the reading of "no shared
    capability": no task outside `D` can wake a task of `D`. A `wakeX` from
    `D` outward is allowed and gives `D` nothing back.

  The hypotheses fix the tick indexes (jumps included) and the `set_table`
  calls as equal in both runs, so the theorem does not explain them.
  `tick_indexes_needed` and `cross_wake_breaks` show that these two
  hypotheses are needed.
- `example_applies`, `example_views_equal`, `example_effects`,
  `example_kernel_shaped`: a concrete pair of runs that satisfies every
  hypothesis and keeps to the kernel's call pattern (`poll` only while nothing
  runs). The other domains have different tasks, wake different tasks, and
  idle at different times; the views of `D = 1` are equal at every step and
  the views of domains 0 and 2 differ.
- `serve_only_own`, `serve_not_early` are read off the definition of the
  model's `serve`. They say nothing about the kernel's `serve` callback.

No theorem in this file constrains the exemption bit beyond its bookkeeping.
That is the subject of `K0Bridge.lean`, and of `K0Close.lean` for the idle
paths that the bridge leaves out.

## What `K0Share.lean` proves

Model: one domain, a ready queue, a tick every `P` steps, the exemption bit,
and an adversary choosing any finite list of `run`, `yield`, `block`, `wake`
events. Yield, block and wake take no time and may occur between any two
steps, including one step before a tick and right after it, but not between
the last step of a period and its tick. Task `0` is a "spinner" that never
yields or blocks, is never stopped or deleted, and is in the system from the
start state on. At most `N` tasks are ready or running at one instant: this is
an assumption, and the model drops a wake above `N`. The start state is any
well-formed state (`Wf`).

- `share_bound`, `share_bound_diff`, `share_bound_prefix`:
  `time <= N * ran + (N - 1) * P + c0`, where `c0` is the number of steps to
  the first tick. The bound quantifies over all lists of these four events.
  For a spinner that becomes ready after the start, use `share_bound_diff`
  from that instant.
- `no_early_cut`, `exec_no_early_cut`: a task is cut by a tick only after it
  has run at least `P` steps since it got the CPU. For the task that runs in
  the start state the count starts from the `age` that `Wf` allows, so for
  that task the conclusion is the assumption. A task that gets the CPU out of
  idle is not covered (the model has no idle).
- `wait_bound`: a ready task with `k` tasks ahead of it (the running task
  included) has started to run within `(k + 1) * P` steps.
- `step_time`, `exec_time`, `step_ran`, `exec_ran_le_time`,
  `alone_time_eq_ran`, `step_age`, `step_cur_keeps`, `age_counts`,
  `started_sound`: the counters mean what the statements read as. `time` grows
  by one per `run`. `ran` grows by one exactly on a `run` taken while the
  spinner runs, never by more than `time`, and by as much as `time` when
  `N = 1`. `age` is 0 right after the CPU changes hands and counts the steps
  since. A task that "started" was at the head of the queue and became the
  running task.
- Tightness: two `decide` examples reach `t = (N - 1) * P + c0` with `ran = 0`
  (`N = 3, P = 2` and `N = 4, P = 3`). There is no tightness theorem for all
  `N` and `P`.

## What `K0ShareWide.lean` proves

Model: the domain of `K0Share.lean` with three behaviours of the kernel that
it leaves out. `step` lets one step pass and `tick` delivers the tick when the
countdown is 0, so yield, block, wake and remove may fall between the last
step of a period and its tick; no step passes while a tick is due. `remove i`
takes a ready task out of the queue from any position; the spinner is never
removed. `wake` has no cap: `N` is not in the transition function, and "no
state along the run has more than `N` tasks ready or running" (`Wide.Capped`)
is a hypothesis.

- `Wide.share_bound`, `Wide.share_bound_diff`, `Wide.share_bound_always`: from
  a state with a countdown `c0 <= P` and the spinner in the system, for every
  behaviour that keeps the cap, `time <= N * ran + (N - 1) * P + c0` at every
  instant of the run. `Wide.cap_needed`: without the cap hypothesis the bound
  fails.
- `Wide.contains` (`Wide.wexec_widen`, `Wide.capped_widen`): every behaviour of
  `K0Share.lean` from a well-formed state is a behaviour of the wide model with
  the same final state, and it keeps the cap. `Wide.share_bound_base`: the
  headline of `K0Share.lean` follows from the wide one. The converse fails
  (`Wide.tick_after_yield`, `Wide.tick_after_yield_not_base`,
  `Wide.remove_not_block_wake`).
- `Wide.no_early_cut`: when a tick takes the CPU from the running task, that
  task has run at least `P` steps since it got the CPU.
- `Wide.wait_bound`, `Wide.wait_bound_passed`, `Wide.wait_bound_tick`: a ready
  task with `k` tasks ahead of it becomes the running task no later than
  `(k + 1) * P` steps of time after the start, unless it is removed; at exactly
  that many steps the tick that is due starts it. Steps are counted by the
  `time` field. "After `(k + 1) * P` steps it has started" is false in this
  model (`Wide.wait_at_bound`): the last tick may be due and not yet
  delivered.

That a timer interrupt which becomes pending during a yield or a block is
taken before the next task has run is a reading of the kernel code, not a Lean
fact.

## What `K0Bridge.lean` proves

`Rel lab s k` relates a `K0Part` state in which a task runs to a `K0Share`
state: the label of the running task, its exemption bit, and the labels of the
running domain's ready queue. `lab` maps task ids to `K0Share` names; a task
with label 0 is the spinner. The countdown, `age`, `time` and `ran` are not
compared.

- Rule by rule (`sameStep_onTick`, `serve_wakes`, `tick_same`, `yield_step`,
  `block_step`, `wake_step`, `boot_start0`): a tick inside the running domain,
  a yield, a block, a wake and boot do to the projection what `K0Share.onTick`
  and `K0Share.step` do, for every state. The side conditions are explicit:
  the running task is not the spinner (yield, block), the queue is not empty
  (block), a woken task does not have label 0, the cap `N` is not reached.
  `yield_spinner_differs`, `block_spinner_differs`, `block_empty_idle` and
  `wake_cap_needed` show that the models differ where the first, the second or
  the fourth condition fails. No theorem in the file shows it for the third (a
  woken task with label 0).
- The window boundary (`cut_parked`, `enter_parked`, `tick_leave`,
  `tick_return`, `window_cycle`): `K0Share` has no window. In `K0Part`, from
  the tick that closes a domain's window to the tick that opens it again, the
  projection moves by one `K0Share.onTick` and then by the timed wakes served
  at the opening. The hypotheses of `window_cycle` are: `K0Part.Inv`; a task
  of the domain runs when the window closes (a domain that is idle at the
  closing tick is not covered); both ticks have new indexes; no task of the
  domain is woken across domains in between; the wakes served at the opening
  meet the side conditions of the rule lemmas (no label 0, below the cap `N`).
  A parked task with the mark keeps the front and loses the mark; a task that
  enters has no mark.
- One domain, run level (`run_projects`, `run_projects_prefix`,
  `timed_share_bound`, `boot_share_bound`): `K0Part` has no time between
  ticks, so the bridge adds a step in which time passes without a tick, a
  countdown and two counters. These are not part of `K0Part`. A timed run in
  which the tables name the running domain only, a tick falls every `P` steps,
  the spinner never yields or blocks, there is no `poll`, and the domain has
  at most `N` tasks, projects step by step onto a `K0Share` behaviour, and
  `time <= N * ran + (N - 1) * P + c0` holds for it, where `ran` counts the
  steps in which the task with label 0 is the running task of the `K0Part`
  state. The start state (`Start`) and the run (`OkRun`) carry more
  conditions than that sentence: `K0Part.Inv` holds; a task runs at the start;
  exactly one task among the running and the ready ones has label 0 and no
  waiting one has; the first countdown is in `1..P`; for `timed_share_bound`
  both counters start at 0. The `N` here counts the waiting tasks too
  (`Start.cap`), while the `N` of `K0Share.lean` counts ready and running
  tasks only, so the two are not the same assumption.

The rule lemmas pin the tick rule of `K0Part.lean`: with the mark not cleared
by a tick, no mark after a yield or a block, a mark at a tick entry, no mark
at boot, or a cut task parked at the other end, one of them is false. This is
a sentence about changed definitions, so no theorem in the files states it.

What it does not give: no share bound for a table with several domains (no
theorem counts a domain's steps across its windows); the time between ticks is
an addition of the bridge, and a jump of the tick index counts as one tick;
the idle paths of `K0Part` have no counterpart in `K0Share` (what the
exemption bit is on those paths is stated in `K0Close.lean`, for `K0Part`
alone); `no_early_cut` and `wait_bound` are not carried over;
`K0ShareWide.lean` is not bridged.

## What `K0Close.lean` proves

Every theorem of the other four files stays true under some changes of their
definitions. A change that no theorem notices is behaviour that no theorem
constrains. This file holds a statement for each such gap that a sensitivity
run found (see "Checks that are not in the repository" for the run, and the
ids in parentheses are the ids of its changes). Each statement is proved for
the definitions as they are. For every id except NV23 the run recorded a
concrete instance of the statement that is false under the change. The file
changes nothing in `K0Part.lean` or `K0Bridge.lean`.

- `R_of_view`, `viewTrace_get`, `viewTrace_length` (NV01, NV02, NV03, NV05):
  what `view` and `viewTrace` hold. The conclusion of `noninterference` is
  written with them and nothing else fixed them. Two `Inv` states with the
  same clock part and the same running domain whose views of `D` are equal
  are related by `R D`, so a view holds the queue, the waiting list and the
  running task of `D` with its slice bit, and nothing less. The view trace
  has one view per step: entry `k` is the view after the first `k` events,
  and the trace of a list of events has one entry more than the list.
- `tick_clears_mark`, `poll_keeps_running`, `poll_idle_mark` (ND25, NX03,
  ND16): the exemption bit on the paths the bridge leaves out. After a tick
  with a new index the running task has no exemption, also when the tick
  finds the domain idle and starts a task. A `poll` without a new tick leaves
  a running task alone, with its bit. A task that such a `poll` starts out of
  idle has the exemption.
- `poll_wl`, `tick_wl` (ND31): the waiting list after a step, in order. After
  a `poll` without a new tick the waiting list of the running domain is the
  old list without the waiters that were due, in the old order. After a tick
  with a new index the same holds for the domain that runs after it.
- `wakeX_idle_noop`, `wake_idle_noop` (NX01): a wake while no task runs
  changes nothing.
- `domAt_zero_cycle` (NT01): a table of zero ticks (empty, or only windows of
  0 ticks) gives the root domain at every tick. Rust rejects such tables, and
  the model accepts them.
- `boot_part` (NT09): at boot the table is the default table, it starts at the
  boot tick, the last processed tick is the boot tick, and no table is
  pending.
- `advance_epoch_le` (NV23): what the clause `epoch <= seen` of `PInv` is for.
  At a new tick `n` the epoch after `advance` is at most `n`, so the offset
  `n - epoch` that `Core::tick` computes does not underflow. No other theorem
  uses the clause.
- `blockIdleRust_pops` (NV28): `blockIdleRust`, the copy of `Core::block` that
  `blockIdleRust_eq` is about, does pop. With a task ready it starts that task
  with the exemption, so `blockIdleRust_eq` is not about a function that does
  nothing.
- `spin_idle` (NB11): `K0Bridge.spin` is 0 for a state in which no task runs,
  so the counter `ran` of `timed_share_bound` never counts an idle step as a
  step of the spinner.

Every `inst_<id>` is one concrete instance of the theorem above it, proved by
`decide`, and every `from_<id>` derives the same instance from the general
theorem. `one`, `one_inv`, `idleX`, `idleQ`, `bitIs`, `bitIs_of`, `enter_bit`
and `domAtPos_zero_cycle` are helpers for these.

These are more statements about the model, and they add nothing about the
Rust code. Six of the ids (ND16, ND25, NX03, ND31, NX01, NT09) are about
behaviour that the recorded results below already held, although no theorem
stated it. The others are about definitions that neither a theorem nor the
recorded results had fixed.

## Recorded results of the model

The proofs are about the model. This part is a test that ties the Rust code to
the model on a fixed set of inputs, and unlike the proofs it runs in CI.

- `crates/k0-part/src/model_vectors.txt` holds 1600 event scripts: a boot
  state, then ticks, polls, yields, blocks, wakes and `set_table` calls. Every
  line ends with the result the model gives for the script, a 64-bit hash
  (FNV-1a) of one canonical line per state, for the boot state and after every
  event. A line holds the last processed tick, the table in force and its
  epoch, the pending table and its start, the running domain, the running task
  and its exemption bit, every domain's queue and waiting list, and the
  decision `next_tick_switches`.
- `ModelVectors.lean` is the executable `vectors`. It runs the scripts through
  `K0Part.boot` and `K0Part.step` and computes the hashes. It imports `K0Part`
  and changes nothing in it. Its header comment defines the script format, the
  line and the hash.
- `crates/k0-part/src/model_tests.rs` is a host test of the crate `k0-part`.
  It runs the same scripts through the unmodified `Core<u64>`, builds the same
  lines from the fields of `Core`, and compares the hashes. CI has no Lean, but
  it runs this test with the other host tests (`tools/host-test.sh`), so it
  fails as soon as the Rust code and the recorded results differ. On a
  mismatch the test prints the script and the lines `Core` gave.

### What a passing test shows and what it does not

It shows that `Core` and `step` gave the same line after every event of the
recorded scripts. The scripts take every branch of `Core::tick`, `poll`,
`yield_now`, `block` and `set_table` and both wake events, with jumps of the
tick index, repeated indexes, two `set_table` calls between two ticks and in
consecutive ticks, single-domain tables, idle stretches, several waiters
expiring at once and deadlines in the past. The test counts the branches
itself and fails on a file that reaches one of them fewer than 100 times. It
also fails on a file that does not hold exactly 1600 scripts, and when `Core`
calls `Env::enter_domain` on anything but a change of the running domain or
leaves the call out on one. The hash covers the state lines only, so that
last check is made beside it.

It is testing, not proof.

- It says nothing about inputs that are not in the file.
- The `Env` of the test is a list model, not the kernel. Queues and waiting
  lists are lists, and expired waiters go to the back of their queue last
  waiter first, which is the kernel's order (`k0_sched::arm_timeout` links at
  the head, `k0_sched::expired` walks from the head). The kernel's intrusive
  queues, its own `Env` and its call sites are not run here.
- Only inputs that mean the same on both sides are in the file: valid tables,
  tick indexes below 2^32, no deadline 0. In the recorded file the tick
  indexes also never go back, but `vectors` does not require that. There
  is one exception. The 170 scripts whose id starts with `i` may hold a
  yield, a block or a wake while nothing runs, which the kernel does not
  raise; they are there because nothing else reaches the early return of
  `Core::yield_now`. For a block in that state the two sides do different
  things (`Core::block` pops the queue, `step` does nothing) and agree only
  because the queue is empty then (`blockIdleRust_eq`).
- What the model leaves out (console input, time inside a tick, caches) is
  left out here too.
- The comparison goes through a 64-bit hash that only keeps the file small.
  Two different traces with the same hash are possible in principle.
- CI does not check that the hashes are the model's. That needs the Lean
  toolchain and the `--check` command below.

### How to check and regenerate

```sh
$ cd proofs/lean
$ lake exe vectors --check ../../crates/k0-part/src/model_vectors.txt
```

The last line must be `scripts: 1600, differing from the model: 0` and the exit
status 0. A script whose stored hash is not the model's is listed and the exit
status is 1. Status 2 means the file could not be read or holds a line that is
not an accepted script. `lake` prints its own build messages on stderr.

To write the hashes again:

```sh
$ lake exe vectors ../../crates/k0-part/src/model_vectors.txt > model_vectors.new
$ mv model_vectors.new ../../crates/k0-part/src/model_vectors.txt
```

Comment lines are kept, so with an unchanged model the new file is the old one
byte for byte. `lake exe vectors --trace FILE` prints the model's lines for
every script of FILE in the layout of the failing test's report, so the two
can be compared line by line.

The scripts came from the generator of a differential harness that is not in
the repository, so the scripts cannot be regenerated from here; the hashes
can. A script can be added by hand: append the line without its hash, write
the hashes again, and change the number of scripts that `model_tests.rs`
requires (1600).

### When the Rust code or the model changes on purpose

- `Core` changes its behaviour: make the same change in `K0Part.lean`, get
  `lake build` to pass again (the theorems must still hold or be restated in
  the open), write the hashes again, then run the host tests. Never write the
  hashes again from anything but the model, and never edit a hash, to make
  the test pass: the file must always satisfy the `--check` command above.
- The model changes: run `--check`. If it still passes, the change did not
  alter any recorded result and nothing else is needed. If it reports
  scripts, the Rust test will fail on those scripts once the hashes are
  written again, until `Core` follows.
- A new event, a new field of the state or a new `Env` call: `showSt` in
  `ModelVectors.lean` and `line` and `step` in `model_tests.rs` change
  together, and scripts that use the new part are added. The list `NEED` in
  `model_tests.rs` names every path the scripts must take.

## What the models leave out

- Time inside a tick. `K0Share` and `K0ShareWide` count whole steps; `K0Part`
  has only tick indexes. Nothing is said about instants, the timer, the guard
  band, the hold before returning to EL0, interrupt latency or the length of
  kernel paths. What makes a tick late or skipped is outside the models.
- Where the tick indexes come from: the model takes the index of every tick
  event as given. That the kernel hands `Core` every tick it counts before any
  other event is a property of the kernel paths, not of the model. The
  hand-off to the root task masks interrupts before `Core::start` reads the
  tick for this reason: a tick counted between that read and the entry to EL0
  would leave `Core` one tick behind the counter, and a table set in that
  state would start one tick early, with no guard for its first change of
  domain.
- The guard (only the decision it is based on, `next_tick_switches`, is
  modelled), caches, predictors, the TLB.
- The real queues: Rust uses intrusive lists, the models use lists.
- The console. Rust `Env::serve` also wakes a console reader, and it reports
  a lost capability or a closed device to a reader of any domain, whichever
  domain is served. Input arrival is external. None of this is modelled.
- IPC rendezvous, capabilities, task creation and `TCB_RESUME`, domain
  changes, faults. Exit is a block nobody wakes. In `K0Part` the only way one
  domain changes another domain's queue or waiting list is `wakeX`; the kernel
  has others that need a capability (ending another task removes it from a
  queue or a waiting list of any domain, resuming a task queues it).
- `Nat` instead of `u64`. Within two ticks of `2^64` the Rust arithmetic
  saturates and the model does not; the kernel's tick counter stops before
  that. Tables are arbitrary lists (Rust limits entries, windows and domain
  ids, and the system call rejects a table without the caller's domain).
- Events the kernel does not raise are allowed in `K0Part`: `poll` while a
  task runs, `tick` while none runs, an index below the last one, a deadline
  that is already past. The model is the wider side there.
- In `K0Share` and `K0ShareWide`: a spinner that arrives after the start
  state, idle, a proof that a domain has at most `N` runnable tasks (the bound
  is relative to that `N`), a late tick or a tick that skips numbers. A task
  still chooses when the others run by the instant at which it yields or
  blocks, so the timing channel through the scheduler inside a domain stays.
- In `K0Share` only: an event between the last step of a period and its tick,
  and the removal of a ready task. Both are in `K0ShareWide`.
- A share bound for a table with several domains. `K0Bridge` links the tick
  rule and timed runs of one domain only.

## How to check

```sh
$ cd proofs/lean
$ lake build
$ lake env leanchecker K0Part K0Share K0ShareWide K0Bridge K0Close
$ lake exe vectors --check ../../crates/k0-part/src/model_vectors.txt
```

The toolchain is pinned in `lean-toolchain` (`leanprover/lean4:v4.35.0`);
`elan` fetches it on first use, and after that nothing is downloaded.

- `lake build` must finish with zero errors, zero warnings and no `sorry`. The
  sources contain no `axiom`, `native_decide`, `unsafe` or `partial`, and one
  `set_option`, a linter option in `K0Share.lean`. The build prints
  `#print axioms` for the headline theorems; allowed results are `propext`,
  `Classical.choice`, `Quot.sound`.
- `leanchecker` comes with the toolchain. It replays the compiled modules
  through the kernel, and must print nothing and exit with status 0. A
  declaration that was added without the kernel passes `lake build` and fails
  here.
- The last command is described under "Recorded results of the model".

The proofs are not run in CI. The host test with the recorded results is.
`lake-manifest.json` and `.lake/` are generated by `lake`.

## Checks that are not in the repository

These were done when the files were reviewed (November 2026). None of them can
be repeated from the repository alone, and each part says what is missing.

### An external type checker

`leanexport` (lean4export 3.1.0) and `nanoda_bin` (nanoda 0.4.19) checked the
five modules `K0Part`, `K0Share`, `K0ShareWide`, `K0Bridge` and `K0Close`. The
two programs are not in the repository. On the machine of the review they were
in the directory of the pinned toolchain, and whether every copy of the
toolchain has them was not confirmed, so this check cannot be repeated from
the repository alone. The commands were:

```sh
$ cd proofs/lean
$ lake build
$ lake env leanexport K0Part K0Share K0ShareWide K0Bridge K0Close > /tmp/k0.ndjson
$ lake env nanoda_bin /tmp/nanoda.json
```

The file `/tmp/nanoda.json` was:

```json
{
    "export_file_path": "/tmp/k0.ndjson",
    "use_stdin": false,
    "permitted_axioms": ["propext", "Classical.choice", "Quot.sound"],
    "unpermitted_axiom_hard_error": false,
    "nat_extension": true,
    "string_extension": true,
    "print_success_message": true
}
```

The last command printed `Checked 60911 declarations with no typechecker
errors` and exited with status 0. The number covers the five modules and what
they import from core Lean, and it changes with the toolchain and with the
list of modules. The line `1 pretty printer errors` that it also printed is
not about type checking. The export also declares `sorryAx`, an axiom of the
prelude. With `"unpermitted_axiom_hard_error": false` nanoda does not admit it
and stops at the first declaration that uses an axiom outside the list, so a
pass means that no exported declaration uses an axiom other than the three.
With `true` it stops on the prelude's `sorryAx` before it checks anything.

### Differential runs

A differential harness ran the Rust `Core<u64>` and `K0Part.step` on 420,000
random event scripts (about 31 million events) and compared one line per
event: no difference. The recorded scripts are a selection from its
generator. The harness, its generator and its seeds are not in the
repository, so that figure cannot be checked from the repository. During the
review, 40,000 more scripts from a second generator (about 1.6 million events,
half of the scripts with tick indexes that go back) gave no difference either.
That generator is not in the repository.

What the repository does allow is to compare the two sides on a script file of
your own. `lake exe vectors FILE` writes the model's hash for every script of
`FILE`, and the host test compares whatever file stands in the place of
`model_vectors.txt`. The test also requires exactly 1600 scripts and every
path 100 times, so for another file those two checks have to be relaxed in a
scratch copy of the tree.

### Changes of the definitions

The definitions of the models were changed one at a time and the files were
built again, to see whether a theorem notices.

- An earlier run made 105 changes, and no theorem noticed 19 of them. Those
  led to the theorems on timed waits, the table layout and task conservation
  in `K0Part.lean`, the counter theorems in `K0Share.lean`, and
  `K0Bridge.lean`.
- The last run, on the four files as they were before `K0Close.lean`, made
  260 changes: those 105 and 155 new ones. The build rejected 254, 5 survived,
  and 1 of the old ones had no counterpart in the final files.
- A rejected build says less than it seems to. Of 12 control rewrites that
  compute the same function, the build rejected 9, because the proofs lean on
  the wording of the definitions. So 254 is an upper bound on what the
  statements hold. Of the 150 new rejections, 125 were put down to a theorem
  whose statement is false under the change, 13 to concrete `decide` examples
  only, and 12 to the wording of proofs only: with edits to proofs alone those
  12 changed files build, so every theorem holds in them as stated. The first
  two numbers come from reading the statements against the change, not from
  counterexamples.
- `K0Close.lean` holds the statements for the gaps. It covers four of the five
  survivors (NT01, NV01, NV28, NB11) and ten of the twelve changes that only
  proof wording rejected (NV02, NV03, NV05, ND16, ND25, NX03, ND31, NX01,
  NT09, NV23). For every one of these ids except NV23 the run recorded that
  the instance `inst_<id>` is false in the changed file. That record is for
  the file `K0Close.lean` was made from, which has the same statements and
  proofs and differs in its header comment only. The two changes left
  without a statement, the fifth survivor NB33 and NB29, alter a value on a
  branch that the hypotheses of every theorem using it exclude. NV06 is NV01,
  NV02, NV03 and NV05 made at once.
- The recorded results are not a net under the theorems. They catch none of
  the five survivors, and seven of the changes on the model side of
  `K0Part.lean` are noticed by theorems and not by the 1600 scripts.

The scripts of both runs and their lists of changes are not in the
repository, so the runs cannot be repeated from it.

## What a proof of the model says

A theorem here says: for the Lean definitions in these files, the stated
property holds for every input. Whether the Rust code does what the
definitions do is established by reading it, by the host tests, and for
`K0Part.step` by the recorded results, none of which is a proof. A mistake in
a model definition makes a theorem about the wrong thing, and the statements
are only as strong as their hypotheses: `Inv`, `NoDup`, `R`, `Sync` and no
`wakeX` into `D` in `K0Part`; `Wf`, the cap `N` and a spinner that is there
from the start in `K0Share`; `Wide.Inv` and `Wide.Capped N` in `K0ShareWide`;
`Start`, `OkRun` and the side conditions of the rule lemmas in `K0Bridge`.
Nothing here covers timing below the scheduler decision, caches, hardware, or
anything outside the models. The most that can be said is that, inside the
model and under those hypotheses, what a domain sees of the scheduler does not
depend on what the other domains do.
