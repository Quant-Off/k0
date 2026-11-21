import K0Part
import K0Share

/-
K0Bridge.lean: the tick rule of `K0Part.lean` is the tick rule of `K0Share.lean`.

WHAT THIS FILE IS
  `K0Part.lean` models the partition scheduler over several domains and keeps
  an exemption bit for the running task.  `K0Share.lean` models one domain and
  proves what that bit buys: a bound on the CPU share of a task that never
  gives up the CPU.  `K0Part.lean` has no theorem about its tick rule, and
  before this file no theorem linked the two.
  This file imports both, changes neither, and proves that `K0Part` does to
  its running domain, rule by rule, what `K0Share` does to its one domain.
  Everything here is about the two MODELS.  Nothing here is about the Rust
  code.

THE PROJECTION
  `Rel lab s k`: in the `K0Part` state `s` a task runs, and the `K0Share`
  state `k` has the label of that task as `cur`, its exemption bit as `ex`
  and the labels of the running domain's ready queue as `q`.  Only these
  three fields are compared.  The countdown, `age`, `time` and `ran` of `k`
  are free.
  `lab : Tid → Nat` gives a task its `K0Share` name.  `K0Share` only asks
  whether a name is 0, so "the spinner" is a task with label 0.  With
  `lab = id` it is task 0.  Any other choice works, one per domain if wanted.
  `Parked lab s d k`: domain `d` does not run, and `k` is what it will look
  like when its window opens (the head of its queue runs, without the mark).

WHAT IS PROVED
  (1) Rule by rule, for every state.  No invariant is needed.
        sameStep_onTick  a tick inside the running domain is `onTick`
        serve_wakes      the timed wakes are wakes at the back of the queue,
                         in the order `serve` appends them
        tick_same, tick_same_run   both, for `step` on a new tick
        yield_step       `yield`, if the running task is not the spinner
        block_step       `block`, same condition, queue not empty
        wake_step        `wake` of a task that waits in the running domain
        boot_start0      at boot the first task has the mark, as in `start0`
      Side conditions, all explicit: a woken task does not have label 0, and
      the cap `N` of `K0Share.step` is not reached.  `K0Part` has no cap.
      Where `K0Share` excludes a case the two sides really differ:
      yield_spinner_differs, block_spinner_differs (the spinner gives up the
      CPU), block_empty_idle (the domain idles; `K0Share` has no idle state),
      wake_cap_needed (a wake at the cap).
  (2) The window boundary.  `K0Share` has no window and no other domain, so
      there is no rule of it to compare with.  What is proved is how `K0Part`
      treats the running task at a boundary, in `K0Share` terms:
        cut_parked       parking is `onTick`, with the task `onTick` would
                         run kept at the head of the queue: a task with the
                         mark keeps the front and loses the mark, a task
                         without it goes behind the one that was at the head
        enter_parked     the task that enters has no mark
        tick_leave, tick_return   the same for `step` on a new tick
        parked_keep      an event of another window leaves a parked domain
                         alone, unless it is a wake across domains into it
        window_cycle     from the tick that closes a window to the tick that
                         opens it again, the domain moves by ONE `onTick` and
                         then by the wakes `serve` does when it opens.
                         Hypotheses: `K0Part.Inv`, no wake across domains into
                         the domain in between, and for the wakes at the
                         opening the side conditions of `serve_wakes`
  (3) One domain, run level.  `K0Part` has no time between ticks, so the
      bridge adds it: a step `pass` (time passes, no tick), a countdown to the
      next tick and two counters (`TSt`, `tstep`).  These belong to the
      bridge, not to `K0Part`; the `K0Part` state moves by `K0Part.step` only.
        run_projects, run_projects_prefix
            a timed run of one domain projects, step after step, onto the
            `K0Share` behaviour `trRun` builds from it
        timed_share_bound, boot_share_bound
            so `K0Share.share_bound` holds for it: steps passed
            `<= N *` steps in which the task with label 0 was the running
            task of the `K0Part` state `+ (N - 1) * P +` the first countdown
      Hypotheses (`Start`, `OkRun`): the table in force and any pending table
      name the running domain only; a tick falls every `P` steps; the spinner
      never yields or blocks; no `poll`; exactly one task with label 0 among
      the running and the ready tasks and none among the waiting ones; at most
      `N` tasks in the domain.  Under them the domain never idles.
  (4) example_start, example_ok, example_events: a concrete run where all
      hypotheses of (3) hold.  example_tight: a run that reaches the constant
      of the bound (`N = 3`, `P = 2`).  example_spinner_yield: without "the
      spinner never yields" the bound fails.  example_window: a concrete
      boundary with two domains where all hypotheses of `window_cycle` hold.
  (1) and (2) pin the tick rule of `K0Part.lean`.  With the mark not cleared
  by a tick, no mark after a yield or a block, a mark at a tick entry, no mark
  at boot, or a cut task parked at the other end, one of them is false.

WHAT IS NOT PROVED
  * Nothing about the Rust code.  Both models are read off it by hand.
  * No share bound for a table with several domains.  `window_cycle` says
    what one boundary does to the projection.  No theorem counts the steps of
    a domain across its windows, and nothing is said about a window that ends
    inside a period (in `K0Part` a window is a whole number of ticks).
  * The time between ticks is not `K0Part`'s.  `OkRun` asks for a tick every
    `P` steps; `K0Part` does not enforce that.  A jump of the tick index over
    several ticks counts as one tick and one step.
  * The idle paths of `K0Part` (the idle loop `poll`, a tick that finds the
    domain idle, a block with an empty queue) have no counterpart in `K0Share`
    and are not covered.
  * Only `share_bound` is carried over.  `K0Share.no_early_cut` and
    `K0Share.wait_bound` are not: `age` is not part of the projection.
  * Nothing about instants, the guard, caches or any channel outside the
    scheduler decision.
`#print axioms` lines are at the end of the file.
-/

namespace K0Bridge

open K0Part (Tid Dom)

/-! ## The projection -/

/-- The projection, as a relation.  `Rel lab s k`: in the `K0Part` state `s` a
    task runs, and the running domain of `s` looks to `K0Share` like `k`:
    the label of the running task is `k.cur`, its exemption bit is `k.ex`, and
    the labels of the running domain's ready queue are `k.q`, in order.
    Only these three fields of `k` are compared.  The countdown `c`, `age`,
    `time` and `ran` of `k` are free. -/
def Rel (lab : Tid → Nat) (s : K0Part.St) (k : K0Share.St) : Prop :=
  ∃ c, s.cur = some (c, k.ex) ∧ lab c = k.cur ∧ (s.q s.rd).map lab = k.q

/-- The tasks that `serve s d n` appends to the queue of `d`, in that order. -/
def woken (s : K0Part.St) (d n : Nat) : List Tid :=
  (((s.wl d).filter (fun w => K0Part.expired w n)).reverse).map Prod.fst

/-- One `K0Share` wake per task, in the order of the list. -/
def wakes (lab : Tid → Nat) (ts : List Tid) : List K0Share.Ev :=
  ts.map (fun t => K0Share.Ev.wake (lab t))

/-- A domain whose window is closed, seen as the `K0Share` state `k` it will
    have when the window opens: the head of its queue will be the running
    task, without the mark, and the rest of the queue is `k.q`. -/
def Parked (lab : Tid → Nat) (s : K0Part.St) (d : Dom) (k : K0Share.St) : Prop :=
  k.ex = false ∧ (s.q d).map lab = k.cur :: k.q

/-! ## Facts about `K0Share` used below -/

theorem exec_append (N P : Nat) (a b : List K0Share.Ev) : ∀ (k : K0Share.St),
    K0Share.exec N P k (a ++ b) = K0Share.exec N P (K0Share.exec N P k a) b := by
  induction a with
  | nil => intro k; rfl
  | cons e a ih => intro k; exact ih _

/-- A list of wakes below the cap, none of them of a task with label 0, is an
    append at the back of the queue and nothing else. -/
theorem exec_wakes (lab : Tid → Nat) (N P : Nat) : ∀ (ts : List Tid) (k : K0Share.St),
    (∀ t ∈ ts, lab t ≠ 0) → k.q.length + ts.length + 1 ≤ N →
    K0Share.exec N P k (wakes lab ts) = { k with q := k.q ++ ts.map lab } := by
  intro ts
  induction ts with
  | nil => intro k _ _; simp [wakes, K0Share.exec]
  | cons t ts ih =>
    intro k hne hcap
    have ht : lab t ≠ 0 := hne t (by simp)
    have hlt : K0Share.cnt k < N := by
      simp only [List.length_cons] at hcap
      simp only [K0Share.cnt]
      omega
    have hs : K0Share.step N P k (.wake (lab t)) = { k with q := k.q ++ [lab t] } := by
      simp [K0Share.step, ht, hlt]
    have h2 := ih { k with q := k.q ++ [lab t] } (fun u hu => hne u (by simp [hu]))
      (by simp only [List.length_cons, List.length_append, List.length_nil] at hcap ⊢; omega)
    show K0Share.exec N P (K0Share.step N P k (.wake (lab t))) (wakes lab ts) = _
    rw [hs, h2]
    simp

/-- `K0Share.onTick` keeps the length of the queue. -/
theorem onTick_q_length (P : Nat) (k : K0Share.St) :
    (K0Share.onTick P k).q.length = k.q.length := by
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  cases kex with
  | true => simp [K0Share.onTick]
  | false => cases kq <;> simp [K0Share.onTick]

/-- After `K0Share.onTick` the running task has no mark. -/
theorem onTick_ex (P : Nat) (k : K0Share.St) : (K0Share.onTick P k).ex = false := by
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  cases kex with
  | true => simp [K0Share.onTick]
  | false => cases kq <;> simp [K0Share.onTick]

/-! ## Rule by rule -/

theorem serve_q (s : K0Part.St) (d n : Nat) :
    (K0Part.serve s d n).q d = s.q d ++ woken s d n := by
  simp [K0Part.serve, K0Part.upd, woken]

/-- The tick rule.  `K0Part.sameStep` (what a tick does inside the running
    domain, after `serve`) is `K0Share.onTick` on the projection: with the mark
    the tick only clears it; without the mark the running task goes to the back
    and the head runs without a mark; with an empty queue nothing moves.
    No side condition. -/
