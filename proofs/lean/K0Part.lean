/-
K0Part.lean: a model of the K0 partition scheduler core.

WHAT THIS FILE IS
  A Lean 4 model of the pure scheduling core `Core` of the crate `k0-part`
  (`Core::tick`, `poll`, `block`, `yield_now`, `set_table`, with the tick rule
  of `k0-slice`), and proofs about that MODEL.  It is a proof about the model,
  not about the Rust code.  The link to the Rust code is a reading of `lib.rs`
  and `k0-slice/src/lib.rs`, the host tests of the Rust crate, and a host
  test that runs recorded event scripts through `Core<u64>` and compares the
  result with what `step` gives for them (`ModelVectors.lean`, README.md).
  None of these is a proof.

WHAT IS MODELLED
  * A table of (domain, ticks) entries with its cycle, `domAt`.
  * A pending table that starts two ticks after the last processed tick and
    takes over at its entry 0, with `epoch` set to its start tick (`Part`).
    `Part.setTable` is the Rust call as written: it promotes nothing and
    discards an earlier pending table.
  * Per-domain ready queues and per-domain waiting lists with optional
    deadlines in GLOBAL ticks, served only when the owner domain is served
    (same-domain tick, entry at a boundary, or the idle loop `poll`).  Expired
    waiters are pushed to the back of their own domain's queue, the one that
    started to wait last going first (`k0_sched::arm_timeout` links a waiter
    at the head of the list and `k0_sched::expired` walks it from the head;
    in the model: `serve_due_order`).
  * The tick rule state of the running task (`exempt` bit, kept inside `cur`).
  * Parking at a boundary (rule applied, then front of the own queue), entering
    a domain (front of its queue, as a tick entry), idle when the running
    domain has nothing ready (no slack donation), and a jump of the tick index
    over several ticks as ONE step.
  * Wakes.  `wake t`: the running task wakes a task of its own domain.
    Timer wakes happen inside `serve`.  `wakeX t`: a wake across domains,
    an explicitly labelled separate event (the model of a shared capability).
    A `wakeX` gives nothing back to the task that makes it.

WHAT IS NOT MODELLED
  * Time, the timer, the guard band, the hold before the return to EL0.  Nothing
    here says anything about instants.  Ticks are given as indexes, and what
    makes a tick late or skipped is outside the model.
  * Caches, predictors, the TLB, interrupt latency, the length of kernel paths.
    Only the channel through the scheduler decision is in scope.
  * Console input.  In Rust `Env::serve` wakes a console reader before the timed
    waits; here `serve` only does timed waits.  The Rust console wake reports a
    lost capability or a closed device to a reader of ANY domain, whichever
    domain is served, so `serve_only_own` is a fact about the model only.
  * IPC rendezvous, capabilities, task creation and `TCB_RESUME` (an enqueue of
    a task that waits nowhere), task domain changes (a task's domain `dom` is a
    fixed function), the removal of a ready or waiting task that another task
    ends (`k0_ipc::teardown`), fault and exit of the running task (exit is
    `block none` of a task nobody wakes).  The only way one domain changes
    another domain's queue or waiting list in the model is `wakeX`.
  * `Core::start` is only an initial state (`boot`).
  * `Core::block` while nothing runs pops the queue in Rust and does nothing
    here.  Under `Inv` that queue is empty, so the two agree (`blockIdleRust_eq`).
  * Events the kernel does not raise are allowed here: `poll` while a task runs,
    `tick` while none runs, an index below `seen`, a deadline that is already
    past, `set_table` while nothing runs.  The model is the wider side.
  * Uniqueness of a task in the structures.  The events make no duplicate
    checks and `Inv` does not ask for it: `Inv` holds for states where a task
    is queued twice (`inv_with_duplicate`).  One group of theorems takes it as
    a separate hypothesis: `step_tasks_perm` and `exec_tasks_perm` need `NoDup`
    (no task twice among the tasks of a domain), which every step keeps
    (`nodup_step`).  Without it a wake can drop an entry of a task that waits
    twice (`wake_needs_nodup`).  No other theorem needs it.
  * Rust uses `u64`.  `plan_at` and `set_table` saturate, `tick` computes
    `n - epoch` directly.  Here `Nat` with truncated subtraction.  `epoch <= seen`
    is part of `PInv`, so `n - epoch` does not underflow for a new tick `n`.
    Within two ticks of `2^64` the two differ; the kernel's tick counter stops
    before that.
  * Tables are arbitrary lists.  The Rust constructors reject an empty table and
    a window of zero ticks, so `cycle` is never 0 there, and the system call
    rejects a table without the caller's domain.
  * A deadline `some 0` has no Rust counterpart (Rust keeps 0 for "no deadline").

FINDINGS recorded while proving and while checking the proofs
  * `set_table` after tick n does NOT in general keep the domain of tick n + 1,
    when an earlier pending table starts exactly at n + 1 (two `set_table`
    calls in two consecutive ticks): the second call discards it.
    `guard_sound` therefore has the hypothesis "no pending table starts at
    n + 1".  `guard_unsound_without_hyp` is a counterexample on a state that
    is reached from boot (`exP_reachable`), and there the guard decision
    itself flips (`guard_decision_flips`).
  * `Core::set_table` and `Core::tick` rely on two facts the Rust code does not
    state: a pending table always starts after the last processed tick, and
    the epoch is never after it (`PInv`).  Both hold from boot and are kept by
    every event (`pinv_pstep`).  Without the first one `set_table` would
    change the domain of the current tick (`setTable_now_needs_pendAfter`).

