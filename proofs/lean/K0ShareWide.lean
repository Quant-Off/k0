import K0Share

/-!
# K0ShareWide: the share bound in a wider model of one domain

This file is a proof about a MODEL of one domain's own timeline. It is not a
proof about the Rust code (`k0-slice`, `k0-part`, `k0-sched`). Nothing here
checks that the Rust code implements the model; the host tests and review do
that job.

`K0Share.lean` leaves out three behaviours of the kernel. This file defines one
model that has all three, and proves the results of `K0Share.lean` again for it.
The state `St`, the tick rule `onTick`, the potential `psi` and `lag` are those
of `K0Share.lean`.

## What is wider than in `K0Share.lean`

* The tick is its own event. In `K0Share.lean` the event `run` lets one step
  pass and delivers the tick when the countdown ends, so no event can fall
  between the last step of a period and its tick. In the kernel that order
  exists: the handlers run with interrupts masked, so a timer interrupt that
  becomes pending during a yield or a block is taken when the next task is
  entered, before that task has run. This is a reading of the kernel code, not
  a Lean fact. Here `step` lets one step pass and needs a countdown above 0,
  `tick` delivers the tick and needs a countdown of 0, and any events may come
  between the two. No step passes while a tick is due.
* `remove i` takes the ready task in queue slot `i` out of the queue, from any
  position (`k0_ipc::teardown` calls `k0_sched::remove_ready`). The spinner is
  never removed. The running task does not leave this way; that is `block`.
* `wake` has no cap, and `N` does not occur in the transition function. The
  headline takes the cap as a hypothesis on the run: `Capped N P s0 es` says
  that no state along the run has more than `N` tasks ready or running.

As in `K0Share.lean`, any finite list of events is a behaviour, and an event
that cannot happen changes nothing (a `step` while a tick is due, a `tick`
that is not due, a yield or a block of the spinner, `wake 0`, a `remove` of an
empty slot or of the spinner).

## What is still NOT modelled

* A spinner that arrives later. Task `0` is in the system (running or ready)
  from the start state on (`Inv`), and `wake 0` changes nothing. For a spinner
  that becomes ready later, use `share_bound_diff` from the state in which it
  is in the queue.
* Idle. The spinner is always ready or running, so the domain is never empty,
  and a task that gets the CPU out of idle is outside `no_early_cut`.
* A proof of the cap. That a domain never has more than `N` tasks ready or
  running is a hypothesis (`Capped`), and the bound is relative to that `N`.
* Time below one step. Steps are whole, every period has its tick, and the
  tick is not late: the model lets no step pass while a tick is due. The switch
  path of the real kernel takes time and the counter is read late.
* Other domains and the partition table (`K0Part.lean`). This is the timeline
  of one domain, windows are not cut.
* Cache and predictor channels. The bound is on CPU shares only. A task still
  chooses when the others run by the instant at which it yields or blocks, so
  the timing channel through the scheduler inside the domain stays.

## Results (all about the model)

* `share_bound` (headline), `share_bound_diff`, `share_bound_always`: from a
  state with a countdown `c0 <= P` and exactly one spinner (`Inv`), for every
  behaviour `es` that is `Capped N`,
  `time <= N * ran + (N - 1) * P + c0`, at the end of the run and at every
  instant of it. For `P = 0` the countdown is 0, no step ever passes, and the
  statement says nothing. `share_bound_reached` is one instance in which the
  hypotheses hold and the constant is reached; there is no theorem of
  tightness for all `N` and `P`.
* `wexec_widen`, `capped_widen`, `contains`: every behaviour of `K0Share.lean`
  from a state with a countdown above 0 is a behaviour of this model with the
  same final state, and it keeps the cap when the start state does.
  `share_bound_base` and `share_bound_diff_base` get the headline of
  `K0Share.lean` back from the headline here, without two hypotheses that the
  former does not need (`ran = 0` at the start, the `age` clause of `Wf`).
* `no_early_cut`: when a tick takes the CPU from the running task, that task
  has run at least `P` steps since it got the CPU. `tick_keeps`: no other tick
  takes it.
* `wait_bound`, `wait_bound_passed`, `wait_bound_tick`: a ready task with `k`
  tasks to run before it becomes the running task at an instant no later than
  `(k + 1) * P` steps of time after the start, unless it is removed. The form
  of `K0Share.lean` ("after `(k + 1) * P` steps it has started") is false here,
  because at exactly that many steps the last tick may still be due
  (`wait_at_bound`). The section says what is proved instead.
  `fate_instance` shows the three outcomes on one state.
* Witnesses: `tick_after_yield` with `tick_after_yield_not_base` and
  `base_mark_needs_step` (an order that `K0Share.lean` cannot express),
  `remove_not_block_wake` (the removal is not a mix of the other events),
  `cap_needed` (the hypothesis `Capped` is needed).
* `wstep_time`, `wstep_ran`, `wstep_age`, `wexec_ran_le_time`: what the three
  counters count. `follow_waits`, `follow_started`, `follow_removed`,
  `fate_started`, `fate_removed`: what the words of the wait bound mean.

`#print axioms` lines are at the end of the file.
-/

namespace K0Share.Wide

/-! ## The model -/

/-- Events of the wide model. `step` lets one step of time pass. `tick` is the
timer tick. `remove i` takes the ready task in queue slot `i` away. -/
inductive Event where
  | step
  | tick
  | yield
  | block
  | wake (t : Nat)
  | remove (i : Nat)
  deriving DecidableEq, Repr

/-- One event. `step` needs a countdown above 0 and `tick` a countdown of 0;
otherwise they change nothing. `yield` and `block` are those of `K0Share.step`.
`wake` has no cap. `remove` leaves the spinner and an empty slot alone. -/
def wstep (P : Nat) (s : St) : Event → St
  | .step =>
    if 1 ≤ s.c then
      { s with time := s.time + 1, age := s.age + 1,
               ran := s.ran + (if s.cur = 0 then 1 else 0), c := s.c - 1 }
    else s
  | .tick => if s.c = 0 then onTick P s else s
  | .yield =>
    if s.cur = 0 then s
    else match s.q with
      | [] => s
      | x :: r => { s with cur := x, ex := true, q := r ++ [s.cur], age := 0 }
  | .block =>
    if s.cur = 0 then s
    else match s.q with
      | [] => s
      | x :: r => { s with cur := x, ex := true, q := r, age := 0 }
  | .wake t => if t = 0 then s else { s with q := s.q ++ [t] }
  | .remove i => if s.q[i]? = some 0 then s else { s with q := s.q.eraseIdx i }

/-- Run a behaviour. -/
def wexec (P : Nat) (s : St) : List Event → St
  | [] => s
  | e :: es => wexec P (wstep P s e) es

/-- The invariant of the share bound: the countdown is in `0..P` (0 is the
instant between the last step of a period and its tick) and exactly one task in
the system is the spinner. It does not mention `N` or `age`. -/
def Inv (P : Nat) (s : St) : Prop :=
  s.c ≤ P ∧ (s.cur :: s.q).count 0 = 1

/-- The invariant of `no_early_cut`: the task without the mark has already used
up `P - c` steps of its slice. -/
def AgeOk (P : Nat) (s : St) : Prop := s.ex = false → P ≤ s.age + s.c

/-- The cap as a hypothesis on a run: no state along the run, the first and the
last included, has more than `N` tasks ready or running. `es.take k` is the run
up to its `k`-th event. -/
def Capped (N P : Nat) (s : St) (es : List Event) : Prop :=
  ∀ k, cnt (wexec P s (es.take k)) ≤ N

/-! ## Runs and the cap -/

theorem wexec_append (P : Nat) : ∀ (a b : List Event) (s : St),
    wexec P s (a ++ b) = wexec P (wexec P s a) b := by
  intro a
  induction a with
  | nil => intro b s; rfl
  | cons e a ih => intro b s; exact ih b (wstep P s e)

theorem capped_head {N P : Nat} {s : St} {es : List Event} (h : Capped N P s es) :
    cnt s ≤ N := by
  simpa [wexec] using h 0

theorem capped_last {N P : Nat} {s : St} {es : List Event} (h : Capped N P s es) :
    cnt (wexec P s es) ≤ N := by
  simpa using h es.length

/-- A prefix of a run that keeps the cap keeps the cap. -/
theorem capped_take {N P : Nat} {s : St} {es : List Event} (h : Capped N P s es) (k : Nat) :
    Capped N P s (es.take k) := by
  intro j
  rw [List.take_take]
  exact h _

theorem capped_nil (N P : Nat) (s : St) : Capped N P s [] ↔ cnt s ≤ N := by
  constructor
  · intro h; exact capped_head h
  · intro h k; simpa [wexec] using h

theorem capped_cons (N P : Nat) (s : St) (e : Event) (es : List Event) :
    Capped N P s (e :: es) ↔ cnt s ≤ N ∧ Capped N P (wstep P s e) es := by
  constructor
  · intro h
    refine ⟨capped_head h, fun k => ?_⟩
    simpa [wexec] using h (k + 1)
  · intro ⟨h0, h⟩ k
    cases k with
    | zero => simpa [wexec] using h0
    | succ k => simpa [wexec] using h k

theorem capped_append (N P : Nat) : ∀ (a b : List Event) (s : St),
    Capped N P s (a ++ b) ↔ Capped N P s a ∧ Capped N P (wexec P s a) b := by
  intro a
  induction a with
  | nil =>
    intro b s
    exact ⟨fun h => ⟨(capped_nil N P s).mpr (capped_head h), h⟩, fun h => h.2⟩
  | cons e a ih =>
    intro b s
    simp only [List.cons_append, capped_cons, ih, wexec, and_assoc]