theorem sameStep_onTick (lab : Tid → Nat) (P : Nat) {s : K0Part.St} {k : K0Share.St}
    (h : Rel lab s k) : Rel lab (K0Part.sameStep s) (K0Share.onTick P k) := by
  obtain ⟨c, hc, hl, hq⟩ := h
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc hl hq
  subst hl hq
  cases kex with
  | true =>
    refine ⟨c, ?_, ?_, ?_⟩ <;> simp [K0Part.sameStep, hc, K0Share.onTick]
  | false =>
    cases hq' : s.q s.rd with
    | nil =>
      refine ⟨c, ?_, ?_, ?_⟩ <;> simp [K0Part.sameStep, hc, hq', K0Share.onTick]
    | cons x r =>
      refine ⟨x, ?_, ?_, ?_⟩ <;>
        simp [K0Part.sameStep, hc, hq', K0Share.onTick, K0Part.upd]

/-- The timed wakes of `serve` are `K0Share` wakes at the back of the queue,
    in the order `serve` appends them.  Side conditions: no woken task has
    label 0, and the cap `N` of `K0Share.step` is not reached. -/
theorem serve_wakes (lab : Tid → Nat) (N P : Nat) {s : K0Part.St} {k : K0Share.St} (n : Nat)
    (h : Rel lab s k) (hne : ∀ t ∈ woken s s.rd n, lab t ≠ 0)
    (hcap : k.q.length + (woken s s.rd n).length + 1 ≤ N) :
    Rel lab (K0Part.serve s s.rd n) (K0Share.exec N P k (wakes lab (woken s s.rd n))) := by
  obtain ⟨c, hc, hl, hq⟩ := h
  rw [exec_wakes lab N P _ k hne hcap]
  refine ⟨c, hc, hl, ?_⟩
  show ((K0Part.serve s s.rd n).q s.rd).map lab = k.q ++ (woken s s.rd n).map lab
  rw [serve_q, List.map_append, hq]

/-- `s` with the clock part moved to tick `n`, the first thing `tickStep` does.
    Queues, waiting lists, the running task and `rd` are those of `s`. -/
def clocked (s : K0Part.St) (n : Nat) : K0Part.St :=
  { s with part := { s.part.advance n with seen := n } }

/-- Unfolding of `K0Part.step` on a new tick that stays in the running domain. -/
theorem step_tick_same (dom : Tid → Dom) (s : K0Part.St) (n : Nat) (hn : s.part.seen < n)
    (hd : (s.part.advance n).domNow n = s.rd) :
    K0Part.step dom s (.tick n) = K0Part.sameStep (K0Part.serve (clocked s n) s.rd n) := by
  have hn' : ¬ n ≤ s.part.seen := by omega
  simp [K0Part.step, K0Part.tickStep, hn', hd, clocked]

/-- Unfolding of `K0Part.step` on a new tick that changes the domain to `d`. -/
theorem step_tick_other (dom : Tid → Dom) (s : K0Part.St) (n : Nat) (d : Dom)
    (hn : s.part.seen < n) (hd : (s.part.advance n).domNow n = d) (hrd : s.rd ≠ d) :
    K0Part.step dom s (.tick n) =
      K0Part.enter (K0Part.serve { K0Part.cut (clocked s n) with rd := d } d n) d := by
  have hn' : ¬ n ≤ s.part.seen := by omega
  have hrd' : ¬ d = s.rd := fun e => hrd e.symm
  simp [K0Part.step, K0Part.tickStep, hn', hd, hrd', clocked]

/-- A new tick that stays in the running domain (`K0Part.step` on `tick n`
    with `n` not seen before and the same domain planned for `n`) is, on the
    projection, the wakes of `serve` followed by `K0Share.onTick`.
    A jump of the index over several ticks is still one `onTick`. -/
theorem tick_same (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (n : Nat) (h : Rel lab s k) (hn : s.part.seen < n)
    (hd : (s.part.advance n).domNow n = s.rd)
    (hne : ∀ t ∈ woken s s.rd n, lab t ≠ 0)
    (hcap : k.q.length + (woken s s.rd n).length + 1 ≤ N) :
    Rel lab (K0Part.step dom s (.tick n))
      (K0Share.onTick P (K0Share.exec N P k (wakes lab (woken s s.rd n)))) := by
  rw [step_tick_same dom s n hn hd]
  have h1 : Rel lab (clocked s n) k := h
  exact sameStep_onTick lab P (serve_wakes lab N P n h1 hne hcap)

/-- A `K0Share` `run` that ends a period (countdown at most 1) is `onTick` on
    the three fields. -/
theorem run_tick_fields (N P : Nat) (k : K0Share.St) (hc : k.c ≤ 1) :
    (K0Share.step N P k .run).cur = (K0Share.onTick P k).cur ∧
    (K0Share.step N P k .run).ex = (K0Share.onTick P k).ex ∧
    (K0Share.step N P k .run).q = (K0Share.onTick P k).q := by
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc
  cases kex with
  | true => simp [K0Share.step, K0Share.runStep, K0Share.onTick, hc]
  | false => cases kq <;> simp [K0Share.step, K0Share.runStep, K0Share.onTick, hc]

/-- So such a `run` is `K0Part.sameStep` on the projection. -/
theorem run_tick (lab : Tid → Nat) (N P : Nat) {s : K0Part.St} {k : K0Share.St}
    (h : Rel lab s k) (hc : k.c ≤ 1) :
    Rel lab (K0Part.sameStep s) (K0Share.step N P k .run) := by
  obtain ⟨f1, f2, f3⟩ := run_tick_fields N P k hc
  obtain ⟨c, hc1, hl, hq⟩ := sameStep_onTick lab P h
  exact ⟨c, by rw [f2]; exact hc1, by rw [f1]; exact hl, by rw [f3]; exact hq⟩

/-- A `K0Share` `run` inside a period (countdown above 1) leaves the three
    fields alone.  `K0Part` has no event for it. -/
theorem run_pass (lab : Tid → Nat) (N P : Nat) {s : K0Part.St} {k : K0Share.St}
    (h : Rel lab s k) (hc : 1 < k.c) : Rel lab s (K0Share.step N P k .run) := by
  obtain ⟨c, hc1, hl, hq⟩ := h
  have hc' : ¬ k.c ≤ 1 := by omega
  refine ⟨c, ?_, ?_, ?_⟩ <;> simp [K0Share.step, K0Share.runStep, hc', hc1, hl, hq]

/-- `tick_same` in `K0Share` events only: the wakes of `serve`, then one `run`
    that ends a period. -/
theorem tick_same_run (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (n : Nat) (h : Rel lab s k) (hn : s.part.seen < n)
    (hd : (s.part.advance n).domNow n = s.rd)
    (hne : ∀ t ∈ woken s s.rd n, lab t ≠ 0)
    (hcap : k.q.length + (woken s s.rd n).length + 1 ≤ N) (hc : k.c ≤ 1) :
    Rel lab (K0Part.step dom s (.tick n))
      (K0Share.exec N P k (wakes lab (woken s s.rd n) ++ [.run])) := by
  rw [step_tick_same dom s n hn hd, exec_append]
  have h1 : Rel lab (clocked s n) k := h
  have h2 := serve_wakes lab N P n h1 hne hcap
  have hc' : (K0Share.exec N P k (wakes lab (woken s s.rd n))).c ≤ 1 := by
    rw [exec_wakes lab N P _ k hne hcap]; exact hc
  exact run_tick lab N P h2 hc'

/-- `yield`.  When the running task is not the spinner, `K0Part`'s yield is
    `K0Share.step .yield` on the projection: the head runs with the mark and
    the task that yielded is at the back; with an empty queue nothing moves. -/
theorem yield_step (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (h : Rel lab s k) (hsp : k.cur ≠ 0) :
    Rel lab (K0Part.step dom s .yield) (K0Share.step N P k .yield) := by
  obtain ⟨c, hc, hl, hq⟩ := h
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc hl hq hsp
  subst hl hq
  cases hq' : s.q s.rd with
  | nil =>
    refine ⟨c, ?_, ?_, ?_⟩ <;>
      simp [K0Part.step, K0Part.yieldStep, hc, hq', K0Share.step, hsp]
  | cons x r =>
    refine ⟨x, ?_, ?_, ?_⟩ <;>
      simp [K0Part.step, K0Part.yieldStep, hc, hq', K0Share.step, hsp, K0Part.upd]

/-- The case `K0Share` excludes.  `K0Part` lets any running task yield;
    `K0Share.step .yield` does nothing when the spinner runs.  If the spinner
    yields with a non-empty queue the two sides differ, so `yield_step` needs
    its hypothesis.  (`0 ∉ k.q`: there is no second task with label 0.) -/
theorem yield_spinner_differs (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (h : Rel lab s k) (hsp : k.cur = 0) (h0 : 0 ∉ k.q) (hq : k.q ≠ []) :
    ¬ Rel lab (K0Part.step dom s .yield) (K0Share.step N P k .yield) := by
  obtain ⟨c, hc, hl, hq1⟩ := h
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc hl hq1 hsp h0 hq
  subst hq1
  cases hq' : s.q s.rd with
  | nil => simp [hq'] at hq
  | cons x r =>
    have hx : lab x ≠ 0 := fun e => h0 (by simp [hq', e])
    rintro ⟨c', hc', hl', -⟩
    simp [K0Part.step, K0Part.yieldStep, hc, hq', K0Share.step, hsp] at hc' hl'
    exact hx (by rw [hc'.1]; exact hl')

/-- `block`.  When the running task is not the spinner and the queue is not
    empty, `K0Part`'s block is `K0Share.step .block` on the projection: the
    head runs with the mark.  The waiting list is outside the projection. -/
theorem block_step (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (dl : Option Nat) (h : Rel lab s k) (hsp : k.cur ≠ 0) (hne : k.q ≠ []) :
    Rel lab (K0Part.step dom s (.block dl)) (K0Share.step N P k .block) := by
  obtain ⟨c, hc, hl, hq⟩ := h
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc hl hq hsp hne
  subst hl hq
  cases hq' : s.q s.rd with
  | nil => simp [hq'] at hne
  | cons x r =>
    refine ⟨x, ?_, ?_, ?_⟩ <;>
      simp [K0Part.step, K0Part.blockStep, K0Part.popBetween, hc, hq', K0Share.step, hsp,
        K0Part.upd]

/-- The spinner's block is excluded like its yield: `K0Share.step .block`
    does nothing when the spinner runs, `K0Part` hands the CPU to the head. -/
theorem block_spinner_differs (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (dl : Option Nat) (h : Rel lab s k) (hsp : k.cur = 0) (h0 : 0 ∉ k.q)
    (hq : k.q ≠ []) :
    ¬ Rel lab (K0Part.step dom s (.block dl)) (K0Share.step N P k .block) := by
  obtain ⟨c, hc, hl, hq1⟩ := h
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc hl hq1 hsp h0 hq
  subst hq1
  cases hq' : s.q s.rd with
  | nil => simp [hq'] at hq
  | cons x r =>
    have hx : lab x ≠ 0 := fun e => h0 (by simp [hq', e])
    rintro ⟨c', hc', hl', -⟩
    simp [K0Part.step, K0Part.blockStep, K0Part.popBetween, hc, hq', K0Share.step, hsp]
      at hc' hl'
    exact hx (by rw [hc'.1]; exact hl')

/-- The other case `K0Share` excludes.  A block with an empty queue leaves the
    domain idle in `K0Part`.  `K0Share` has no idle state, so the result has no
    projection at all. -/
theorem block_empty_idle (dom : Tid → Dom) (lab : Tid → Nat) {s : K0Part.St} {k : K0Share.St}
    (dl : Option Nat) (h : Rel lab s k) (he : k.q = []) (k' : K0Share.St) :
    ¬ Rel lab (K0Part.step dom s (.block dl)) k' := by
  obtain ⟨c, hc, -, hq⟩ := h
  have hq' : s.q s.rd = [] := by
    rw [he] at hq
    exact List.map_eq_nil_iff.mp hq
  rintro ⟨c', hc', -, -⟩
  simp [K0Part.step, K0Part.blockStep, K0Part.popBetween, hc, hq'] at hc'

/-- `wake`.  A wake of a task that waits in the running domain is a
    `K0Share` wake of its label: an append at the back of the queue.  Side
    conditions: the task does not have label 0, and the cap of `K0Share.step`
    is not reached (`k.q.length + 1 < N`). -/
theorem wake_step (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (t : Tid) (h : Rel lab s k) (hd : dom t = s.rd)
    (hw : ∃ w ∈ s.wl s.rd, w.1 = t) (hne : lab t ≠ 0) (hcap : k.q.length + 1 < N) :
    Rel lab (K0Part.step dom s (.wake t)) (K0Share.step N P k (.wake (lab t))) := by
  obtain ⟨c, hc, hl, hq⟩ := h
  have hany : (s.wl s.rd).any (fun w => decide (w.1 = t)) = true := by
    obtain ⟨w, hw1, hw2⟩ := hw
    exact List.any_eq_true.mpr ⟨w, hw1, by simp [hw2]⟩
  have hs : K0Share.step N P k (.wake (lab t)) = { k with q := k.q ++ [lab t] } := by
    simp [K0Share.step, hne, K0Share.cnt, hcap]
  rw [hs]
  refine ⟨c, ?_, hl, ?_⟩
  · simp [K0Part.step, K0Part.doWake, hc, hd, hany]
  · simp [K0Part.step, K0Part.doWake, hc, hd, hany, K0Part.upd, hq]

/-- A wake of a task that does not wait changes nothing in `K0Part`.  It
    projects onto no `K0Share` event. -/
theorem wake_not_waiting (dom : Tid → Dom) (s : K0Part.St) (t : Tid)
    (hw : ¬ ∃ w ∈ s.wl (dom t), w.1 = t) : K0Part.step dom s (.wake t) = s := by
  have hany : (s.wl (dom t)).any (fun w => decide (w.1 = t)) = false := by
    cases hb : (s.wl (dom t)).any (fun w => decide (w.1 = t)) with
    | false => rfl
    | true =>
      obtain ⟨w, hw1, hw2⟩ := List.any_eq_true.mp hb
      exact absurd ⟨w, hw1, by simpa using hw2⟩ hw
  simp [K0Part.step, K0Part.doWake, hany]

/-- The cap is a real side condition.  `K0Part` has no bound on the number of
    tasks; at the cap `K0Share.step` drops the wake and the two sides differ. -/
theorem wake_cap_needed (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (t : Tid) (h : Rel lab s k) (hd : dom t = s.rd)
    (hw : ∃ w ∈ s.wl s.rd, w.1 = t) (hcap : ¬ k.q.length + 1 < N) :
    ¬ Rel lab (K0Part.step dom s (.wake t)) (K0Share.step N P k (.wake (lab t))) := by
  obtain ⟨c, hc, -, hq⟩ := h
  have hany : (s.wl s.rd).any (fun w => decide (w.1 = t)) = true := by
    obtain ⟨w, hw1, hw2⟩ := hw
    exact List.any_eq_true.mpr ⟨w, hw1, by simp [hw2]⟩
  have hs : K0Share.step N P k (.wake (lab t)) = k := by
    simp [K0Share.step, K0Share.cnt, hcap]
  rw [hs]
  rintro ⟨c', -, -, hq'⟩
  have hl := congrArg List.length hq'
  simp [K0Part.step, K0Part.doWake, hc, hd, hany, K0Part.upd, ← hq] at hl

/-- Boot.  The state of `K0Part.boot` projects onto `K0Share.start0`: the first
    task runs with the mark.  (`start` is the offset inside a period; it only
    sets the countdown, which is not compared.) -/
theorem boot_start0 (lab : Tid → Nat) (now : Nat) (first : Tid) (q0 : Dom → List Tid)
    (w0 : Dom → List (Tid × Option Nat)) (P start : Nat) :
    Rel lab (K0Part.boot now first q0 w0)
      (K0Share.start0 (lab first) ((q0 0).map lab) P start) :=
  ⟨first, rfl, rfl, rfl⟩

/-! ## The window boundary

`K0Share` has one domain and no window.  What is shown here is how `K0Part`
treats the running task and its queue at a boundary, in `K0Share` terms. -/

/-- Parking at a boundary is `K0Share.onTick`, with the task that `onTick`
    would run kept at the head of the queue.  A task with the mark stays at
    the front and loses the mark; a task without it goes behind the task that
    was at the head (or stays, if the queue was empty). -/
theorem cut_parked (lab : Tid → Nat) (P : Nat) {s : K0Part.St} {k : K0Share.St}
    (h : Rel lab s k) :
    (K0Part.cut s).cur = none ∧ Parked lab (K0Part.cut s) s.rd (K0Share.onTick P k) := by
  obtain ⟨c, hc, hl, hq⟩ := h
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc hl hq
  subst hl hq
  cases kex with
  | true =>
    refine ⟨?_, ?_, ?_⟩ <;> simp [K0Part.cut, hc, K0Share.onTick, K0Part.upd]
  | false =>
    cases hq' : s.q s.rd with
    | nil =>
      refine ⟨?_, ?_, ?_⟩ <;> simp [K0Part.cut, hc, hq', K0Share.onTick, K0Part.upd]
    | cons x r =>
      refine ⟨?_, ?_, ?_⟩ <;> simp [K0Part.cut, hc, hq', K0Share.onTick, K0Part.upd]

/-- Entering a domain: the head of its queue runs, without the mark. -/
theorem enter_parked (lab : Tid → Nat) {s : K0Part.St} {k : K0Share.St}
    (h : Parked lab s s.rd k) : Rel lab (K0Part.enter s s.rd) k := by
  obtain ⟨hex, hq⟩ := h
  cases hq' : s.q s.rd with
  | nil => simp [hq'] at hq
  | cons x r =>
    rw [hq'] at hq
    simp only [List.map_cons, List.cons.injEq] at hq
    refine ⟨x, ?_, hq.1, ?_⟩
    · simp [K0Part.enter, hq', hex]
    · simp [K0Part.enter, hq', K0Part.upd, hq.2]

/-- A window that closes and opens again with nothing in between is one
    `K0Share.onTick` on the projection. -/
theorem cut_enter_onTick (lab : Tid → Nat) (P : Nat) {s : K0Part.St} {k : K0Share.St}
    (h : Rel lab s k) :
    Rel lab (K0Part.enter (K0Part.cut s) s.rd) (K0Share.onTick P k) := by
  have h1 := (cut_parked lab P h).2
  have h2 : Parked lab (K0Part.cut s) (K0Part.cut s).rd (K0Share.onTick P k) := by
    rw [K0Part.cut_rd]; exact h1
  have h3 := enter_parked lab h2
  rw [K0Part.cut_rd] at h3
  exact h3

theorem woken_congr {s s' : K0Part.St} {d : Dom} (n : Nat) (h : s.wl d = s'.wl d) :
    woken s d n = woken s' d n := by
  unfold woken; rw [h]

theorem enter_cons (s : K0Part.St) (d : Dom) (x : Tid) (r : List Tid) (h : s.q d = x :: r) :
    K0Part.enter s d = { s with q := K0Part.upd s.q d r, cur := some (x, false) } := by
  simp [K0Part.enter, h]

/-- A new tick that leaves the running domain `d`: afterwards `d`'s queue
    holds, parked, the `K0Share` state `onTick` gives. -/
theorem tick_leave (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) {s : K0Part.St}
    {k : K0Share.St} (n : Nat) (h : Rel lab s k) (hn : s.part.seen < n)
    (hd : (s.part.advance n).domNow n ≠ s.rd) :
    Parked lab (K0Part.step dom s (.tick n)) s.rd (K0Share.onTick P k) := by
  have hd' : s.rd ≠ (s.part.advance n).domNow n := fun e => hd e.symm
  have h1 : Rel lab (clocked s n) k := h
  obtain ⟨hex, hq⟩ := (cut_parked lab P h1).2
  refine ⟨hex, ?_⟩
  rw [← hq, step_tick_other dom s n _ hn rfl hd']
  have e : (K0Part.enter (K0Part.serve
        { K0Part.cut (clocked s n) with rd := (s.part.advance n).domNow n }
        ((s.part.advance n).domNow n) n) ((s.part.advance n).domNow n)).q s.rd =
      (K0Part.cut (clocked s n)).q s.rd := by
    unfold K0Part.enter
    split <;> simp [K0Part.serve, K0Part.upd, hd']
  rw [e]
  rfl

/-- A new tick that opens the window of a parked domain `d`: the head of `d`'s
    queue runs without the mark, and the wakes of `serve` come after it.
    Side conditions as in `serve_wakes`. -/
theorem tick_return (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s : K0Part.St}
    {k : K0Share.St} (n : Nat) (d : Dom) (h : Parked lab s d k) (hn : s.part.seen < n)
    (hd : (s.part.advance n).domNow n = d) (hrd : s.rd ≠ d)
    (hne : ∀ t ∈ woken s d n, lab t ≠ 0)
    (hcap : k.q.length + (woken s d n).length + 1 ≤ N) :
    Rel lab (K0Part.step dom s (.tick n)) (K0Share.exec N P k (wakes lab (woken s d n))) := by
  have hrd' : ¬ d = s.rd := fun e => hrd e.symm
  obtain ⟨hex, hq⟩ := h
  rw [exec_wakes lab N P _ k hne hcap, step_tick_other dom s n d hn hd hrd]
  cases hq' : s.q d with
  | nil => simp [hq'] at hq
  | cons x r =>
    rw [hq'] at hq
    simp only [List.map_cons, List.cons.injEq] at hq
    have hcq : (K0Part.cut (clocked s n)).q d = x :: r := by
      rw [← hq']
      unfold K0Part.cut
      split
      · rfl
      · simp [K0Part.upd, hrd', clocked]
    have hcw : (K0Part.cut (clocked s n)).wl d = s.wl d := by
      unfold K0Part.cut
      split <;> rfl
    have hw : woken { K0Part.cut (clocked s n) with rd := d } d n = woken s d n :=
      woken_congr n hcw
    have hsq : (K0Part.serve { K0Part.cut (clocked s n) with rd := d } d n).q d
        = x :: (r ++ woken s d n) := by
      rw [serve_q, hw]
      show (K0Part.cut (clocked s n)).q d ++ _ = _
      rw [hcq]
      rfl
    rw [enter_cons _ d x _ hsq]
    refine ⟨x, ?_, hq.1, ?_⟩
    · show some (x, false) = some (x, k.ex)
      rw [hex]
    · show (K0Part.upd _ d (r ++ woken s d n) d).map lab = k.q ++ (woken s d n).map lab
      rw [K0Part.upd_same, List.map_append, hq.2]

/-- A new tick leaves the queue of a domain alone when that domain neither
    runs before the tick nor after it. -/
theorem tick_other_q (dom : Tid → Dom) (s : K0Part.St) (d : Dom) (n : Nat)
    (hn : s.part.seen < n) (hrd : s.rd ≠ d) (hstay : (K0Part.step dom s (.tick n)).rd ≠ d) :
    (K0Part.step dom s (.tick n)).q d = s.q d := by
  by_cases hd : (s.part.advance n).domNow n = s.rd
  · rw [step_tick_same dom s n hn hd]
    have h1 := K0Part.serve_R0_other (clocked s n) s.rd n d hrd
    have h2 := K0Part.sameStep_R0_other (K0Part.serve (clocked s n) s.rd n) d hrd
    exact (h1.trans h2).q.symm
  · have hd' : s.rd ≠ (s.part.advance n).domNow n := fun e => hd e.symm
    rw [step_tick_other dom s n _ hn rfl hd'] at hstay ⊢
    have hnew : (s.part.advance n).domNow n ≠ d := by simpa using hstay
    have h1 := K0Part.cut_R0_other (clocked s n) d hrd
    have h2 := K0Part.serve_R0_other
      { K0Part.cut (clocked s n) with rd := (s.part.advance n).domNow n }
      ((s.part.advance n).domNow n) n d hnew
    have h3 := K0Part.enter_R0_other (K0Part.serve
      { K0Part.cut (clocked s n) with rd := (s.part.advance n).domNow n }
      ((s.part.advance n).domNow n) n) ((s.part.advance n).domNow n) d hnew
    exact ((h2.trans h3).q.symm).trans h1.q.symm

/-- While its window is closed, the parked picture of `d` does not change: an
    event of another domain's window that is not a wake across domains into
    `d` and does not open `d`'s window leaves it as it is. -/
theorem parked_keep (dom : Tid → Dom) (lab : Tid → Nat) {s : K0Part.St} {k : K0Share.St}
    {d : Dom} (hi : K0Part.Inv dom s) (h : Parked lab s d k) (hrd : s.rd ≠ d) (e : K0Part.Ev)
    (hnc : K0Part.NoCross dom d e) (hstay : (K0Part.step dom s e).rd ≠ d) :
    Parked lab (K0Part.step dom s e) d k := by
  have hq : (K0Part.step dom s e).q d = s.q d := by
    by_cases hc : K0Part.clockEv s e
    · cases e with
      | tick n => exact tick_other_q dom s d n hc hrd hstay
      | poll n =>
        have hn : s.part.seen < n := hc
        rw [K0Part.poll_eq_tick dom s n hn] at hstay ⊢
        exact tick_other_q dom s d n hn hrd hstay
      | setTable t => rfl
      | yield => exact absurd hc (fun hf => hf)
      | block dl => exact absurd hc (fun hf => hf)
      | wake t => exact absurd hc (fun hf => hf)
      | wakeX t => exact absurd hc (fun hf => hf)
    · exact (K0Part.local_R0_other hi hrd e hc hnc).q.symm
  exact ⟨h.1, by rw [hq]; exact h.2⟩

/-- `d` stays closed along the events, and none of them is a wake across
    domains into `d`. -/
def Away (dom : Tid → Dom) (d : Dom) : K0Part.St → List K0Part.Ev → Prop
  | _, [] => True
  | s, e :: es =>
    K0Part.NoCross dom d e ∧ (K0Part.step dom s e).rd ≠ d ∧ Away dom d (K0Part.step dom s e) es

instance awayDec (dom : Tid → Dom) (d : Dom) :
    (es : List K0Part.Ev) → (s : K0Part.St) → Decidable (Away dom d s es)
  | [], _ => isTrue trivial
  | e :: es, s =>
    have : Decidable (Away dom d (K0Part.step dom s e) es) := awayDec dom d es _
    inferInstanceAs (Decidable (K0Part.NoCross dom d e ∧ (K0Part.step dom s e).rd ≠ d ∧
      Away dom d (K0Part.step dom s e) es))

theorem parked_run (dom : Tid → Dom) (lab : Tid → Nat) {k : K0Share.St} {d : Dom} :
    ∀ (es : List K0Part.Ev) (s : K0Part.St), K0Part.Inv dom s → Parked lab s d k → s.rd ≠ d →
    Away dom d s es →
    Parked lab (K0Part.exec dom s es) d k ∧ (K0Part.exec dom s es).rd ≠ d := by
  intro es
  induction es with
  | nil => intro s _ h hrd _; exact ⟨h, hrd⟩
  | cons e es ih =>
    intro s hi h hrd ha
    exact ih _ (K0Part.inv_step s hi e) (parked_keep dom lab hi h hrd e ha.1 ha.2.1) ha.2.1 ha.2.2

/-- ONE WINDOW BOUNDARY, in full.  Domain `s.rd` runs and looks like `k`.  A
    new tick `n1` closes its window; then come any events `es` during which
    the window stays closed and nothing wakes across domains into it; then a
    new tick `n2` opens the window again.  At that point the domain looks like
    `K0Share.onTick P k` followed by the wakes `serve` does at `n2`: the
    boundary is one tick of the domain's own timeline, whatever the other
    domains did.  The mark of a task parked with it is gone, not kept for
    later.  Side conditions as in `serve_wakes`.
    Not claimed: anything about the number of steps the domain gets. -/
theorem window_cycle (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) {s s2 : K0Part.St}
    {k : K0Share.St} (hi : K0Part.Inv dom s) (h : Rel lab s k)
    (n1 : Nat) (hn1 : s.part.seen < n1) (hleave : (s.part.advance n1).domNow n1 ≠ s.rd)
    (es : List K0Part.Ev) (haway : Away dom s.rd (K0Part.step dom s (.tick n1)) es)
    (hs2 : s2 = K0Part.exec dom (K0Part.step dom s (.tick n1)) es)
    (n2 : Nat) (hn2 : s2.part.seen < n2) (hback : (s2.part.advance n2).domNow n2 = s.rd)
    (hne : ∀ t ∈ woken s2 s.rd n2, lab t ≠ 0)
    (hcap : k.q.length + (woken s2 s.rd n2).length + 1 ≤ N) :
    Rel lab (K0Part.step dom s2 (.tick n2))
      (K0Share.exec N P (K0Share.onTick P k) (wakes lab (woken s2 s.rd n2))) := by
  have h1 := tick_leave dom lab P n1 h hn1 hleave
  have hrd1 : (K0Part.step dom s (.tick n1)).rd ≠ s.rd := by
    have hd' : s.rd ≠ (s.part.advance n1).domNow n1 := fun e => hleave e.symm
    rw [step_tick_other dom s n1 _ hn1 rfl hd']
    simpa using hleave
  obtain ⟨h2, hrd2⟩ := parked_run dom lab es _ (K0Part.inv_step s hi (.tick n1)) h1 hrd1 haway
  rw [← hs2] at h2 hrd2
  exact tick_return dom lab N P n2 s.rd h2 hn2 hback hrd2 hne
    (by rw [onTick_q_length]; exact hcap)

/-! ## One domain, run level

`K0Part` has ticks as indexes and no time between them.  `K0Share` counts
steps, and a tick falls every `P` steps.  To compare runs, the bridge adds
what `K0Part` lacks: a step `pass` in which time passes and no tick falls, a
countdown to the next tick, and two counters.  None of this is in `K0Part`. -/

/-- A table that names domain `d` only, and has at least one tick. -/
def Names (d : Dom) (t : K0Part.Table) : Prop :=
  (∀ e ∈ t, e.1 = d) ∧ 0 < K0Part.cycle t

instance (d : Dom) (t : K0Part.Table) : Decidable (Names d t) :=
  inferInstanceAs (Decidable ((∀ e ∈ t, e.1 = d) ∧ 0 < K0Part.cycle t))

/-- The table in force and the pending table, if there is one, name `d` only. -/
def PartOne (d : Dom) (p : K0Part.Part) : Prop :=
  Names d p.tbl ∧ ∀ st t, p.pend = some (st, t) → Names d t

theorem names_domAtPos (d : Dom) : ∀ (t : K0Part.Table) (p : Nat),
    (∀ e ∈ t, e.1 = d) → p < K0Part.cycle t → K0Part.domAtPos t p = d := by
  intro t
  induction t with
  | nil => intro p _ hp; simp [K0Part.cycle] at hp
  | cons e r ih =>
    intro p hall hp
    obtain ⟨d', l⟩ := e
    simp only [K0Part.cycle] at hp
    simp only [K0Part.domAtPos]
    split
    · exact hall (d', l) (by simp)
    · exact ih (p - l) (fun e he => hall e (by simp [he])) (by omega)

/-- Such a table gives `d` at every tick. -/
theorem names_domAt {d : Dom} {t : K0Part.Table} (h : Names d t) (j : Nat) :
    K0Part.domAt t j = d :=
  names_domAtPos d t _ h.1 (Nat.mod_lt _ h.2)

/-- With one domain named, every new tick stays in it. -/
theorem partOne_domNow {d : Dom} {p : K0Part.Part} (h : PartOne d p) (n : Nat) :
    (p.advance n).domNow n = d := by
  unfold K0Part.Part.advance K0Part.Part.domNow
  rcases hp : p.pend with _ | ⟨st, t⟩
  · exact names_domAt h.1 _
  · by_cases hs : st ≤ n
    · simp only [hs, ite_true]
      exact names_domAt (h.2 st t hp) _
    · simp only [hs, ite_false]
      exact names_domAt h.1 _

theorem partOne_tick {d : Dom} {p : K0Part.Part} (h : PartOne d p) (n : Nat) :
    PartOne d { p.advance n with seen := n } := by
  unfold K0Part.Part.advance
  rcases hp : p.pend with _ | ⟨st, t⟩
  · refine ⟨h.1, ?_⟩
    intro st t ht
    simp [hp] at ht
  · by_cases hs : st ≤ n
    · simp only [hs, ite_true]
      refine ⟨h.2 st t hp, ?_⟩
      intro st' t' ht
      simp at ht
    · simp only [hs, ite_false]
      refine ⟨h.1, ?_⟩
      intro st' t' ht
      exact h.2 st' t' ht

theorem partOne_setTable {d : Dom} {p : K0Part.Part} (h : PartOne d p) {t : K0Part.Table}
    (ht : Names d t) : PartOne d (p.setTable t) := by
  refine ⟨h.1, ?_⟩
  intro st t' he
  simp only [K0Part.Part.setTable, Option.some.injEq, Prod.mk.injEq] at he
  rw [← he.2]; exact ht

/-- One step of a timed run: time passes and no tick falls, or an event of
    `K0Part`.  A `tick` event is the step of time at whose end the tick falls;
    every other event takes no time. -/
inductive TEv where
  | pass
  | ev (e : K0Part.Ev)
deriving DecidableEq

/-- A `K0Part` state with the clock of the bridge: `c` steps are left until
    the next tick (the tick falls at the end of the step that finds `c = 1`),
    `time` steps have passed, and in `ran` of them the running task of the
    `K0Part` state had label 0. -/
structure TSt where
  s : K0Part.St
  c : Nat
  time : Nat
  ran : Nat

/-- 1 if the running task of `s` has label 0 (the spinner runs), else 0. -/
def spin (lab : Tid → Nat) (s : K0Part.St) : Nat :=
  match s.cur with
  | some (t, _) => if lab t = 0 then 1 else 0
  | none => 0

/-- One step of a timed run with period `P`.  The `K0Part` state moves by
    `K0Part.step` and by nothing else.  Every `tick` event is charged one
    step, also one with an old index; `OkRun` rules those out. -/
def tstep (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) (x : TSt) : TEv → TSt
  | .pass => { x with c := x.c - 1, time := x.time + 1, ran := x.ran + spin lab x.s }
  | .ev (.tick n) =>
    { s := K0Part.step dom x.s (.tick n), c := P, time := x.time + 1,
      ran := x.ran + spin lab x.s }
  | .ev e => { x with s := K0Part.step dom x.s e }

def texec (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) (x : TSt) : List TEv → TSt
  | [] => x
  | e :: es => texec dom lab P (tstep dom lab P x e) es

/-- What a step may be in state `x`, for the run to be one that `K0Share`
    describes.
  * `pass` only inside a period, a `tick` only at its end, with an index not
    seen before.  Together: a tick every `P` steps.
  * The spinner never yields and never blocks.
  * No `poll` (the kernel does not call it while a task runs).
  * `set_table` only with a table that names the running domain only. -/
def OkEv (lab : Tid → Nat) (x : TSt) : TEv → Prop
  | .pass => 1 < x.c
  | .ev (.tick n) => x.c = 1 ∧ x.s.part.seen < n
  | .ev (.poll _) => False
  | .ev .yield => spin lab x.s = 0
  | .ev (.block _) => spin lab x.s = 0
  | .ev (.wake _) => True
  | .ev (.wakeX _) => True
  | .ev (.setTable t) => Names x.s.rd t

/-- Every step of the run is allowed in the state it is taken in. -/
def OkRun (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) : TSt → List TEv → Prop
  | _, [] => True
  | x, e :: es => OkEv lab x e ∧ OkRun dom lab P (tstep dom lab P x e) es

instance okEvDec (lab : Tid → Nat) (x : TSt) : (e : TEv) → Decidable (OkEv lab x e)
  | .pass => inferInstanceAs (Decidable (1 < x.c))
  | .ev (.tick n) => inferInstanceAs (Decidable (x.c = 1 ∧ x.s.part.seen < n))
  | .ev (.poll _) => inferInstanceAs (Decidable False)
  | .ev .yield => inferInstanceAs (Decidable (spin lab x.s = 0))
  | .ev (.block _) => inferInstanceAs (Decidable (spin lab x.s = 0))
  | .ev (.wake _) => inferInstanceAs (Decidable True)
  | .ev (.wakeX _) => inferInstanceAs (Decidable True)
  | .ev (.setTable t) => inferInstanceAs (Decidable (Names x.s.rd t))

instance okRunDec (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) :
    (es : List TEv) → (x : TSt) → Decidable (OkRun dom lab P x es)
  | [], _ => isTrue trivial
  | e :: es, x =>
    match okEvDec lab x e, okRunDec dom lab P es (tstep dom lab P x e) with
    | isTrue h1, isTrue h2 => isTrue ⟨h1, h2⟩
    | isFalse h1, _ => isFalse (fun h => h1 h.1)
    | _, isFalse h2 => isFalse (fun h => h2 h.2)

/-- The `K0Share` events one step of a timed run projects onto.  They depend
    on the `K0Part` state: a new tick is the wakes of `serve` and then `run`;
    a wake of a task that does not wait in the running domain is nothing. -/
def tr (dom : Tid → Dom) (lab : Tid → Nat) (s : K0Part.St) : TEv → List K0Share.Ev
  | .pass => [.run]
  | .ev (.tick n) => wakes lab (woken s s.rd n) ++ [.run]
  | .ev .yield => [.yield]
  | .ev (.block _) => [.block]
  | .ev (.wake t) =>
    if dom t = s.rd ∧ (s.wl s.rd).any (fun w => decide (w.1 = t)) = true then [.wake (lab t)]
    else []
  | .ev _ => []

/-- The `K0Share` behaviour a timed run projects onto, step after step. -/
def trRun (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) : TSt → List TEv → List K0Share.Ev
  | _, [] => []
  | x, e :: es => tr dom lab x.s e ++ trRun dom lab P (tstep dom lab P x e) es

/-! ### Small facts used by the simulation -/

theorem filter_split_length {α : Type} (p : α → Bool) : ∀ (l : List α),
    (l.filter p).length + (l.filter (fun a => !p a)).length = l.length := by
  intro l
  induction l with
  | nil => rfl
  | cons a l ih =>
    cases h : p a with
    | true =>
      have h1 : (a :: l).filter p = a :: l.filter p := List.filter_cons_of_pos h
      have h2 : (a :: l).filter (fun a => !p a) = l.filter (fun a => !p a) :=
        List.filter_cons_of_neg (by simp [h])
      rw [h1, h2]
      simp only [List.length_cons]
      omega
    | false =>
      have h1 : (a :: l).filter p = l.filter p := List.filter_cons_of_neg (by simp [h])
      have h2 : (a :: l).filter (fun a => !p a) = a :: l.filter (fun a => !p a) :=
        List.filter_cons_of_pos (by simp [h])
      rw [h1, h2]
      simp only [List.length_cons]
      omega

theorem filter_length_lt {α : Type} (p : α → Bool) : ∀ (l : List α),
    (∃ a ∈ l, p a = false) → (l.filter p).length < l.length := by
  intro l
  induction l with
  | nil => rintro ⟨a, ha, -⟩; simp at ha
  | cons b l ih =>
    rintro ⟨a, ha, hpa⟩
    have hle : (l.filter p).length ≤ l.length := List.length_filter_le p l
    cases hb : p b with
    | false =>
      have h1 : (b :: l).filter p = l.filter p := List.filter_cons_of_neg (by simp [hb])
      rw [h1]
      simp only [List.length_cons]
      omega
    | true =>
      have hal : a ∈ l := by
        rcases List.mem_cons.mp ha with rfl | h
        · rw [hb] at hpa; cases hpa
        · exact h
      have := ih ⟨a, hal, hpa⟩
      have h1 : (b :: l).filter p = b :: l.filter p := List.filter_cons_of_pos hb
      rw [h1]
      simp only [List.length_cons]
      omega

/-- `serve` moves each waiter to the queue or keeps it: nothing is lost or made. -/
theorem woken_length (s : K0Part.St) (d n : Nat) :
    (woken s d n).length + ((s.wl d).filter (fun w => !K0Part.expired w n)).length
      = (s.wl d).length := by
  simp only [woken, List.length_map, List.length_reverse]
  exact filter_split_length _ _

theorem mem_woken {s : K0Part.St} {d n : Nat} {t : Tid} (h : t ∈ woken s d n) :
    ∃ w ∈ s.wl d, w.1 = t := by
  simp only [woken, List.mem_map, List.mem_reverse, List.mem_filter] at h
  obtain ⟨w, ⟨hw, -⟩, rfl⟩ := h
  exact ⟨w, hw, rfl⟩

theorem sameStep_wl (s : K0Part.St) : (K0Part.sameStep s).wl = s.wl := by
  unfold K0Part.sameStep
  split
  · rfl
  · split <;> rfl
  · unfold K0Part.enter; split <;> rfl

theorem tick_same_rd (dom : Tid → Dom) (s : K0Part.St) (n : Nat) (hn : s.part.seen < n)
    (hd : (s.part.advance n).domNow n = s.rd) : (K0Part.step dom s (.tick n)).rd = s.rd := by
  rw [step_tick_same dom s n hn hd]
  simp [clocked]

theorem tick_same_wl (dom : Tid → Dom) (s : K0Part.St) (n : Nat) (hn : s.part.seen < n)
    (hd : (s.part.advance n).domNow n = s.rd) :
    (K0Part.step dom s (.tick n)).wl s.rd
      = (s.wl s.rd).filter (fun w => !K0Part.expired w n) := by
  rw [step_tick_same dom s n hn hd, sameStep_wl]
  simp [K0Part.serve, K0Part.upd, clocked]

theorem yield_wl (dom : Tid → Dom) (s : K0Part.St) : (K0Part.step dom s .yield).wl = s.wl := by
  show (K0Part.yieldStep s).wl = s.wl
  unfold K0Part.yieldStep
  split
  · rfl
  · split <;> rfl

theorem popBetween_wl (s : K0Part.St) : (K0Part.popBetween s).wl = s.wl := by
  unfold K0Part.popBetween; split <;> rfl

theorem block_wl (dom : Tid → Dom) {s : K0Part.St} {c : Tid} {ex : Bool} (dl : Option Nat)
    (hc : s.cur = some (c, ex)) (hd : dom c = s.rd) :
    (K0Part.step dom s (.block dl)).wl s.rd = s.wl s.rd ++ [(c, dl)] := by
  show (K0Part.blockStep dom s dl).wl s.rd = _
  simp only [K0Part.blockStep, hc]
  rw [popBetween_wl]
  simp [K0Part.upd, hd]

theorem wake_eq (dom : Tid → Dom) {s : K0Part.St} (t : Tid) (hc : s.cur.isSome = true)
    (hd : dom t = s.rd) (hany : (s.wl s.rd).any (fun w => decide (w.1 = t)) = true) :
    K0Part.step dom s (.wake t) =
      { s with wl := K0Part.upd s.wl s.rd ((s.wl s.rd).filter (fun w => !decide (w.1 = t))),
               q := K0Part.upd s.q s.rd (s.q s.rd ++ [t]) } := by
  simp [K0Part.step, K0Part.doWake, hc, hd, hany]

theorem wake_noop (dom : Tid → Dom) (s : K0Part.St) (t : Tid)
    (h : ¬ (dom t = s.rd ∧ (s.wl s.rd).any (fun w => decide (w.1 = t)) = true)) :
    K0Part.step dom s (.wake t) = s := by
  by_cases hd : dom t = s.rd
  · have hany : (s.wl s.rd).any (fun w => decide (w.1 = t)) = false := by
      cases hb : (s.wl s.rd).any (fun w => decide (w.1 = t)) with
      | false => rfl
      | true => exact absurd ⟨hd, hb⟩ h
    simp [K0Part.step, K0Part.doWake, hd, hany]
  · simp [K0Part.step, hd]

/-- A wake across domains leaves the running task and the running domain's
    queue and waiting list alone. -/
theorem wakeX_frame (dom : Tid → Dom) (s : K0Part.St) (t : Tid) :
    (K0Part.step dom s (.wakeX t)).cur = s.cur ∧
    K0Part.R0 s.rd s (K0Part.step dom s (.wakeX t)) := by
  simp only [K0Part.step]
  split
  · next hg =>
    refine ⟨?_, K0Part.doWake_R0_other s t s.rd hg.2⟩
    unfold K0Part.doWake; split <;> rfl
  · exact ⟨rfl, K0Part.R0.refl _ _⟩

theorem run_pass_frame (N P : Nat) (k : K0Share.St) (hc : 1 < k.c) :
    (K0Share.step N P k .run).c = k.c - 1 ∧ (K0Share.step N P k .run).time = k.time + 1 ∧
    (K0Share.step N P k .run).ran = k.ran + (if k.cur = 0 then 1 else 0) ∧
    (K0Share.step N P k .run).q = k.q := by
  have hc' : ¬ k.c ≤ 1 := by omega
  simp [K0Share.step, K0Share.runStep, hc']

theorem run_tick_frame (N P : Nat) (k : K0Share.St) (hc : k.c ≤ 1) :
    (K0Share.step N P k .run).c = P ∧ (K0Share.step N P k .run).time = k.time + 1 ∧
    (K0Share.step N P k .run).ran = k.ran + (if k.cur = 0 then 1 else 0) ∧
    (K0Share.step N P k .run).q.length = k.q.length := by
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at hc
  cases kex with
  | true => simp [K0Share.step, K0Share.runStep, K0Share.onTick, hc]
  | false => cases kq <;> simp [K0Share.step, K0Share.runStep, K0Share.onTick, hc]

theorem yield_frame (N P : Nat) (k : K0Share.St) :
    (K0Share.step N P k .yield).c = k.c ∧ (K0Share.step N P k .yield).time = k.time ∧
    (K0Share.step N P k .yield).ran = k.ran ∧
    (K0Share.step N P k .yield).q.length = k.q.length := by
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  by_cases h0 : kc = 0
  · simp [K0Share.step, h0]
  · cases kq <;> simp [K0Share.step, h0]

theorem block_frame (N P : Nat) (k : K0Share.St) (h0 : k.cur ≠ 0) (hq : k.q ≠ []) :
    (K0Share.step N P k .block).c = k.c ∧ (K0Share.step N P k .block).time = k.time ∧
    (K0Share.step N P k .block).ran = k.ran ∧
    (K0Share.step N P k .block).q.length + 1 = k.q.length := by
  obtain ⟨kc, kex, kq, kcd, kage, ktime, kran⟩ := k
  simp only at h0 hq
  cases kq with
  | nil => exact absurd rfl hq
  | cons a r => simp [K0Share.step, h0]

/-- In a well-formed `K0Share` state where the spinner does not run, the
    spinner is ready, so the queue is not empty.  This is why the domain never
    idles in the runs below. -/
theorem wf_queue_ne {N P : Nat} {k : K0Share.St} (hw : K0Share.Wf N P k) (hc : k.cur ≠ 0) :
    k.q ≠ [] := by
  intro he
  have h3 := hw.2.2.1
  rw [he] at h3
  simp [hc] at h3

theorem spin_eq {lab : Tid → Nat} {s : K0Part.St} {k : K0Share.St} (h : Rel lab s k) :
    spin lab s = if k.cur = 0 then 1 else 0 := by
  obtain ⟨c, hc, hl, -⟩ := h
  simp [spin, hc, hl]

/-! ### The simulation -/

/-- The invariant of the simulation.  The timed state `x` and the `K0Share`
    state `k` agree on the projection, on the countdown and on both counters;
    `k` is well formed; the table names the running domain only; no task with
    label 0 waits; and the tasks of the running domain, ready, running or
    waiting, are at most `N`. -/
structure Sim (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) (x : TSt) (k : K0Share.St) :
    Prop where
  rel : Rel lab x.s k
  c : k.c = x.c
  time : k.time = x.time
  ran : k.ran = x.ran
  wf : K0Share.Wf N P k
  inv : K0Part.Inv dom x.s
  one : PartOne x.s.rd x.s.part
  waiters : ∀ w ∈ x.s.wl x.s.rd, lab w.1 ≠ 0
  cap : k.q.length + (x.s.wl x.s.rd).length + 1 ≤ N

section sim
variable {dom : Tid → Dom} {lab : Tid → Nat} {N P : Nat} {x : TSt} {k : K0Share.St}

theorem sim_pass (h : Sim dom lab N P x k) (hc : 1 < x.c) :
    Sim dom lab N P (tstep dom lab P x .pass) (K0Share.exec N P k [.run]) := by
  have hk : 1 < k.c := by rw [h.c]; exact hc
  obtain ⟨f1, f2, f3, f4⟩ := run_pass_frame N P k hk
  refine ⟨run_pass lab N P h.rel hk, ?_, ?_, ?_, (K0Share.step_pot N P k h.wf .run).1,
    h.inv, h.one, h.waiters, ?_⟩
  · show (K0Share.step N P k .run).c = x.c - 1
    rw [f1, h.c]
  · show (K0Share.step N P k .run).time = x.time + 1
    rw [f2, h.time]
  · show (K0Share.step N P k .run).ran = x.ran + spin lab x.s
    rw [f3, h.ran, spin_eq h.rel]
  · show (K0Share.step N P k .run).q.length + (x.s.wl x.s.rd).length + 1 ≤ N
    rw [f4]; exact h.cap

theorem sim_tick (h : Sim dom lab N P x k) (n : Nat) (hc : x.c = 1)
    (hn : x.s.part.seen < n) :
    Sim dom lab N P (tstep dom lab P x (.ev (.tick n)))
      (K0Share.exec N P k (wakes lab (woken x.s x.s.rd n) ++ [.run])) := by
  have hd := partOne_domNow h.one n
  have hne : ∀ t ∈ woken x.s x.s.rd n, lab t ≠ 0 := by
    intro t ht
    obtain ⟨w, hw, rfl⟩ := mem_woken ht
    exact h.waiters w hw
  have hlen := woken_length x.s x.s.rd n
  have hcap : k.q.length + (woken x.s x.s.rd n).length + 1 ≤ N := by
    have := h.cap; omega
  have hk : k.c ≤ 1 := by rw [h.c, hc]; exact Nat.le_refl 1
  have hw := exec_wakes lab N P _ k hne hcap
  have hk1 : (K0Share.exec N P k (wakes lab (woken x.s x.s.rd n))).c ≤ 1 := by
    rw [hw]; exact hk
  obtain ⟨f1, f2, f3, f4⟩ := run_tick_frame N P _ hk1
  have hrd := tick_same_rd dom x.s n hn hd
  have e : K0Share.exec N P k (wakes lab (woken x.s x.s.rd n) ++ [.run]) =
      K0Share.step N P (K0Share.exec N P k (wakes lab (woken x.s x.s.rd n))) .run :=
    exec_append N P _ _ k
  refine ⟨tick_same_run dom lab N P n h.rel hn hd hne hcap hk, ?_, ?_, ?_,
    (K0Share.exec_pot N P _ k h.wf).1, K0Part.inv_step x.s h.inv (.tick n), ?_, ?_, ?_⟩
  · show (K0Share.exec N P k (wakes lab (woken x.s x.s.rd n) ++ [.run])).c = P
    rw [e, f1]
  · show (K0Share.exec N P k (wakes lab (woken x.s x.s.rd n) ++ [.run])).time = x.time + 1
    rw [e, f2, hw]
    show k.time + 1 = x.time + 1
    rw [h.time]
  · show (K0Share.exec N P k (wakes lab (woken x.s x.s.rd n) ++ [.run])).ran
      = x.ran + spin lab x.s
    rw [e, f3, hw, spin_eq h.rel]
    show k.ran + _ = x.ran + _
    rw [h.ran]
  · show PartOne (K0Part.step dom x.s (.tick n)).rd (K0Part.step dom x.s (.tick n)).part
    have hn' : ¬ n ≤ x.s.part.seen := by omega
    rw [hrd, K0Part.step_part]
    simp only [K0Part.pstep, K0Part.pTick, hn', ite_false]
    exact partOne_tick h.one n
  · intro w hw'
    have hw2 : w ∈ (K0Part.step dom x.s (.tick n)).wl x.s.rd := by
      rw [← hrd]; exact hw'
    rw [tick_same_wl dom x.s n hn hd] at hw2
    exact h.waiters w (List.mem_filter.mp hw2).1
  · show (K0Share.exec N P k (wakes lab (woken x.s x.s.rd n) ++ [.run])).q.length
      + ((K0Part.step dom x.s (.tick n)).wl (K0Part.step dom x.s (.tick n)).rd).length + 1 ≤ N
    rw [e, f4, hw, hrd, tick_same_wl dom x.s n hn hd]
    show (k.q ++ (woken x.s x.s.rd n).map lab).length + _ + 1 ≤ N
    simp only [List.length_append, List.length_map]
    have := h.cap
    omega

theorem sim_yield (h : Sim dom lab N P x k) (hs : spin lab x.s = 0) :
    Sim dom lab N P (tstep dom lab P x (.ev .yield)) (K0Share.exec N P k [.yield]) := by
  have hsp : k.cur ≠ 0 := by
    intro e; rw [spin_eq h.rel, e] at hs; simp at hs
  obtain ⟨f1, f2, f3, f4⟩ := yield_frame N P k
  have hrd : (K0Part.step dom x.s .yield).rd = x.s.rd := K0Part.step_rd_pub_free dom x.s .yield rfl
  have hpt : (K0Part.step dom x.s .yield).part = x.s.part := K0Part.step_part dom x.s .yield
  have hwl := yield_wl dom x.s
  refine ⟨yield_step dom lab N P h.rel hsp, ?_, ?_, ?_, (K0Share.step_pot N P k h.wf .yield).1,
    K0Part.inv_step x.s h.inv .yield, ?_, ?_, ?_⟩
  · show (K0Share.step N P k .yield).c = x.c
    rw [f1, h.c]
  · show (K0Share.step N P k .yield).time = x.time
    rw [f2, h.time]
  · show (K0Share.step N P k .yield).ran = x.ran
    rw [f3, h.ran]
  · show PartOne (K0Part.step dom x.s .yield).rd (K0Part.step dom x.s .yield).part
    rw [hrd, hpt]; exact h.one
  · show ∀ w ∈ (K0Part.step dom x.s .yield).wl (K0Part.step dom x.s .yield).rd, lab w.1 ≠ 0
    rw [hrd, hwl]; exact h.waiters
  · show (K0Share.step N P k .yield).q.length
      + ((K0Part.step dom x.s .yield).wl (K0Part.step dom x.s .yield).rd).length + 1 ≤ N
    rw [f4, hrd, hwl]; exact h.cap

theorem sim_block (h : Sim dom lab N P x k) (dl : Option Nat) (hs : spin lab x.s = 0) :
    Sim dom lab N P (tstep dom lab P x (.ev (.block dl))) (K0Share.exec N P k [.block]) := by
  have hsp : k.cur ≠ 0 := by
    intro e; rw [spin_eq h.rel, e] at hs; simp at hs
  have hq := wf_queue_ne h.wf hsp
  obtain ⟨f1, f2, f3, f4⟩ := block_frame N P k hsp hq
  obtain ⟨c0, hcur, hlab, -⟩ := h.rel
  have hrd : (K0Part.step dom x.s (.block dl)).rd = x.s.rd :=
    K0Part.step_rd_pub_free dom x.s (.block dl) rfl
  have hpt : (K0Part.step dom x.s (.block dl)).part = x.s.part :=
    K0Part.step_part dom x.s (.block dl)
  have hwl := block_wl dom dl hcur (h.inv.cur_dom c0 k.ex hcur)
  refine ⟨block_step dom lab N P dl h.rel hsp hq, ?_, ?_, ?_,
    (K0Share.step_pot N P k h.wf .block).1, K0Part.inv_step x.s h.inv (.block dl), ?_, ?_, ?_⟩
  · show (K0Share.step N P k .block).c = x.c
    rw [f1, h.c]
  · show (K0Share.step N P k .block).time = x.time
    rw [f2, h.time]
  · show (K0Share.step N P k .block).ran = x.ran
    rw [f3, h.ran]
  · show PartOne (K0Part.step dom x.s (.block dl)).rd (K0Part.step dom x.s (.block dl)).part
    rw [hrd, hpt]; exact h.one
  · show ∀ w ∈ (K0Part.step dom x.s (.block dl)).wl (K0Part.step dom x.s (.block dl)).rd,
      lab w.1 ≠ 0
    rw [hrd, hwl]
    intro w hw
    rcases List.mem_append.mp hw with h1 | h1
    · exact h.waiters w h1
    · simp only [List.mem_singleton] at h1
      rw [h1]
      show lab c0 ≠ 0
      rw [hlab]; exact hsp
  · show (K0Share.step N P k .block).q.length
      + ((K0Part.step dom x.s (.block dl)).wl (K0Part.step dom x.s (.block dl)).rd).length
      + 1 ≤ N
    rw [hrd, hwl]
    simp only [List.length_append, List.length_singleton]
    have := h.cap
    omega

theorem sim_wake (h : Sim dom lab N P x k) (t : Tid) :
    Sim dom lab N P (tstep dom lab P x (.ev (.wake t)))
      (K0Share.exec N P k (tr dom lab x.s (.ev (.wake t)))) := by
  obtain ⟨c0, hcur, hlab, hq0⟩ := h.rel
  by_cases hg : dom t = x.s.rd ∧ (x.s.wl x.s.rd).any (fun w => decide (w.1 = t)) = true
  · obtain ⟨hd, hany⟩ := hg
    have htr : tr dom lab x.s (.ev (.wake t)) = [.wake (lab t)] := by
      simp [tr, hd, hany]
    obtain ⟨w, hw1, hw2⟩ := List.any_eq_true.mp hany
    have hw2' : w.1 = t := by simpa using hw2
    have hne : lab t ≠ 0 := by rw [← hw2']; exact h.waiters w hw1
    have hpos : 0 < (x.s.wl x.s.rd).length := List.length_pos_of_mem hw1
    have hcap : k.q.length + 1 < N := by have := h.cap; omega
    have hs : K0Share.step N P k (.wake (lab t)) = { k with q := k.q ++ [lab t] } := by
      simp [K0Share.step, hne, K0Share.cnt, hcap]
    have hsome : x.s.cur.isSome = true := by rw [hcur]; rfl
    have he := wake_eq dom t hsome hd hany
    have hlt : ((x.s.wl x.s.rd).filter (fun w => !decide (w.1 = t))).length
        < (x.s.wl x.s.rd).length :=
      filter_length_lt _ _ ⟨w, hw1, by simp [hw2']⟩
    have hrd : (K0Part.step dom x.s (.wake t)).rd = x.s.rd :=
      K0Part.step_rd_pub_free dom x.s (.wake t) rfl
    have hpt : (K0Part.step dom x.s (.wake t)).part = x.s.part :=
      K0Part.step_part dom x.s (.wake t)
    have hwl : (K0Part.step dom x.s (.wake t)).wl x.s.rd
        = (x.s.wl x.s.rd).filter (fun w => !decide (w.1 = t)) := by
      rw [he]; exact K0Part.upd_same _ _ _
    rw [htr]
    refine ⟨wake_step dom lab N P t h.rel hd ⟨w, hw1, hw2'⟩ hne hcap, ?_, ?_, ?_,
      (K0Share.step_pot N P k h.wf (.wake (lab t))).1,
      K0Part.inv_step x.s h.inv (.wake t), ?_, ?_, ?_⟩
    · show (K0Share.step N P k (.wake (lab t))).c = x.c
      rw [hs]; exact h.c
    · show (K0Share.step N P k (.wake (lab t))).time = x.time
      rw [hs]; exact h.time
    · show (K0Share.step N P k (.wake (lab t))).ran = x.ran
      rw [hs]; exact h.ran
    · show PartOne (K0Part.step dom x.s (.wake t)).rd (K0Part.step dom x.s (.wake t)).part
      rw [hrd, hpt]; exact h.one
    · show ∀ w ∈ (K0Part.step dom x.s (.wake t)).wl (K0Part.step dom x.s (.wake t)).rd,
        lab w.1 ≠ 0
      rw [hrd, hwl]
      intro w' hw'
      exact h.waiters w' (List.mem_filter.mp hw').1
    · show (K0Share.step N P k (.wake (lab t))).q.length
        + ((K0Part.step dom x.s (.wake t)).wl (K0Part.step dom x.s (.wake t)).rd).length
        + 1 ≤ N
      rw [hs, hrd, hwl]
      show (k.q ++ [lab t]).length + _ + 1 ≤ N
      simp only [List.length_append, List.length_singleton]
      have := h.cap
      omega
  · have htr : tr dom lab x.s (.ev (.wake t)) = [] := by
      simp only [tr, hg, ite_false]
    have he := wake_noop dom x.s t hg
    rw [htr]
    show Sim dom lab N P { x with s := K0Part.step dom x.s (.wake t) } k
    rw [he]
    exact h

theorem sim_wakeX (h : Sim dom lab N P x k) (t : Tid) :
    Sim dom lab N P (tstep dom lab P x (.ev (.wakeX t))) (K0Share.exec N P k []) := by
  obtain ⟨hcur, hr⟩ := wakeX_frame dom x.s t
  obtain ⟨c0, hc0, hlab, hq0⟩ := h.rel
  refine ⟨⟨c0, ?_, hlab, ?_⟩, h.c, h.time, h.ran, h.wf, K0Part.inv_step x.s h.inv (.wakeX t),
    ?_, ?_, ?_⟩
  · show (K0Part.step dom x.s (.wakeX t)).cur = some (c0, k.ex)
    rw [hcur]; exact hc0
  · show ((K0Part.step dom x.s (.wakeX t)).q (K0Part.step dom x.s (.wakeX t)).rd).map lab = k.q
    rw [← hr.rd, ← hr.q]; exact hq0
  · show PartOne (K0Part.step dom x.s (.wakeX t)).rd (K0Part.step dom x.s (.wakeX t)).part
    rw [← hr.rd, ← hr.part]; exact h.one
  · show ∀ w ∈ (K0Part.step dom x.s (.wakeX t)).wl (K0Part.step dom x.s (.wakeX t)).rd,
      lab w.1 ≠ 0
    rw [← hr.rd, ← hr.wl]; exact h.waiters
  · show k.q.length
      + ((K0Part.step dom x.s (.wakeX t)).wl (K0Part.step dom x.s (.wakeX t)).rd).length
      + 1 ≤ N
    rw [← hr.rd, ← hr.wl]; exact h.cap

theorem sim_setTable (h : Sim dom lab N P x k) (t : K0Part.Table) (ht : Names x.s.rd t) :
    Sim dom lab N P (tstep dom lab P x (.ev (.setTable t))) (K0Share.exec N P k []) :=
  ⟨h.rel, h.c, h.time, h.ran, h.wf, K0Part.inv_step x.s h.inv (.setTable t),
    partOne_setTable h.one ht, h.waiters, h.cap⟩

/-- One allowed step of a timed run is matched by the `K0Share` events `tr`
    gives for it, and the invariant holds again. -/
theorem sim_step (h : Sim dom lab N P x k) (e : TEv) (hok : OkEv lab x e) :
    Sim dom lab N P (tstep dom lab P x e) (K0Share.exec N P k (tr dom lab x.s e)) := by
  cases e with
  | pass => exact sim_pass h hok
  | ev e =>
    cases e with
    | tick n => exact sim_tick h n hok.1 hok.2
    | poll n => exact absurd hok (fun hf => hf)
    | yield => exact sim_yield h hok
    | block dl => exact sim_block h dl hok
    | wake t => exact sim_wake h t
    | wakeX t => exact sim_wakeX h t
    | setTable t => exact sim_setTable h t hok

end sim

/-- The invariant along a whole allowed run. -/
theorem sim_run (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) :
    ∀ (es : List TEv) (x : TSt) (k : K0Share.St), Sim dom lab N P x k →
    OkRun dom lab P x es →
    Sim dom lab N P (texec dom lab P x es) (K0Share.exec N P k (trRun dom lab P x es)) := by
  intro es
  induction es with
  | nil => intro x k h _; exact h
  | cons e es ih =>
    intro x k h hok
    show Sim dom lab N P (texec dom lab P (tstep dom lab P x e) es)
      (K0Share.exec N P k (tr dom lab x.s e ++ trRun dom lab P (tstep dom lab P x e) es))
    rw [exec_append]
    exact ih _ _ (sim_step h e hok.1) hok.2

/-! ### Statements for one domain -/

/-- The `K0Share` state a timed state starts from: the projection of `x.s`,
    with the countdown and the counters of `x`.  `age` is not compared; `P`
    makes the state well formed whatever the mark is. -/
def share0 (lab : Tid → Nat) (P : Nat) (x : TSt) : K0Share.St :=
  match x.s.cur with
  | some (c, ex) =>
    { cur := lab c, ex := ex, q := (x.s.q x.s.rd).map lab, c := x.c, age := P,
      time := x.time, ran := x.ran }
  | none => { cur := 0, ex := false, q := [], c := x.c, age := P, time := x.time, ran := x.ran }

/-- The start conditions, in `K0Part` terms.
  * `inv`: the invariant of `K0Part`.
  * `one`: the table in force and a pending table name the running domain only.
  * `c_pos`, `c_le`: the countdown to the first tick is in `1..P`.
  * `spinner`: a task runs, and exactly one task among the running one and the
    ready ones of the running domain has label 0.
  * `waiters`: no task with label 0 waits in the running domain.
  * `cap`: the running domain has at most `N` tasks, ready, running or waiting. -/
structure Start (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) (x : TSt) : Prop where
  inv : K0Part.Inv dom x.s
  one : PartOne x.s.rd x.s.part
  c_pos : 1 ≤ x.c
  c_le : x.c ≤ P
  spinner : ∃ c ex, x.s.cur = some (c, ex) ∧ ((c :: x.s.q x.s.rd).map lab).count 0 = 1
  waiters : ∀ w ∈ x.s.wl x.s.rd, lab w.1 ≠ 0
  cap : (x.s.q x.s.rd).length + (x.s.wl x.s.rd).length + 1 ≤ N

theorem start_sim {dom : Tid → Dom} {lab : Tid → Nat} {N P : Nat} {x : TSt}
    (h : Start dom lab N P x) : Sim dom lab N P x (share0 lab P x) := by
  obtain ⟨c, ex, hcur, hcount⟩ := h.spinner
  have e : share0 lab P x =
      { cur := lab c, ex := ex, q := (x.s.q x.s.rd).map lab, c := x.c, age := P,
        time := x.time, ran := x.ran } := by
    simp [share0, hcur]
  rw [e]
  refine ⟨⟨c, hcur, rfl, rfl⟩, rfl, rfl, rfl, ?_, h.inv, h.one, h.waiters, ?_⟩
  · refine ⟨h.c_pos, h.c_le, hcount, ?_, ?_⟩
    · have := h.cap
      simp only [List.length_cons, List.length_map]
      omega
    · intro _; exact Nat.le_add_right _ _
  · simp only [List.length_map]; exact h.cap

/-- Every prefix of an allowed run is an allowed run. -/
theorem okRun_take (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) :
    ∀ (es : List TEv) (x : TSt) (i : Nat), OkRun dom lab P x es →
    OkRun dom lab P x (es.take i) := by
  intro es
  induction es with
  | nil => intro x i _; simp [OkRun]
  | cons e es ih =>
    intro x i hok
    cases i with
    | zero => exact trivial
    | succ i => exact ⟨hok.1, ih _ i hok.2⟩

/-- The projection of a prefix is a prefix of the projection. -/
theorem trRun_take (dom : Tid → Dom) (lab : Tid → Nat) (P : Nat) :
    ∀ (es : List TEv) (x : TSt) (i : Nat),
    trRun dom lab P x es = trRun dom lab P x (es.take i)
      ++ trRun dom lab P (texec dom lab P x (es.take i)) (es.drop i) := by
  intro es
  induction es with
  | nil => intro x i; simp [trRun, texec]
  | cons e es ih =>
    intro x i
    cases i with
    | zero => rfl
    | succ i =>
      show tr dom lab x.s e ++ trRun dom lab P (tstep dom lab P x e) es =
        (tr dom lab x.s e ++ trRun dom lab P (tstep dom lab P x e) (es.take i))
          ++ trRun dom lab P (texec dom lab P (tstep dom lab P x e) (es.take i)) (es.drop i)
      rw [List.append_assoc, ← ih]

/-- RUN LEVEL.  A timed run of `K0Part` that starts as `Start` says and is
    allowed (`OkRun`: a tick every `P` steps, the spinner never yields or
    blocks, no `poll`, tables of one domain) projects onto the `K0Share`
    behaviour `trRun` builds from it: after the run the projection, the
    countdown, the steps passed and the steps of the spinner are those of the
    `K0Share` state. -/
theorem run_projects (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) (x : TSt)
    (hs : Start dom lab N P x) (es : List TEv) (hok : OkRun dom lab P x es) :
    Rel lab (texec dom lab P x es).s
      (K0Share.exec N P (share0 lab P x) (trRun dom lab P x es)) ∧
    (K0Share.exec N P (share0 lab P x) (trRun dom lab P x es)).c = (texec dom lab P x es).c ∧
    (K0Share.exec N P (share0 lab P x) (trRun dom lab P x es)).time
      = (texec dom lab P x es).time ∧
    (K0Share.exec N P (share0 lab P x) (trRun dom lab P x es)).ran
      = (texec dom lab P x es).ran := by
  have h := sim_run dom lab N P es x _ (start_sim hs) hok
  exact ⟨h.rel, h.c, h.time, h.ran⟩

/-- The same after every step: for each `i`, the first `i` steps of the run
    project onto a prefix of the one `K0Share` behaviour `trRun x es`. -/
theorem run_projects_prefix (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) (x : TSt)
    (hs : Start dom lab N P x) (es : List TEv) (hok : OkRun dom lab P x es) (i : Nat) :
    (∃ rest, trRun dom lab P x es = trRun dom lab P x (es.take i) ++ rest) ∧
    Rel lab (texec dom lab P x (es.take i)).s
      (K0Share.exec N P (share0 lab P x) (trRun dom lab P x (es.take i))) ∧
    (K0Share.exec N P (share0 lab P x) (trRun dom lab P x (es.take i))).time
      = (texec dom lab P x (es.take i)).time ∧
    (K0Share.exec N P (share0 lab P x) (trRun dom lab P x (es.take i))).ran
      = (texec dom lab P x (es.take i)).ran := by
  have h := run_projects dom lab N P x hs (es.take i) (okRun_take dom lab P es x i hok)
  exact ⟨⟨_, trRun_take dom lab P es x i⟩, h.1, h.2.2.1, h.2.2.2⟩

/-- THE SHARE BOUND FOR ONE-DOMAIN TIMED RUNS OF `K0Part`.  From a start as in
    `Start` with both counters at 0, after any allowed run: the steps that
    passed are at most `N` times the steps in which the task with label 0 was
    the running task of the `K0Part` state, plus `(N - 1) * P` plus the
    countdown to the first tick.  This is `K0Share.share_bound` carried over
    by `run_projects`.  The steps, the countdown and the counters are those of
    the bridge (`TSt`, `tstep`), not of `K0Part`. -/
theorem timed_share_bound (dom : Tid → Dom) (lab : Tid → Nat) (N P : Nat) (x : TSt)
    (hs : Start dom lab N P x) (ht : x.time = 0) (hr : x.ran = 0) (es : List TEv)
    (hok : OkRun dom lab P x es) :
    (texec dom lab P x es).time ≤ N * (texec dom lab P x es).ran + (N - 1) * P + x.c := by
  have h0 := start_sim hs
  have h := sim_run dom lab N P es x _ h0 hok
  have hb := K0Share.share_bound N P (share0 lab P x) h0.wf (by rw [h0.time, ht])
    (by rw [h0.ran, hr]) (trRun dom lab P x es)
  rw [h.time, h.ran, h0.c] at hb
  exact hb

/-- The timed state at boot: `K0Part.boot`, with `start` steps of the first
    period already gone and both counters at 0. -/
def tboot (now : Nat) (first : Tid) (q0 : Dom → List Tid) (w0 : Dom → List (Tid × Option Nat))
    (P start : Nat) : TSt :=
  { s := K0Part.boot now first q0 w0, c := P - start, time := 0, ran := 0 }

/-- `Start` holds at boot (default table, domain 0) when the given queues and
    waiting lists respect the domains, the first task is in domain 0, exactly
    one of the first task and the ready tasks of domain 0 has label 0, no
    waiting task of domain 0 has label 0, and domain 0 has at most `N` tasks. -/
theorem start_boot (dom : Tid → Dom) (lab : Tid → Nat) (N P start now : Nat) (first : Tid)
    (q0 : Dom → List Tid) (w0 : Dom → List (Tid × Option Nat))
    (hq : ∀ d t, t ∈ q0 d → dom t = d) (hw : ∀ d w, w ∈ w0 d → dom w.1 = d)
    (hf : dom first = 0) (hs : start < P)
    (h1 : ((first :: q0 0).map lab).count 0 = 1) (hw0 : ∀ w ∈ w0 0, lab w.1 ≠ 0)
    (hN : (q0 0).length + (w0 0).length + 1 ≤ N) :
    Start dom lab N P (tboot now first q0 w0 P start) := by
  have hone : PartOne 0 (K0Part.boot now first q0 w0).part := by
    refine ⟨⟨?_, ?_⟩, ?_⟩
    · intro e he
      have he' : e = (0, 1) := by simpa [K0Part.boot, K0Part.defaultTable] using he
      rw [he']
    · show 0 < K0Part.cycle K0Part.defaultTable
      decide
    · intro st t ht
      simp [K0Part.boot] at ht
  refine ⟨K0Part.inv_boot dom now first q0 w0 hq hw hf, hone, ?_, ?_,
    ⟨first, true, rfl, h1⟩, hw0, hN⟩
  · show 1 ≤ P - start
    omega
  · show P - start ≤ P
    omega

/-- The share bound from boot, with the default table: as long as every
    `set_table` call names domain 0 only. -/
theorem boot_share_bound (dom : Tid → Dom) (lab : Tid → Nat) (N P start now : Nat)
    (first : Tid) (q0 : Dom → List Tid) (w0 : Dom → List (Tid × Option Nat))
    (hq : ∀ d t, t ∈ q0 d → dom t = d) (hw : ∀ d w, w ∈ w0 d → dom w.1 = d)
    (hf : dom first = 0) (hs : start < P)
    (h1 : ((first :: q0 0).map lab).count 0 = 1) (hw0 : ∀ w ∈ w0 0, lab w.1 ≠ 0)
    (hN : (q0 0).length + (w0 0).length + 1 ≤ N)
    (es : List TEv) (hok : OkRun dom lab P (tboot now first q0 w0 P start) es) :
    (texec dom lab P (tboot now first q0 w0 P start) es).time ≤
      N * (texec dom lab P (tboot now first q0 w0 P start) es).ran + (N - 1) * P
        + (P - start) :=
  timed_share_bound dom lab N P _
    (start_boot dom lab N P start now first q0 w0 hq hw hf hs h1 hw0 hN) rfl rfl es hok

/-! ## A concrete run: the hypotheses hold together

Tasks 0, 1 and 2 are in domain 0 (`K0Part.dom0`).  Labels are the ids, so
task 0 is the spinner.  Period 2, at most 3 tasks, default table. -/

deriving instance DecidableEq for K0Share.Ev

def exBoot : TSt := tboot 0 0 (fun d => if d = 0 then [1, 2] else []) (fun _ => []) 2 0

/-- Ticks 1 to 7 with a jump over tick 5, a yield, a timed block that expires
    at tick 6, an untimed block and its wake, a wake of a task that does not
    wait, a wake across domains, and a `set_table` that names domain 0. -/
def exRun : List TEv :=
  [.pass, .ev (.tick 1), .pass, .ev (.tick 2), .ev .yield, .pass, .ev (.tick 3),
   .ev (.block (some 6)), .ev (.setTable [(0, 3)]), .pass, .ev (.tick 4), .pass,
   .ev (.tick 6), .ev (.block none), .ev .yield, .ev (.wake 1), .ev (.wake 1),
   .ev (.wakeX 10), .pass, .ev (.tick 7)]

theorem example_start : Start K0Part.dom0 id 3 2 exBoot := by
  apply start_boot
  · intro d t ht
    by_cases h0 : d = 0
    · subst h0
      simp at ht
      rcases ht with rfl | rfl <;> decide
    · simp [h0] at ht
  · intro d w hw; simp at hw
  · decide
  · decide
  · decide
  · intro w hw; simp at hw
  · decide

theorem example_ok : OkRun K0Part.dom0 id 2 exBoot exRun := by decide

/-- The `K0Share` behaviour the run projects onto, and the counters. -/
theorem example_events :
    trRun K0Part.dom0 id 2 exBoot exRun =
      [.run, .run, .run, .run, .yield, .run, .run, .block, .run, .run, .run, .wake 2, .run,
       .block, .yield, .wake 1, .run, .run] ∧
    (texec K0Part.dom0 id 2 exBoot exRun).time = 12 ∧
    (texec K0Part.dom0 id 2 exBoot exRun).ran = 10 := by decide

/-- The constant of `timed_share_bound` is reached by a timed run of `K0Part`
    (`N = 3`, `P = 2`, first countdown 2).  Task 1 runs first with the mark,
    task 2 is ready, the spinner is last: after 6 steps the spinner has not
    run, and `3 * 0 + (3 - 1) * 2 + 2 = 6`. -/
def exTight : TSt := tboot 0 1 (fun d => if d = 0 then [2, 0] else []) (fun _ => []) 2 0

def exTightRun : List TEv :=
  [.pass, .ev (.tick 1), .pass, .ev (.tick 2), .pass, .ev (.tick 3)]

theorem example_tight :
    OkRun K0Part.dom0 id 2 exTight exTightRun ∧
    (texec K0Part.dom0 id 2 exTight exTightRun).time = 6 ∧
    (texec K0Part.dom0 id 2 exTight exTightRun).ran = 0 ∧
    3 * (texec K0Part.dom0 id 2 exTight exTightRun).ran + (3 - 1) * 2 + exTight.c = 6 := by
  decide

theorem example_tight_start : Start K0Part.dom0 id 3 2 exTight := by
  apply start_boot
  · intro d t ht
    by_cases h0 : d = 0
    · subst h0
      simp at ht
      rcases ht with rfl | rfl <;> decide
    · simp [h0] at ht
  · intro d w hw; simp at hw
  · decide
  · decide
  · decide
  · intro w hw; simp at hw
  · decide

/-- "The spinner never yields" is needed.  Two tasks, period 1.  Task 0 yields
    each time it gets the CPU: the start is as `Start` says, the run is not
    allowed, 6 steps pass, task 0 runs in none of them, and the bound of
    `timed_share_bound` would be 2. -/
def exBad : TSt := tboot 0 0 (fun d => if d = 0 then [1] else []) (fun _ => []) 1 0

def exBadRun : List TEv :=
  [.ev .yield, .ev (.tick 1), .ev (.tick 2), .ev .yield, .ev (.tick 3), .ev (.tick 4),
   .ev .yield, .ev (.tick 5), .ev (.tick 6)]

theorem example_spinner_yield :
    ¬ OkRun K0Part.dom0 id 1 exBad exBadRun ∧
    (texec K0Part.dom0 id 1 exBad exBadRun).time = 6 ∧
    (texec K0Part.dom0 id 1 exBad exBadRun).ran = 0 ∧
    ¬ ((texec K0Part.dom0 id 1 exBad exBadRun).time ≤
        2 * (texec K0Part.dom0 id 1 exBad exBadRun).ran + (2 - 1) * 1 + exBad.c) := by decide

theorem example_bad_start : Start K0Part.dom0 id 2 1 exBad := by
  apply start_boot
  · intro d t ht
    by_cases h0 : d = 0
    · subst h0
      simp at ht
      subst ht
      decide
    · simp [h0] at ht
  · intro d w hw; simp at hw
  · decide
  · decide
  · decide
  · intro w hw; simp at hw
  · decide

/-! ## A concrete window boundary: the hypotheses of `window_cycle` hold together

Domains 0 and 1 with one tick each (table set at boot, in force from tick 2).
Domain 0 has tasks 0, 1, 2; domain 1 has tasks 10, 11. -/

/-- After tick 2 and a block of task 1 until tick 4: task 2 runs in domain 0
    with the mark, task 0 is ready, task 1 waits. -/
def exW : K0Part.St :=
  K0Part.exec K0Part.dom0
    (K0Part.boot 0 0 (fun d => if d = 0 then [1, 2] else if d = 1 then [10, 11] else [])
      (fun _ => []))
    [.setTable [(0, 1), (1, 1)], .tick 1, .tick 2, .block (some 4)]

def exWk : K0Share.St :=
  { cur := 2, ex := true, q := [0], c := 1, age := 0, time := 0, ran := 0 }

/-- What domain 1 does in its window. -/
def exWes : List K0Part.Ev := [.yield, .block (some 9)]

/-- Tick 3 closed the window of domain 0 and domain 1 has done `exWes`. -/
def exW2 : K0Part.St :=
  K0Part.exec K0Part.dom0 (K0Part.step K0Part.dom0 exW (.tick 3)) exWes

theorem exW_inv : K0Part.Inv K0Part.dom0 exW := by
  apply K0Part.inv_exec
  apply K0Part.inv_boot
  · intro d t ht
    by_cases h0 : d = 0
    · subst h0
      simp at ht
      rcases ht with rfl | rfl <;> decide
    · by_cases h1 : d = 1
      · subst h1
        simp at ht
        rcases ht with rfl | rfl <;> decide
      · simp [h0, h1] at ht
  · intro d w hw; simp at hw
  · decide

/-- `window_cycle` applies: tick 4 opens the window of domain 0 again.  Task 2,
    parked with the mark, runs without it; task 0 is behind it; task 1, whose
    wait ended at tick 4, is at the back. -/
theorem example_window :
    Rel id (K0Part.step K0Part.dom0 exW2 (.tick 4))
      (K0Share.exec 3 2 (K0Share.onTick 2 exWk) [.wake 1]) ∧
    (K0Part.step K0Part.dom0 exW2 (.tick 4)).cur = some (2, false) ∧
    (K0Part.step K0Part.dom0 exW2 (.tick 4)).q 0 = [0, 1] := by
  refine ⟨?_, by decide, by decide⟩
  have hw : woken exW2 exW.rd 4 = [1] := by decide
  have h := window_cycle K0Part.dom0 id 3 2 (s := exW) (s2 := exW2) (k := exWk) exW_inv
    ⟨2, by decide, by decide, by decide⟩ 3 (by decide) (by decide) exWes (by decide) rfl
    4 (by decide) (by decide) (by decide) (by decide)
  rw [hw] at h
  exact h

/-! ## Headline theorems and their axioms -/

#print axioms sameStep_onTick
#print axioms serve_wakes
#print axioms tick_same
#print axioms tick_same_run
#print axioms yield_step
#print axioms yield_spinner_differs
#print axioms block_step
#print axioms block_spinner_differs
#print axioms block_empty_idle
#print axioms wake_step
#print axioms wake_not_waiting
#print axioms wake_cap_needed
#print axioms boot_start0
#print axioms cut_parked
#print axioms enter_parked
#print axioms cut_enter_onTick
#print axioms tick_leave
#print axioms tick_return
#print axioms parked_keep
#print axioms window_cycle
#print axioms sim_step
#print axioms run_projects
#print axioms run_projects_prefix
#print axioms timed_share_bound
#print axioms start_boot
#print axioms boot_share_bound
#print axioms example_start
#print axioms example_ok
#print axioms example_events
#print axioms example_tight
#print axioms example_tight_start
#print axioms example_spinner_yield
#print axioms example_window
#print axioms example_bad_start

end K0Bridge