THEOREM INDEX (all about the model)
  (0) pinv_pstep, pinv_pexec, inv_boot, inv_step, inv_exec (the invariants);
      sample_inv, sample_nodup (the state of the first `#eval` satisfies `Inv`
      and `NoDup`)
  (1) guard_sound, guard_sound_event, guard_unsound_without_hyp
  (2) plan_setTable_ge / plan_setTable_lt / plan_setTable_now (clock part),
      table_effective_at (events).  A table governs only until a later call
      (`first_table_never_governs`).
      The layout of a table, which those statements take from `cycle` and
      `domAtPos`: cycle_append, cycle_eq_sum, domAtPos_window, window_cover,
      domAt_window (entry i governs exactly its window, the windows follow in
      the order of the entries, the table repeats), plan_setTable_window (the
      same for the plan after a `set_table` call).
  (3) running_domain_function, running_domain_same, tick_jump_domain
  (4) no_slack_donation (`Inv` restated over runs)
  (5) noninterference, noninterference_final.  Hypotheses: both states satisfy
      `Inv`; `R D s s'` (same clock part and running domain, same queue and
      waiting list of D, same running task if D runs; other domains arbitrary);
      `Sync`, which compares the two lists place by place.  A clock event (a
      tick index not seen before, or a `set_table` call) is the same public
      event in both runs; a new tick may come through `tick` in one run and
      through the idle loop's `poll` in the other (`poll_eq_tick`).  Every
      other event is local to the running domain: an action of its task, its
      idle loop taking an interrupt without a new tick, or a repeated index.
      While D runs the local events are the same in both runs.  While another
      domain runs they are unrelated, and `tick 0` pads the run that has fewer
      (`tick_zero_noop`).  No `wakeX` into D in either run.
      The hypotheses fix as equal in both runs, and so the theorem does not
      explain: the tick indexes, jumps included (`tick_indexes_needed`), and
      the `set_table` calls.
      Conclusion: D's `view` (queue, waiting list, running task of D with its
      slice bit) is equal after every step.  Not claimed: anything about
      instants, caches, the guard, console input, or the Rust code.
  (6) cross_wake_breaks (the `wakeX` hypothesis is needed), example_views_equal
      and example_applies (a concrete pair of runs where the hypotheses hold,
      the views of D agree and the views of the other domains differ),
      example_effects and example_kernel_shaped (what the example exercises).
  (7) serve_only_own, serve_not_early.  Both are read off the definition of
      `serve`.  They say nothing about the kernel's `serve` callback.
      Timed waits do fire, and only for the domain that runs: serve_wakes_due,
      serve_due_order (due waiters are queued, the last waiter first),
      tick_serves (a new tick serves the domain that runs after it),
      tick_frame and tick_old_domain (it serves no other domain; at a domain
      change the old domain's due waiters stay until it runs again),
      poll_serves and poll_frame (the same for the idle loop's `poll` without a
      new tick), block_keeps_deadline, waiter_leaves_only (a waiter leaves its
      list only when its domain is served at or after its deadline, or by a
      wake), example_timed_waits.  They are about tick indexes: none says
      when a tick comes or that one comes.
  (8) blockIdleRust_eq, poll_eq_tick, tick_repeat_noop, tick_zero_noop (where
      the model's events differ from the kernel's calls, and why that does not
      matter; a tick index that is not new changes nothing).
  (9) No task is lost and none is duplicated.  `tasks d s`: the running task
      if d runs, d's queue, the tasks of d's waiting list.
      step_tasks_perm, exec_tasks_perm: from a state with `Inv` and `NoDup`,
      the tasks of every domain after a step (a run) are a permutation of
      the tasks before.  nodup_step, nodup_exec, nodup_boot: `NoDup` is kept and
      holds at boot for lists without duplicates.  step_tasks_perm_of: only a
      wake needs `NoDup`, and only for the woken task's waiting list.
      step_tasks_mem, exec_tasks_mem: with `Inv` alone a task is among the
      tasks of d after a run exactly if it was before.  wake_needs_nodup: the
      hypothesis cannot be dropped.  wake_effect, wakeX_effect: a woken task
      goes to the back of its own queue, and a wake of a task that does not
      wait changes nothing.  Not claimed: the order inside a queue beyond
      these, or anything about task creation and exit (not modelled).
  No theorem in this file constrains the exemption bit beyond its bookkeeping.
  `K0Bridge.lean` shows that the rules that set and clear it here are the
  rules of `K0Share.lean`, which proves the share that the bit buys.
`#print axioms` lines are at the end of the file.
-/

namespace K0Part

abbrev Dom := Nat
abbrev Tid := Nat
/-- Entries (domain, ticks), in order. -/
abbrev Table := List (Dom × Nat)

/-! ## Tables -/

/-- Number of ticks of one cycle. -/
def cycle : Table → Nat
  | [] => 0
  | (_, l) :: r => l + cycle r

/-- Domain at position `p` inside one cycle (mirrors `Table::domain_at` loop). -/
def domAtPos : Table → Nat → Dom
  | [], _ => 0
  | (d, l) :: r, p => if p < l then d else domAtPos r (p - l)

/-- Domain `offset` ticks after the table's entry 0 started (`Table::domain_at`). -/
def domAt (t : Table) (n : Nat) : Dom := domAtPos t (n % cycle t)

/-- The default table: root domain, one tick. -/
def defaultTable : Table := [(0, 1)]

/-! ### The layout of a table

`cycle`, `domAtPos` and `domAt` are the vocabulary of every plan theorem below.
The theorems here say what they compute, in terms of the list of entries: the
windows lie one after the other in the order of the entries, entry `(d, l)`
covers exactly `l` ticks, and the table repeats. -/

/-- `cycle` adds up over `++`.  (A helper: it does not fix what `cycle` counts;
    `cycle_eq_sum` and `domAtPos_window` do.) -/
theorem cycle_append (a b : Table) : cycle (a ++ b) = cycle a + cycle b := by
  induction a with
  | nil => simp [cycle]
  | cons x a ih => obtain ⟨d, l⟩ := x; simp [cycle, ih, Nat.add_assoc]

/-- One cycle is as long as the windows together. -/
theorem cycle_eq_sum (t : Table) : cycle t = (t.map Prod.snd).sum := by
  induction t with
  | nil => rfl
  | cons x t ih => obtain ⟨d, l⟩ := x; simp [cycle, ih]

/-- The window of an entry.  In the table `a ++ (d, l) :: b` the positions
    `cycle a`, ..., `cycle a + l - 1` of a cycle belong to `d`: the window starts
    where the entries before it end and is `l` ticks long. -/
theorem domAtPos_window (a b : Table) (d : Dom) (l p : Nat) (hp : p < l) :
    domAtPos (a ++ (d, l) :: b) (cycle a + p) = d := by
  induction a with
  | nil => simp [cycle, domAtPos, hp]
  | cons x a ih =>
    obtain ⟨d0, l0⟩ := x
    have h1 : ¬ (l0 + cycle a + p < l0) := by omega
    have h2 : l0 + cycle a + p - l0 = cycle a + p := by omega
    simp [cycle, domAtPos, h1, h2, ih]

/-- Every position of a cycle lies in the window of some entry.  With
    `domAtPos_window` this fixes `domAtPos` on a whole cycle. -/
theorem window_cover (t : Table) (pos : Nat) (h : pos < cycle t) :
    ∃ a d l b p, t = a ++ (d, l) :: b ∧ p < l ∧ pos = cycle a + p := by
  induction t generalizing pos with
  | nil => simp [cycle] at h
  | cons x t ih =>
    obtain ⟨d0, l0⟩ := x
    by_cases h0 : pos < l0
    · exact ⟨[], d0, l0, t, pos, rfl, h0, by simp [cycle]⟩
    · have h1 : pos - l0 < cycle t := by simp only [cycle] at h; omega
      obtain ⟨a, d, l, b, p, rfl, hp, hpos⟩ := ih (pos - l0) h1
      exact ⟨(d0, l0) :: a, d, l, b, p, rfl, hp, by simp only [cycle]; omega⟩

/-- The table repeats.  Tick `k * cycle + cycle a + p` after the start of the
    table `a ++ (d, l) :: b`, with `p < l`, belongs to `d`, for every round `k`.
    Together with `window_cover` this is the specification of `domAt` for a
    table whose cycle is not 0: entry by entry, in order, round after round. -/
theorem domAt_window (a b : Table) (d : Dom) (l p k : Nat) (hp : p < l) :
    domAt (a ++ (d, l) :: b) (k * cycle (a ++ (d, l) :: b) + cycle a + p) = d := by
  unfold domAt
  have hc : cycle (a ++ (d, l) :: b) = cycle a + (l + cycle b) := by rw [cycle_append]; rfl
  have hlt : cycle a + p < cycle (a ++ (d, l) :: b) := by omega
  have hm : (k * cycle (a ++ (d, l) :: b) + cycle a + p) % cycle (a ++ (d, l) :: b)
      = cycle a + p := by
    rw [Nat.add_assoc, Nat.mul_comm, Nat.mul_add_mod, Nat.mod_eq_of_lt hlt]
  rw [hm]
  exact domAtPos_window a b d l p hp

/-! ## The clock part of the state: table, pending table, epoch -/

/-- `tbl`/`epoch`: table in force and the tick where its entry 0 started.
    `pend = some (start, t)`: table `t` takes over at tick `start`.
    `seen`: last processed tick index. -/
structure Part where
  tbl : Table
  epoch : Nat
  pend : Option (Nat × Table)
  seen : Nat
deriving Repr, DecidableEq

/-- Domain of tick `k` as planned now (`Core::plan_at`). -/
def Part.plan (p : Part) (k : Nat) : Dom :=
  match p.pend with
  | some (s, t) => if s ≤ k then domAt t (k - s) else domAt p.tbl (k - p.epoch)
  | none => domAt p.tbl (k - p.epoch)

/-- Promote a pending table whose start is `≤ n` (`Core::advance`). -/
def Part.advance (p : Part) (n : Nat) : Part :=
  match p.pend with
  | some (s, t) => if s ≤ n then { p with tbl := t, epoch := s, pend := none } else p
  | none => p

/-- Domain of tick `n` read from the table in force (what `tick` uses after `advance`). -/
def Part.domNow (p : Part) (n : Nat) : Dom := domAt p.tbl (n - p.epoch)

/-- `Core::set_table t`: the new table is pending from `seen + 2`.  Nothing is
    promoted and an earlier pending table is discarded, as in the Rust call. -/
def Part.setTable (p : Part) (t : Table) : Part :=
  { p with pend := some (p.seen + 2, t) }

/-- A pending table starts after the last processed tick. -/
def PendAfter (p : Part) : Prop := ∀ k t, p.pend = some (k, t) → p.seen < k

/-- The clock part is well formed.  True at boot and kept by every event
    (`pinv_pstep`); a field of `Inv`.  The Rust code relies on both clauses
    without stating them: `Core::tick` computes `n - epoch` without a check,
    and `Core::set_table` does not look at the pending table it replaces. -/
structure PInv (p : Part) : Prop where
  epoch_le : p.epoch ≤ p.seen
  pend_after : PendAfter p

/-- Effect of a tick event with index `n` on the clock part. -/
def pTick (p : Part) (n : Nat) : Part :=
  if n ≤ p.seen then p else { p.advance n with seen := n }

theorem plan_seen (p : Part) (m k : Nat) : ({ p with seen := m } : Part).plan k = p.plan k := by
  simp [Part.plan]

theorem domNow_advance (p : Part) (n : Nat) : (p.advance n).domNow n = p.plan n := by
  unfold Part.advance Part.plan Part.domNow
  rcases hp : p.pend with _ | ⟨s, t⟩
  · simp
  · by_cases hs : s ≤ n <;> simp [hs]

theorem plan_advance (p : Part) (n k : Nat) (hk : n ≤ k) : (p.advance n).plan k = p.plan k := by
  unfold Part.advance Part.plan
  rcases hp : p.pend with _ | ⟨s, t⟩
  · simp [hp]
  · by_cases hs : s ≤ n
    · have : s ≤ k := Nat.le_trans hs hk
      simp [hs, this]
    · simp [hs, hp]

/-- (2a) After `set_table` with last processed tick `seen`, tick `k >= seen + 2`
    follows the new table's entry at `(k - (seen + 2)) mod cycle`. -/
theorem plan_setTable_ge (p : Part) (t : Table) (k : Nat) (hk : p.seen + 2 ≤ k) :
    (p.setTable t).plan k = domAtPos t ((k - (p.seen + 2)) % cycle t) := by
  simp [Part.setTable, Part.plan, hk, domAt]

/-- (2a) entry by entry.  After `set_table (a ++ (d, l) :: b)` with last
    processed tick `seen`, the new table starts at `seen + 2`, and in every
    round `k` the `l` ticks from `cycle a` on belong to `d`.  As `plan_setTable_ge`,
    this is the plan until a later `set_table` call. -/
theorem plan_setTable_window (p : Part) (a b : Table) (d : Dom) (l i k : Nat) (hi : i < l) :
    (p.setTable (a ++ (d, l) :: b)).plan
      (p.seen + 2 + (k * cycle (a ++ (d, l) :: b) + cycle a + i)) = d := by
  rw [plan_setTable_ge _ _ _ (by omega)]
  have h : p.seen + 2 + (k * cycle (a ++ (d, l) :: b) + cycle a + i) - (p.seen + 2)
      = k * cycle (a ++ (d, l) :: b) + cycle a + i := by omega
  rw [h]
  exact domAt_window a b d l i k hi

/-- (2b) Ticks `k < seen + 2` keep the domain they had.  For `k <= seen` that
    needs nothing beyond `PendAfter`.  For `k = seen + 1` it needs that no
    earlier pending table starts at `seen + 1` (the call discards that table). -/
theorem plan_setTable_lt (p : Part) (t : Table) (k : Nat) (hpa : PendAfter p)
    (h2 : k < p.seen + 2) (hk : k = p.seen + 1 → ∀ t0, p.pend ≠ some (p.seen + 1, t0)) :
    (p.setTable t).plan k = p.plan k := by
  have hn : ¬ (p.seen + 2 ≤ k) := by omega
  unfold Part.setTable Part.plan
  rcases hp : p.pend with _ | ⟨s, t0⟩
  · simp [hn]
  · have h1 : p.seen < s := hpa s t0 hp
    have : ¬ s ≤ k := by
      intro hsk
      have hk1 : k = p.seen + 1 := by omega
      have hs1 : s = p.seen + 1 := by omega
      exact hk hk1 t0 (by rw [hp, hs1])
    simp [this, hn]

/-- (1) `guard_sound`: `set_table` after tick `seen` does not change the domain
    of tick `seen + 1`, provided no earlier pending table starts exactly at
    `seen + 1`. -/
theorem guard_sound (p : Part) (t : Table) (hpa : PendAfter p)
    (hk : ∀ t0, p.pend ≠ some (p.seen + 1, t0)) :
    (p.setTable t).plan (p.seen + 1) = p.plan (p.seen + 1) :=
  plan_setTable_lt p t (p.seen + 1) hpa (by omega) (fun _ => hk)

/-- Counterexample to `guard_sound` without its hypothesis: table set at tick 1
    starts at 3; a second call at tick 2 discards it, so tick 3 changes. -/
def exP : Part := { tbl := [(0, 1)], epoch := 0, pend := some (3, [(1, 1)]), seen := 2 }

/-- The counterexample is a state the model can be in. -/
theorem exP_pendAfter : PendAfter exP := by
  intro k t h
  simp only [exP, Option.some.injEq, Prod.mk.injEq] at h
  rw [← h.1]; decide

theorem guard_unsound_without_hyp :
    (exP.setTable [(2, 1)]).plan 3 ≠ exP.plan 3 := by decide

#eval (exP.plan 3, (exP.setTable [(2, 1)]).plan 3)

/-- The counterexample at the level of the guard decision: before the second
    call the next tick changes the domain, after it it does not. -/
theorem guard_decision_flips :
    exP.plan (exP.seen + 1) ≠ exP.plan exP.seen ∧
    (exP.setTable [(2, 1)]).plan (exP.seen + 1) = exP.plan exP.seen := by decide

/-- The current tick's domain is not changed by `set_table` at that tick. -/
theorem plan_setTable_now (p : Part) (t : Table) (hpa : PendAfter p) :
    (p.setTable t).plan p.seen = p.plan p.seen := by
  have hn : ¬ (p.seen + 2 ≤ p.seen) := by omega
  unfold Part.setTable Part.plan
  rcases hp : p.pend with _ | ⟨s, t0⟩
  · simp [hn]
  · have : ¬ s ≤ p.seen := by have := hpa s t0 hp; omega
    simp [this, hn]

/-- `PendAfter` is needed for that: on a state where a pending table is
    already due, the call would discard a table that should be in force and so
    change the domain of the current tick.  The Rust code never reaches such a
    state (`pendAfter_pstep`). -/
def exQ : Part := { tbl := [(0, 1)], epoch := 0, pend := some (1, [(1, 1)]), seen := 2 }

theorem setTable_now_needs_pendAfter :
    (exQ.setTable [(2, 1)]).plan exQ.seen ≠ exQ.plan exQ.seen := by decide


/-! ## Events and the scheduler state -/

/-- Events of the model.
  * `tick n`: timer interrupt taken from EL0, tick index `n` (a jump is `n > seen + 1`).
  * `poll n`: the idle loop after an interrupt, `env.now() = n`.
  * `yield`, `block dl`, `wake t`, `wakeX t`: actions of the RUNNING task.
    `block dl`: it waits, with deadline `dl` in global ticks (`none`: untimed,
    also the model of exit).  `wake t`: it wakes a waiting task of its own
    domain.  `wakeX t`: it wakes a waiting task of ANOTHER domain through a
    shared object (the only cross-domain wake of the model).
  * `setTable t`: the control capability's `set_table`. -/
inductive Ev where
  | tick (n : Nat)
  | poll (n : Nat)
  | yield
  | block (dl : Option Nat)
  | wake (t : Tid)
  | wakeX (t : Tid)
  | setTable (t : Table)
deriving DecidableEq, Repr

/-- Scheduler state.  `cur = some (t, ex)`: `t` runs, `ex` = tick rule exemption.
    `q d`: ready queue of domain `d`.  `wl d`: waiting tasks of domain `d` in the
    order they started to wait, with an optional deadline in global ticks. -/
structure St where
  part : Part
  rd : Dom
  cur : Option (Tid × Bool)
  q : Dom → List Tid
  wl : Dom → List (Tid × Option Nat)

/-- Function update. -/
def upd {α : Type} (f : Dom → α) (d : Dom) (v : α) : Dom → α :=
  fun x => if x = d then v else f x

/-- A waiter is expired at tick `n`. -/
def expired (w : Tid × Option Nat) (n : Nat) : Bool :=
  match w.2 with
  | some k => decide (k ≤ n)
  | none => false

/-- `Env::serve d n` (timed waits only): expired waiters of domain `d` to the
    back of `d`'s queue, the last one to start waiting first.  That is the
    order in which `k0_sched::expired` hands them to `k0_sched::enqueue`:
    `arm_timeout` links a waiter at the head of the list and `expired` walks
    the list from the head.  No other domain is touched. -/
def serve (s : St) (d n : Nat) : St :=
  { s with
    wl := upd s.wl d ((s.wl d).filter (fun w => !expired w n))
    q := upd s.q d (s.q d ++ (((s.wl d).filter (fun w => expired w n)).reverse).map Prod.fst) }

/-- `enter(d)`: pop the front of `d`'s queue as a tick entry (no exemption), or idle. -/
def enter (s : St) (d : Dom) : St :=
  match s.q d with
  | x :: r => { s with q := upd s.q d r, cur := some (x, false) }
  | [] => { s with cur := none }

/-- Boundary parking of the running task: rule applied, then front of the own queue. -/
def cut (s : St) : St :=
  match s.cur with
  | none => s
  | some (c, ex) =>
    { s with cur := none,
             q := upd s.q s.rd
               (if ex then c :: s.q s.rd
                else match s.q s.rd with
                  | x :: r => x :: (r ++ [c])
                  | [] => [c]) }

/-- Tick inside the same domain, after `serve`: `Keep` / `Rotate` / idle entry. -/
def sameStep (s : St) : St :=
  match s.cur with
  | some (c, true) => { s with cur := some (c, false) }
  | some (c, false) =>
    match s.q s.rd with
    | x :: r => { s with q := upd s.q s.rd (r ++ [c]), cur := some (x, false) }
    | [] => s
  | none => enter s s.rd

/-- `Core::tick`. -/
def tickStep (s : St) (n : Nat) : St :=
  if n ≤ s.part.seen then s else
  let p := s.part.advance n
  let new := p.domNow n
  let s1 : St := { s with part := { p with seen := n } }
  if new = s.rd then sameStep (serve s1 s.rd n)
  else
    let s2 : St := { cut s1 with rd := new }
    enter (serve s2 new n) new

/-- Pop the own queue as a between-ticks entry (exemption), or idle. -/
def popBetween (s : St) : St :=
  match s.q s.rd with
  | x :: r => { s with q := upd s.q s.rd r, cur := some (x, true) }
  | [] => { s with cur := none }

/-- `Core::poll`. -/
def pollRest (s1 : St) : St :=
  match s1.cur with
  | some _ => s1
  | none => popBetween s1

def pollStep (s : St) (n : Nat) : St :=
  if s.part.seen < n then tickStep s n else pollRest (serve s s.rd n)

/-- `Core::yield_now`. -/
def yieldStep (s : St) : St :=
  match s.cur with
  | none => s
  | some (c, _) =>
    match s.q s.rd with
    | x :: r => { s with q := upd s.q s.rd (r ++ [c]), cur := some (x, true) }
    | [] => s

variable (dom : Tid → Dom)

/-- The running task starts to wait (in its own domain's list), then `Core::block`. -/
def blockStep (s : St) (dl : Option Nat) : St :=
  match s.cur with
  | none => s
  | some (c, _) =>
    popBetween { s with cur := none, wl := upd s.wl (dom c) (s.wl (dom c) ++ [(c, dl)]) }

/-- `k0_sched::enqueue` of a waiting task: into its OWN domain's queue, never `Core`. -/
def doWake (s : St) (t : Tid) : St :=
  if (s.wl (dom t)).any (fun w => w.1 = t) then
    { s with wl := upd s.wl (dom t) ((s.wl (dom t)).filter (fun w => !decide (w.1 = t))),
             q := upd s.q (dom t) (s.q (dom t) ++ [t]) }
  else s

/-- One step of the model. -/
def step (s : St) : Ev → St
  | .tick n => tickStep s n
  | .poll n => pollStep s n
  | .yield => yieldStep s
  | .block dl => blockStep dom s dl
  | .wake t => if s.cur.isSome = true ∧ dom t = s.rd then doWake dom s t else s
  | .wakeX t => if s.cur.isSome = true ∧ dom t ≠ s.rd then doWake dom s t else s
  | .setTable t => { s with part := s.part.setTable t }

/-- Run a list of events. -/
def exec (s : St) : List Ev → St
  | [] => s
  | e :: es => exec (step dom s e) es

/-- A state at boot: default table, root domain, `first` running with the
    between-ticks exemption (`Core::start`), other tasks given. -/
def boot (now : Nat) (first : Tid) (q0 : Dom → List Tid) (w0 : Dom → List (Tid × Option Nat)) : St :=
  { part := { tbl := defaultTable, epoch := now, pend := none, seen := now },
    rd := 0, cur := some (first, true), q := q0, wl := w0 }

/-! ## Clock part as a function of the public events only -/

/-- Effect of an event on the clock part. -/
def pstep (p : Part) : Ev → Part
  | .tick n => pTick p n
  | .poll n => pTick p n
  | .setTable t => p.setTable t
  | _ => p

def pexec (p : Part) : List Ev → Part
  | [] => p
  | e :: es => pexec (pstep p e) es

/-- Public events: ticks and `set_table` calls.  For the clock part a `poll n`
    is the tick `n`.  (A `poll n` with `n <= seen` leaves the clock alone but
    still serves the running domain and may start a task.) -/
inductive Pub where
  | tick (n : Nat)
  | setTable (t : Table)
deriving DecidableEq

def pubOf : Ev → Option Pub
  | .tick n => some (.tick n)
  | .poll n => some (.tick n)
  | .setTable t => some (.setTable t)
  | _ => none

/-! ## Quick runs with #eval -/

/-- Tasks 0..9 are domain 0, 10..19 domain 1, 20..29 domain 2. -/
def dom0 (t : Tid) : Dom := t / 10

/-- Compact observation of a state. -/
def St.show (s : St) : String :=
  s!"rd={s.rd} seen={s.part.seen} cur={s.cur} q={(List.range 3).map s.q} wl={(List.range 3).map s.wl}"

def sample : St :=
  boot 0 0 (fun d => if d = 0 then [1] else if d = 1 then [10, 11] else if d = 2 then [20] else [])
    (fun d => if d = 1 then [(12, some 5)] else [])

-- table set at tick 0 takes over at tick 2: [(1,1),(0,1),(2,1)]
#eval
  let es := [Ev.setTable [(1,1),(0,1),(2,1)], .tick 1, .tick 2, .tick 3, .tick 4, .tick 5, .tick 9]
  let rec go (s : St) : List Ev → List String
    | [] => [s.show]
    | e :: r => s.show :: go (step dom0 s e) r
  go sample es


/-! ## Frame lemmas: which fields each primitive touches -/

section frame
variable (s : St) (d n : Nat)

@[simp] theorem serve_part : (serve s d n).part = s.part := rfl
@[simp] theorem serve_rd : (serve s d n).rd = s.rd := rfl
@[simp] theorem serve_cur : (serve s d n).cur = s.cur := rfl

@[simp] theorem enter_part : (enter s d).part = s.part := by
  unfold enter; split <;> rfl
@[simp] theorem enter_rd : (enter s d).rd = s.rd := by
  unfold enter; split <;> rfl

@[simp] theorem cut_part : (cut s).part = s.part := by
  unfold cut; split <;> rfl
@[simp] theorem cut_rd : (cut s).rd = s.rd := by
  unfold cut; split <;> rfl

@[simp] theorem sameStep_part : (sameStep s).part = s.part := by
  unfold sameStep; split <;> (try split) <;> (try split) <;> simp
@[simp] theorem sameStep_rd : (sameStep s).rd = s.rd := by
  unfold sameStep; split <;> (try split) <;> (try split) <;> simp

@[simp] theorem popBetween_part : (popBetween s).part = s.part := by
  unfold popBetween; split <;> rfl
@[simp] theorem popBetween_rd : (popBetween s).rd = s.rd := by
  unfold popBetween; split <;> rfl

end frame

theorem step_part (s : St) (e : Ev) : (step dom s e).part = pstep s.part e := by
  cases e with
  | tick n =>
    simp only [step, tickStep, pstep, pTick]
    split
    · rfl
    · split <;> simp
  | poll n =>
    simp only [step, pollStep, pstep]
    split
    · next h =>
      have : ¬ n ≤ s.part.seen := by omega
      simp only [tickStep, pTick, this, ite_false]
      split <;> simp
    · next h =>
      have : n ≤ s.part.seen := by omega
      simp only [pTick, this, ite_true]
      unfold pollRest; split <;> simp
  | yield =>
    simp only [step, yieldStep, pstep]
    split <;> (try split) <;> rfl
  | block dl =>
    simp only [step, blockStep, pstep]
    split <;> simp
  | wake t =>
    simp only [step, pstep]
    split
    · unfold doWake; split <;> rfl
    · rfl
  | wakeX t =>
    simp only [step, pstep]
    split
    · unfold doWake; split <;> rfl
    · rfl
  | setTable t => rfl

theorem step_rd_pub_free (s : St) (e : Ev) (h : pubOf e = none) : (step dom s e).rd = s.rd := by
  cases e <;> simp [pubOf] at h
  · simp only [step, yieldStep]; split <;> (try split) <;> rfl
  · simp only [step, blockStep]; split <;> simp
  · simp only [step]; split
    · unfold doWake; split <;> rfl
    · rfl
  · simp only [step]; split
    · unfold doWake; split <;> rfl
    · rfl


/-- Every event keeps `PInv`: a tick promotes a pending table that is due (its
    start becomes the epoch), and `set_table` makes one that starts two ticks
    ahead. -/
theorem pinv_pstep (p : Part) (e : Ev) (h : PInv p) : PInv (pstep p e) := by
  cases e with
  | tick n | poll n =>
    simp only [pstep, pTick]
    split
    · exact h
    · next hn =>
      unfold Part.advance
      rcases hp : p.pend with _ | ⟨s0, t0⟩
      · refine ⟨?_, ?_⟩
        · have := h.epoch_le; simp; omega
        · intro k t hkt; simp [hp] at hkt
      · by_cases hs : s0 ≤ n
        · simp only [hs, ite_true]
          refine ⟨hs, ?_⟩
          intro k t hkt; simp at hkt
        · simp only [hs, ite_false]
          refine ⟨?_, ?_⟩
          · have := h.epoch_le; simp; omega
          · intro k t hkt
            simp [hp] at hkt
            obtain ⟨rfl, rfl⟩ := hkt
            show n < s0
            omega
  | setTable t =>
    refine ⟨h.epoch_le, ?_⟩
    intro k t' hkt
    simp only [pstep, Part.setTable, Option.some.injEq, Prod.mk.injEq] at hkt
    show p.seen < k
    omega
  | yield | block _ | wake _ | wakeX _ => exact h

theorem pinv_pexec (p : Part) (es : List Ev) (h : PInv p) : PInv (pexec p es) := by
  induction es generalizing p with
  | nil => exact h
  | cons e es ih => exact ih _ (pinv_pstep p e h)


/-! ## Invariants -/

/-- Membership facts of queues and wait lists: a task sits only in its own
    domain's queue and wait list. -/
structure QW (s : St) : Prop where
  q_dom : ∀ d t, t ∈ s.q d → dom t = d
  wl_dom : ∀ d w, w ∈ s.wl d → dom w.1 = d

/-- The queue and running-task part of the invariant. -/
structure Inv0 (s : St) : Prop where
  rd_plan : s.rd = s.part.plan s.part.seen
  qw : QW dom s
  cur_dom : ∀ t b, s.cur = some (t, b) → dom t = s.rd
  idle_empty : s.cur = none → s.q s.rd = []

/-- The invariant of every reachable state. -/
structure Inv (s : St) : Prop extends Inv0 dom s where
  clock : PInv s.part

theorem inv_boot (now first : Nat) (q0 : Dom → List Tid) (w0 : Dom → List (Tid × Option Nat))
    (hq : ∀ d t, t ∈ q0 d → dom t = d) (hw : ∀ d w, w ∈ w0 d → dom w.1 = d)
    (hf : dom first = 0) : Inv dom (boot now first q0 w0) := by
  refine ⟨⟨?_, ⟨hq, hw⟩, ?_, ?_⟩, ?_⟩
  · simp [boot, Part.plan, defaultTable, domAt, domAtPos, cycle]
  · intro t b h
    simp [boot] at h
    simp [boot, ← h.1, hf]
  · intro h; simp [boot] at h
  · exact ⟨Nat.le_refl _, by intro k t h; simp [boot] at h⟩

section prim
variable {dom}

theorem upd_same {α : Type} (f : Dom → α) (d : Dom) (v : α) : upd f d v d = v := by simp [upd]
theorem upd_other {α : Type} (f : Dom → α) (d x : Dom) (v : α) (h : x ≠ d) : upd f d v x = f x := by
  simp [upd, h]

theorem serve_QW (s : St) (d n : Nat) (h : QW dom s) : QW dom (serve s d n) := by
  constructor
  · intro x t ht
    simp only [serve, upd] at ht
    split at ht
    · next hx =>
      subst hx
      rcases List.mem_append.mp ht with h1 | h1
      · exact h.q_dom _ _ h1
      · obtain ⟨w, hw, rfl⟩ := List.mem_map.mp h1
        exact h.wl_dom _ _ (List.mem_filter.mp (List.mem_reverse.mp hw)).1
    · exact h.q_dom _ _ ht
  · intro x w hw
    simp only [serve, upd] at hw
    split at hw
    · next hx => subst hx; exact h.wl_dom _ _ (List.mem_filter.mp hw).1
    · exact h.wl_dom _ _ hw

theorem enter_QW (s : St) (d : Dom) (h : QW dom s) : QW dom (enter s d) := by
  unfold enter
  split
  · next x r hr =>
    constructor
    · intro y t ht
      simp only [upd] at ht
      split at ht
      · next hy => subst hy; exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_of_mem _ ht)
      · exact h.q_dom _ _ ht
    · exact h.wl_dom
  · exact ⟨h.q_dom, h.wl_dom⟩

theorem enter_cur_dom (s : St) (h : QW dom s) :
    ∀ t b, (enter s s.rd).cur = some (t, b) → dom t = s.rd := by
  intro t b hc
  unfold enter at hc
  split at hc
  · next x r hr =>
    simp at hc
    rw [← hc.1]
    exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_self)
  · simp at hc