/-! ## Facts about the queue -/

theorem count_erase {q : List Nat} {i : Nat} (h : q[i]? ≠ some 0) :
    (q.eraseIdx i).count 0 = q.count 0 := by
  induction q generalizing i with
  | nil => simp
  | cons x xs ih =>
    cases i with
    | zero =>
      have hx : x ≠ 0 := by simpa using h
      simp [hx]
    | succ i =>
      have h' : xs[i]? ≠ some 0 := by simpa using h
      simp [List.count_cons, ih h']

theorem pos_erase {q : List Nat} {i : Nat} (h : q[i]? ≠ some 0) :
    pos (q.eraseIdx i) ≤ pos q := by
  induction q generalizing i with
  | nil => simp
  | cons x xs ih =>
    cases i with
    | zero =>
      have hx : x ≠ 0 := by simpa using h
      simp [pos, hx]
    | succ i =>
      have h' : xs[i]? ≠ some 0 := by simpa using h
      have := ih h'
      by_cases hx : x = 0
      · simp [pos, hx]
      · simp [pos, hx]
        omega

/-- A running task other than the spinner has the spinner in the queue. -/
theorem mem_of_count {cur : Nat} {q : List Nat} (hc : cur ≠ 0) (h : (cur :: q).count 0 = 1) :
    0 ∈ q ∧ q.count 0 = 1 := by
  have h1 : q.count 0 = 1 := by simpa [List.count_cons, hc] using h
  refine ⟨?_, h1⟩
  cases Decidable.em (0 ∈ q) with
  | inl hm => exact hm
  | inr hn =>
    have : List.count 0 q = 0 := List.count_eq_zero.mpr hn
    omega

/-! ## The potential argument -/

theorem inv_wstep (P : Nat) (s : St) (e : Event) (hi : Inv P s) : Inv P (wstep P s e) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h2, h3⟩ := hi
  simp only at h2 h3
  cases e with
  | step =>
    by_cases hc : 1 ≤ c
    · refine ⟨?_, ?_⟩
      · simp [wstep, hc]; omega
      · simpa [wstep, hc] using h3
    · have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .step = ⟨cur, ex, q, c, age, time, ran⟩ := by
        simp [wstep, hc]
      rw [this]; exact ⟨h2, h3⟩
  | tick =>
    by_cases hc0 : c = 0
    · subst hc0
      cases ex with
      | true => exact ⟨by simp [wstep, onTick], by simpa [wstep, onTick] using h3⟩
      | false =>
        cases q with
        | nil => exact ⟨by simp [wstep, onTick], by simpa [wstep, onTick] using h3⟩
        | cons x r =>
          refine ⟨by simp [wstep, onTick], ?_⟩
          simp [wstep, onTick, List.count_cons] at h3 ⊢
          omega
    · have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .tick = ⟨cur, ex, q, c, age, time, ran⟩ := by
        simp [wstep, hc0]
      rw [this]; exact ⟨h2, h3⟩
  | yield =>
    by_cases hc : cur = 0
    · have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .yield = ⟨cur, ex, q, c, age, time, ran⟩ := by
        simp [wstep, hc]
      rw [this]; exact ⟨h2, h3⟩
    · cases q with
      | nil => simp [hc] at h3
      | cons x r =>
        refine ⟨by simpa [wstep, hc] using h2, ?_⟩
        simp [wstep, hc, List.count_cons] at h3 ⊢
        omega
  | block =>
    by_cases hc : cur = 0
    · have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .block = ⟨cur, ex, q, c, age, time, ran⟩ := by
        simp [wstep, hc]
      rw [this]; exact ⟨h2, h3⟩
    · cases q with
      | nil => simp [hc] at h3
      | cons x r =>
        refine ⟨by simpa [wstep, hc] using h2, ?_⟩
        simp [wstep, hc, List.count_cons] at h3 ⊢
        omega
  | wake t =>
    by_cases ht : t = 0
    · have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ (.wake t) = ⟨cur, ex, q, c, age, time, ran⟩ := by
        simp [wstep, ht]
      rw [this]; exact ⟨h2, h3⟩
    · refine ⟨by simpa [wstep, ht] using h2, ?_⟩
      simp [wstep, ht, List.count_cons] at h3 ⊢
      omega
  | remove i =>
    by_cases hr : q[i]? = some 0
    · have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ (.remove i) = ⟨cur, ex, q, c, age, time, ran⟩ := by
        simp [wstep, hr]
      rw [this]; exact ⟨h2, h3⟩
    · refine ⟨by simpa [wstep, hr] using h2, ?_⟩
      have := count_erase hr
      simp [wstep, hr, List.count_cons] at h3 ⊢
      omega

/-- No event raises `lag + psi`. The cap is used at one place only: when the
tick cuts the spinner, the tasks that now run before it are at most `N - 1`. -/
theorem pot_wstep (N P : Nat) (s : St) (e : Event) (hi : Inv P s) (hN : cnt s ≤ N) :
    lag N (wstep P s e) + psi N P (wstep P s e) ≤ lag N s + psi N P s := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h2, h3⟩ := hi
  simp only at h2 h3
  have hlen : q.length + 1 ≤ N := hN
  obtain ⟨M, rfl⟩ : ∃ M, N = M + 1 := ⟨N - 1, by omega⟩
  have hP0 := nonneg_mul (pos q) P
  cases e with
  | step =>
    by_cases hc1 : 1 ≤ c
    · obtain ⟨c', rfl⟩ : ∃ c', c = c' + 1 := ⟨c - 1, by omega⟩
      by_cases hc : cur = 0
      · subst hc
        cases ex with
        | true =>
          simp [wstep, lag, psi, Int.mul_add, Int.add_mul]
          omega
        | false =>
          simp [wstep, lag, psi, Int.mul_add, Int.add_mul, Int.mul_sub]
          omega
      · cases ex <;> simp [wstep, lag, psi, hc, Int.add_mul] <;> omega
    · simp [wstep, hc1]
  | tick =>
    by_cases hc0 : c = 0
    · subst hc0
      by_cases hc : cur = 0
      · subst hc
        have hq : 0 ∉ q := List.count_eq_zero.mp (by simpa using h3)
        cases ex with
        | true => simp [wstep, onTick, lag, psi]
        | false =>
          cases q with
          | nil =>
            have hMP := nonneg_mul M P
            simp [wstep, onTick, lag, psi]
            omega
          | cons x r =>
            have hx : x ≠ 0 := fun e => hq (by simp [e])
            have hr0 : 0 ∉ r := fun e => hq (by simp [e])
            have hrl : r.length + 1 ≤ M := by simpa using hlen
            have hm : r.length * P + P ≤ M * P := by
              have := Nat.mul_le_mul_right P hrl
              rw [Nat.add_mul, Nat.one_mul] at this
              exact this
            have hm' : (r.length : Int) * P + P ≤ (M : Int) * P := by exact_mod_cast hm
            simp [wstep, onTick, lag, psi, hx, pos_append_self hr0]
            omega
      · obtain ⟨hq, _⟩ := mem_of_count hc h3
        cases ex with
        | true => simp [wstep, onTick, lag, psi, hc]
        | false =>
          cases q with
          | nil => simp at hq
          | cons x r =>
            by_cases hx : x = 0
            · subst hx
              simp [wstep, onTick, lag, psi, hc, pos_cons_zero]
            · have hr := mem_tail hq hx
              simp [wstep, onTick, lag, psi, hc, hx, pos_cons_ne r hx,
                pos_append_of_mem cur hr, Int.add_mul]
              omega
    · simp [wstep, hc0]
  | yield =>
    by_cases hc : cur = 0
    · simp [wstep, hc]
    · obtain ⟨hq, _⟩ := mem_of_count hc h3
      cases q with
      | nil => simp at hq
      | cons x r =>
        by_cases hx : x = 0
        · subst hx
          cases ex <;> simp [wstep, lag, psi, hc, pos_cons_zero] at hP0 ⊢ <;> omega
        · have hr := mem_tail hq hx
          cases ex <;>
          simp [wstep, lag, psi, hc, hx, pos_cons_ne r hx, pos_append_of_mem cur hr,
            Int.add_mul] at hP0 ⊢ <;> omega
  | block =>
    by_cases hc : cur = 0
    · simp [wstep, hc]
    · obtain ⟨hq, _⟩ := mem_of_count hc h3
      cases q with
      | nil => simp at hq
      | cons x r =>
        by_cases hx : x = 0
        · subst hx
          cases ex <;> simp [wstep, lag, psi, hc, pos_cons_zero] at hP0 ⊢ <;> omega
        · cases ex <;>
          simp [wstep, lag, psi, hc, hx, pos_cons_ne r hx, Int.add_mul] at hP0 ⊢ <;> omega
  | wake t =>
    by_cases ht : t = 0
    · simp [wstep, ht]
    · by_cases hc : cur = 0
      · simp [wstep, ht, lag, psi, hc]
      · obtain ⟨hq, _⟩ := mem_of_count hc h3
        simp [wstep, ht, lag, psi, hc, pos_append_of_mem t hq]
  | remove i =>
    by_cases hr : q[i]? = some 0
    · simp [wstep, hr]
    · have hmul : (pos (q.eraseIdx i) : Int) * P ≤ (pos q : Int) * P := by
        exact_mod_cast Nat.mul_le_mul_right P (pos_erase hr)
      by_cases hc : cur = 0
      · simp [wstep, hr, lag, psi, hc]
      · cases ex <;> simp [wstep, hr, lag, psi, hc] <;> omega

theorem pot_wexec (N P : Nat) : ∀ (es : List Event) (s : St), Inv P s → Capped N P s es →
    Inv P (wexec P s es) ∧
    lag N (wexec P s es) + psi N P (wexec P s es) ≤ lag N s + psi N P s := by
  intro es
  induction es with
  | nil => intro s hi _; exact ⟨hi, Int.le_refl _⟩
  | cons e es ih =>
    intro s hi hN
    obtain ⟨h0, hN'⟩ := (capped_cons N P s e es).mp hN
    have hl1 := pot_wstep N P s e hi h0
    obtain ⟨hi2, hl2⟩ := ih _ (inv_wstep P s e hi) hN'
    exact ⟨hi2, by simp only [wexec]; omega⟩

theorem pot_nonneg (N P : Nat) (s : St) (hi : Inv P s) : 0 ≤ psi N P s := by
  obtain ⟨h2, _⟩ := hi
  have hP0 := nonneg_mul (pos s.q) P
  by_cases hc : s.cur = 0
  · cases hex : s.ex with
    | true => simp [psi, hc, hex]
    | false =>
      simp only [psi, hc, hex]
      simp
      exact Int.mul_nonneg (Int.natCast_nonneg _) (by omega)
  · cases hex : s.ex <;> simp [psi, hc, hex] <;> omega

theorem pot_le (N P : Nat) (s : St) (hi : Inv P s) (hN : cnt s ≤ N) :
    psi N P s ≤ ((N - 1 : Nat) : Int) * P + s.c := by
  obtain ⟨h2, h3⟩ := hi
  have hlen : s.q.length + 1 ≤ N := hN
  obtain ⟨M, rfl⟩ : ∃ M, N = M + 1 := ⟨N - 1, by omega⟩
  have hMc : (0:Int) ≤ (M : Int) * (s.c : Int) := nonneg_mul M s.c
  by_cases hc : s.cur = 0
  · cases hex : s.ex with
    | true =>
      simp [psi, hc, hex]
      have := nonneg_mul M P
      omega
    | false =>
      simp [psi, hc, hex, Int.mul_sub]
      omega
  · obtain ⟨hq, _⟩ := mem_of_count hc h3
    have hp := pos_lt hq
    have hm : (pos s.q + 1) * P ≤ M * P := Nat.mul_le_mul_right P (by omega)
    rw [Nat.add_mul, Nat.one_mul] at hm
    have hm' : (pos s.q : Int) * P + P ≤ (M : Int) * P := by exact_mod_cast hm
    cases hex : s.ex <;> simp [psi, hc, hex] <;> omega

/-! ## The share bound -/

/-- HEADLINE, difference form. From any state `s0` with a countdown `c0 <= P`
and exactly one spinner, along any behaviour `es` of the wide model that never
holds more than `N` tasks ready or running: the steps that passed minus `N`
times the steps in which the spinner ran grew by at most `(N-1)*P + c0`.

The cap is a hypothesis; nothing here shows that a domain keeps it. The
statement needs no hypothesis on `age`. -/
theorem share_bound_diff (N P : Nat) (s0 : St) (hi : Inv P s0) (es : List Event)
    (hN : Capped N P s0 es) :
    (((wexec P s0 es).time : Int) - s0.time) ≤
      (N : Int) * (((wexec P s0 es).ran : Int) - s0.ran) + ((N - 1 : Nat) : Int) * P + s0.c := by
  obtain ⟨hi1, hl⟩ := pot_wexec N P es s0 hi hN
  have h0 := pot_nonneg N P _ hi1
  have h1 := pot_le N P s0 hi (capped_head hN)
  simp only [lag] at hl
  rw [Int.mul_sub]
  omega

/-- HEADLINE. A system that starts at time 0 with the countdown `c0` to its
first tick and with the spinner in it. For every `N`, `P` and every behaviour
`es` of the wide model: if no state along the run has more than `N` tasks ready
or running, then the number of steps that have passed is at most `N` times the
number of steps in which the spinner ran, plus `(N-1)*P + c0`.

This is a statement about the model. `N` is not derived: the theorem says
nothing about a run that at some instant holds more than `N` tasks
(`cap_needed`). The spinner must be in the system from `s0` on. -/
theorem share_bound (N P : Nat) (s0 : St) (hi : Inv P s0) (ht : s0.time = 0)
    (es : List Event) (hN : Capped N P s0 es) :
    (wexec P s0 es).time ≤ N * (wexec P s0 es).ran + (N - 1) * P + s0.c := by
  have h := share_bound_diff N P s0 hi es hN
  have hr := nonneg_mul N s0.ran
  rw [ht, Int.mul_sub] at h
  refine Int.ofNat_le.mp ?_
  simp only [Int.natCast_add, Int.natCast_mul]
  simp at h
  omega

/-- The headline at every instant of the run: `es.take k` is the run up to its
`k`-th event. -/
theorem share_bound_always (N P : Nat) (s0 : St) (hi : Inv P s0) (ht : s0.time = 0)
    (es : List Event) (hN : Capped N P s0 es) (k : Nat) :
    (wexec P s0 (es.take k)).time ≤
      N * (wexec P s0 (es.take k)).ran + (N - 1) * P + s0.c :=
  share_bound N P s0 hi ht _ (capped_take hN k)

/-- The hypotheses of `share_bound` hold together and the constant is reached:
`N = 3`, `P = 2`, `c0 = 2`, the first task has the mark and the spinner is at
the back. After `(N-1)*P + c0 = 6` steps the spinner has not run once. This is
the first tightness instance of `K0Share.lean` written in the events of this
file. There is no theorem of tightness for all `N` and `P`. -/
theorem share_bound_reached :
    Inv 2 (start0 1 [1, 0] 2 0) ∧ (start0 1 [1, 0] 2 0).time = 0 ∧
    Capped 3 2 (start0 1 [1, 0] 2 0) [.step, .step, .tick, .step, .step, .tick, .step, .step] ∧
    (wexec 2 (start0 1 [1, 0] 2 0)
      [.step, .step, .tick, .step, .step, .tick, .step, .step]).time = 6 ∧
    (wexec 2 (start0 1 [1, 0] 2 0)
      [.step, .step, .tick, .step, .step, .tick, .step, .step]).ran = 0 := by
  refine ⟨⟨by decide, by decide⟩, rfl, ?_, by decide, by decide⟩
  simp only [capped_cons, capped_nil]
  decide

/-! ## The wide model contains `K0Share.lean`

`widen` rewrites a behaviour of `K0Share.lean` as a behaviour of the wide model:
a `run` is a `step`, followed by a `tick` when the countdown ends; a wake that
the cap of `K0Share.step` drops is no event at all. -/

/-- One event of `K0Share.lean` as events of the wide model, at state `s`. -/
def widen1 (N : Nat) (s : St) : Ev → List Event
  | .run => if s.c ≤ 1 then [.step, .tick] else [.step]
  | .yield => [.yield]
  | .block => [.block]
  | .wake t => if t ≠ 0 ∧ cnt s < N then [.wake t] else []

/-- A behaviour of `K0Share.lean` from state `s` as a behaviour of the wide
model. -/
def widen (N P : Nat) (s : St) : List Ev → List Event
  | [] => []
  | e :: es => widen1 N s e ++ widen N P (step N P s e) es

theorem wexec_widen1 (N P : Nat) (s : St) (e : Ev) (h1 : 1 ≤ s.c) :
    wexec P s (widen1 N s e) = step N P s e := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only at h1
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · have hc1 : c = 1 := by omega
      subst hc1
      cases ex <;> cases q <;> simp [widen1, wexec, wstep, step, runStep, onTick]
    · simp [widen1, wexec, wstep, step, runStep, hc, h1]
  | yield => rfl
  | block => rfl
  | wake t =>
    by_cases hg : t ≠ 0 ∧ cnt ⟨cur, ex, q, c, age, time, ran⟩ < N
    · simp [widen1, wexec, wstep, step, hg]
    · simp [widen1, wexec, step, hg]

/-- In `K0Share.lean` the countdown stays above 0 (for `P >= 1`). -/
theorem base_c_pos (N P : Nat) (hP : 1 ≤ P) (s : St) (e : Ev) (h1 : 1 ≤ s.c) :
    1 ≤ (step N P s e).c := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only at h1
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · cases ex <;> cases q <;> simpa [step, runStep, onTick, hc] using hP
    · simp [step, runStep, hc]; omega
  | yield =>
    by_cases h0 : cur = 0
    · simpa [step, h0] using h1
    · cases q <;> simpa [step, h0] using h1
  | block =>
    by_cases h0 : cur = 0
    · simpa [step, h0] using h1
    · cases q <;> simpa [step, h0] using h1
  | wake t =>
    simp only [step]
    split <;> exact h1

/-- In `K0Share.lean` no event takes the number of tasks above `N`. This is the
cap of `wake` at work; it needs no other part of `Wf`. -/
theorem base_cnt_le (N P : Nat) (s : St) (e : Ev) (h : cnt s ≤ N) : cnt (step N P s e) ≤ N := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only [cnt] at h ⊢
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · cases ex <;> cases q <;> simpa [step, runStep, onTick, hc] using h
    · simpa [step, runStep, hc] using h
  | yield =>
    by_cases h0 : cur = 0
    · simpa [step, h0] using h
    · cases q <;> simpa [step, h0] using h
  | block =>
    by_cases h0 : cur = 0
    · simpa [step, h0] using h
    · cases q with
      | nil => simpa [step, h0] using h
      | cons x r => simp [step, h0] at h ⊢; omega
  | wake t =>
    by_cases hg : t ≠ 0 ∧ cnt ⟨cur, ex, q, c, age, time, ran⟩ < N
    · have hlt : q.length + 1 < N := hg.2
      simp [step, hg]
      omega
    · simpa [step, hg] using h

theorem capped_widen1 (N P : Nat) (s : St) (e : Ev) (h1 : 1 ≤ s.c) (h0 : cnt s ≤ N) :
    Capped N P s (widen1 N s e) := by
  have hn : cnt (step N P s e) ≤ N := base_cnt_le N P s e h0
  have he := wexec_widen1 N P s e h1
  cases e with
  | run =>
    by_cases hc : s.c ≤ 1
    · have hs : cnt (wstep P s .step) = cnt s := by
        simp only [wstep]; split <;> rfl
      have hl : widen1 N s .run = [.step, .tick] := ite_eq_left hc
      rw [hl] at he ⊢
      simp only [capped_cons, capped_nil]
      exact ⟨h0, by omega, by rw [← he] at hn; exact hn⟩
    · have hl : widen1 N s .run = [.step] := ite_eq_right hc
      rw [hl] at he ⊢
      simp only [capped_cons, capped_nil]
      exact ⟨h0, by rw [← he] at hn; exact hn⟩
  | yield =>
    simp only [widen1, capped_cons, capped_nil]
    exact ⟨h0, by rw [← he] at hn; exact hn⟩
  | block =>
    simp only [widen1, capped_cons, capped_nil]
    exact ⟨h0, by rw [← he] at hn; exact hn⟩
  | wake t =>
    by_cases hg : t ≠ 0 ∧ cnt s < N
    · have hl : widen1 N s (.wake t) = [.wake t] := ite_eq_left hg
      rw [hl] at he ⊢
      simp only [capped_cons, capped_nil]
      exact ⟨h0, by rw [← he] at hn; exact hn⟩
    · have hl : widen1 N s (.wake t) = [] := ite_eq_right hg
      rw [hl]
      exact (capped_nil N P s).mpr h0

/-- Every behaviour of `K0Share.lean` from a state with a countdown above 0,
rewritten by `widen`, ends in the same state in the wide model. All seven
fields are equal, the counters `time`, `ran` and `age` included. -/
theorem wexec_widen (N P : Nat) (hP : 1 ≤ P) : ∀ (es : List Ev) (s : St), 1 ≤ s.c →
    wexec P s (widen N P s es) = exec N P s es := by
  intro es
  induction es with
  | nil => intro s _; rfl
  | cons e es ih =>
    intro s h1
    simp only [widen, exec, wexec_append, wexec_widen1 N P s e h1]
    exact ih _ (base_c_pos N P hP s e h1)

/-- The rewritten behaviour never holds more than `N` tasks ready or running,
when the start state does not. -/
theorem capped_widen (N P : Nat) (hP : 1 ≤ P) : ∀ (es : List Ev) (s : St), 1 ≤ s.c →
    cnt s ≤ N → Capped N P s (widen N P s es) := by
  intro es
  induction es with
  | nil => intro s _ h0; exact (capped_nil N P s).mpr h0
  | cons e es ih =>
    intro s h1 h0
    simp only [widen, capped_append, wexec_widen1 N P s e h1]
    exact ⟨capped_widen1 N P s e h1 h0,
      ih _ (base_c_pos N P hP s e h1) (base_cnt_le N P s e h0)⟩

/-- A well-formed state of `K0Share.lean` meets the hypotheses of this file. -/
theorem inv_of_wf {N P : Nat} {s : St} (hw : Wf N P s) :
    Inv P s ∧ AgeOk P s ∧ cnt s ≤ N ∧ 1 ≤ s.c ∧ 1 ≤ P :=
  ⟨⟨hw.2.1, hw.2.2.1⟩, hw.2.2.2.2, hw.2.2.2.1, hw.1, Nat.le_trans hw.1 hw.2.1⟩

/-- Every behaviour of `K0Share.lean` from a well-formed state is a behaviour of
the wide model that keeps the cap, with the same final state. The converse is
false (`tick_after_yield_not_base`, `remove_not_block_wake`, `cap_needed`). -/
theorem contains (N P : Nat) (s : St) (hw : Wf N P s) (es : List Ev) :
    ∃ es' : List Event, wexec P s es' = exec N P s es ∧ Capped N P s es' :=
  ⟨widen N P s es, wexec_widen N P (inv_of_wf hw).2.2.2.2 es s hw.1,
    capped_widen N P (inv_of_wf hw).2.2.2.2 es s hw.1 hw.2.2.2.1⟩

/-- The headline of `K0Share.lean`, obtained from `share_bound` of this file, so
the headline here is not the weaker one of the two. The hypotheses are those of
`K0Share.share_bound` without two that it does not need: `ran = 0` at the
start, and the `age` clause of `Wf`. -/
theorem share_bound_base (N P : Nat) (s0 : St) (h1 : 1 ≤ s0.c) (h2 : s0.c ≤ P)
    (h3 : (s0.cur :: s0.q).count 0 = 1) (h4 : (s0.cur :: s0.q).length ≤ N)
    (ht : s0.time = 0) (es : List Ev) :
    (exec N P s0 es).time ≤ N * (exec N P s0 es).ran + (N - 1) * P + s0.c := by
  have hP : 1 ≤ P := Nat.le_trans h1 h2
  rw [← wexec_widen N P hP es s0 h1]
  exact share_bound N P s0 ⟨h2, h3⟩ ht _ (capped_widen N P hP es s0 h1 h4)

/-- The same for the difference form: `K0Share.share_bound_diff` from any state,
without the `age` clause of `Wf`. -/
theorem share_bound_diff_base (N P : Nat) (s0 : St) (h1 : 1 ≤ s0.c) (h2 : s0.c ≤ P)
    (h3 : (s0.cur :: s0.q).count 0 = 1) (h4 : (s0.cur :: s0.q).length ≤ N) (es : List Ev) :
    (((exec N P s0 es).time : Int) - s0.time) ≤
      (N : Int) * (((exec N P s0 es).ran : Int) - s0.ran) + ((N - 1 : Nat) : Int) * P + s0.c := by
  have hP : 1 ≤ P := Nat.le_trans h1 h2
  rw [← wexec_widen N P hP es s0 h1]
  exact share_bound_diff N P s0 ⟨h2, h3⟩ _ (capped_widen N P hP es s0 h1 h4)

/-! ## A task that got the CPU is not cut before `P` steps -/

theorem ageOk_wstep (P : Nat) (s : St) (e : Event) (h : AgeOk P s) : AgeOk P (wstep P s e) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only [AgeOk] at h ⊢
  cases e with
  | step =>
    by_cases hc : 1 ≤ c
    · intro hex
      have := h (by simpa [wstep, hc] using hex)
      simp [wstep, hc]
      omega
    · simpa [wstep, hc] using h
  | tick =>
    by_cases hc0 : c = 0
    · cases ex <;> cases q <;> simp [wstep, onTick, hc0]
    · simpa [wstep, hc0] using h
  | yield =>
    by_cases hc : cur = 0
    · simpa [wstep, hc] using h
    · cases q with
      | nil => simpa [wstep, hc] using h
      | cons x r => simp [wstep, hc]
  | block =>
    by_cases hc : cur = 0
    · simpa [wstep, hc] using h
    · cases q with
      | nil => simpa [wstep, hc] using h
      | cons x r => simp [wstep, hc]
  | wake t =>
    by_cases ht : t = 0
    · simpa [wstep, ht] using h
    · simpa [wstep, ht] using h
  | remove i =>
    by_cases hr : q[i]? = some 0
    · simpa [wstep, hr] using h
    · simpa [wstep, hr] using h

theorem ageOk_wexec (P : Nat) : ∀ (es : List Event) (s : St), AgeOk P s →
    AgeOk P (wexec P s es) := by
  intro es
  induction es with
  | nil => intro s h; exact h
  | cons e es ih => intro s h; exact ih _ (ageOk_wstep P s e h)

/-- A tick takes the CPU from the running task only when it is due and the
running task has no mark: in every other case the running task, the queue and
`age` are as before. -/
theorem tick_keeps (P : Nat) (s : St) (h : ¬ (s.c = 0 ∧ s.ex = false)) :
    (wstep P s .tick).cur = s.cur ∧ (wstep P s .tick).q = s.q ∧
    (wstep P s .tick).age = s.age := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only at h
  by_cases hc0 : c = 0
  · cases ex with
    | true => simp [wstep, onTick, hc0]
    | false => exact absurd ⟨hc0, rfl⟩ h
  · simp [wstep, hc0]

/-- "No early cut" for the wide model. Along any behaviour from a state that
meets `AgeOk`: at an instant at which the tick is due (countdown 0) and the
running task has no mark, so that the tick takes the CPU from it if anybody
waits, that task has run at least `P` steps since it got the CPU. Needs neither
the cap nor the spinner. `age` is tied to "steps since the task got the CPU" by
`wstep_age`. A task that gets the CPU out of idle is not covered (no idle). -/
theorem no_early_cut (P : Nat) (s0 : St) (ha : AgeOk P s0) (pre : List Event)
    (hc : (wexec P s0 pre).c = 0) (hex : (wexec P s0 pre).ex = false) :
    P ≤ (wexec P s0 pre).age := by
  have := ageOk_wexec P pre s0 ha hex
  omega

/-! ## A ready task starts within `(k+1)*P` steps of time

A ready task is a slot `i` of the queue (`i` tasks ahead of it; with the running
task, `k = i + 1` tasks run before it). `follow s i e` says what the event `e`
does to the task in slot `i`, and `fate` follows the task along a behaviour until
it starts or is removed. `follow_waits`, `follow_started`, `follow_removed`,
`fate_started` and `fate_removed` show that this really follows the same task.

Time is the field `time`. A `tick`, a `step` while a tick is due, and every
other event take no time, so the number of events says nothing. -/

/-- What became of a ready task. `waits j`: it is still ready, now in slot `j`.
`started t`: it became the running task when `time` was `t`. `removed`: a
`remove` took it out of the queue. -/
inductive Fate where
  | waits (j : Nat)
  | started (t : Nat)
  | removed
  deriving DecidableEq, Repr

/-- The head of the queue gets the CPU at time `t`: slot 0 starts, the others
move up by one. -/
def moveUp (t : Nat) : Nat → Fate
  | 0 => .started t
  | j + 1 => .waits j

/-- What the event `e` does to the task in queue slot `i` of `s`. -/
def follow (s : St) (i : Nat) : Event → Fate
  | .step => .waits i
  | .tick => if s.c = 0 ∧ s.ex = false ∧ s.q ≠ [] then moveUp s.time i else .waits i
  | .yield => if s.cur ≠ 0 ∧ s.q ≠ [] then moveUp s.time i else .waits i
  | .block => if s.cur ≠ 0 ∧ s.q ≠ [] then moveUp s.time i else .waits i
  | .wake _ => .waits i
  | .remove j =>
    if s.q[j]? = some 0 then .waits i
    else if j < i then .waits (i - 1)
    else if j = i then .removed
    else .waits i

/-- What a behaviour does to the task in queue slot `i` of `s`: the first event
that starts or removes it decides. -/
def fate (P : Nat) : St → Nat → List Event → Fate
  | _, i, [] => .waits i
  | s, i, e :: es =>
    match follow s i e with
    | .waits j => fate P (wstep P s e) j es
    | .started t => .started t
    | .removed => .removed

/-- `waits j` is right: the same task is in slot `j` after the event. -/
theorem follow_waits (P : Nat) (s : St) (i j : Nat) (e : Event)
    (h : follow s i e = .waits j) (hi : i < s.q.length) :
    j < (wstep P s e).q.length ∧ (wstep P s e).q[j]? = s.q[i]? := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only at hi
  cases e with
  | step =>
    simp only [follow, Fate.waits.injEq] at h
    subst h
    simp only [wstep]
    split <;> exact ⟨hi, rfl⟩
  | tick =>
    simp only [follow] at h
    split at h
    · rename_i hcut
      obtain ⟨hc0, hex, hq⟩ := hcut
      subst hc0; subst hex
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [moveUp] at h
        | succ i =>
          simp only [moveUp, Fate.waits.injEq] at h
          subst h
          simp [wstep, onTick] at hi ⊢
          refine ⟨by omega, ?_⟩
          rw [List.getElem?_append_left (by omega)]
    · rename_i hn
      simp only [Fate.waits.injEq] at h
      subst h
      have : (wstep P ⟨cur, ex, q, c, age, time, ran⟩ .tick).q = q := by
        by_cases hc0 : c = 0
        · cases ex with
          | true => simp [wstep, onTick, hc0]
          | false =>
            cases q with
            | nil => simp [wstep, onTick, hc0]
            | cons x r => exact absurd ⟨hc0, rfl, by simp⟩ hn
        · simp [wstep, hc0]
      rw [this]; exact ⟨hi, rfl⟩
  | yield =>
    simp only [follow] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [moveUp] at h
        | succ i =>
          simp only [moveUp, Fate.waits.injEq] at h
          subst h
          simp [wstep, hc] at hi ⊢
          refine ⟨by omega, ?_⟩
          rw [List.getElem?_append_left (by omega)]
    · rename_i hn
      simp only [Fate.waits.injEq] at h
      subst h
      have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .yield = ⟨cur, ex, q, c, age, time, ran⟩ := by
        by_cases hc : cur = 0
        · simp [wstep, hc]
        · cases q with
          | nil => simp [wstep]
          | cons x r => exact absurd ⟨hc, by simp⟩ hn
      rw [this]; exact ⟨hi, rfl⟩
  | block =>
    simp only [follow] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [moveUp] at h
        | succ i =>
          simp only [moveUp, Fate.waits.injEq] at h
          subst h
          simp [wstep, hc] at hi ⊢
          omega
    · rename_i hn
      simp only [Fate.waits.injEq] at h
      subst h
      have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .block = ⟨cur, ex, q, c, age, time, ran⟩ := by
        by_cases hc : cur = 0
        · simp [wstep, hc]
        · cases q with
          | nil => simp [wstep]
          | cons x r => exact absurd ⟨hc, by simp⟩ hn
      rw [this]; exact ⟨hi, rfl⟩
  | wake t =>
    simp only [follow, Fate.waits.injEq] at h
    subst h
    by_cases ht : t = 0
    · simp only [wstep, ht]
      exact ⟨hi, rfl⟩
    · simp [wstep, ht]
      refine ⟨by omega, ?_⟩
      rw [List.getElem?_append_left hi]
  | remove j' =>
    simp only [follow] at h
    by_cases hr : q[j']? = some 0
    · simp only [hr, ite_true, Fate.waits.injEq] at h
      subst h
      have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ (.remove j') = ⟨cur, ex, q, c, age, time, ran⟩ := by
        simp [wstep, hr]
      rw [this]; exact ⟨hi, rfl⟩
    · simp only [hr, ite_false] at h
      have hq' : (wstep P ⟨cur, ex, q, c, age, time, ran⟩ (.remove j')).q = q.eraseIdx j' := by
        simp [wstep, hr]
      rw [hq', List.length_eraseIdx, List.getElem?_eraseIdx]
      by_cases hlt : j' < i
      · simp only [hlt, ite_true, Fate.waits.injEq] at h
        subst h
        have h1 : j' < q.length := by omega
        have h2 : ¬ (i - 1 < j') := by omega
        have h3 : i - 1 + 1 = i := by omega
        simp only [h1, h2, h3, ite_true, ite_false]
        exact ⟨by omega, trivial⟩
      · simp only [hlt, ite_false] at h
        by_cases heq : j' = i
        · simp [heq] at h
        · simp only [heq, ite_false, Fate.waits.injEq] at h
          subst h
          have h2 : i < j' := by omega
          simp only [h2, ite_true]
          refine ⟨?_, trivial⟩
          split <;> omega

/-- `started t` is right: the task was at the head of the queue, it is the
running task after the event, the event took no time, and `t` is that time. -/
theorem follow_started (P : Nat) (s : St) (i t : Nat) (e : Event)
    (h : follow s i e = .started t) :
    i = 0 ∧ t = s.time ∧ (wstep P s e).time = s.time ∧
    ∃ x r, s.q = x :: r ∧ (wstep P s e).cur = x := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | step => simp [follow] at h
  | tick =>
    simp only [follow] at h
    split at h
    · rename_i hcut
      obtain ⟨hc0, hex, hq⟩ := hcut
      subst hc0; subst hex
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero =>
          simp only [moveUp, Fate.started.injEq] at h
          exact ⟨rfl, h.symm, by simp [wstep, onTick], x, r, rfl, by simp [wstep, onTick]⟩
        | succ i => simp [moveUp] at h
    · simp at h
  | yield =>
    simp only [follow] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero =>
          simp only [moveUp, Fate.started.injEq] at h
          exact ⟨rfl, h.symm, by simp [wstep, hc], x, r, rfl, by simp [wstep, hc]⟩
        | succ i => simp [moveUp] at h
    · simp at h
  | block =>
    simp only [follow] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero =>
          simp only [moveUp, Fate.started.injEq] at h
          exact ⟨rfl, h.symm, by simp [wstep, hc], x, r, rfl, by simp [wstep, hc]⟩
        | succ i => simp [moveUp] at h
    · simp at h
  | wake t' => simp [follow] at h
  | remove j' =>
    simp only [follow] at h
    split at h
    · simp at h
    · split at h
      · simp at h
      · split at h <;> simp at h

/-- `removed` is right: the event is `remove` of that very slot, the task in it
is not the spinner, and the queue loses exactly that slot. -/
theorem follow_removed (P : Nat) (s : St) (i : Nat) (e : Event)
    (h : follow s i e = .removed) :
    e = .remove i ∧ s.q[i]? ≠ some 0 ∧ (wstep P s e).q = s.q.eraseIdx i := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | step => simp [follow] at h
  | tick =>
    simp only [follow] at h
    split at h
    · cases i <;> simp [moveUp] at h
    · simp at h
  | yield =>
    simp only [follow] at h
    split at h
    · cases i <;> simp [moveUp] at h
    · simp at h
  | block =>
    simp only [follow] at h
    split at h
    · cases i <;> simp [moveUp] at h
    · simp at h
  | wake t' => simp [follow] at h
  | remove j' =>
    simp only [follow] at h
    by_cases hr : q[j']? = some 0
    · simp [hr] at h
    · simp only [hr, ite_false] at h
      by_cases hlt : j' < i
      · simp [hlt] at h
      · simp only [hlt, ite_false] at h
        by_cases heq : j' = i
        · subst heq
          exact ⟨rfl, hr, by simp [wstep, hr]⟩
        · simp [heq] at h

/-- While the task waits, `time + wt` does not grow: every step of time takes
one off the bound `wt`, and no other event raises it. No invariant is needed. -/
theorem wt_follow (P : Nat) (s : St) (i j : Nat) (e : Event)
    (h : follow s i e = .waits j) (hi : i < s.q.length) :
    (wstep P s e).time + wt P (wstep P s e) j ≤ s.time + wt P s i := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only at hi
  cases e with
  | step =>
    simp only [follow, Fate.waits.injEq] at h
    subst h
    by_cases hc : 1 ≤ c
    · cases ex <;> simp [wstep, hc, wt] <;> omega
    · simp [wstep, hc]
  | tick =>
    simp only [follow] at h
    split at h
    · rename_i hcut
      obtain ⟨hc0, hex, hq⟩ := hcut
      subst hc0; subst hex
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [moveUp] at h
        | succ i =>
          simp only [moveUp, Fate.waits.injEq] at h
          subst h
          simp [wstep, onTick, wt, Nat.add_mul]
          omega
    · rename_i hn
      simp only [Fate.waits.injEq] at h
      subst h
      by_cases hc0 : c = 0
      · cases ex with
        | true => simp [wstep, onTick, hc0, wt]
        | false =>
          cases q with
          | nil => simp at hi
          | cons x r => exact absurd ⟨hc0, rfl, by simp⟩ hn
      · simp [wstep, hc0]
  | yield =>
    simp only [follow] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [moveUp] at h
        | succ i =>
          simp only [moveUp, Fate.waits.injEq] at h
          subst h
          cases ex <;> simp [wstep, hc, wt, Nat.add_mul] <;> omega
    · rename_i hn
      simp only [Fate.waits.injEq] at h
      subst h
      have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .yield = ⟨cur, ex, q, c, age, time, ran⟩ := by
        by_cases hc : cur = 0
        · simp [wstep, hc]
        · cases q with
          | nil => simp [wstep]
          | cons x r => exact absurd ⟨hc, by simp⟩ hn
      rw [this]; exact Nat.le_refl _
  | block =>
    simp only [follow] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [moveUp] at h
        | succ i =>
          simp only [moveUp, Fate.waits.injEq] at h
          subst h
          cases ex <;> simp [wstep, hc, wt, Nat.add_mul] <;> omega
    · rename_i hn
      simp only [Fate.waits.injEq] at h
      subst h
      have : wstep P ⟨cur, ex, q, c, age, time, ran⟩ .block = ⟨cur, ex, q, c, age, time, ran⟩ := by
        by_cases hc : cur = 0
        · simp [wstep, hc]
        · cases q with
          | nil => simp [wstep]
          | cons x r => exact absurd ⟨hc, by simp⟩ hn
      rw [this]; exact Nat.le_refl _
  | wake t =>
    simp only [follow, Fate.waits.injEq] at h
    subst h
    by_cases ht : t = 0
    · simp [wstep, ht]
    · simp [wstep, ht, wt]
  | remove j' =>
    have hji : j ≤ i := by
      simp only [follow] at h
      split at h
      · simp only [Fate.waits.injEq] at h; omega
      · split at h
        · simp only [Fate.waits.injEq] at h; omega
        · split at h
          · simp at h
          · simp only [Fate.waits.injEq] at h; omega
    have hm := Nat.mul_le_mul_right P hji
    by_cases hr : q[j']? = some 0
    · simp [wstep, hr, wt]; omega
    · simp [wstep, hr, wt]; omega

/-- Along a behaviour, for a task that still waits at the end: it is the same
task, and `time + wt` is not above its start value. -/
theorem fate_waits (P : Nat) : ∀ (es : List Event) (s : St) (i j : Nat), i < s.q.length →
    fate P s i es = .waits j →
    j < (wexec P s es).q.length ∧ (wexec P s es).q[j]? = s.q[i]? ∧
    (wexec P s es).time + wt P (wexec P s es) j ≤ s.time + wt P s i := by
  intro es
  induction es with
  | nil =>
    intro s i j hi h
    simp only [fate, Fate.waits.injEq] at h
    subst h
    exact ⟨hi, rfl, Nat.le_refl _⟩
  | cons e es ih =>
    intro s i j hi h
    simp only [fate] at h
    cases hf : follow s i e with
    | waits j1 =>
      rw [hf] at h
      simp only at h
      obtain ⟨hj1, hq1⟩ := follow_waits P s i j1 e hf hi
      have hw1 := wt_follow P s i j1 e hf hi
      obtain ⟨a, b, c⟩ := ih _ j1 j hj1 h
      exact ⟨a, by rw [← hq1]; exact b, by simp only [wexec]; omega⟩
    | started t => rw [hf] at h; simp at h
    | removed => rw [hf] at h; simp at h

/-- `fate = started t` means what it says: the behaviour splits as
`pre ++ e :: post` such that the task is at the head of the queue after `pre`,
is the running task after `e`, and `time` is `t` then. That instant is no later
than `wt` steps of time after the start. -/
theorem fate_started (P : Nat) : ∀ (es : List Event) (s : St) (i t : Nat), i < s.q.length →
    fate P s i es = .started t →
    t ≤ s.time + wt P s i ∧
    ∃ pre e post, es = pre ++ e :: post ∧ (wexec P s pre).q.head? = s.q[i]? ∧
      some (wexec P s (pre ++ [e])).cur = s.q[i]? ∧ (wexec P s (pre ++ [e])).time = t := by
  intro es
  induction es with
  | nil => intro s i t _ h; simp [fate] at h
  | cons e es ih =>
    intro s i t hi h
    simp only [fate] at h
    cases hf : follow s i e with
    | waits j1 =>
      rw [hf] at h
      simp only at h
      obtain ⟨hj1, hq1⟩ := follow_waits P s i j1 e hf hi
      have hw1 := wt_follow P s i j1 e hf hi
      obtain ⟨hb, pre, e', post, hes, h1, h2, h3⟩ := ih _ j1 t hj1 h
      refine ⟨by omega, e :: pre, e', post, by rw [hes]; rfl, ?_, ?_, h3⟩
      · rw [← hq1]; exact h1
      · rw [← hq1]; exact h2
    | started t' =>
      rw [hf] at h
      simp only [Fate.started.injEq] at h
      subst h
      obtain ⟨hi0, ht, htime, x, r, hq, hcur⟩ := follow_started P s i t' e hf
      subst hi0
      refine ⟨by omega, [], e, es, rfl, ?_, ?_, ?_⟩
      · simp [wexec, hq]
      · simp [wexec, hq, hcur]
      · simp only [List.nil_append, wexec]; omega
    | removed => rw [hf] at h; simp at h

/-- `fate = removed` means what it says: a `remove` event hit the slot in which
the task was, and the task is not the spinner. -/
theorem fate_removed (P : Nat) : ∀ (es : List Event) (s : St) (i : Nat), i < s.q.length →
    fate P s i es = .removed →
    s.q[i]? ≠ some 0 ∧
    ∃ pre j post, es = pre ++ Event.remove j :: post ∧ (wexec P s pre).q[j]? = s.q[i]? ∧
      (wexec P s (pre ++ [Event.remove j])).q = (wexec P s pre).q.eraseIdx j := by
  intro es
  induction es with
  | nil => intro s i _ h; simp [fate] at h
  | cons e es ih =>
    intro s i hi h
    simp only [fate] at h
    cases hf : follow s i e with
    | waits j1 =>
      rw [hf] at h
      simp only at h
      obtain ⟨hj1, hq1⟩ := follow_waits P s i j1 e hf hi
      obtain ⟨hne, pre, j, post, hes, h1, h2⟩ := ih _ j1 hj1 h
      refine ⟨by rw [← hq1]; exact hne, e :: pre, j, post, by rw [hes]; rfl, ?_, h2⟩
      rw [← hq1]; exact h1
    | started t' => rw [hf] at h; simp at h
    | removed =>
      obtain ⟨he, hne, hq⟩ := follow_removed P s i e hf
      subst he
      exact ⟨hne, [], i, es, rfl, rfl, hq⟩

theorem fate_append (P : Nat) : ∀ (a b : List Event) (s : St) (i : Nat),
    fate P s i (a ++ b) =
      match fate P s i a with
      | .waits j => fate P (wexec P s a) j b
      | .started t => .started t
      | .removed => .removed := by
  intro a
  induction a with
  | nil => intro b s i; rfl
  | cons e a ih =>
    intro b s i
    simp only [List.cons_append, fate, wexec]
    cases follow s i e with
    | waits j1 => exact ih b _ j1
    | started t => rfl
    | removed => rfl

/-- The bound `wt` at the start is at most `(k+1)*P`. -/
theorem wt_start (P : Nat) (s : St) (hc : s.c ≤ P) (k : Nat) (hk : 1 ≤ k) :
    wt P s (k - 1) ≤ (k + 1) * P := by
  obtain ⟨m, rfl⟩ : ∃ m, k = m + 1 := ⟨k - 1, by omega⟩
  simp only [wt, Nat.add_sub_cancel, Nat.add_mul, Nat.one_mul]
  cases s.ex <;> simp <;> omega

/-- LEMMA 3 for the wide model. A ready task with `k` tasks to run before it
(the running one included), that is in queue slot `k - 1`, in a state whose
countdown is at most `P`. Along any behaviour:

* if the task became the running task, it did so at an instant no later than
  `(k+1)*P` steps of time after the start;
* as long as it waits, no more than `(k+1)*P` steps of time have passed.

Nothing is said about a task that was removed. The cap, the spinner and `age`
play no part. This is not "after `(k+1)*P` steps it has started": at exactly
`(k+1)*P` steps the last tick may still be due (`wait_at_bound`); see
`wait_bound_passed` and `wait_bound_tick`. -/
theorem wait_bound (P : Nat) (s : St) (hc : s.c ≤ P) (k : Nat) (hk : 1 ≤ k)
    (hq : k - 1 < s.q.length) (es : List Event) :
    (∀ t, fate P s (k - 1) es = .started t → t ≤ s.time + (k + 1) * P) ∧
    (∀ j, fate P s (k - 1) es = .waits j → (wexec P s es).time ≤ s.time + (k + 1) * P) := by
  have hwt := wt_start P s hc k hk
  constructor
  · intro t h
    have := (fate_started P es s (k - 1) t hq h).1
    omega
  · intro j h
    have := (fate_waits P es s (k - 1) j hq h).2.2
    omega

/-- Once more than `(k+1)*P` steps of time have passed, a task that was not
removed has become the running task, and it did so no later than `(k+1)*P`
steps after the start. -/
theorem wait_bound_passed (P : Nat) (s : St) (hc : s.c ≤ P) (k : Nat) (hk : 1 ≤ k)
    (hq : k - 1 < s.q.length) (es : List Event)
    (hes : s.time + (k + 1) * P < (wexec P s es).time)
    (hrm : fate P s (k - 1) es ≠ .removed) :
    ∃ t, fate P s (k - 1) es = .started t ∧ t ≤ s.time + (k + 1) * P := by
  obtain ⟨h1, h2⟩ := wait_bound P s hc k hk hq es
  cases hf : fate P s (k - 1) es with
  | waits j => have := h2 j hf; omega
  | started t => exact ⟨t, rfl, h1 t hf⟩
  | removed => exact absurd hf hrm

/-- When exactly `(k+1)*P` steps of time (or more) have passed and the task
still waits, then the tick is due, and that tick starts the task. No step can
pass before it. -/
theorem wait_bound_tick (P : Nat) (hP : 1 ≤ P) (s : St) (hc : s.c ≤ P) (k : Nat) (hk : 1 ≤ k)
    (hq : k - 1 < s.q.length) (es : List Event) (j : Nat)
    (hes : s.time + (k + 1) * P ≤ (wexec P s es).time)
    (hf : fate P s (k - 1) es = .waits j) :
    (wexec P s es).c = 0 ∧
    fate P s (k - 1) (es ++ [.tick]) = .started (wexec P s es).time := by
  have hwt := wt_start P s hc k hk
  obtain ⟨hj, _, hb⟩ := fate_waits P es s (k - 1) j hq hf
  have h0 : wt P (wexec P s es) j = 0 := by omega
  simp only [wt] at h0
  have hc0 : (wexec P s es).c = 0 := by omega
  have hex : (wexec P s es).ex = false := by
    cases hx : (wexec P s es).ex with
    | false => rfl
    | true => rw [hx] at h0; simp at h0; omega
  have hj0 : j = 0 := by
    cases j with
    | zero => rfl
    | succ j =>
      have : 1 * P ≤ (j + 1) * P := Nat.mul_le_mul_right P (by omega)
      omega
  subst hj0
  refine ⟨hc0, ?_⟩
  have hne : (wexec P s es).q ≠ [] := by
    intro h; rw [h] at hj; simp at hj
  rw [fate_append, hf]
  simp [fate, follow, moveUp, hc0, hex, hne]

/-- The form of `K0Share.wait_bound` is false in the wide model. `P = 1`,
`k = 1`: after `(k+1)*P = 2` steps of time the task in slot 0 still waits,
because the second tick is due and has not been delivered. That tick starts it,
at time 2. -/
theorem wait_at_bound :
    let s : St := { cur := 1, ex := true, q := [7, 0], c := 1, age := 0, time := 0, ran := 0 }
    (wexec 1 s [.step, .tick, .step]).time = 2 ∧
    fate 1 s 0 [.step, .tick, .step] = .waits 0 ∧
    fate 1 s 0 [.step, .tick, .step, .tick] = .started 2 := by
  decide

/-- The three fates on one state (`P = 1`, the queue is `[7, 8, 0]`). The task in
slot 1 moves up when the task ahead of it is removed and starts at the second
tick, at time 2. The task in slot 0 is removed. The spinner, in slot 2, is not
removed by a `remove` of its slot. -/
theorem fate_instance :
    let s : St := { cur := 1, ex := true, q := [7, 8, 0], c := 1, age := 0, time := 0, ran := 0 }
    fate 1 s 1 [.remove 0, .step, .tick, .step, .tick] = .started 2 ∧
    fate 1 s 0 [.remove 0, .step, .tick, .step, .tick] = .removed ∧
    fate 1 s 2 [.remove 2] = .waits 2 := by
  decide

/-! ## What the counters count

`time`, `ran` and `age` are fields that the rules write and never read. These
statements tie them to the events from outside the definition of `wstep`. -/

/-- `time` grows by one on a `step` that is taken (countdown above 0) and on
nothing else. -/
theorem wstep_time (P : Nat) (s : St) (e : Event) :
    (wstep P s e).time = s.time + (if e = .step ∧ 1 ≤ s.c then 1 else 0) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | step => by_cases hc : 1 ≤ c <;> simp [wstep, hc]
  | tick =>
    by_cases hc0 : c = 0
    · cases ex <;> cases q <;> simp [wstep, onTick, hc0]
    · simp [wstep, hc0]
  | yield =>
    by_cases hc : cur = 0
    · simp [wstep, hc]
    · cases q <;> simp [wstep, hc]
  | block =>
    by_cases hc : cur = 0
    · simp [wstep, hc]
    · cases q <;> simp [wstep, hc]
  | wake t => by_cases ht : t = 0 <;> simp [wstep, ht]
  | remove i => by_cases hr : q[i]? = some 0 <;> simp [wstep, hr]

/-- `ran` grows by one on a `step` that is taken while the spinner is the
running task, and on nothing else. -/
theorem wstep_ran (P : Nat) (s : St) (e : Event) :
    (wstep P s e).ran = s.ran + (if e = .step ∧ 1 ≤ s.c ∧ s.cur = 0 then 1 else 0) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | step => by_cases hc : 1 ≤ c <;> by_cases h0 : cur = 0 <;> simp [wstep, hc, h0]
  | tick =>
    by_cases hc0 : c = 0
    · cases ex <;> cases q <;> simp [wstep, onTick, hc0]
    · simp [wstep, hc0]
  | yield =>
    by_cases hc : cur = 0
    · simp [wstep, hc]
    · cases q <;> simp [wstep, hc]
  | block =>
    by_cases hc : cur = 0
    · simp [wstep, hc]
    · cases q <;> simp [wstep, hc]
  | wake t => by_cases ht : t = 0 <;> simp [wstep, ht]
  | remove i => by_cases hr : q[i]? = some 0 <;> simp [wstep, hr]

/-- `age` is 0 right after the head of the queue got the CPU (by a yield, a
block or a cut; `follow_started` says what that means), it grows by one on a
`step` that is taken, and no other event changes it. -/
theorem wstep_age (P : Nat) (s : St) (e : Event) :
    (wstep P s e).age =
      if follow s 0 e = .started s.time then 0
      else s.age + (if e = .step ∧ 1 ≤ s.c then 1 else 0) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | step => by_cases hc : 1 ≤ c <;> simp [wstep, follow, hc]
  | tick =>
    by_cases hc0 : c = 0
    · cases ex <;> cases q <;> simp [wstep, onTick, follow, moveUp, hc0]
    · simp [wstep, follow, hc0]
  | yield =>
    by_cases hc : cur = 0
    · simp [wstep, follow, hc]
    · cases q <;> simp [wstep, follow, moveUp, hc]
  | block =>
    by_cases hc : cur = 0
    · simp [wstep, follow, hc]
    · cases q <;> simp [wstep, follow, moveUp, hc]
  | wake t => by_cases ht : t = 0 <;> simp [wstep, follow, ht]
  | remove i =>
    have hf : follow ⟨cur, ex, q, c, age, time, ran⟩ 0 (.remove i) ≠ .started time := by
      simp only [follow]
      split
      · simp
      · split
        · simp
        · split <;> simp
    by_cases hr : q[i]? = some 0 <;> simp [wstep, hr, hf]

/-- Along any behaviour `time` and `ran` do not fall, and `ran` grows by no
more than `time`: the spinner is not credited more steps than pass. -/
theorem wexec_ran_le_time (P : Nat) : ∀ (es : List Event) (s : St),
    s.time ≤ (wexec P s es).time ∧ s.ran ≤ (wexec P s es).ran ∧
    (wexec P s es).ran + s.time ≤ (wexec P s es).time + s.ran := by
  intro es
  induction es with
  | nil => intro s; exact ⟨Nat.le_refl _, Nat.le_refl _, by simp only [wexec]; omega⟩
  | cons e es ih =>
    intro s
    obtain ⟨h1, h2, h3⟩ := ih (wstep P s e)
    have ht := wstep_time P s e
    have hr := wstep_ran P s e
    simp only [wexec]
    by_cases ha : e = .step ∧ 1 ≤ s.c
    · rw [ite_eq_left ha] at ht
      by_cases hb : e = .step ∧ 1 ≤ s.c ∧ s.cur = 0
      · rw [ite_eq_left hb] at hr
        omega
      · rw [ite_eq_right hb] at hr
        omega
    · have hb : ¬ (e = .step ∧ 1 ≤ s.c ∧ s.cur = 0) := fun h => ha ⟨h.1, h.2.1⟩
      rw [ite_eq_right ha] at ht
      rw [ite_eq_right hb] at hr
      omega

/-! ## Witnesses: the wide model is strictly wider, and the cap is needed -/

/-- In `K0Share.lean` the mark is taken only by a `run`, and that `run` first
gives the marked task one step: no task loses the mark before it has run. -/
theorem base_mark_needs_step (N P : Nat) (s : St) (e : Ev) (h : s.ex = true)
    (h' : (step N P s e).ex = false) :
    (step N P s e).cur = s.cur ∧ (step N P s e).age = s.age + 1 ∧
    (step N P s e).time = s.time + 1 := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only at h
  subst h
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · simp [step, runStep, onTick, hc]
    · simp [step, runStep, hc] at h'
  | yield =>
    by_cases h0 : cur = 0
    · simp [step, h0] at h'
    · cases q <;> simp [step, h0] at h'
  | block =>
    by_cases h0 : cur = 0
    · simp [step, h0] at h'
    · cases q <;> simp [step, h0] at h'
  | wake t =>
    simp only [step] at h'
    split at h' <;> simp at h'

/-- An order that `K0Share.lean` cannot express. The running task (with the
mark) yields to the spinner after the last step of the period and before its
tick: the spinner enters with the mark (`age = 0`, countdown 0) and the tick
takes the mark at once, before the spinner has run (`age` still 0, `ran = 0`).
By `base_mark_needs_step` no event of `K0Share.lean` does this, and
`tick_after_yield_not_base` shows that no behaviour of it does. -/
theorem tick_after_yield :
    let s0 : St := { cur := 1, ex := true, q := [0], c := 1, age := 0, time := 0, ran := 0 }
    let s1 := wexec 3 s0 [.step, .yield]
    let s2 := wexec 3 s0 [.step, .yield, .tick]
    (s1.cur = 0 ∧ s1.ex = true ∧ s1.age = 0 ∧ s1.c = 0) ∧
    (s2.cur = 0 ∧ s2.ex = false ∧ s2.age = 0 ∧ s2.c = 3 ∧ s2.q = [1] ∧ s2.time = 1 ∧
      s2.ran = 0) := by
  decide

/-- The state that `tick_after_yield` reaches is out of reach of `K0Share.lean`:
from the same start state, for every `N` and `P`, no behaviour of it comes to
an instant at which one step has passed, the spinner has not run, and the
spinner is the running task without the mark. -/
theorem tick_after_yield_not_base (N P : Nat) (es : List Ev) :
    let s0 : St := { cur := 1, ex := true, q := [0], c := 1, age := 0, time := 0, ran := 0 }
    ¬ ((exec N P s0 es).time = 1 ∧ (exec N P s0 es).ran = 0 ∧ (exec N P s0 es).cur = 0 ∧
      (exec N P s0 es).ex = false) := by
  intro s0
  have key : ∀ (es : List Ev) (s : St),
      ((s.time = 0 → s.ex = true ∧ s.c = 1 ∧ s.ran = 0) ∧
        (s.time = 1 → s.ran = 0 → s.cur = 0 → s.ex = true)) →
      ((exec N P s es).time = 1 → (exec N P s es).ran = 0 → (exec N P s es).cur = 0 →
        (exec N P s es).ex = true) := by
    intro es
    induction es with
    | nil => intro s hJ; exact hJ.2
    | cons e es ih =>
      intro s hJ
      apply ih
      obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
      obtain ⟨h1, h2⟩ := hJ
      simp only at h1 h2
      cases e with
      | run =>
        by_cases ht : time = 0
        · obtain ⟨hex, hc, hr⟩ := h1 ht
          subst ht; subst hex; subst hc; subst hr
          by_cases h0 : cur = 0 <;> simp [step, runStep, onTick, h0]
        · have hrun : (step N P ⟨cur, ex, q, c, age, time, ran⟩ .run).time = time + 1 := by
            by_cases hc : c ≤ 1
            · cases ex <;> cases q <;> simp [step, runStep, onTick, hc]
            · simp [step, runStep, hc]
          refine ⟨fun h => ?_, fun h => ?_⟩ <;> (rw [hrun] at h; omega)
      | yield =>
        by_cases h0 : cur = 0
        · have : step N P ⟨cur, ex, q, c, age, time, ran⟩ .yield =
              ⟨cur, ex, q, c, age, time, ran⟩ := by simp [step, h0]
          rw [this]; exact ⟨h1, h2⟩
        · cases q with
          | nil =>
            have : step N P ⟨cur, ex, [], c, age, time, ran⟩ .yield =
                ⟨cur, ex, [], c, age, time, ran⟩ := by simp [step]
            rw [this]; exact ⟨h1, h2⟩
          | cons x r =>
            have : step N P ⟨cur, ex, x :: r, c, age, time, ran⟩ .yield =
                ⟨x, true, r ++ [cur], c, 0, time, ran⟩ := by simp [step, h0]
            rw [this]
            exact ⟨fun h => ⟨rfl, (h1 h).2⟩, fun _ _ _ => rfl⟩
      | block =>
        by_cases h0 : cur = 0
        · have : step N P ⟨cur, ex, q, c, age, time, ran⟩ .block =
              ⟨cur, ex, q, c, age, time, ran⟩ := by simp [step, h0]
          rw [this]; exact ⟨h1, h2⟩
        · cases q with
          | nil =>
            have : step N P ⟨cur, ex, [], c, age, time, ran⟩ .block =
                ⟨cur, ex, [], c, age, time, ran⟩ := by simp [step]
            rw [this]; exact ⟨h1, h2⟩
          | cons x r =>
            have : step N P ⟨cur, ex, x :: r, c, age, time, ran⟩ .block =
                ⟨x, true, r, c, 0, time, ran⟩ := by simp [step, h0]
            rw [this]
            exact ⟨fun h => ⟨rfl, (h1 h).2⟩, fun _ _ _ => rfl⟩
      | wake t =>
        simp only [step]
        split
        · exact ⟨h1, h2⟩
        · exact ⟨h1, h2⟩
  intro ⟨a, b, c, d⟩
  have := key es s0 ⟨fun _ => ⟨rfl, rfl, rfl⟩, fun h => absurd h (by decide)⟩ a b c
  rw [d] at this
  cases this

/-- `remove` is not a mix of the other events. The spinner runs and two tasks
are ready. No list of events without `remove` reaches, in zero time, the queue
that `remove 0` leaves: `yield` and `block` do nothing for the spinner, `wake`
only appends, and a `step` takes time. (With time it can be reached: the task
at the head gets the CPU at a tick and blocks.) -/
theorem remove_not_block_wake (P : Nat) (es : List Event) (hno : ∀ i, Event.remove i ∉ es) :
    let s : St := { cur := 0, ex := false, q := [1, 2], c := 2, age := 2, time := 0, ran := 0 }
    (wexec P s es).time = 0 → (wexec P s es).q ≠ (wstep P s (.remove 0)).q := by
  intro s
  have key : ∀ (es : List Event) (t : St), (∀ i, Event.remove i ∉ es) → t.cur = 0 →
      2 ≤ t.q.length → t.time = 0 → t.c = 2 →
      (wexec P t es).time = 0 → 2 ≤ (wexec P t es).q.length := by
    intro es
    induction es with
    | nil => intro t _ _ hl _ _ _; exact hl
    | cons e es ih =>
      intro t hno hc hl ht hcc h
      have hno' : ∀ i, Event.remove i ∉ es := fun i hm => hno i (List.mem_cons_of_mem _ hm)
      simp only [wexec] at h ⊢
      cases e with
      | step =>
        have h1 := wstep_time P t .step
        have h2 := (wexec_ran_le_time P es (wstep P t .step)).1
        simp [hcc] at h1
        omega
      | tick =>
        have : wstep P t .tick = t := by simp [wstep, hcc]
        rw [this] at h ⊢
        exact ih t hno' hc hl ht hcc h
      | yield =>
        have : wstep P t .yield = t := by simp [wstep, hc]
        rw [this] at h ⊢
        exact ih t hno' hc hl ht hcc h
      | block =>
        have : wstep P t .block = t := by simp [wstep, hc]
        rw [this] at h ⊢
        exact ih t hno' hc hl ht hcc h
      | wake w =>
        by_cases hw : w = 0
        · have : wstep P t (.wake w) = t := by simp [wstep, hw]
          rw [this] at h ⊢
          exact ih t hno' hc hl ht hcc h
        · refine ih _ hno' ?_ ?_ ?_ ?_ h
          · simpa [wstep, hw] using hc
          · simp [wstep, hw]; omega
          · simpa [wstep, hw] using ht
          · simpa [wstep, hw] using hcc
      | remove i => exact absurd (List.mem_cons_self) (hno i)
  intro h heq
  have := key es s hno rfl (by simp [s]) rfl rfl h
  rw [heq] at this
  simp [wstep, s] at this

/-- The hypothesis `Capped` of `share_bound` is needed. `N = 1`: the spinner is
alone, then a second task is woken. The run is not `Capped 1`, plain round robin
gives the spinner every other period, and the `N = 1` bound `time <= ran + c0`
fails after 10 steps (the spinner ran 6). -/
theorem cap_needed :
    let s0 : St := start0 0 [] 2 0
    let es : List Event := [.wake 1, .step, .step, .tick, .step, .step, .tick, .step, .step,
      .tick, .step, .step, .tick, .step, .step, .tick]
    Inv 2 s0 ∧ s0.time = 0 ∧ ¬ Capped 1 2 s0 es ∧
    ¬ ((wexec 2 s0 es).time ≤ 1 * (wexec 2 s0 es).ran + (1 - 1) * 2 + s0.c) := by
  refine ⟨⟨by decide, by decide⟩, rfl, ?_, by decide⟩
  intro h
  exact absurd (h 1) (by decide)

#print axioms share_bound_diff
#print axioms share_bound
#print axioms share_bound_always
#print axioms share_bound_reached
#print axioms wexec_widen
#print axioms capped_widen
#print axioms contains
#print axioms share_bound_base
#print axioms share_bound_diff_base
#print axioms no_early_cut
#print axioms tick_keeps
#print axioms wait_bound
#print axioms wait_bound_passed
#print axioms wait_bound_tick
#print axioms fate_started
#print axioms fate_removed
#print axioms wait_at_bound
#print axioms fate_instance
#print axioms wstep_time
#print axioms wstep_ran
#print axioms wstep_age
#print axioms wexec_ran_le_time
#print axioms base_mark_needs_step
#print axioms tick_after_yield
#print axioms tick_after_yield_not_base
#print axioms remove_not_block_wake
#print axioms cap_needed

end K0Share.Wide