theorem enter_idle (s : St) (d : Dom) : (enter s d).cur = none → (enter s d).q d = [] := by
  unfold enter
  split
  · next x r hr => simp
  · next hr => intro _; simpa using hr

theorem cut_QW (s : St) (h : QW dom s) (hc : ∀ t b, s.cur = some (t, b) → dom t = s.rd) :
    QW dom (cut s) := by
  unfold cut
  split
  · exact h
  · next c ex hcur =>
    have hcd : dom c = s.rd := hc c ex hcur
    constructor
    · intro y t ht
      simp only [upd] at ht
      split at ht
      · next hy =>
        subst hy
        split at ht
        · rcases List.mem_cons.mp ht with rfl | h1
          · exact hcd
          · exact h.q_dom _ _ h1
        · split at ht
          · next x r hr =>
            rcases List.mem_cons.mp ht with rfl | h1
            · exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_self)
            · rcases List.mem_append.mp h1 with h2 | h2
              · exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_of_mem _ h2)
              · simp at h2; subst h2; exact hcd
          · simp at ht; subst ht; exact hcd
      · exact h.q_dom _ _ ht
    · exact h.wl_dom

theorem sameStep_QW (s : St) (h : QW dom s) (hc : ∀ t b, s.cur = some (t, b) → dom t = s.rd) :
    QW dom (sameStep s) := by
  unfold sameStep
  split
  · exact ⟨h.q_dom, h.wl_dom⟩
  · next c hcur =>
    split
    · next x r hr =>
      have hcd : dom c = s.rd := hc c false hcur
      constructor
      · intro y t ht
        simp only [upd] at ht
        split at ht
        · next hy =>
          subst hy
          rcases List.mem_append.mp ht with h2 | h2
          · exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_of_mem _ h2)
          · simp at h2; subst h2; exact hcd
        · exact h.q_dom _ _ ht
      · exact h.wl_dom
    · exact h
  · exact enter_QW s _ h

theorem sameStep_cur_dom (s : St) (h : QW dom s) (hc : ∀ t b, s.cur = some (t, b) → dom t = s.rd) :
    ∀ t b, (sameStep s).cur = some (t, b) → dom t = s.rd := by
  intro t b hb
  unfold sameStep at hb
  split at hb
  · next c hcur => simp at hb; rw [← hb.1]; exact hc c true hcur
  · next c hcur =>
    split at hb
    · next x r hr =>
      simp at hb; rw [← hb.1]
      exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_self)
    · exact hc t b hb
  · exact enter_cur_dom s h t b hb

theorem sameStep_idle (s : St) : (sameStep s).cur = none → (sameStep s).q s.rd = [] := by
  intro hn
  unfold sameStep at hn ⊢
  split at hn
  · simp at hn
  · split at hn
    · simp at hn
    · simp_all
  · exact enter_idle s s.rd hn

theorem popBetween_QW (s : St) (h : QW dom s) : QW dom (popBetween s) := by
  unfold popBetween
  split
  · next x r hr =>
    constructor
    · intro y t ht
      simp only [upd] at ht
      split at ht
      · next hy => subst hy; exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_of_mem _ ht)
      · exact h.q_dom _ _ ht
    · exact h.wl_dom
  · exact ⟨h.q_dom, h.wl_dom⟩

theorem popBetween_cur_dom (s : St) (h : QW dom s) :
    ∀ t b, (popBetween s).cur = some (t, b) → dom t = s.rd := by
  intro t b hb
  unfold popBetween at hb
  split at hb
  · next x r hr =>
    simp at hb; rw [← hb.1]
    exact h.q_dom _ _ (by rw [hr]; exact List.mem_cons_self)
  · simp at hb

theorem popBetween_idle (s : St) : (popBetween s).cur = none → (popBetween s).q s.rd = [] := by
  unfold popBetween
  split
  · next x r hr => simp
  · next hr => intro _; simpa using hr


theorem plan_tick_part (p : Part) (n : Nat) :
    ({ p.advance n with seen := n } : Part).plan n = p.plan n := by
  rw [plan_seen, plan_advance _ _ _ (Nat.le_refl n)]

theorem inv_tick (s : St) (hi : Inv0 dom s) (n : Nat) : Inv0 dom (tickStep s n) := by
  unfold tickStep
  split
  · exact hi
  · next hn =>
    have hqw : QW dom { s with part := { s.part.advance n with seen := n } } := ⟨hi.qw.q_dom, hi.qw.wl_dom⟩
    by_cases hnew : (s.part.advance n).domNow n = s.rd
    · simp only [hnew, ite_true]
      have hpl : s.rd = s.part.plan n := by rw [← domNow_advance, hnew]
      have hq1 := serve_QW _ s.rd n hqw
      refine ⟨?_, sameStep_QW _ hq1 ?_, ?_, ?_⟩
      · simp only [sameStep_part, sameStep_rd, serve_part, serve_rd]
        rw [plan_tick_part]; exact hpl
      · intro t b h; simpa using hi.cur_dom t b h
      · intro t b h
        have := sameStep_cur_dom _ hq1 (by intro t b h; simpa using hi.cur_dom t b h) t b h
        simpa using this
      · intro h
        have := sameStep_idle (serve { s with part := { s.part.advance n with seen := n } } s.rd n) h
        simpa using this
    · simp only [hnew, ite_false]
      have hc1 : ∀ t b, ({ s with part := { s.part.advance n with seen := n } } : St).cur = some (t, b) →
          dom t = ({ s with part := { s.part.advance n with seen := n } } : St).rd := by
        intro t b h; simpa using hi.cur_dom t b h
      have hcut := cut_QW _ hqw hc1
      have hq2 : QW dom ({ cut { s with part := { s.part.advance n with seen := n } } with rd := (s.part.advance n).domNow n } : St) :=
        ⟨hcut.q_dom, hcut.wl_dom⟩
      have hq3 := serve_QW _ ((s.part.advance n).domNow n) n hq2
      have hpl : (s.part.advance n).domNow n = s.part.plan n := domNow_advance _ _
      refine ⟨?_, enter_QW _ _ hq3, ?_, ?_⟩
      · simp only [enter_part, serve_part, enter_rd, serve_rd, cut_part]
        rw [plan_tick_part]; exact hpl
      · intro t b h
        have := enter_cur_dom _ hq3 t b (by simpa using h)
        simpa using this
      · intro h
        have := enter_idle (serve { cut { s with part := { s.part.advance n with seen := n } } with rd := (s.part.advance n).domNow n } ((s.part.advance n).domNow n) n) ((s.part.advance n).domNow n) (by simpa using h)
        simpa using this


theorem inv_poll (s : St) (hi : Inv0 dom s) (n : Nat) : Inv0 dom (pollStep s n) := by
  unfold pollStep
  split
  · exact inv_tick s hi n
  · have hq1 := serve_QW s s.rd n hi.qw
    unfold pollRest
    split
    · next c hc =>
      refine ⟨by simpa using hi.rd_plan, hq1, ?_, ?_⟩
      · intro t b h; simpa using hi.cur_dom t b h
      · intro h; simp_all
    · next hc =>
      refine ⟨by simpa using hi.rd_plan, popBetween_QW _ hq1, ?_, ?_⟩
      · intro t b h
        have := popBetween_cur_dom _ hq1 t b h
        simpa using this
      · intro h
        have := popBetween_idle (serve s s.rd n) h
        simpa using this

theorem inv_yield (s : St) (hi : Inv0 dom s) : Inv0 dom (yieldStep s) := by
  unfold yieldStep
  split
  · exact hi
  · next c ex hc =>
    split
    · next x r hr =>
      have hcd : dom c = s.rd := hi.cur_dom c ex hc
      refine ⟨hi.rd_plan, ⟨?_, hi.qw.wl_dom⟩, ?_, ?_⟩
      · intro y t ht
        simp only [upd] at ht
        split at ht
        · next hy =>
          subst hy
          rcases List.mem_append.mp ht with h2 | h2
          · exact hi.qw.q_dom _ _ (by rw [hr]; exact List.mem_cons_of_mem _ h2)
          · simp at h2; subst h2; exact hcd
        · exact hi.qw.q_dom _ _ ht
      · intro t b h
        simp at h; rw [← h.1]
        exact hi.qw.q_dom _ _ (by rw [hr]; exact List.mem_cons_self)
      · intro h; simp at h
    · exact hi

theorem inv_block (s : St) (hi : Inv0 dom s) (dl : Option Nat) : Inv0 dom (blockStep dom s dl) := by
  unfold blockStep
  split
  · exact hi
  · next c ex hc =>
    have hq1 : QW dom ({ s with cur := none, wl := upd s.wl (dom c) (s.wl (dom c) ++ [(c, dl)]) } : St) := by
      refine ⟨hi.qw.q_dom, ?_⟩
      intro y w hw
      simp only [upd] at hw
      split at hw
      · next hy =>
        subst hy
        rcases List.mem_append.mp hw with h2 | h2
        · exact hi.qw.wl_dom _ _ h2
        · simp at h2; subst h2; rfl
      · exact hi.qw.wl_dom _ _ hw
    refine ⟨by simpa using hi.rd_plan, popBetween_QW _ hq1, ?_, ?_⟩
    · intro t b h
      have := popBetween_cur_dom _ hq1 t b h
      simpa using this
    · intro h
      have := popBetween_idle ({ s with cur := none, wl := upd s.wl (dom c) (s.wl (dom c) ++ [(c, dl)]) } : St) h
      simpa using this

theorem doWake_QW (s : St) (t : Tid) (h : QW dom s) : QW dom (doWake dom s t) := by
  unfold doWake
  split
  · constructor
    · intro y u hu
      simp only [upd] at hu
      split at hu
      · next hy =>
        subst hy
        rcases List.mem_append.mp hu with h2 | h2
        · exact h.q_dom _ _ h2
        · simp at h2; subst h2; rfl
      · exact h.q_dom _ _ hu
    · intro y w hw
      simp only [upd] at hw
      split at hw
      · next hy => subst hy; exact h.wl_dom _ _ (List.mem_filter.mp hw).1
      · exact h.wl_dom _ _ hw
  · exact h

theorem inv_wake_gen (s : St) (hi : Inv0 dom s) (t : Tid) (hg : s.cur.isSome = true) :
    Inv0 dom (doWake dom s t) := by
  have hq := doWake_QW s t hi.qw
  have hcur : (doWake dom s t).cur = s.cur := by unfold doWake; split <;> rfl
  have hrd : (doWake dom s t).rd = s.rd := by unfold doWake; split <;> rfl
  have hpt : (doWake dom s t).part = s.part := by unfold doWake; split <;> rfl
  refine ⟨by rw [hrd, hpt]; exact hi.rd_plan, hq, ?_, ?_⟩
  · intro u b h; rw [hcur] at h; rw [hrd]; exact hi.cur_dom u b h
  · intro h; rw [hcur] at h; rw [h] at hg; simp at hg

theorem inv_step (s : St) (hi : Inv dom s) (e : Ev) : Inv dom (step dom s e) := by
  refine ⟨?_, by rw [step_part]; exact pinv_pstep _ _ hi.clock⟩
  cases e with
  | tick n => exact inv_tick s hi.toInv0 n
  | poll n => exact inv_poll s hi.toInv0 n
  | yield => exact inv_yield s hi.toInv0
  | block dl => exact inv_block s hi.toInv0 dl
  | wake t =>
    simp only [step]
    split
    · next h => exact inv_wake_gen s hi.toInv0 t h.1
    · exact hi.toInv0
  | wakeX t =>
    simp only [step]
    split
    · next h => exact inv_wake_gen s hi.toInv0 t h.1
    · exact hi.toInv0
  | setTable t =>
    refine ⟨?_, ⟨hi.qw.q_dom, hi.qw.wl_dom⟩, hi.cur_dom, hi.idle_empty⟩
    show s.rd = (s.part.setTable t).plan (s.part.setTable t).seen
    have : (s.part.setTable t).seen = s.part.seen := rfl
    rw [this, plan_setTable_now _ _ hi.clock.pend_after]; exact hi.rd_plan

theorem inv_exec (s : St) (hi : Inv dom s) (es : List Ev) : Inv dom (exec dom s es) := by
  induction es generalizing s with
  | nil => exact hi
  | cons e es ih => exact ih _ (inv_step s hi e)


/-! ## Theorems (3), (4), (2) at the level of events -/

/-- Effect of a public event on the clock part. -/
def pstepP (p : Part) : Pub → Part
  | .tick n => pTick p n
  | .setTable t => p.setTable t

def pexecP (p : Part) : List Pub → Part
  | [] => p
  | e :: es => pexecP (pstepP p e) es

theorem pstep_eq (p : Part) (e : Ev) :
    pstep p e = match pubOf e with | none => p | some b => pstepP p b := by
  cases e <;> rfl

theorem pexec_eq (p : Part) (es : List Ev) : pexec p es = pexecP p (es.filterMap pubOf) := by
  induction es generalizing p with
  | nil => rfl
  | cons e es ih =>
    simp only [pexec, List.filterMap_cons]
    rw [pstep_eq]
    cases h : pubOf e with
    | none => simpa using ih p
    | some b => simpa [pexecP] using ih (pstepP p b)

theorem exec_part (s : St) (es : List Ev) : (exec dom s es).part = pexec s.part es := by
  induction es generalizing s with
  | nil => rfl
  | cons e es ih => simp only [exec, pexec]; rw [ih, step_part]

/-- (3) The running domain after any sequence of events is the domain of the last
    processed tick, and that is a function of the clock part alone, which in turn
    is a function of the public events (ticks and `set_table` calls) only. -/
theorem running_domain_function (s : St) (hi : Inv dom s) (es : List Ev) :
    (exec dom s es).rd =
      (pexecP s.part (es.filterMap pubOf)).plan (pexecP s.part (es.filterMap pubOf)).seen := by
  have h := (inv_exec s hi es).rd_plan
  rw [exec_part, pexec_eq] at h
  exact h

/-- (3') Two runs with the same clock part and the same public events end in
    the same running domain, whatever the other events and queues are. -/
theorem running_domain_same (s s' : St) (hi : Inv dom s) (hi' : Inv dom s')
    (hp : s.part = s'.part) (es es' : List Ev) (h : es.filterMap pubOf = es'.filterMap pubOf) :
    (exec dom s es).rd = (exec dom s' es').rd := by
  rw [running_domain_function s hi, running_domain_function s' hi', hp, h]

/-- (4) No slack donation.  After any sequence of events: a task that runs belongs
    to the running domain, which is the domain of the last processed tick; and if
    the core idles, the running domain's ready queue is empty. -/
theorem no_slack_donation (s : St) (hi : Inv dom s) (es : List Ev) :
    (∀ t b, (exec dom s es).cur = some (t, b) →
        dom t = (exec dom s es).rd ∧
        dom t = (exec dom s es).part.plan (exec dom s es).part.seen) ∧
    ((exec dom s es).cur = none → (exec dom s es).q (exec dom s es).rd = []) := by
  have h := inv_exec s hi es
  refine ⟨fun t b hc => ⟨h.cur_dom t b hc, ?_⟩, h.idle_empty⟩
  rw [h.cur_dom t b hc]; exact h.rd_plan

theorem seen_mono_pstep (p : Part) (e : Ev) : p.seen ≤ (pstep p e).seen := by
  cases e <;> simp [pstep, pTick, Part.setTable, Part.advance] <;>
    (try split) <;> (try split) <;> simp <;> omega

theorem seen_mono_pexec (p : Part) (es : List Ev) : p.seen ≤ (pexec p es).seen := by
  induction es generalizing p with
  | nil => exact Nat.le_refl _
  | cons e es ih => exact Nat.le_trans (seen_mono_pstep p e) (ih _)

def NoSet : Ev → Prop
  | .setTable _ => False
  | _ => True

theorem plan_pstep_noset (p : Part) (e : Ev) (h : NoSet e) (k : Nat) (hk : (pstep p e).seen ≤ k) :
    (pstep p e).plan k = p.plan k := by
  cases e with
  | tick n | poll n =>
    by_cases hn : n ≤ p.seen
    · simp [pstep, pTick, hn]
    · have hk' : n ≤ k := by simpa [pstep, pTick, hn] using hk
      simp [pstep, pTick, hn, plan_seen, plan_advance p n k hk']
  | setTable t => exact h.elim
  | _ => rfl

theorem plan_pexec_noset (p : Part) (es : List Ev) (h : ∀ e ∈ es, NoSet e) (k : Nat)
    (hk : (pexec p es).seen ≤ k) : (pexec p es).plan k = p.plan k := by
  induction es generalizing p with
  | nil => rfl
  | cons e es ih =>
    simp only [pexec] at hk ⊢
    rw [ih (pstep p e) (fun e' he' => h e' (List.mem_cons_of_mem _ he')) hk]
    exact plan_pstep_noset p e (h e List.mem_cons_self) k
      (Nat.le_trans (seen_mono_pexec _ es) hk)

theorem setTable_seen (p : Part) (t : Table) : (p.setTable t).seen = p.seen := rfl

/-- (2) `table_effective_at`: after `set_table` with last processed tick `n` and no later
    `set_table`, if the last processed tick `k` is `>= n + 2`, the running domain
    is the new table's entry at `(k - (n + 2)) mod cycle`. -/
theorem table_effective_at (s : St) (hi : Inv dom s) (t : Table) (es : List Ev)
    (hno : ∀ e ∈ es, NoSet e)
    (hk : s.part.seen + 2 ≤ (exec dom s (.setTable t :: es)).part.seen) :
    (exec dom s (.setTable t :: es)).rd =
      domAtPos t (((exec dom s (.setTable t :: es)).part.seen - (s.part.seen + 2)) % cycle t) := by
  have h := (inv_exec s hi (.setTable t :: es)).rd_plan
  rw [h]
  have hp : (exec dom s (.setTable t :: es)).part = pexec (s.part.setTable t) es := by
    rw [exec_part]; rfl
  rw [hp] at hk ⊢
  rw [plan_pexec_noset _ es hno _ (Nat.le_refl _)]
  exact plan_setTable_ge _ _ _ hk


end prim

/-! ## Non-interference -/

section ni
variable (dom : Tid → Dom)

/-- The part of the state that domain `D` can see, besides the public clock:
    its queue, its waiting list, and which of its tasks runs (with the slice bit). -/
structure View where
  q : List Tid
  wl : List (Tid × Option Nat)
  run : Option (Tid × Bool)
deriving DecidableEq, Repr

/-- `D`'s view of a state. -/
def view (D : Dom) (s : St) : View :=
  { q := s.q D, wl := s.wl D,
    run := match s.cur with
      | some (t, b) => if dom t = D then some (t, b) else none
      | none => none }

/-- The views after every step: the initial one and one per event. -/
def viewTrace (D : Dom) (s : St) : List Ev → List View
  | [] => [view dom D s]
  | e :: es => view dom D s :: viewTrace D (step dom s e) es

end ni

section ni2
variable {dom : Tid → Dom}

/-- Relation between two runs seen from `D`, without the running task. -/
structure R0 (D : Dom) (s s' : St) : Prop where
  part : s.part = s'.part
  rd : s.rd = s'.rd
  q : s.q D = s'.q D
  wl : s.wl D = s'.wl D

/-- Relation between two runs seen from `D`: same clock part, same running domain,
    same queue and waiting list of `D`, and the same running task when `D` runs.
    Nothing is said about other domains. -/
structure R (D : Dom) (s s' : St) : Prop extends R0 D s s' where
  cur : s.rd = D → s.cur = s'.cur

theorem R0.trans {D : Dom} {a b c : St} (h1 : R0 D a b) (h2 : R0 D b c) : R0 D a c :=
  ⟨h1.part.trans h2.part, h1.rd.trans h2.rd, h1.q.trans h2.q, h1.wl.trans h2.wl⟩

theorem R0.symm {D : Dom} {a b : St} (h1 : R0 D a b) : R0 D b a :=
  ⟨h1.part.symm, h1.rd.symm, h1.q.symm, h1.wl.symm⟩

theorem R0.refl (D : Dom) (a : St) : R0 D a a := ⟨rfl, rfl, rfl, rfl⟩

/-- Equal views from the relation, for two well formed states. -/
theorem view_eq {D : Dom} {s s' : St} (hi : Inv dom s) (hi' : Inv dom s') (h : R D s s') :
    view dom D s = view dom D s' := by
  unfold view
  have hq := h.q
  have hw := h.wl
  by_cases hD : s.rd = D
  · have hc := h.cur hD
    simp [hq, hw, hc]
  · have e1 : (match s.cur with | some (t, b) => if dom t = D then some (t, b) else none | none => none) = none := by
      split
      · next t b hc =>
        have : dom t ≠ D := by rw [hi.cur_dom t b hc]; exact hD
        simp [this]
      · rfl
    have e2 : (match s'.cur with | some (t, b) => if dom t = D then some (t, b) else none | none => none) = none := by
      split
      · next t b hc =>
        have : dom t ≠ D := by rw [hi'.cur_dom t b hc, ← h.rd]; exact hD
        simp [this]
      · rfl
    simp [hq, hw, e1, e2]

theorem R_of_other {D : Dom} {s s' x x' : St} (h : R D s s') (hne : s.rd ≠ D)
    (hx : R0 D s x) (hx' : R0 D s' x') : R D x x' :=
  ⟨hx.symm.trans (h.toR0.trans hx'), fun hD => absurd (hx.rd.trans hD) hne⟩

/-- Frame: a step that changes nothing `D` sees and keeps `rd` and `cur`. -/
theorem R_frame {D : Dom} {s s' x x' : St} (h : R D s s') (hx : R0 D s x) (hx' : R0 D s' x')
    (hc : x.cur = s.cur) (hc' : x'.cur = s'.cur) : R D x x' := by
  refine ⟨hx.symm.trans (h.toR0.trans hx'), ?_⟩
  intro hD
  rw [hc, hc']
  apply h.cur
  rw [hx.rd]; exact hD

/-! frame lemmas: operations on another domain are invisible to `D` -/

theorem serve_R0_other (s : St) (d n D : Nat) (h : d ≠ D) : R0 D s (serve s d n) := by
  refine ⟨rfl, rfl, ?_, ?_⟩ <;> simp [serve, upd, Ne.symm h]

theorem enter_R0_other (s : St) (d D : Nat) (h : d ≠ D) : R0 D s (enter s d) := by
  unfold enter
  split
  · refine ⟨rfl, rfl, ?_, rfl⟩
    simp [upd, Ne.symm h]
  · exact ⟨rfl, rfl, rfl, rfl⟩

theorem cut_R0_other (s : St) (D : Nat) (h : s.rd ≠ D) : R0 D s (cut s) := by
  unfold cut
  split
  · exact R0.refl _ _
  · refine ⟨rfl, rfl, ?_, rfl⟩
    simp [upd, Ne.symm h]

theorem sameStep_R0_other (s : St) (D : Nat) (h : s.rd ≠ D) : R0 D s (sameStep s) := by
  unfold sameStep
  split
  · exact ⟨rfl, rfl, rfl, rfl⟩
  · split
    · refine ⟨rfl, rfl, ?_, rfl⟩
      simp [upd, Ne.symm h]
    · exact R0.refl _ _
  · exact enter_R0_other s _ D h

theorem popBetween_R0_other (s : St) (D : Nat) (h : s.rd ≠ D) : R0 D s (popBetween s) := by
  unfold popBetween
  split
  · refine ⟨rfl, rfl, ?_, rfl⟩
    simp [upd, Ne.symm h]
  · exact ⟨rfl, rfl, rfl, rfl⟩

theorem yield_R0_other (s : St) (D : Nat) (h : s.rd ≠ D) : R0 D s (yieldStep s) := by
  unfold yieldStep
  split
  · exact R0.refl _ _
  · split
    · refine ⟨rfl, rfl, ?_, rfl⟩
      simp [upd, Ne.symm h]
    · exact R0.refl _ _

theorem block_R0_other (s : St) (hi : Inv dom s) (D : Nat) (dl : Option Nat) (h : s.rd ≠ D) :
    R0 D s (blockStep dom s dl) := by
  unfold blockStep
  split
  · exact R0.refl _ _
  · next c ex hc =>
    have hcd : dom c ≠ D := by rw [hi.cur_dom c ex hc]; exact h
    have h1 : R0 D s { s with cur := none, wl := upd s.wl (dom c) (s.wl (dom c) ++ [(c, dl)]) } :=
      ⟨rfl, rfl, rfl, by simp [upd, Ne.symm hcd]⟩
    exact h1.trans (popBetween_R0_other _ D h)

theorem doWake_R0_other (s : St) (t D : Nat) (h : dom t ≠ D) : R0 D s (doWake dom s t) := by
  unfold doWake
  split
  · refine ⟨rfl, rfl, ?_, ?_⟩ <;> simp [upd, Ne.symm h]
  · exact R0.refl _ _


/-! pair lemmas: the primitives applied to two related states -/

theorem R0_serve {D : Dom} {s s' : St} (h : R0 D s s') (d n : Nat) :
    R0 D (serve s d n) (serve s' d n) := by
  refine ⟨h.part, h.rd, ?_, ?_⟩
  · simp only [serve, upd]
    split
    · next hDd => subst hDd; rw [h.q, h.wl]
    · exact h.q
  · simp only [serve, upd]
    split
    · next hDd => subst hDd; rw [h.wl]
    · exact h.wl

theorem R_serve {D : Dom} {s s' : St} (h : R D s s') (d n : Nat) :
    R D (serve s d n) (serve s' d n) :=
  ⟨R0_serve h.toR0 d n, fun hD => h.cur hD⟩

theorem R_withPart {D : Dom} {s s' : St} (h : R D s s') (p : Part) :
    R D { s with part := p } { s' with part := p } :=
  ⟨⟨rfl, h.rd, h.q, h.wl⟩, fun hD => by simpa using h.cur hD⟩

theorem R_enter {D : Dom} {s s' : St} (h : R0 D s s') :
    R D (enter s s.rd) (enter s' s.rd) := by
  by_cases hD : s.rd = D
  · subst hD
    have hq : s'.q s.rd = s.q s.rd := h.q.symm
    unfold enter
    rw [hq]
    split
    · refine ⟨⟨h.part, h.rd, ?_, h.wl⟩, fun _ => rfl⟩
      simp [upd]
    · exact ⟨⟨h.part, h.rd, h.q, h.wl⟩, fun _ => rfl⟩
  · exact ⟨(enter_R0_other s _ D hD).symm.trans (h.trans (enter_R0_other s' s.rd D hD)),
      fun hh => absurd (by simpa using hh) hD⟩

theorem R0_cut {D : Dom} {s s' : St} (h : R D s s') : R0 D (cut s) (cut s') := by
  by_cases hD : s.rd = D
  · subst hD
    have hc := h.cur rfl
    have hr := h.rd
    have hq : s'.q s.rd = s.q s.rd := h.q.symm
    unfold cut
    rw [← hc, ← hr]
    split
    · exact h.toR0
    · refine ⟨h.part, rfl, ?_, h.wl⟩
      simp [upd, hq]
  · exact (cut_R0_other s D hD).symm.trans (h.toR0.trans (cut_R0_other s' D (by rw [← h.rd]; exact hD)))

theorem R_sameStep {D : Dom} {s s' : St} (h : R D s s') : R D (sameStep s) (sameStep s') := by
  by_cases hD : s.rd = D
  · subst hD
    have hc := h.cur rfl
    have hr := h.rd
    have hq : s'.q s.rd = s.q s.rd := h.q.symm
    unfold sameStep
    rw [← hc, ← hr, hq]
    split
    · exact ⟨⟨h.part, rfl, h.q, h.wl⟩, fun _ => rfl⟩
    · split
      · refine ⟨⟨h.part, rfl, ?_, h.wl⟩, fun _ => rfl⟩
        simp [upd]
      · exact ⟨⟨h.part, h.rd, h.q, h.wl⟩, fun _ => hc⟩
    · exact R_enter h.toR0
  · exact R_of_other h hD (sameStep_R0_other s D hD)
      (sameStep_R0_other s' D (by rw [← h.rd]; exact hD))

theorem R_popBetween {D : Dom} {s s' : St} (h : R D s s') : R D (popBetween s) (popBetween s') := by
  by_cases hD : s.rd = D
  · subst hD
    have hr := h.rd
    have hq : s'.q s.rd = s.q s.rd := h.q.symm
    unfold popBetween
    rw [← hr, hq]
    split
    · refine ⟨⟨h.part, rfl, ?_, h.wl⟩, fun _ => rfl⟩
      simp [upd]
    · exact ⟨⟨h.part, rfl, h.q, h.wl⟩, fun _ => rfl⟩
  · exact R_of_other h hD (popBetween_R0_other s D hD)
      (popBetween_R0_other s' D (by rw [← h.rd]; exact hD))

theorem R_pollRest {D : Dom} {s s' : St} (h : R D s s') : R D (pollRest s) (pollRest s') := by
  by_cases hD : s.rd = D
  · have hc := h.cur hD
    unfold pollRest
    rw [← hc]
    split
    · exact h
    · exact R_popBetween h
  · have e1 : R0 D s (pollRest s) := by
      unfold pollRest; split
      · exact R0.refl _ _
      · exact popBetween_R0_other s D hD
    have e2 : R0 D s' (pollRest s') := by
      unfold pollRest; split
      · exact R0.refl _ _
      · exact popBetween_R0_other s' D (by rw [← h.rd]; exact hD)
    exact R_of_other h hD e1 e2

theorem R_tick {D : Dom} {s s' : St} (h : R D s s') (n : Nat) :
    R D (tickStep s n) (tickStep s' n) := by
  obtain ⟨⟨hp, hr, hq, hw⟩, hc⟩ := h
  obtain ⟨p', rd', c', q', w'⟩ := s'
  dsimp only at hp hr hq hw hc
  subst hp
  subst hr
  have h : R D s ⟨s.part, s.rd, c', q', w'⟩ := ⟨⟨rfl, rfl, hq, hw⟩, hc⟩
  unfold tickStep
  by_cases h1 : n ≤ s.part.seen
  · simp only [h1, ite_true]; exact h
  · simp only [h1, ite_false]
    have h2 := R_withPart h { s.part.advance n with seen := n }
    by_cases hnew : (s.part.advance n).domNow n = s.rd
    · simp only [hnew, ite_true]
      exact R_sameStep (R_serve h2 s.rd n)
    · simp only [hnew, ite_false]
      have h3 := R0_cut h2
      have h4 := R0_serve (⟨h3.part, rfl, h3.q, h3.wl⟩ :
        R0 D ({ cut { s with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St)
          ({ cut { (⟨s.part, s.rd, c', q', w'⟩ : St) with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St)) ((s.part.advance n).domNow n) n
      exact R_enter h4

theorem R_poll {D : Dom} {s s' : St} (h : R D s s') (n : Nat) :
    R D (pollStep s n) (pollStep s' n) := by
  have hp := h.part
  have hr := h.rd
  obtain ⟨⟨hp, hr, hq, hw⟩, hc⟩ := h
  obtain ⟨p', rd', c', q', w'⟩ := s'
  dsimp only at hp hr hq hw hc
  subst hp
  subst hr
  have h : R D s ⟨s.part, s.rd, c', q', w'⟩ := ⟨⟨rfl, rfl, hq, hw⟩, hc⟩
  unfold pollStep
  by_cases h1 : s.part.seen < n
  · simp only [h1, ite_true]; exact R_tick h n
  · simp only [h1, ite_false]
    exact R_pollRest (R_serve h s.rd n)

theorem R_setTable {D : Dom} {s s' : St} (h : R D s s') (t : Table) :
    R D (step dom s (.setTable t)) (step dom s' (.setTable t)) := by
  simp only [step]
  rw [← h.part]
  exact R_withPart h _


/-! actions of the running task -/

/-- No wake across domains into `D`. -/
def NoCross (dom : Tid → Dom) (D : Dom) : Ev → Prop
  | .wakeX t => dom t ≠ D
  | _ => True

theorem act_R0_other {D : Dom} {s : St} (hi : Inv dom s) (hne : s.rd ≠ D) (e : Ev)
    (hnc : NoCross dom D e) : R0 D s (step dom s e) ∨ pubOf e ≠ none := by
  by_cases hp : pubOf e = none
  · left
    cases e with
    | tick n => simp [pubOf] at hp
    | poll n => simp [pubOf] at hp
    | setTable t => simp [pubOf] at hp
    | yield => exact yield_R0_other s D hne
    | block dl => exact block_R0_other s hi D dl hne
    | wake t =>
      simp only [step]
      split
      · next hg => exact doWake_R0_other s t D (by rw [hg.2]; exact hne)
      · exact R0.refl _ _
    | wakeX t =>
      simp only [step]
      split
      · exact doWake_R0_other s t D hnc
      · exact R0.refl _ _
  · right; exact hp

theorem R_wakeX {D : Dom} {s s' : St} (h : R D s s') (t : Tid) (hnc : dom t ≠ D) :
    R D (step dom s (.wakeX t)) (step dom s' (.wakeX t)) := by
  have e1 : R0 D s (step dom s (.wakeX t)) := by
    simp only [step]; split
    · exact doWake_R0_other s t D hnc
    · exact R0.refl _ _
  have e2 : R0 D s' (step dom s' (.wakeX t)) := by
    simp only [step]; split
    · exact doWake_R0_other s' t D hnc
    · exact R0.refl _ _
  have c1 : (step dom s (.wakeX t)).cur = s.cur := by
    simp only [step]; split
    · unfold doWake; split <;> rfl
    · rfl
  have c2 : (step dom s' (.wakeX t)).cur = s'.cur := by
    simp only [step]; split
    · unfold doWake; split <;> rfl
    · rfl
  exact R_frame h e1 e2 c1 c2

theorem R_yield {D : Dom} {s s' : St} (h : R D s s') (hD : s.rd = D) :
    R D (yieldStep s) (yieldStep s') := by
  obtain ⟨⟨hp, hr, hq, hw⟩, hc⟩ := h
  obtain ⟨p', rd', c', q', w'⟩ := s'
  dsimp only at hp hr hq hw hc
  have hc := hc hD
  subst hp; subst hr; subst hc; subst hD
  have hq' : q' s.rd = s.q s.rd := hq.symm
  unfold yieldStep
  dsimp only
  rw [hq']
  split
  · exact ⟨⟨rfl, rfl, hq, hw⟩, fun _ => rfl⟩
  · split
    · refine ⟨⟨rfl, rfl, ?_, hw⟩, fun _ => rfl⟩
      simp [upd]
    · exact ⟨⟨rfl, rfl, hq, hw⟩, fun _ => rfl⟩

theorem R_block {D : Dom} {s s' : St} (hi : Inv dom s) (h : R D s s') (hD : s.rd = D) (dl : Option Nat) :
    R D (blockStep dom s dl) (blockStep dom s' dl) := by
  obtain ⟨⟨hp, hr, hq, hw⟩, hc⟩ := h
  obtain ⟨p', rd', c', q', w'⟩ := s'
  dsimp only at hp hr hq hw hc
  have hc := hc hD
  subst hp; subst hr; subst hc
  unfold blockStep
  dsimp only
  split
  · exact ⟨⟨rfl, rfl, hq, hw⟩, fun _ => rfl⟩
  · next c ex hcur =>
    have hcd : dom c = D := by rw [hi.cur_dom c ex hcur]; exact hD
    refine R_popBetween ⟨⟨rfl, rfl, hq, ?_⟩, fun _ => rfl⟩
    simp [upd, hcd, hw]

theorem R_wake {D : Dom} {s s' : St} (h : R D s s') (hD : s.rd = D) (t : Tid) :
    R D (step dom s (.wake t)) (step dom s' (.wake t)) := by
  obtain ⟨⟨hp, hr, hq, hw⟩, hc⟩ := h
  obtain ⟨p', rd', c', q', w'⟩ := s'
  dsimp only at hp hr hq hw hc
  have hc := hc hD
  subst hp; subst hr; subst hc
  simp only [step]
  by_cases hg : s.cur.isSome = true ∧ dom t = s.rd
  · simp only [hg, and_self, ite_true]
    have hdt : dom t = D := by rw [hg.2]; exact hD
    unfold doWake
    rw [hdt] at *
    have hw' : w' D = s.wl D := hw.symm
    simp only [hw']
    split
    · refine ⟨⟨rfl, rfl, ?_, ?_⟩, fun _ => rfl⟩
      · simp [upd, hq]
      · simp [upd, hw]
    · exact ⟨⟨rfl, rfl, hq, hw⟩, fun _ => rfl⟩
  · simp only [hg, ite_false]
    exact ⟨⟨rfl, rfl, hq, hw⟩, fun _ => rfl⟩


/-- The event moves the clock part: a tick index not seen before (through
    `tick` or through the idle loop's `poll`), or a `set_table` call. -/
def clockEv (s : St) : Ev → Prop
  | .tick n => s.part.seen < n
  | .poll n => s.part.seen < n
  | .setTable _ => True
  | _ => False

instance (s : St) (e : Ev) : Decidable (clockEv s e) := by
  cases e <;> simp only [clockEv] <;> infer_instance

theorem clockEv_congr {s s' : St} (h : s.part = s'.part) (e : Ev) : clockEv s e ↔ clockEv s' e := by
  cases e <;> simp [clockEv, h]

/-- A new tick is the same step whether the kernel takes it from EL0 (`tick`)
    or from the idle loop (`poll`).  Which of the two happens depends on
    whether the running domain idles. -/
theorem poll_eq_tick (dom : Tid → Dom) (s : St) (n : Nat) (h : s.part.seen < n) :
    step dom s (.poll n) = step dom s (.tick n) := by
  simp [step, pollStep, h]

/-- A tick event whose index is not new (`n <= seen`: a repeated or an older
    index) changes nothing at all. -/
theorem tick_repeat_noop (dom : Tid → Dom) (s : St) (n : Nat) (h : n ≤ s.part.seen) :
    step dom s (.tick n) = s := by
  simp [step, tickStep, h]

/-- An event that does nothing in every state.  It is not a clock event, so it
    can stand in a run where the other run has one more local event. -/
theorem tick_zero_noop (dom : Tid → Dom) (s : St) : step dom s (.tick 0) = s :=
  tick_repeat_noop dom s 0 (Nat.zero_le _)

/-- How two event lists of two runs may differ, as seen from `D`, at one step.
    * A clock event is the same public event in both runs.  A new tick may
      come through `tick` in one run and through `poll` in the other.
    * Any other event is local to the running domain: an action of its task,
      its idle loop taking an interrupt without a new tick (`poll n` with
      `n <= seen`), or a repeated tick index (a no-op).  Both runs have a local
      event there.  If `D` is the running domain, it is the same event.  If
      another domain runs, the two events are unrelated. -/
def Shape (D : Dom) (s : St) (e e' : Ev) : Prop :=
  (clockEv s e → pubOf e' = pubOf e) ∧
  (¬ clockEv s e → ¬ clockEv s e' ∧ (s.rd = D → e' = e))

/-- The hypotheses of non-interference at one step: `Shape`, and no wake across
    domains into `D` in either run. -/
def Ok (dom : Tid → Dom) (D : Dom) (s : St) (e e' : Ev) : Prop :=
  Shape D s e e' ∧ NoCross dom D e ∧ NoCross dom D e'

/-- The two event lists agree step by step, as seen from `D`. -/
def Sync (dom : Tid → Dom) (D : Dom) : St → St → List Ev → List Ev → Prop
  | _, _, [], [] => True
  | s, s', e :: es, e' :: es' =>
      Ok dom D s e e' ∧ Sync dom D (step dom s e) (step dom s' e') es es'
  | _, _, _, _ => False

/-- The same event in both runs keeps the relation. -/
theorem R_step_same {D : Dom} {s s' : St} (hi : Inv dom s) (hi' : Inv dom s') (hR : R D s s')
    (e : Ev) (hnc : NoCross dom D e) : R D (step dom s e) (step dom s' e) := by
  cases e with
  | tick n => exact R_tick hR n
  | poll n => exact R_poll hR n
  | setTable t => exact R_setTable hR t
  | wakeX t => exact R_wakeX hR t hnc
  | yield =>
    by_cases hD : s.rd = D
    · exact R_yield hR hD
    · exact R_of_other hR hD (yield_R0_other s D hD) (yield_R0_other s' D (by rw [← hR.rd]; exact hD))
  | block dl =>
    by_cases hD : s.rd = D
    · exact R_block hi hR hD dl
    · exact R_of_other hR hD (block_R0_other s hi D dl hD)
        (block_R0_other s' hi' D dl (by rw [← hR.rd]; exact hD))
  | wake t =>
    by_cases hD : s.rd = D
    · exact R_wake hR hD t
    · have hne' : s'.rd ≠ D := by rw [← hR.rd]; exact hD
      have a1 := (act_R0_other hi hD (.wake t) trivial).resolve_right (fun h => h rfl)
      have a2 := (act_R0_other hi' hne' (.wake t) trivial).resolve_right (fun h => h rfl)
      exact R_of_other hR hD a1 a2

/-- A local event of another domain is invisible to `D`. -/
theorem local_R0_other {D : Dom} {s : St} (hi : Inv dom s) (hne : s.rd ≠ D) (e : Ev)
    (hc : ¬ clockEv s e) (hnc : NoCross dom D e) : R0 D s (step dom s e) := by
  cases e with
  | tick n =>
    have hn : n ≤ s.part.seen := by simp only [clockEv] at hc; omega
    simp only [step, tickStep, hn, ite_true]
    exact R0.refl _ _
  | poll n =>
    have hn : ¬ s.part.seen < n := hc
    simp only [step, pollStep, hn, ite_false]
    have h1 : R0 D s (serve s s.rd n) := serve_R0_other s s.rd n D hne
    have h2 : R0 D (serve s s.rd n) (pollRest (serve s s.rd n)) := by
      unfold pollRest; split
      · exact R0.refl _ _
      · exact popBetween_R0_other _ D (by simpa using hne)
    exact h1.trans h2
  | setTable t => exact absurd trivial hc
  | yield => exact (act_R0_other hi hne .yield hnc).resolve_right (fun h => h rfl)
  | block dl => exact (act_R0_other hi hne (.block dl) hnc).resolve_right (fun h => h rfl)
  | wake t => exact (act_R0_other hi hne (.wake t) hnc).resolve_right (fun h => h rfl)
  | wakeX t => exact (act_R0_other hi hne (.wakeX t) hnc).resolve_right (fun h => h rfl)

theorem R_step {D : Dom} {s s' : St} (hi : Inv dom s) (hi' : Inv dom s') (hR : R D s s')
    (e e' : Ev) (hok : Ok dom D s e e') : R D (step dom s e) (step dom s' e') := by
  by_cases hc : clockEv s e
  · have hp := hok.1.1 hc
    have tk : ∀ n, s.part.seen < n → R D (step dom s (.tick n)) (step dom s' (.tick n)) :=
      fun n _ => R_tick hR n
    have key : ∀ n, s.part.seen < n → pubOf e' = some (.tick n) →
        R D (step dom s (.tick n)) (step dom s' e') := by
      intro n hn hpe
      have hn' : s'.part.seen < n := by rw [← hR.part]; exact hn
      cases e' with
      | tick m =>
        simp only [pubOf, Option.some.injEq, Pub.tick.injEq] at hpe
        subst hpe; exact tk m hn
      | poll m =>
        simp only [pubOf, Option.some.injEq, Pub.tick.injEq] at hpe
        subst hpe; rw [poll_eq_tick dom s' m hn']; exact tk m hn
      | setTable t => simp [pubOf] at hpe
      | yield => simp [pubOf] at hpe
      | block dl => simp [pubOf] at hpe
      | wake t => simp [pubOf] at hpe
      | wakeX t => simp [pubOf] at hpe
    cases e with
    | tick n => exact key n hc hp
    | poll n => rw [poll_eq_tick dom s n hc]; exact key n hc hp
    | setTable t =>
      cases e' with
      | setTable t' =>
        simp only [pubOf, Option.some.injEq, Pub.setTable.injEq] at hp
        subst hp; exact R_setTable hR t'
      | tick m => simp [pubOf] at hp
      | poll m => simp [pubOf] at hp
      | yield => simp [pubOf] at hp
      | block dl => simp [pubOf] at hp
      | wake t => simp [pubOf] at hp
      | wakeX t => simp [pubOf] at hp
    | yield => exact absurd hc (by simp [clockEv])
    | block dl => exact absurd hc (by simp [clockEv])
    | wake t => exact absurd hc (by simp [clockEv])
    | wakeX t => exact absurd hc (by simp [clockEv])
  · obtain ⟨hc', hsame⟩ := hok.1.2 hc
    by_cases hD : s.rd = D
    · have he := hsame hD
      subst he
      exact R_step_same hi hi' hR e' hok.2.1
    · have hne' : s'.rd ≠ D := by rw [← hR.rd]; exact hD
      have hc2 : ¬ clockEv s' e' := fun h => hc' ((clockEv_congr hR.part e').mpr h)
      exact R_of_other hR hD (local_R0_other hi hD e hc hok.2.1)
        (local_R0_other hi' hne' e' hc2 hok.2.2)

/-- (5) Non-interference.  Let `D` be a domain.  Take two runs from two
    well formed states that look the same from `D` (`R D`: same clock part, same
    running domain, same queue and waiting list of `D`, same running task if
    `D` runs; every other domain may be arbitrary).  If the event lists agree
    step by step as in `Sync` (same new ticks and `set_table` calls, a tick
    counting as the same whether it comes through `tick` or `poll`; the same
    local events whenever `D` runs; otherwise arbitrary local events of the
    running domain, among them its idle loop's interrupts; no wake across
    domains into `D`), then `D`'s view (queue, timed waits, which of its tasks
    runs with the slice bit) is the same after every step, and the relation
    still holds at the end.  The runs are compared position by position; a run
    with fewer local events is padded with `tick 0` (`tick_zero_noop`). -/
theorem noninterference (D : Dom) :
    ∀ (es es' : List Ev) (s s' : St), Inv dom s → Inv dom s' → R D s s' →
      Sync dom D s s' es es' →
      viewTrace dom D s es = viewTrace dom D s' es' ∧ R D (exec dom s es) (exec dom s' es') := by
  intro es
  induction es with
  | nil =>
    intro es' s s' hi hi' hR hs
    cases es' with
    | nil => exact ⟨by simp [viewTrace, view_eq hi hi' hR], hR⟩
    | cons e' es' => simp [Sync] at hs
  | cons e es ih =>
    intro es' s s' hi hi' hR hs
    cases es' with
    | nil => simp [Sync] at hs
    | cons e' es' =>
      obtain ⟨hok, hrest⟩ := hs
      have hR' := R_step hi hi' hR e e' hok
      obtain ⟨h1, h2⟩ := ih es' _ _ (inv_step _ hi e) (inv_step _ hi' e') hR' hrest
      refine ⟨?_, h2⟩
      simp only [viewTrace]
      rw [h1, view_eq hi hi' hR]

/-- The final views agree. -/
theorem noninterference_final (D : Dom) (es es' : List Ev) (s s' : St)
    (hi : Inv dom s) (hi' : Inv dom s') (hR : R D s s') (hs : Sync dom D s s' es es') :
    view dom D (exec dom s es) = view dom D (exec dom s' es') :=
  view_eq (inv_exec s hi es) (inv_exec s' hi' es') (noninterference D es es' s s' hi hi' hR hs).2

end ni2

/-! ## (6) The hypothesis "no wake across domains into D" is needed -/

/-- A state: root task 0 runs, task 10 of domain 1 waits without deadline. -/
def s0 : St :=
  boot 0 0 (fun _ => []) (fun d => if d = 1 then [(10, none)] else [])

theorem s0_inv : Inv dom0 s0 := by
  apply inv_boot
  · intro d t ht; simp at ht
  · intro d w hw
    by_cases hd : d = 1
    · subst hd; simp at hw; subst hw; decide
    · simp [hd] at hw
  · decide

/-- (6) With a cross-domain wake into `D = 1`, the views of two runs that agree in
    all other respects differ: run 1 wakes task 10 through a shared object, run 2
    does something else, and `D`'s queue differs after the step.  Everything
    else of `Ok` holds: same state, `Shape`, no cross wake in the second run. -/
theorem cross_wake_breaks :
    Inv dom0 s0 ∧ R 1 s0 s0 ∧ Shape 1 s0 (.wakeX 10) .yield ∧
    NoCross dom0 1 (.yield) ∧ ¬ NoCross dom0 1 (.wakeX 10) ∧
    view dom0 1 (step dom0 s0 (.wakeX 10)) ≠ view dom0 1 (step dom0 s0 .yield) := by
  refine ⟨s0_inv, ⟨⟨rfl, rfl, rfl, rfl⟩, fun _ => rfl⟩, ?_, trivial, ?_, ?_⟩
  · refine ⟨fun h => ?_, fun _ => ⟨fun h => h, fun h => ?_⟩⟩
    · exact h.elim
    · simp [s0, boot] at h
  · simp [NoCross, dom0]
  · decide


/-! ## Decidable checks of the hypotheses, for concrete examples -/

instance (dom : Tid → Dom) (D : Dom) (e : Ev) : Decidable (NoCross dom D e) := by
  cases e <;> simp only [NoCross] <;> infer_instance

instance (D : Dom) (s : St) (e e' : Ev) : Decidable (Shape D s e e') := by
  unfold Shape; infer_instance

instance (dom : Tid → Dom) (D : Dom) (s : St) (e e' : Ev) : Decidable (Ok dom D s e e') := by
  unfold Ok; infer_instance

instance (dom : Tid → Dom) (D : Dom) :
    (s s' : St) → (es es' : List Ev) → Decidable (Sync dom D s s' es es')
  | _, _, [], [] => isTrue trivial
  | s, s', e :: es, e' :: es' =>
      have : Decidable (Sync dom D (step dom s e) (step dom s' e') es es') :=
        instDecidableSync dom D (step dom s e) (step dom s' e') es es'
      inferInstanceAs (Decidable (Ok dom D s e e' ∧ Sync dom D (step dom s e) (step dom s' e') es es'))
  | _, _, [], _ :: _ => isFalse (fun h => h)
  | _, _, _ :: _, [] => isFalse (fun h => h)


/-! ## (5) is not vacuous: a concrete pair of runs -/

/-- Domain 1 has the same queue and the same timed wait in both runs.  The other
    domains differ: run A has ready tasks in domains 0 and 2 and a waiter in
    domain 0; run B has nothing ready there and two waiters in domain 2. -/
def sA : St :=
  boot 0 0 (fun d => if d = 0 then [1] else if d = 1 then [10] else if d = 2 then [20] else [])
    (fun d => if d = 0 then [(5, none)] else if d = 1 then [(11, some 4)] else [])

def sB : St :=
  boot 0 0 (fun d => if d = 1 then [10] else [])
    (fun d => if d = 1 then [(11, some 4)] else if d = 2 then [(21, none), (22, none)] else [])

/-- Same new ticks and `set_table`; the same events while domain 1 runs (places
    4 to 7 and 13); unrelated events while domains 0 and 2 run.
    * Place 2: run A wakes a task of domain 0, run B's only task blocks, so
      domain 0 idles in run B from there on.
    * Ticks 2 and 5 reach run B through its idle loop (`poll`), run A from EL0
      (`tick`).  Tick 10 is the other way round.
    * Place 4: domain 1 wakes task 21 of domain 2 (`wakeX`, outward).  It takes
      effect in run B only.  Domain 1 sees nothing of that.
    * Places 9 and 16: an interrupt without a new tick reaches the idle loop of
      one run (`poll 4`, `poll 9`) while the other run does something else or
      nothing (`tick 0` pads).
    * Ticks 6, 8 and 11 are skipped in both runs (jumps). -/
def esA : List Ev :=
  [.setTable [(1, 2), (0, 1), (2, 1)], .tick 1, .wake 5, .tick 2, .wakeX 21, .tick 3, .wake 11,
   .yield, .tick 4, .yield, .tick 5, .yield, .tick 7, .block (some 9), .tick 9, .block none,
   .poll 9, .poll 10, .tick 12]

def esB : List Ev :=
  [.setTable [(1, 2), (0, 1), (2, 1)], .tick 1, .block none, .poll 2, .wakeX 21, .tick 3, .wake 11,
   .yield, .tick 4, .poll 4, .poll 5, .wake 22, .tick 7, .block (some 9), .tick 9, .yield,
   .tick 0, .tick 10, .tick 12]

#eval (viewTrace dom0 1 sA esA).map fun v => (v.q, v.wl, v.run)
#eval (viewTrace dom0 1 sB esB).map fun v => (v.q, v.wl, v.run)
#eval (viewTrace dom0 0 sA esA).map fun v => (v.q, v.wl, v.run)
#eval (viewTrace dom0 0 sB esB).map fun v => (v.q, v.wl, v.run)

theorem example_views_equal : viewTrace dom0 1 sA esA = viewTrace dom0 1 sB esB := by decide

/-- The relation and the hypotheses hold in the example (so the theorem applies to it)
    while the views of both other domains differ. -/
theorem example_applies :
    Inv dom0 sA ∧ Inv dom0 sB ∧ R 1 sA sB ∧ Sync dom0 1 sA sB esA esB ∧
    viewTrace dom0 0 sA esA ≠ viewTrace dom0 0 sB esB ∧
    viewTrace dom0 2 sA esA ≠ viewTrace dom0 2 sB esB := by
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
  · apply inv_boot
    · intro d t ht
      by_cases h0 : d = 0
      · subst h0; simp at ht; subst ht; decide
      · by_cases h1 : d = 1
        · subst h1; simp at ht; subst ht; decide
        · by_cases h2 : d = 2
          · subst h2; simp at ht; subst ht; decide
          · simp [h0, h1, h2] at ht
    · intro d w hw
      by_cases h0 : d = 0
      · subst h0; simp at hw; subst hw; decide
      · by_cases h1 : d = 1
        · subst h1; simp at hw; subst hw; decide
        · simp [h0, h1] at hw
    · decide
  · apply inv_boot
    · intro d t ht
      by_cases h1 : d = 1
      · subst h1; simp at ht; subst ht; decide
      · simp [h1] at ht
    · intro d w hw
      by_cases h1 : d = 1
      · subst h1; simp at hw; subst hw; decide
      · by_cases h2 : d = 2
        · subst h2; simp at hw; rcases hw with rfl | rfl <;> decide
        · simp [h1, h2] at hw
    · decide
  · exact ⟨⟨rfl, rfl, rfl, rfl⟩, fun _ => by decide⟩
  · decide
  · decide
  · decide

/-- State after the first `k` events of a run. -/
def after (s : St) (es : List Ev) (k : Nat) : St := exec dom0 s (es.take k)

/-- Everything of a state for domains 0 to 2. -/
def obs (s : St) : Nat × Nat × Option (Tid × Bool) × List (List Tid) × List (List (Tid × Option Nat)) :=
  (s.rd, s.part.seen, s.cur, (List.range 3).map s.q, (List.range 3).map s.wl)

/-- Places of the local events of a run that change its state. -/
def effective (s : St) (es : List Ev) : List Nat :=
  (List.range es.length).filter fun k =>
    obs (after s es k) != obs (after s es (k + 1)) &&
      (match es[k]? with | some e => !decide (clockEv (after s es k) e) | none => false)

/-- What the example exercises.  Run A: a wake inside domain 0 (place 2), a wake
    inside domain 1 (6), yields and blocks.  Run B: the outward `wakeX` of
    domain 1 (4) and a wake inside domain 2 (11) take effect as well. -/
theorem example_effects :
    effective sA esA = [2, 6, 7, 9, 13, 15] ∧ effective sB esB = [2, 4, 6, 7, 11, 13, 15] := by
  decide

/-- The example keeps to the kernel's call pattern: a `poll` occurs only while
    its run has no task on the CPU, and a new tick comes through `tick` only
    while one is. -/
theorem example_kernel_shaped :
    ((List.range esA.length).all fun k => match esA[k]? with
      | some (.poll _) => (after sA esA k).cur.isNone
      | some (.tick n) => (after sA esA k).cur.isSome || decide (n ≤ (after sA esA k).part.seen)
      | _ => true) = true ∧
    ((List.range esB.length).all fun k => match esB[k]? with
      | some (.poll _) => (after sB esB k).cur.isNone
      | some (.tick n) => (after sB esB k).cur.isSome || decide (n ≤ (after sB esB k).part.seen)
      | _ => true) = true := by
  decide


/-! ## Witnesses for the remarks in the header -/

/-- `exP` of `guard_unsound_without_hyp` is reached from boot: tick 1, a
    `set_table` call, tick 2. -/
theorem exP_reachable :
    pexec (boot 0 0 (fun _ => []) (fun _ => [])).part [.tick 1, .setTable [(1, 1)], .tick 2] = exP := by
  decide

/-- A table governs only until a later call: a second call one tick later
    discards the first table, which then never governs any tick. -/
def bootP : St :=
  boot 0 0 (fun d => if d = 1 then [10] else if d = 2 then [20] else []) (fun _ => [])

theorem first_table_never_governs :
    (exec dom0 bootP [.setTable [(1, 1)], .tick 1, .setTable [(2, 1)], .tick 2]).rd = 0 ∧
    (exec dom0 bootP [.setTable [(1, 1)], .tick 1, .setTable [(2, 1)], .tick 2, .tick 3]).rd = 2 ∧
    (exec dom0 bootP [.setTable [(1, 1)], .tick 1, .setTable [(2, 1)], .tick 2, .tick 3, .tick 4]).rd = 2 := by
  decide

/-- The tick indexes are an input the theorem leans on.  Same state, the same
    (absent) actions, and the views of domain 1 differ when one run sees ticks
    1 and 2 and the other skips tick 1. -/
def jS : St :=
  { part := { tbl := [(0, 1), (1, 1)], epoch := 0, pend := none, seen := 0 },
    rd := 0, cur := some (0, false),
    q := fun d => if d = 1 then [10, 11] else [], wl := fun _ => [] }

theorem tick_indexes_needed :
    (view dom0 1 (exec dom0 jS [.tick 1, .tick 2])).q = [11, 10] ∧
    (view dom0 1 (exec dom0 jS [.yield, .tick 2])).q = [10, 11] ∧
    ¬ Sync dom0 1 jS jS [.tick 1, .tick 2] [.yield, .tick 2] := by decide

/-- The state `sample` of the `#eval` near the top is an `Inv` state. -/
theorem sample_inv : Inv dom0 sample := by
  apply inv_boot
  · intro d t ht
    by_cases h0 : d = 0
    · subst h0; simp at ht; subst ht; decide
    · by_cases h1 : d = 1
      · subst h1; simp at ht; rcases ht with rfl | rfl <;> decide
      · by_cases h2 : d = 2
        · subst h2; simp at ht; subst ht; decide
        · simp [h0, h1, h2] at ht
  · intro d w hw
    by_cases h1 : d = 1
    · subst h1; simp at hw; subst hw; decide
    · simp [h1] at hw
  · decide

/-- `Inv` has no uniqueness: the running task may also sit in a queue. -/
theorem inv_with_duplicate :
    Inv dom0 (boot 0 0 (fun d => if d = 0 then [0, 0] else []) (fun _ => [])) := by
  apply inv_boot
  · intro d t ht
    by_cases h0 : d = 0
    · subst h0; simp at ht; subst ht; decide
    · simp [h0] at ht
  · intro d w hw; simp at hw
  · decide


/-! ## Further facts about the model -/

section extra
variable {dom : Tid → Dom}

/-- The decision the guard arms on: is the tick after the last processed one a
    domain change?  (`Core::next_tick_switches`) -/
def nextSwitches (s : St) : Prop := s.part.plan (s.part.seen + 1) ≠ s.rd

/-- The guard decision is not changed by a `set_table` call, unless an earlier
    pending table starts exactly at the next tick (see `guard_unsound_without_hyp`). -/
theorem guard_sound_event (s : St) (t : Table) (hpa : PendAfter s.part)
    (hk : ∀ t0, s.part.pend ≠ some (s.part.seen + 1, t0)) :
    nextSwitches (step dom s (.setTable t)) ↔ nextSwitches s := by
  unfold nextSwitches
  show (s.part.setTable t).plan ((s.part.setTable t).seen + 1) ≠ s.rd ↔ _
  rw [setTable_seen, guard_sound _ _ hpa hk]

/-- A tick event with index `n > seen` (a jump when `n > seen + 1`) makes the
    domain of tick `n` run, whatever the skipped ticks would have been. -/
theorem tick_jump_domain (s : St) (n : Nat) (hn : s.part.seen < n) :
    (step dom s (Ev.tick n)).rd = s.part.plan n ∧ (step dom s (Ev.tick n)).part.seen = n := by
  have hn' : ¬ n ≤ s.part.seen := by omega
  have hpl : (s.part.advance n).domNow n = s.part.plan n := domNow_advance _ _
  simp only [step, tickStep, hn', ite_false]
  by_cases hnew : (s.part.advance n).domNow n = s.rd
  · simp only [hnew, ite_true, sameStep_rd, sameStep_part, serve_rd, serve_part]
    exact ⟨by rw [← hpl, hnew], trivial⟩
  · simp only [hnew, ite_false, enter_rd, enter_part, serve_rd, serve_part, cut_part]
    exact ⟨hpl, trivial⟩

/-- Timer wakes stay in the served domain. -/
theorem serve_only_own (s : St) (d n e : Nat) (h : e ≠ d) :
    (serve s d n).q e = s.q e ∧ (serve s d n).wl e = s.wl e := by
  simp [serve, upd, h]

/-- A timer wake never fires early: a task that `serve` moves into the queue
    already was in it, or has a deadline `<= n`. -/
theorem serve_not_early (s : St) (d n t : Nat) (h : t ∈ (serve s d n).q d) :
    t ∈ s.q d ∨ ∃ w ∈ s.wl d, w.1 = t ∧ ∃ k, w.2 = some k ∧ k ≤ n := by
  simp only [serve, upd, ite_true] at h
  rcases List.mem_append.mp h with h1 | h1
  · exact Or.inl h1
  · right
    obtain ⟨w, hw, rfl⟩ := List.mem_map.mp h1
    obtain ⟨hw1, hw2⟩ := List.mem_filter.mp (List.mem_reverse.mp hw)
    refine ⟨w, hw1, rfl, ?_⟩
    unfold expired at hw2
    revert hw2
    rcases w with ⟨a, b⟩
    cases b <;> simp

/-- `Core::block` as written when nothing runs: clear `running`, pop the queue. -/
def blockIdleRust (s : St) : St := popBetween { s with cur := none }

/-- Under `Inv` that is what `blockStep` does when nothing runs: nothing. -/
theorem blockIdleRust_eq (s : St) (hi : Inv dom s) (hc : s.cur = none) (dl : Option Nat) :
    blockIdleRust s = blockStep dom s dl := by
  have hq : s.q s.rd = [] := hi.idle_empty hc
  obtain ⟨p, rd, c, q, w⟩ := s
  simp only at hc hq
  subst hc
  simp [blockIdleRust, blockStep, popBetween, hq]

end extra


/-! ## Timed waits fire, and only in the domain that runs

`serve_not_early` says that a timer wake is never early.  The theorems here
say that it does happen and where: a waiter whose deadline has come is moved
when its domain is served, a tick and an idle poll serve the domain that
runs, and they serve no other domain (`waiter_leaves_only` puts the "only"
into one statement).  "Due at `n`" is written out (`w.2 = some k` and
`k <= n`), not through `expired`, so that the statements do not lean on that
definition.  Ticks are indexes; nothing here is about instants, and nothing
says that a tick ever comes. -/

section fire

/-- `expired` written out: the waiter has a deadline and it is not after `n`. -/
theorem expired_iff (w : Tid × Option Nat) (n : Nat) :
    expired w n = true ↔ ∃ k, w.2 = some k ∧ k ≤ n := by
  obtain ⟨a, b⟩ := w
  cases b <;> simp [expired]

theorem not_expired_iff (w : Tid × Option Nat) (n : Nat) :
    expired w n = false ↔ ∀ k, w.2 = some k → n < k := by
  obtain ⟨a, b⟩ := w
  cases b <;> simp [expired]

/-- `serve d n` wakes every waiter of `d` that is due at `n`: it is in `d`'s
    queue afterwards, no due waiter is left in `d`'s waiting list, a waiter
    that is not due stays, and the tasks that were queued keep their places in
    front.  (That nothing else is queued is `serve_not_early`.) -/
theorem serve_wakes_due (s : St) (d n : Nat) :
    (∀ w ∈ s.wl d, ∀ k, w.2 = some k → k ≤ n → w.1 ∈ (serve s d n).q d) ∧
    (∀ w ∈ (serve s d n).wl d, ∀ k, w.2 = some k → n < k) ∧
    (∀ w ∈ s.wl d, (∀ k, w.2 = some k → n < k) → w ∈ (serve s d n).wl d) ∧
    (∃ r, (serve s d n).q d = s.q d ++ r) := by
  refine ⟨?_, ?_, ?_, ⟨(((s.wl d).filter (fun w => expired w n)).reverse).map Prod.fst, ?_⟩⟩
  · intro w hw k hk hkn
    simp only [serve, upd, ite_true]
    apply List.mem_append_right
    exact List.mem_map.mpr ⟨w, List.mem_reverse.mpr
      (List.mem_filter.mpr ⟨hw, (expired_iff w n).mpr ⟨k, hk, hkn⟩⟩), rfl⟩
  · intro w hw
    simp only [serve, upd, ite_true] at hw
    have h := (List.mem_filter.mp hw).2
    exact (not_expired_iff w n).mp (by simpa using h)
  · intro w hw h
    simp only [serve, upd, ite_true]
    exact List.mem_filter.mpr ⟨hw, by simpa using (not_expired_iff w n).mpr h⟩
  · simp only [serve, upd, ite_true]

/-- The order of timer wakes.  The queue of `d` keeps its tasks in front, and
    of two due waiters the one that started to wait later (`w2`) is queued
    before the earlier one (`w1`). -/
theorem serve_due_order (s : St) (d n : Nat) (a b c : List (Tid × Option Nat))
    (w1 w2 : Tid × Option Nat) (k1 k2 : Nat)
    (hwl : s.wl d = a ++ w1 :: (b ++ w2 :: c))
    (h1 : w1.2 = some k1) (hk1 : k1 ≤ n) (h2 : w2.2 = some k2) (hk2 : k2 ≤ n) :
    ∃ x y z, (serve s d n).q d = s.q d ++ (x ++ w2.1 :: (y ++ w1.1 :: z)) := by
  have e1 : expired w1 n = true := (expired_iff w1 n).mpr ⟨k1, h1, hk1⟩
  have e2 : expired w2 n = true := (expired_iff w2 n).mpr ⟨k2, h2, hk2⟩
  refine ⟨((c.filter (fun w => expired w n)).reverse).map Prod.fst,
    ((b.filter (fun w => expired w n)).reverse).map Prod.fst,
    ((a.filter (fun w => expired w n)).reverse).map Prod.fst, ?_⟩
  simp [serve, upd, hwl, e1, e2]

@[simp] theorem enter_wl (s : St) (d : Dom) : (enter s d).wl = s.wl := by
  unfold enter; split <;> rfl
@[simp] theorem cut_wl (s : St) : (cut s).wl = s.wl := by
  unfold cut; split <;> rfl
@[simp] theorem sameStep_wl (s : St) : (sameStep s).wl = s.wl := by
  unfold sameStep; split <;> (try split) <;> simp
@[simp] theorem popBetween_wl (s : St) : (popBetween s).wl = s.wl := by
  unfold popBetween; split <;> rfl
@[simp] theorem pollRest_wl (s : St) : (pollRest s).wl = s.wl := by
  unfold pollRest; split <;> simp
@[simp] theorem pollRest_rd (s : St) : (pollRest s).rd = s.rd := by
  unfold pollRest; split <;> simp

/-- A queued task of `d` is still queued after `enter d`, or it runs. -/
theorem enter_mem (s : St) (d : Dom) (x : Tid) (h : x ∈ s.q d) :
    x ∈ (enter s d).q d ∨ ∃ b, (enter s d).cur = some (x, b) := by
  unfold enter
  split
  · next y r hr =>
    rw [hr] at h
    rcases List.mem_cons.mp h with rfl | h1
    · exact Or.inr ⟨false, rfl⟩
    · exact Or.inl (by simpa [upd] using h1)
  · next hr => rw [hr] at h; simp at h

theorem popBetween_mem (s : St) (x : Tid) (h : x ∈ s.q s.rd) :
    x ∈ (popBetween s).q s.rd ∨ ∃ b, (popBetween s).cur = some (x, b) := by
  unfold popBetween
  split
  · next y r hr =>
    rw [hr] at h
    rcases List.mem_cons.mp h with rfl | h1
    · exact Or.inr ⟨true, rfl⟩
    · exact Or.inl (by simpa [upd] using h1)
  · next hr => rw [hr] at h; simp at h

theorem sameStep_mem (s : St) (x : Tid) (h : x ∈ s.q s.rd) :
    x ∈ (sameStep s).q s.rd ∨ ∃ b, (sameStep s).cur = some (x, b) := by
  unfold sameStep
  split
  · exact Or.inl h
  · split
    · next y r hr =>
      rw [hr] at h
      rcases List.mem_cons.mp h with rfl | h1
      · exact Or.inr ⟨false, rfl⟩
      · exact Or.inl (by simp [upd, h1])
    · exact Or.inl h
  · exact enter_mem s s.rd x h

theorem pollRest_mem (s : St) (x : Tid) (h : x ∈ s.q s.rd) :
    x ∈ (pollRest s).q s.rd ∨ ∃ b, (pollRest s).cur = some (x, b) := by
  unfold pollRest
  split
  · exact Or.inl h
  · exact popBetween_mem s x h

/-- A new tick serves the domain that runs after it.  After a tick event with
    a new index `n`: no waiter that is due at `n` is left in the waiting list
    of the domain that now runs, each waiter of that domain that was due is in
    its queue or is the running task, and each of its waiters that was not due
    still waits.  This holds for a tick inside a window and for a tick that
    changes the domain, jumps included, and for a new index that comes through
    `poll` (`poll_eq_tick`).
    It does not say that the waiter runs soon, and it says nothing about a
    domain that does not get the tick (see `tick_frame`, `tick_old_domain`). -/
theorem tick_serves (dom : Tid → Dom) (s : St) (n : Nat) (hn : s.part.seen < n) :
    (∀ w ∈ (step dom s (.tick n)).wl (step dom s (.tick n)).rd,
        ∀ k, w.2 = some k → n < k) ∧
    (∀ w ∈ s.wl (step dom s (.tick n)).rd, ∀ k, w.2 = some k → k ≤ n →
        w.1 ∈ (step dom s (.tick n)).q (step dom s (.tick n)).rd ∨
        ∃ b, (step dom s (.tick n)).cur = some (w.1, b)) ∧
    (∀ w ∈ s.wl (step dom s (.tick n)).rd, (∀ k, w.2 = some k → n < k) →
        w ∈ (step dom s (.tick n)).wl (step dom s (.tick n)).rd) := by
  have hn' : ¬ n ≤ s.part.seen := by omega
  simp only [step, tickStep, hn', ite_false]
  by_cases hnew : (s.part.advance n).domNow n = s.rd
  · simp only [hnew, ite_true, sameStep_rd, serve_rd, sameStep_wl]
    have hs := serve_wakes_due { s with part := { s.part.advance n with seen := n } } s.rd n
    refine ⟨hs.2.1, ?_, hs.2.2.1⟩
    intro w hw k hk hkn
    exact sameStep_mem (serve { s with part := { s.part.advance n with seen := n } } s.rd n)
      w.1 (hs.1 w hw k hk hkn)
  · simp only [hnew, ite_false, enter_rd, serve_rd, enter_wl]
    have hs := serve_wakes_due
      ({ cut { s with part := { s.part.advance n with seen := n } } with
          rd := (s.part.advance n).domNow n } : St) ((s.part.advance n).domNow n) n
    refine ⟨hs.2.1, ?_, ?_⟩
    · intro w hw k hk hkn
      exact enter_mem _ _ w.1 (hs.1 w (by simpa using hw) k hk hkn)
    · intro w hw h
      exact hs.2.2.1 w (by simpa using hw) h

/-- A tick touches no third domain: the queue and the waiting list of a domain
    that neither ran before the tick nor runs after it are unchanged. -/
theorem tick_frame (dom : Tid → Dom) (s : St) (n : Nat) (e : Dom)
    (h1 : e ≠ s.rd) (h2 : e ≠ (step dom s (.tick n)).rd) :
    (step dom s (.tick n)).q e = s.q e ∧ (step dom s (.tick n)).wl e = s.wl e := by
  by_cases hn : n ≤ s.part.seen
  · rw [tick_repeat_noop dom s n hn]; exact ⟨rfl, rfl⟩
  · simp only [step, tickStep, hn, ite_false] at h2 ⊢
    by_cases hnew : (s.part.advance n).domNow n = s.rd
    · simp only [hnew, ite_true]
      have a := serve_R0_other { s with part := { s.part.advance n with seen := n } } s.rd n e
        (Ne.symm h1)
      have b := sameStep_R0_other
        (serve { s with part := { s.part.advance n with seen := n } } s.rd n) e (Ne.symm h1)
      exact ⟨(a.q.trans b.q).symm, (a.wl.trans b.wl).symm⟩
    · simp only [hnew, ite_false, enter_rd, serve_rd] at h2 ⊢
      have a := cut_R0_other { s with part := { s.part.advance n with seen := n } } e (Ne.symm h1)
      have b := serve_R0_other
        ({ cut { s with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St) ((s.part.advance n).domNow n) n e
        (Ne.symm h2)
      have c := enter_R0_other
        (serve ({ cut { s with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St) ((s.part.advance n).domNow n) n)
        ((s.part.advance n).domNow n) e (Ne.symm h2)
      exact ⟨(a.q.trans (b.q.trans c.q)).symm, (a.wl.trans (b.wl.trans c.wl)).symm⟩

/-- The queue of the running domain after `cut`: the old queue and the task
    that ran, nothing else. -/
theorem cut_q_perm (s : St) :
    ((cut s).q s.rd).Perm
      (match s.cur with | some (c, _) => c :: s.q s.rd | none => s.q s.rd) := by
  rcases hc : s.cur with _ | ⟨c, ex⟩
  · simp [cut, hc]
  · cases ex with
    | true => simp [cut, hc, upd]
    | false =>
      rcases hr : s.q s.rd with _ | ⟨x, r⟩
      · simp [cut, hc, upd, hr]
      · simp only [cut, hc, upd, ite_true, hr, Bool.false_eq_true, ite_false]
        exact List.perm_append_singleton c (x :: r)

/-- A tick that changes the domain does not serve the domain that ran before:
    its waiting list is as it was, so its due waiters stay until that domain
    runs again.  Its queue gets the task that ran, and nothing else. -/
theorem tick_old_domain (dom : Tid → Dom) (s : St) (n : Nat)
    (h : (step dom s (.tick n)).rd ≠ s.rd) :
    (step dom s (.tick n)).wl s.rd = s.wl s.rd ∧
    ((step dom s (.tick n)).q s.rd).Perm
      (match s.cur with | some (c, _) => c :: s.q s.rd | none => s.q s.rd) := by
  by_cases hn : n ≤ s.part.seen
  · rw [tick_repeat_noop dom s n hn] at h; exact absurd rfl h
  · simp only [step, tickStep, hn, ite_false] at h ⊢
    by_cases hnew : (s.part.advance n).domNow n = s.rd
    · simp [hnew] at h
    · simp only [hnew, ite_false, enter_wl]
      have b := serve_R0_other
        ({ cut { s with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St) ((s.part.advance n).domNow n) n s.rd hnew
      have c := enter_R0_other
        (serve ({ cut { s with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St) ((s.part.advance n).domNow n) n)
        ((s.part.advance n).domNow n) s.rd hnew
      refine ⟨b.wl.symm.trans (by simp), ?_⟩
      rw [← c.q, ← b.q]
      exact cut_q_perm { s with part := { s.part.advance n with seen := n } }

/-- The idle loop taking an interrupt without a new tick (`poll n`, `n <= seen`)
    serves the running domain, which stays the running domain: no waiter due
    at `n` is left in its waiting list, each of its waiters that was due is in
    its queue or is the running task, and each that was not due still waits. -/
theorem poll_serves (dom : Tid → Dom) (s : St) (n : Nat) (hn : n ≤ s.part.seen) :
    (step dom s (.poll n)).rd = s.rd ∧
    (∀ w ∈ (step dom s (.poll n)).wl s.rd, ∀ k, w.2 = some k → n < k) ∧
    (∀ w ∈ s.wl s.rd, ∀ k, w.2 = some k → k ≤ n →
        w.1 ∈ (step dom s (.poll n)).q s.rd ∨
        ∃ b, (step dom s (.poll n)).cur = some (w.1, b)) ∧
    (∀ w ∈ s.wl s.rd, (∀ k, w.2 = some k → n < k) → w ∈ (step dom s (.poll n)).wl s.rd) := by
  have hn' : ¬ s.part.seen < n := by omega
  simp only [step, pollStep, hn', ite_false, pollRest_rd, serve_rd, pollRest_wl]
  have hs := serve_wakes_due s s.rd n
  refine ⟨trivial, hs.2.1, ?_, hs.2.2.1⟩
  intro w hw k hk hkn
  exact pollRest_mem (serve s s.rd n) w.1 (hs.1 w hw k hk hkn)

/-- Such a poll touches no other domain. -/
theorem poll_frame (dom : Tid → Dom) (s : St) (n : Nat) (hn : n ≤ s.part.seen) (e : Dom)
    (he : e ≠ s.rd) :
    (step dom s (.poll n)).q e = s.q e ∧ (step dom s (.poll n)).wl e = s.wl e := by
  have hn' : ¬ s.part.seen < n := by omega
  simp only [step, pollStep, hn', ite_false, pollRest_wl]
  have a := serve_R0_other s s.rd n e (Ne.symm he)
  refine ⟨?_, a.wl.symm⟩
  rw [a.q]
  unfold pollRest
  split
  · rfl
  · exact ((popBetween_R0_other (serve s s.rd n) e (Ne.symm he)).q).symm

/-- A task that blocks is appended to the waiting list of its own domain with
    the deadline it gave. -/
theorem block_keeps_deadline (dom : Tid → Dom) (s : St) (c : Tid) (b : Bool) (dl : Option Nat)
    (hc : s.cur = some (c, b)) :
    (step dom s (.block dl)).wl (dom c) = s.wl (dom c) ++ [(c, dl)] ∧
    (c, dl) ∈ (step dom s (.block dl)).wl (dom c) := by
  have h : (step dom s (.block dl)).wl (dom c) = s.wl (dom c) ++ [(c, dl)] := by
    simp [step, blockStep, hc, upd]
  exact ⟨h, by rw [h]; simp⟩

theorem due_or_not (w : Tid × Option Nat) (n : Nat) :
    (∃ k, w.2 = some k ∧ k ≤ n) ∨ (∀ k, w.2 = some k → n < k) := by
  obtain ⟨a, b⟩ := w
  cases b with
  | none => right; intro k h; simp at h
  | some k0 =>
    by_cases h : k0 ≤ n
    · left; exact ⟨k0, rfl, h⟩
    · right; intro k hk; simp at hk; omega

/-- A tick does not touch the waiting list of a domain that does not run
    after it. -/
theorem tick_wl_other (dom : Tid → Dom) (s : St) (n : Nat) (e : Dom)
    (h : e ≠ (step dom s (.tick n)).rd) : (step dom s (.tick n)).wl e = s.wl e := by
  by_cases he : e = s.rd
  · subst he; exact (tick_old_domain dom s n (Ne.symm h)).1
  · exact (tick_frame dom s n e he h).2

theorem tick_leaves (dom : Tid → Dom) (s : St) (n : Nat) (d : Dom) (w : Tid × Option Nat)
    (hn : s.part.seen < n) (hw : w ∈ s.wl d) (hl : w ∉ (step dom s (.tick n)).wl d) :
    (step dom s (.tick n)).rd = d ∧ ∃ k, w.2 = some k ∧ k ≤ n := by
  by_cases hd : d = (step dom s (.tick n)).rd
  · subst hd
    rcases due_or_not w n with h | h
    · exact ⟨rfl, h⟩
    · exact absurd ((tick_serves dom s n hn).2.2 w hw h) hl
  · rw [tick_wl_other dom s n d hd] at hl; exact absurd hw hl

theorem yieldStep_wl (s : St) : (yieldStep s).wl = s.wl := by
  unfold yieldStep; split <;> (try split) <;> rfl

theorem blockStep_wl_mono (dom : Tid → Dom) (s : St) (dl : Option Nat) (d : Dom)
    (w : Tid × Option Nat) (hw : w ∈ s.wl d) : w ∈ (blockStep dom s dl).wl d := by
  unfold blockStep
  split
  · exact hw
  · next c ex hc =>
    simp only [popBetween_wl, upd]
    split
    · next hd => subst hd; exact List.mem_append_left _ hw
    · exact hw

theorem doWake_leaves (dom : Tid → Dom) (s : St) (t : Tid) (d : Dom) (w : Tid × Option Nat)
    (hw : w ∈ s.wl d) (hl : w ∉ (doWake dom s t).wl d) : w.1 = t := by
  unfold doWake at hl
  split at hl
  · simp only [upd] at hl
    split at hl
    · next hd =>
      subst hd
      by_cases h : w.1 = t
      · exact h
      · exact absurd (List.mem_filter.mpr ⟨hw, by simp [h]⟩) hl
    · exact absurd hw hl
  · exact absurd hw hl

/-- A waiter leaves its waiting list in two ways only.  Either its domain is
    served: a tick or poll event after which its domain runs, with an index at
    or after its deadline.  Or a task wakes it.  So a timed wait fires only
    for the domain that runs and never early, no event takes a waiter out of
    the list of a domain that does not run after it (except a wake), and
    `yield`, `block` and `set_table` take no waiter out at all. -/
theorem waiter_leaves_only (dom : Tid → Dom) (s : St) (e : Ev) (d : Dom) (w : Tid × Option Nat)
    (hw : w ∈ s.wl d) (hl : w ∉ (step dom s e).wl d) :
    (∃ n, (e = .tick n ∨ e = .poll n) ∧ (step dom s e).rd = d ∧ ∃ k, w.2 = some k ∧ k ≤ n) ∨
    e = .wake w.1 ∨ e = .wakeX w.1 := by
  cases e with
  | tick n =>
    by_cases hn : n ≤ s.part.seen
    · rw [tick_repeat_noop dom s n hn] at hl; exact absurd hw hl
    · exact Or.inl ⟨n, Or.inl rfl, tick_leaves dom s n d w (by omega) hw hl⟩
  | poll n =>
    by_cases hn : s.part.seen < n
    · rw [poll_eq_tick dom s n hn] at hl ⊢
      exact Or.inl ⟨n, Or.inr rfl, tick_leaves dom s n d w hn hw hl⟩
    · have hn' : n ≤ s.part.seen := by omega
      have hp := poll_serves dom s n hn'
      by_cases hd : d = s.rd
      · subst hd
        rcases due_or_not w n with h | h
        · exact Or.inl ⟨n, Or.inr rfl, hp.1, h⟩
        · exact absurd (hp.2.2.2 w hw h) hl
      · rw [(poll_frame dom s n hn' d hd).2] at hl; exact absurd hw hl
  | yield =>
    have h : (step dom s .yield).wl = s.wl := yieldStep_wl s
    rw [h] at hl; exact absurd hw hl
  | block dl => exact absurd (blockStep_wl_mono dom s dl d w hw) hl
  | wake t =>
    simp only [step] at hl
    split at hl
    · exact Or.inr (Or.inl (by rw [doWake_leaves dom s t d w hw hl]))
    · exact absurd hw hl
  | wakeX t =>
    simp only [step] at hl
    split at hl
    · exact Or.inr (Or.inr (by rw [doWake_leaves dom s t d w hw hl]))
    · exact absurd hw hl
  | setTable t => exact absurd hw hl

end fire


/-! ## No task is lost and none is duplicated

For a domain `d` the tasks of `d` in a state are: the running task if `d` runs,
then `d`'s queue, then the tasks of `d`'s waiting list (`tasks`).  A step
permutes the tasks of every domain: it moves tasks between the three places
and neither drops nor copies one.  A wake needs one hypothesis for that: the
woken task is not twice in its waiting list (`doWake` removes every entry of
it and queues it once).  `NoDup` (no task twice among the tasks of a domain)
gives it, and every step keeps `NoDup`.  `NoDup` is not part of `Inv`.

The theorems are about membership and multiplicity.  They do not give the
order inside a queue (for that see `serve_due_order`, `wake_effect`,
`block_keeps_deadline`), and the model has no event that creates or ends a
task, so "no task is lost" is a statement about the events it has. -/

section conserve
variable {dom : Tid → Dom}

/-- The running task as a list, for the domain that runs; empty otherwise. -/
def runOf (d : Dom) (s : St) : List Tid :=
  if s.rd = d then (match s.cur with | some (c, _) => [c] | none => []) else []

/-- The tasks of domain `d`: the running task if `d` runs, then `d`'s queue,
    then the tasks of `d`'s waiting list. -/
def tasks (d : Dom) (s : St) : List Tid :=
  runOf d s ++ s.q d ++ (s.wl d).map Prod.fst

/-- No task occurs twice among the tasks of a domain.  With `Inv` (a task sits
    only in its own domain's structures) no task occurs twice in the state.
    Not part of `Inv`; a hypothesis of `step_tasks_perm`. -/
def NoDup (s : St) : Prop := ∀ d, (tasks d s).Nodup

theorem tasks_serve (s : St) (e n : Nat) (d : Dom) :
    (tasks d (serve s e n)).Perm (tasks d s) := by
  by_cases h : d = e
  · subst h
    have hp : ((((s.wl d).filter (fun w => expired w n)).reverse).map Prod.fst ++
        ((s.wl d).filter (fun w => !expired w n)).map Prod.fst).Perm ((s.wl d).map Prod.fst) := by
      have h1 := ((List.reverse_perm ((s.wl d).filter (fun w => expired w n))).map
        Prod.fst).append_right (((s.wl d).filter (fun w => !expired w n)).map Prod.fst)
      have h2 := (List.filter_append_perm (fun w => expired w n) (s.wl d)).map Prod.fst
      rw [List.map_append] at h2
      exact h1.trans h2
    simp only [tasks, runOf, serve, upd, ite_true, List.append_assoc]
    exact (hp.append_left _).append_left _
  · simp [tasks, runOf, serve, upd, h]

theorem tasks_rd (s : St) (r : Dom) (d : Dom) (hc : s.cur = none) :
    tasks d { s with rd := r } = tasks d s := by
  simp [tasks, runOf, hc]

theorem cut_cur (s : St) : (cut s).cur = none := by
  unfold cut; split
  · next h => exact h
  · rfl

theorem tasks_cut (s : St) (d : Dom) : (tasks d (cut s)).Perm (tasks d s) := by
  by_cases h : s.rd = d
  · subst h
    have hq := cut_q_perm s
    rcases hc : s.cur with _ | ⟨c, ex⟩
    · simp only [hc] at hq
      simp only [tasks, runOf, cut_rd, cut_cur, cut_wl, hc, ite_true, List.nil_append]
      exact hq.append_right _
    · simp only [hc] at hq
      simp only [tasks, runOf, cut_rd, cut_cur, cut_wl, hc, ite_true, List.nil_append]
      exact hq.append_right _
  · have hq := (cut_R0_other s d h).q
    simp [tasks, runOf, h, ← hq]

theorem tasks_enter (s : St) (d : Dom) (hc : s.cur = none) :
    tasks d (enter s s.rd) = tasks d s := by
  unfold enter
  split
  · next x r hr =>
    by_cases h : s.rd = d
    · subst h; simp [tasks, runOf, upd, hr, hc]
    · have h' : ¬ d = s.rd := fun e => h e.symm
      simp [tasks, runOf, upd, h, h']
  · simp [tasks, runOf, hc]

theorem tasks_popBetween (s : St) (d : Dom) (hc : s.cur = none) :
    tasks d (popBetween s) = tasks d s := by
  unfold popBetween
  split
  · next x r hr =>
    by_cases h : s.rd = d
    · subst h; simp [tasks, runOf, upd, hr, hc]
    · have h' : ¬ d = s.rd := fun e => h e.symm
      simp [tasks, runOf, upd, h, h']
  · simp [tasks, runOf, hc]

theorem tasks_sameStep (s : St) (d : Dom) : (tasks d (sameStep s)).Perm (tasks d s) := by
  unfold sameStep
  split
  · next c hc => simp [tasks, runOf, hc]
  · next c hc =>
    split
    · next x r hr =>
      by_cases h : s.rd = d
      · subst h
        simp only [tasks, runOf, upd, ite_true, hc, hr]
        exact (List.perm_append_singleton c (x :: r)).append_right _
      · have h' : ¬ d = s.rd := fun e => h e.symm
        simp [tasks, runOf, upd, h, h']
    · exact List.Perm.refl _
  · next hc => rw [tasks_enter s d hc]

theorem tasks_yield (s : St) (d : Dom) : (tasks d (yieldStep s)).Perm (tasks d s) := by
  unfold yieldStep
  split
  · exact List.Perm.refl _
  · next c ex hc =>
    split
    · next x r hr =>
      by_cases h : s.rd = d
      · subst h
        simp only [tasks, runOf, upd, ite_true, hc, hr]
        exact (List.perm_append_singleton c (x :: r)).append_right _
      · have h' : ¬ d = s.rd := fun e => h e.symm
        simp [tasks, runOf, upd, h, h']
    · exact List.Perm.refl _

theorem tasks_pollRest (s : St) (d : Dom) : tasks d (pollRest s) = tasks d s := by
  unfold pollRest
  split
  · rfl
  · next hc => exact tasks_popBetween s d hc

theorem tasks_tick (s : St) (n : Nat) (d : Dom) : (tasks d (tickStep s n)).Perm (tasks d s) := by
  unfold tickStep
  split
  · exact List.Perm.refl _
  · by_cases hnew : (s.part.advance n).domNow n = s.rd
    · simp only [hnew, ite_true]
      exact (tasks_sameStep _ d).trans
        (tasks_serve { s with part := { s.part.advance n with seen := n } } s.rd n d)
    · simp only [hnew, ite_false]
      have hc : (cut { s with part := { s.part.advance n with seen := n } }).cur = none :=
        cut_cur _
      have h1 := tasks_enter
        (serve ({ cut { s with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St) ((s.part.advance n).domNow n) n) d hc
      have h2 := tasks_serve
        ({ cut { s with part := { s.part.advance n with seen := n } } with
            rd := (s.part.advance n).domNow n } : St) ((s.part.advance n).domNow n) n d
      have h3 := tasks_rd (cut { s with part := { s.part.advance n with seen := n } })
        ((s.part.advance n).domNow n) d hc
      have h4 := tasks_cut { s with part := { s.part.advance n with seen := n } } d
      rw [serve_rd] at h1
      rw [h1]
      exact h2.trans (h3 ▸ h4)

theorem tasks_poll (s : St) (n : Nat) (d : Dom) : (tasks d (pollStep s n)).Perm (tasks d s) := by
  unfold pollStep
  split
  · exact tasks_tick s n d
  · rw [tasks_pollRest]; exact tasks_serve s s.rd n d

theorem tasks_block (s : St) (hi : Inv dom s) (dl : Option Nat) (d : Dom) :
    (tasks d (blockStep dom s dl)).Perm (tasks d s) := by
  unfold blockStep
  split
  · exact List.Perm.refl _
  · next c ex hc =>
    have hcd : dom c = s.rd := hi.cur_dom c ex hc
    rw [tasks_popBetween _ d rfl]
    by_cases h : s.rd = d
    · subst h
      simp only [tasks, runOf, upd, ite_true, hc, hcd, List.map_append, List.map_cons,
        List.map_nil, List.nil_append, ← List.append_assoc]
      exact List.perm_append_singleton c _
    · have h' : ¬ d = s.rd := fun e => h e.symm
      simp [tasks, runOf, upd, h, h', hcd]

/-- Removing the entries of `t` from a waiting list in which `t` waits once,
    and putting `t` in front, gives the same tasks. -/
theorem wake_perm (l : List (Tid × Option Nat)) (t : Tid) (hn : (l.map Prod.fst).Nodup)
    (hm : ∃ w ∈ l, w.1 = t) :
    (t :: (l.filter (fun w => !decide (w.1 = t))).map Prod.fst).Perm (l.map Prod.fst) := by
  induction l with
  | nil => obtain ⟨w, hw, _⟩ := hm; simp at hw
  | cons w r ih =>
    simp only [List.map_cons, List.nodup_cons] at hn
    by_cases hw : w.1 = t
    · have hall : r.filter (fun w => !decide (w.1 = t)) = r := by
        apply List.filter_eq_self.mpr
        intro a ha
        have hne : a.1 ≠ t := by
          intro h
          apply hn.1
          rw [hw, ← h]
          exact List.mem_map_of_mem ha
        simp [hne]
      simp [hw, hall]
    · have hm' : ∃ w' ∈ r, w'.1 = t := by
        obtain ⟨w', hw', ht⟩ := hm
        rcases List.mem_cons.mp hw' with rfl | h
        · exact absurd ht hw
        · exact ⟨w', h, ht⟩
      have h := ih hn.2 hm'
      simp only [List.filter_cons, hw, decide_false, Bool.not_false, ite_true, List.map_cons]
      exact (List.Perm.swap _ _ _).trans (h.cons _)

theorem tasks_doWake (s : St) (t : Tid) (hn : ((s.wl (dom t)).map Prod.fst).Nodup) (d : Dom) :
    (tasks d (doWake dom s t)).Perm (tasks d s) := by
  unfold doWake
  split
  · next hany =>
    have hm : ∃ w ∈ s.wl (dom t), w.1 = t := by
      obtain ⟨w, hw, h⟩ := List.any_eq_true.mp hany
      exact ⟨w, hw, by simpa using h⟩
    have hp := wake_perm (s.wl (dom t)) t hn hm
    by_cases h : d = dom t
    · subst h
      simp only [tasks, runOf, upd, ite_true, List.append_assoc]
      exact (hp.append_left _).append_left _
    · simp [tasks, runOf, upd, h]
  · exact List.Perm.refl _

/-- One step permutes the tasks of every domain.  `Inv` is used for `block`
    only (the task that blocks belongs to the running domain).  The hypothesis
    `hw` is used for a wake only: the woken task is at most once in its
    waiting list. -/
theorem step_tasks_perm_of (s : St) (hi : Inv dom s) (e : Ev)
    (hw : ∀ t, e = .wake t ∨ e = .wakeX t → ((s.wl (dom t)).map Prod.fst).Nodup) (d : Dom) :
    (tasks d (step dom s e)).Perm (tasks d s) := by
  cases e with
  | tick n => exact tasks_tick s n d
  | poll n => exact tasks_poll s n d
  | yield => exact tasks_yield s d
  | block dl => exact tasks_block s hi dl d
  | wake t =>
    simp only [step]
    split
    · exact tasks_doWake s t (hw t (Or.inl rfl)) d
    · exact List.Perm.refl _
  | wakeX t =>
    simp only [step]
    split
    · exact tasks_doWake s t (hw t (Or.inr rfl)) d
    · exact List.Perm.refl _
  | setTable t => exact List.Perm.refl _

/-- No task is lost and none is duplicated by a step: for every event, from a
    state that satisfies `Inv` and has no task twice, the tasks of every domain
    after the step are a permutation of the tasks before.  In particular no
    task moves to another domain.  The order inside the queue and the
    waiting list is not given by this theorem. -/
theorem step_tasks_perm (s : St) (hi : Inv dom s) (hu : NoDup s) (e : Ev) (d : Dom) :
    (tasks d (step dom s e)).Perm (tasks d s) :=
  step_tasks_perm_of s hi e (fun t _ => (List.nodup_append.mp (hu (dom t))).2.1) d

/-- Every step keeps `NoDup`. -/
theorem nodup_step (s : St) (hi : Inv dom s) (hu : NoDup s) (e : Ev) : NoDup (step dom s e) :=
  fun d => (step_tasks_perm s hi hu e d).nodup_iff.mpr (hu d)

theorem nodup_exec (s : St) (hi : Inv dom s) (hu : NoDup s) (es : List Ev) :
    NoDup (exec dom s es) := by
  induction es generalizing s with
  | nil => exact hu
  | cons e es ih => exact ih _ (inv_step s hi e) (nodup_step s hi hu e)

/-- The same over a run: after any list of events the tasks of every domain
    are a permutation of the tasks at the start. -/
theorem exec_tasks_perm (s : St) (hi : Inv dom s) (hu : NoDup s) (es : List Ev) (d : Dom) :
    (tasks d (exec dom s es)).Perm (tasks d s) := by
  induction es generalizing s with
  | nil => exact List.Perm.refl _
  | cons e es ih =>
    exact (ih _ (inv_step s hi e) (nodup_step s hi hu e)).trans (step_tasks_perm s hi hu e d)

theorem mem_tasks_doWake (s : St) (t : Tid) (d : Dom) (x : Tid) :
    x ∈ tasks d (doWake dom s t) ↔ x ∈ tasks d s := by
  unfold doWake
  split
  · next hany =>
    obtain ⟨w0, hw0, h0⟩ := List.any_eq_true.mp hany
    have ht : w0.1 = t := by simpa using h0
    by_cases h : d = dom t
    · subst h
      simp only [tasks, upd, ite_true, List.mem_append, List.mem_map, List.mem_filter,
        List.mem_singleton]
      constructor
      · rintro ((h1 | h1 | h1) | ⟨w, ⟨hw, _⟩, hx⟩)
        · exact Or.inl (Or.inl h1)
        · exact Or.inl (Or.inr h1)
        · exact Or.inr ⟨w0, hw0, by rw [ht, h1]⟩
        · exact Or.inr ⟨w, hw, hx⟩
      · rintro ((h1 | h1) | ⟨w, hw, hx⟩)
        · exact Or.inl (Or.inl h1)
        · exact Or.inl (Or.inr (Or.inl h1))
        · by_cases hwt : w.1 = t
          · exact Or.inl (Or.inr (Or.inr (by rw [← hx, hwt])))
          · exact Or.inr ⟨w, ⟨hw, by simp [hwt]⟩, hx⟩
    · simp [tasks, runOf, upd, h]
  · exact Iff.rfl

/-- Without `NoDup`: no task is lost by a step and none comes from nowhere.
    For every event and every `Inv` state a task is among the tasks of `d`
    after the step exactly if it was before.  This says nothing about how
    often it occurs; `step_tasks_perm` does, with `NoDup`. -/
theorem step_tasks_mem (s : St) (hi : Inv dom s) (e : Ev) (d : Dom) (x : Tid) :
    x ∈ tasks d (step dom s e) ↔ x ∈ tasks d s := by
  cases e with
  | wake t =>
    simp only [step]
    split
    · exact mem_tasks_doWake s t d x
    · exact Iff.rfl
  | wakeX t =>
    simp only [step]
    split
    · exact mem_tasks_doWake s t d x
    · exact Iff.rfl
  | tick n => exact (step_tasks_perm_of s hi (.tick n) (by simp) d).mem_iff
  | poll n => exact (step_tasks_perm_of s hi (.poll n) (by simp) d).mem_iff
  | yield => exact (step_tasks_perm_of s hi .yield (by simp) d).mem_iff
  | block dl => exact (step_tasks_perm_of s hi (.block dl) (by simp) d).mem_iff
  | setTable t => exact (step_tasks_perm_of s hi (.setTable t) (by simp) d).mem_iff

/-- The same over a run. -/
theorem exec_tasks_mem (s : St) (hi : Inv dom s) (es : List Ev) (d : Dom) (x : Tid) :
    x ∈ tasks d (exec dom s es) ↔ x ∈ tasks d s := by
  induction es generalizing s with
  | nil => exact Iff.rfl
  | cons e es ih => exact (ih _ (inv_step s hi e)).trans (step_tasks_mem s hi e d x)

/-- `NoDup` at boot, from the lists given to `boot`. -/
theorem nodup_boot (now first : Nat) (q0 : Dom → List Tid) (w0 : Dom → List (Tid × Option Nat))
    (h0 : (first :: (q0 0 ++ (w0 0).map Prod.fst)).Nodup)
    (h : ∀ d, d ≠ 0 → (q0 d ++ (w0 d).map Prod.fst).Nodup) :
    NoDup (boot now first q0 w0) := by
  intro d
  by_cases hd : d = 0
  · subst hd; simpa [tasks, runOf, boot] using h0
  · have h' : ¬ 0 = d := fun e => hd e.symm
    simpa [tasks, runOf, boot, h'] using h d hd

/-- What `doWake` does in the woken task's domain. -/
theorem doWake_effect (s : St) (t : Tid) :
    ((∃ dl, (t, dl) ∈ s.wl (dom t)) →
        (doWake dom s t).q (dom t) = s.q (dom t) ++ [t] ∧
        (∀ w ∈ (doWake dom s t).wl (dom t), w.1 ≠ t) ∧
        (∀ w ∈ s.wl (dom t), w.1 ≠ t → w ∈ (doWake dom s t).wl (dom t))) ∧
    ((∀ dl, (t, dl) ∉ s.wl (dom t)) → doWake dom s t = s) := by
  refine ⟨?_, ?_⟩
  · rintro ⟨dl, hdl⟩
    have hany : (s.wl (dom t)).any (fun w => decide (w.1 = t)) = true :=
      List.any_eq_true.mpr ⟨(t, dl), hdl, by simp⟩
    refine ⟨by simp [doWake, hany, upd], ?_, ?_⟩
    · intro w hw
      simp only [doWake, hany, ite_true, upd] at hw
      simpa using (List.mem_filter.mp hw).2
    · intro w hw hne
      simp only [doWake, hany, ite_true, upd]
      exact List.mem_filter.mpr ⟨hw, by simp [hne]⟩
  · intro hno
    have hany : ¬ (s.wl (dom t)).any (fun w => decide (w.1 = t)) = true := by
      intro h
      obtain ⟨⟨a, b⟩, hw, hwt⟩ := List.any_eq_true.mp h
      have ht : a = t := by simpa using hwt
      subst ht
      exact hno b hw
    simp [doWake, hany]

end conserve

section wake

/-- What a wake does.  If the running task wakes a task `t` of its own domain
    that waits, `t` goes to the back of its domain's queue and all its entries
    leave the waiting list; the other waiters stay.  If `t` does not wait,
    nothing changes (a task that is ready or runs is not queued again). -/
theorem wake_effect (dom : Tid → Dom) (s : St) (t : Tid) (hc : s.cur.isSome = true)
    (hd : dom t = s.rd) :
    ((∃ dl, (t, dl) ∈ s.wl (dom t)) →
        (step dom s (.wake t)).q (dom t) = s.q (dom t) ++ [t] ∧
        (∀ w ∈ (step dom s (.wake t)).wl (dom t), w.1 ≠ t) ∧
        (∀ w ∈ s.wl (dom t), w.1 ≠ t → w ∈ (step dom s (.wake t)).wl (dom t))) ∧
    ((∀ dl, (t, dl) ∉ s.wl (dom t)) → step dom s (.wake t) = s) := by
  have h : step dom s (.wake t) = doWake dom s t := by simp [step, hc, hd]
  rw [h]
  exact doWake_effect s t

/-- The same for a wake across domains (`wakeX`). -/
theorem wakeX_effect (dom : Tid → Dom) (s : St) (t : Tid) (hc : s.cur.isSome = true)
    (hd : dom t ≠ s.rd) :
    ((∃ dl, (t, dl) ∈ s.wl (dom t)) →
        (step dom s (.wakeX t)).q (dom t) = s.q (dom t) ++ [t] ∧
        (∀ w ∈ (step dom s (.wakeX t)).wl (dom t), w.1 ≠ t) ∧
        (∀ w ∈ s.wl (dom t), w.1 ≠ t → w ∈ (step dom s (.wakeX t)).wl (dom t))) ∧
    ((∀ dl, (t, dl) ∉ s.wl (dom t)) → step dom s (.wakeX t) = s) := by
  have h : step dom s (.wakeX t) = doWake dom s t := by simp [step, hc, hd]
  rw [h]
  exact doWake_effect s t

end wake

/-! Witnesses for the two sections above -/

/-- `sample` has no task twice, so `exec_tasks_perm` applies to its runs. -/
theorem sample_nodup : NoDup sample := by
  apply nodup_boot
  · decide
  · intro d hd
    by_cases h1 : d = 1
    · subst h1; decide
    · by_cases h2 : d = 2
      · subst h2; decide
      · simp [hd, h1, h2]

/-- A state with a task twice in a waiting list: task 1 of the root domain
    waits twice while task 0 runs. -/
def sDup : St :=
  boot 0 0 (fun _ => []) (fun d => if d = 0 then [(1, none), (1, none)] else [])

/-- `NoDup` is needed in `step_tasks_perm`: `sDup` satisfies `Inv`, and one wake
    of task 1 takes both entries out of the waiting list and queues it once. -/
theorem wake_needs_nodup :
    Inv dom0 sDup ∧ ¬ NoDup sDup ∧
    ¬ (tasks 0 (step dom0 sDup (.wake 1))).Perm (tasks 0 sDup) := by
  refine ⟨?_, ?_, ?_⟩
  · apply inv_boot
    · intro d t ht; simp at ht
    · intro d w hw
      by_cases hd : d = 0
      · subst hd; simp at hw; subst hw; decide
      · simp [hd] at hw
    · decide
  · intro h
    exact absurd (h 0) (by decide)
  · decide

/-- A state for timed waits: task 0 of the root domain runs, task 10 of domain 1
    is ready, task 11 of domain 1 and task 20 of domain 2 wait with deadline 3. -/
def sT : St :=
  boot 0 0 (fun d => if d = 1 then [10] else [])
    (fun d => if d = 1 then [(11, some 3)] else if d = 2 then [(20, some 3)] else [])

/-- The table `[(0, 1), (1, 1), (2, 1)]` set at tick 0 starts at tick 2, so
    tick 3 belongs to domain 1 and tick 4 to domain 2.  Both timed waits are
    due at tick 3.  Tick 3 wakes task 11 (domain 1 runs) and leaves task 20
    waiting; tick 4 wakes task 20, which then runs.  (The slice bit of the
    running task is left out on purpose.) -/
theorem example_timed_waits :
    let es : List Ev := [.setTable [(0, 1), (1, 1), (2, 1)], .tick 1, .tick 2, .tick 3]
    (exec dom0 sT es).rd = 1 ∧
    (exec dom0 sT es).cur.map Prod.fst = some 10 ∧ (exec dom0 sT es).q 1 = [11] ∧
    (exec dom0 sT es).wl 1 = [] ∧ (exec dom0 sT es).wl 2 = [(20, some 3)] ∧
    (exec dom0 sT (es ++ [.tick 4])).rd = 2 ∧
    (exec dom0 sT (es ++ [.tick 4])).cur.map Prod.fst = some 20 ∧
    (exec dom0 sT (es ++ [.tick 4])).wl 2 = [] := by
  decide


/-! ## Headline theorems and their axioms -/

#print axioms pinv_pexec
#print axioms inv_exec
#print axioms guard_sound
#print axioms guard_sound_event
#print axioms guard_unsound_without_hyp
#print axioms guard_decision_flips
#print axioms exP_reachable
#print axioms setTable_now_needs_pendAfter
#print axioms cycle_append
#print axioms cycle_eq_sum
#print axioms domAtPos_window
#print axioms window_cover
#print axioms domAt_window
#print axioms plan_setTable_ge
#print axioms plan_setTable_window
#print axioms plan_setTable_lt
#print axioms plan_setTable_now
#print axioms table_effective_at
#print axioms first_table_never_governs
#print axioms running_domain_function
#print axioms running_domain_same
#print axioms tick_jump_domain
#print axioms no_slack_donation
#print axioms noninterference
#print axioms noninterference_final
#print axioms cross_wake_breaks
#print axioms tick_indexes_needed
#print axioms example_views_equal
#print axioms example_applies
#print axioms example_effects
#print axioms example_kernel_shaped
#print axioms serve_only_own
#print axioms serve_not_early
#print axioms blockIdleRust_eq
#print axioms poll_eq_tick
#print axioms tick_repeat_noop
#print axioms tick_zero_noop
#print axioms inv_with_duplicate
#print axioms sample_inv
#print axioms sample_nodup
#print axioms serve_wakes_due
#print axioms serve_due_order
#print axioms tick_serves
#print axioms tick_frame
#print axioms tick_old_domain
#print axioms poll_serves
#print axioms poll_frame
#print axioms block_keeps_deadline
#print axioms waiter_leaves_only
#print axioms example_timed_waits
#print axioms step_tasks_perm_of
#print axioms step_tasks_perm
#print axioms nodup_step
#print axioms nodup_exec
#print axioms nodup_boot
#print axioms exec_tasks_perm
#print axioms step_tasks_mem
#print axioms exec_tasks_mem
#print axioms wake_needs_nodup
#print axioms wake_effect
#print axioms wakeX_effect

end K0Part
