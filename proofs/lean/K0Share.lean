/-!
# K0Share: the share bound inside one domain

This file is a proof about a MODEL of one domain's own timeline. It is not a
proof about the Rust code (`k0-slice`, `k0-part`). Nothing here checks that the
Rust code implements the model; the host tests and review do that job.

## What is modelled

* One domain, seen on its own timeline. Time advances in whole steps. A tick
  falls every `P` steps (`P >= 1`, a parameter). `c` is the number of steps left
  until the next tick, `1 <= c <= P`. At time 0, `c = P - start` where `start`
  is the offset of time 0 inside a period.
* Tasks are in a first-in first-out ready queue `q`. `cur` is the running task.
  Task `0` is the spinner: it never yields and never blocks, nobody stops or
  deletes it, and it is in the system (running or ready) from the start state
  on. Every other task is controlled by the adversary and is anonymous (only
  its being "not the spinner" matters).
* `ex` is the "between ticks" mark of `k0_slice::Slice` (`exempt`). `onTick` is
  the same-domain branch of `k0_part::Core::tick` (the verdict of
  `k0_slice::Slice::on_tick`, then the rotation): with the mark the tick only
  clears it, without the mark the running task goes to the back and the head
  enters by the tick's decision (no mark). If the queue is empty the running
  task stays.
* Events (the adversary's choices, any finite list of them is a behaviour):
  `run` (one step passes; the tick falls when `c` reaches 0, inside the same
  event), `yield`, `block`, `wake t`. `yield`, `block` and `wake` take no time.
  After `yield` or `block` the head runs with the mark.
* `wake t` puts a task at the back of the ready queue when fewer than `N` tasks
  are ready or running. A blocked task is not counted, so `N` bounds the tasks
  that are ready or running at one instant, not the tasks that exist. A wake
  above `N` is dropped. This is an assumption about the domain (it never has
  more than `N` runnable tasks), not a fact the model derives. The woken task
  is anonymous, so a task that is already ready may be "woken" again; this is
  only a bigger adversary than the real one (it can still never have more than
  `N` tasks).

## Adversary strength

Any finite list of events is a behaviour, so the adversary may act adaptively
and at zero cost in time: any number of `yield`, `block` and `wake` between two
steps, also one step before a tick and right after it. The initial state is any
well-formed state (`Wf`): any queue order, any mark `ex`, any countdown
`1 <= c <= P`, any `age` that is consistent with `ex`.

The adversary of this file has three limits that `K0ShareWide.lean` removes.
That file defines a wider model, proves the share bound, "no early cut" and a
wait bound for it, and shows that every behaviour of this file from a
well-formed state is a behaviour of the wider model (`Wide.contains`).

* No event falls between the last step of a period and its tick (`run` does
  both). In the kernel the handlers run with interrupts masked, so a timer
  interrupt that becomes pending during a yield or a block is taken as soon as
  the next task is entered, before that task has run. In `K0ShareWide.lean`
  the tick is its own event.
* A ready task is never taken out of the queue. In the kernel a ready task
  that is deleted leaves the queue from any position (`k0_ipc::teardown` calls
  `k0_sched::remove_ready`). In `K0ShareWide.lean` that is the event `remove`.
* The cap `N` is a rule of `wake`: a wake above `N` is dropped without a
  trace. In `K0ShareWide.lean` no wake is dropped, and the cap is a hypothesis
  on the behaviour (`Wide.Capped`).

These limits remain, here and in `K0ShareWide.lean`:

* The spinner is in the system from the start state on and never arrives later
  (`wake 0` changes nothing). For a spinner that becomes ready after the
  start, use `share_bound_diff` from the state in which it is in the queue.
  `c0` is then the countdown at that instant.
* Idle is not modelled (see below).
* The bound is relative to `N`. That the domain never has more than `N` tasks
  ready or running is assumed, not derived.
* Time is counted in whole steps (see below).

`Core::start` begins with one task and an empty queue. `start0` has the mark
set and allows any queue, so `share_bound_prefix` covers `Core::start` when the
first task is the spinner. A start without the mark, and a spinner that joins
later, are covered by `share_bound` and `share_bound_diff`, which take any
well-formed state.

## What is NOT modelled

* Other domains and the partition table (that is `K0Part.lean`, which this file
  does not import). This is the timeline of one domain, windows are not cut.
* IPC, timeouts, capability operations, faults, task exit, console wake, as far
  as their effect on the ready queue is a mix of `block` and `wake`, which the
  adversary may do at any instant anyway. One effect is not such a mix: the
  removal of a ready task. This file has no event for it; `K0ShareWide.lean`
  has.
* Idle. The spinner is always ready or running, so the domain is never empty.
  A task that gets the CPU out of idle (`Core::poll`, or `Core::tick` with
  nothing running) is therefore outside `no_early_cut`.
* Timing noise of the real kernel (the switch path takes time, the counter is
  read late, a late tick that skips numbers is one event in `Core::tick`).
  Steps are exact here and every period has its tick.
* Cache and predictor channels. The bound is on CPU shares only. A task still
  chooses when the others run by the instant at which it yields or blocks, so
  the timing channel through the scheduler inside the domain stays.

## Results

* `share_bound` (headline): for every `N >= 1`, `P >= 1`, every behaviour `es` and
  every time `t` (a prefix of `es` is a time),
  `t <= N * ran(t) + (N-1)*P + c0`, with `c0 = P - start`.
  Two `example`s at the end reach the constant (`N = 3, P = 2` and
  `N = 4, P = 3`, with `ran = 0`). There is no theorem of tightness for all
  `N` and `P`; a search outside this repository suggested it, but that is not
  part of what Lean checks here.
* `no_early_cut`: a task that got the CPU is never cut by a tick before it has
  run `P` steps.
* `wait_bound`: a ready task with `k` tasks to run before it (the running one
  included) has become the running task within `(k+1)*P` steps.
* `step_time`, `exec_time`, `step_ran`, `exec_ran_le_time`, `alone_time_eq_ran`,
  `adv_head_none`, `step_cur_keeps`, `step_age`, `age_counts`, `started_sound`:
  what the counters `time`, `ran`, `age` and the helper definitions `runs`,
  `started` stand for. Without them a change to one of these (a frozen `time`,
  a `ran` that counts twice) would leave every theorem above intact.
-/

set_option linter.unusedSimpArgs false

namespace K0Share

/-- Adversary events. `run` lets one step pass. -/
inductive Ev where
  | run
  | yield
  | block
  | wake (t : Nat)

/-- State of the domain. `age` counts the steps the running task has run since it
got the CPU. It is a ghost field: no rule reads it, and in a start state it is
any value that `Wf` allows. `time` and `ran` count steps in total and steps in
which the spinner (task 0) was the running task. What the three counters count
is fixed by `step_time`, `step_ran` and `step_age`. -/
structure St where
  cur : Nat
  ex : Bool
  q : List Nat
  c : Nat
  age : Nat
  time : Nat
  ran : Nat
  deriving Repr

/-- The tick rule of `k0_slice` inside the domain. `P` is the period; the
countdown restarts. -/
def onTick (P : Nat) (s : St) : St :=
  if s.ex then { s with ex := false, c := P }
  else match s.q with
    | [] => { s with c := P }
    | x :: r => { s with cur := x, q := r ++ [s.cur], c := P, age := 0 }

/-- One step of time passes. The tick falls after it when the countdown ends. -/
def runStep (P : Nat) (s : St) : St :=
  let s1 : St := { s with time := s.time + 1, age := s.age + 1,
                          ran := s.ran + (if s.cur = 0 then 1 else 0) }
  if s.c ≤ 1 then onTick P s1 else { s1 with c := s.c - 1 }

/-- Number of tasks in the system (running and ready). -/
def cnt (s : St) : Nat := s.q.length + 1

/-- One event. Events that the model excludes (the spinner yields or blocks, a
wake above `N` tasks, `wake 0`) change nothing. A yield or a block with an empty
queue changes nothing either; from a well-formed state that case cannot occur,
because a running task other than the spinner has the spinner behind it. -/
def step (N P : Nat) (s : St) : Ev → St
  | .run => runStep P s
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
  | .wake t => if t ≠ 0 ∧ cnt s < N then { s with q := s.q ++ [t] } else s

/-- Run a behaviour. -/
def exec (N P : Nat) (s : St) : List Ev → St
  | [] => s
  | e :: es => exec N P (step N P s e) es

/-- Well-formed states: countdown in `1..P`, exactly one spinner in the system,
at most `N` tasks, and the task without the mark has already used up
`P - c` steps of its slice (`age + c >= P`). The last clause, on `age`, is
there for `no_early_cut`. The share bound does not need it (see
`share_bound_diff`). -/
def Wf (N P : Nat) (s : St) : Prop :=
  1 ≤ s.c ∧ s.c ≤ P ∧ (s.cur :: s.q).count 0 = 1 ∧ (s.cur :: s.q).length ≤ N ∧
  (s.ex = false → P ≤ s.age + s.c)

/-- Number of tasks ahead of the first spinner in a queue. -/
def pos : List Nat → Nat
  | [] => 0
  | x :: xs => if x = 0 then 0 else pos xs + 1

/-- The potential. It bounds from above how much `time - N * ran` can still
grow. -/
def psi (N P : Nat) (s : St) : Int :=
  if s.cur = 0 then
    (if s.ex then 0 else ((N - 1 : Nat) : Int) * ((P : Int) - s.c))
  else (s.c : Int) + (if s.ex then (P : Int) else 0) + (pos s.q : Int) * P

/-- `time - N * ran` -/
def lag (N : Nat) (s : St) : Int := (s.time : Int) - (N : Int) * (s.ran : Int)

/-- A start in the manner of `Core::start`: the first task entered between ticks
and `start` steps of the period are gone. `Core::start` itself has an empty
queue; here the queue is any `q`. -/
def start0 (cur : Nat) (q : List Nat) (P start : Nat) : St :=
  { cur := cur, ex := true, q := q, c := P - start, age := 0, time := 0, ran := 0 }


/-! ## Facts about `pos` and the spinner count -/

theorem pos_lt {q : List Nat} (h : 0 ∈ q) : pos q < q.length := by
  induction q with
  | nil => simp at h
  | cons x xs ih =>
    by_cases hx : x = 0
    · simp [pos, hx]
    · have h0 : 0 ∈ xs := by
        simp only [List.mem_cons] at h
        rcases h with h | h
        · exact absurd h.symm hx
        · exact h
      have := ih h0
      simp [pos, hx]
      omega

theorem pos_append_of_mem (y : Nat) {q : List Nat} (h : 0 ∈ q) : pos (q ++ [y]) = pos q := by
  induction q with
  | nil => simp at h
  | cons x xs ih =>
    by_cases hx : x = 0
    · simp [pos, hx]
    · have h0 : 0 ∈ xs := by
        simp only [List.mem_cons] at h
        rcases h with h | h
        · exact absurd h.symm hx
        · exact h
      simp [pos, hx, ih h0]

theorem pos_append_self {r : List Nat} (h : 0 ∉ r) : pos (r ++ [0]) = r.length := by
  induction r with
  | nil => simp [pos]
  | cons x xs ih =>
    have hx : x ≠ 0 := fun e => h (by simp [e])
    have h' : 0 ∉ xs := fun e => h (by simp [e])
    simp [pos, hx, ih h']

theorem pos_cons_ne {x : Nat} (r : List Nat) (h : x ≠ 0) : pos (x :: r) = pos r + 1 := by
  simp [pos, h]

theorem pos_cons_zero (r : List Nat) : pos (0 :: r) = 0 := by
  simp [pos]

theorem mem_tail {x : Nat} {r : List Nat} (h : 0 ∈ x :: r) (hx : x ≠ 0) : 0 ∈ r := by
  simp only [List.mem_cons] at h
  rcases h with h | h
  · exact absurd h.symm hx
  · exact h

theorem nonneg_mul (a b : Nat) : (0 : Int) ≤ (a : Int) * (b : Int) := by
  exact_mod_cast Nat.zero_le (a * b)

/-! ## One step of the potential argument -/

theorem run_pot (N P : Nat) (s : St) (hw : Wf N P s) :
    Wf N P (step N P s .run) ∧
    lag N (step N P s .run) + psi N P (step N P s .run) ≤ lag N s + psi N P s := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  simp only at h1 h2 h3 h4 h5
  obtain ⟨M, rfl⟩ : ∃ M, N = M + 1 := ⟨N - 1, by simp at h4; omega⟩
  by_cases hc : cur = 0
  · subst hc
    simp at h3
    have hq : 0 ∉ q := List.count_eq_zero.mp h3
    have hlen : q.length ≤ M := by simpa using h4
    by_cases hc1 : c = 1
    · subst hc1
      cases ex with
      | true =>
        simp [step, runStep, onTick, Wf, lag, psi, Int.mul_add, Int.add_mul]
        omega
      | false =>
        cases q with
        | nil =>
          have hMP : (0:Int) ≤ (M : Int) * ((P : Int) - 1) :=
            Int.mul_nonneg (by omega) (by omega)
          rw [Int.mul_sub] at hMP
          simp [step, runStep, onTick, Wf, lag, psi, Int.mul_add, Int.add_mul, Int.mul_sub]
          omega
        | cons x r =>
          have hx : x ≠ 0 := fun e => hq (by simp [e])
          have hr0 : 0 ∉ r := fun e => hq (by simp [e])
          have hr3 : List.count 0 r = 0 := List.count_eq_zero.mpr hr0
          have hrl : r.length + 1 ≤ M := by simpa using hlen
          have hm : r.length * P + P ≤ M * P := by
            have := Nat.mul_le_mul_right P hrl
            rw [Nat.add_mul, Nat.one_mul] at this
            exact this
          have hm' : (r.length : Int) * P + P ≤ (M : Int) * P := by exact_mod_cast hm
          simp [step, runStep, onTick, Wf, lag, psi, hx, pos_append_self hr0,
            Int.mul_add, Int.add_mul, Int.mul_sub]
          omega
    · have hnle : ¬ c ≤ 1 := by omega
      cases ex with
      | true =>
        simp [step, runStep, onTick, Wf, lag, psi, hnle, Int.mul_add, Int.add_mul]
        omega
      | false =>
        have h5' := h5 rfl
        obtain ⟨c', rfl⟩ : ∃ c', c = c' + 2 := ⟨c - 2, by omega⟩
        simp [step, runStep, Wf, lag, psi, Int.mul_add, Int.add_mul, Int.mul_sub]
        omega
  · have hq : 0 ∈ q := by
      cases Decidable.em (0 ∈ q) with
      | inl hm => exact hm
      | inr hn =>
        have : List.count 0 q = 0 := List.count_eq_zero.mpr hn
        simp [List.count_cons, hc] at h3
        omega
    have hp := pos_lt hq
    have h4' : q.length ≤ M := by simp at h4; omega
    have hP0 := nonneg_mul (pos q) P
    by_cases hc1 : c = 1
    · subst hc1
      cases ex with
      | true =>
        simp [step, runStep, onTick, Wf, lag, psi, hc, Int.mul_add, Int.add_mul]
        simp [List.count_cons, hc] at h3
        omega
      | false =>
        cases q with
        | nil => simp at hq
        | cons x r =>
          by_cases hx : x = 0
          · subst hx
            simp [List.count_cons, hc] at h3
            have hr3 : List.count 0 r = 0 := h3
            have hr0 : 0 ∉ r := List.count_eq_zero.mp h3
            simp [step, runStep, onTick, Wf, lag, psi, hc, pos_cons_zero, Int.mul_add, Int.add_mul]
            simp at h4
            omega
          · have hr := mem_tail hq hx
            simp [List.count_cons, hc, hx] at h3
            have hr0 : (List.count 0 r) = 1 := by omega
            simp [step, runStep, onTick, Wf, lag, psi, hc, hx, pos_cons_ne r hx,
              pos_append_of_mem cur hr, List.count_cons, Int.mul_add, Int.add_mul]
            simp at h4
            omega
    · have hnle : ¬ c ≤ 1 := by omega
      obtain ⟨c', rfl⟩ : ∃ c', c = c' + 2 := ⟨c - 2, by omega⟩
      cases ex with
      | true =>
        simp [step, runStep, Wf, lag, psi, hc, Int.mul_add, Int.add_mul]
        simp [List.count_cons, hc] at h3
        omega
      | false =>
        have h5' := h5 rfl
        simp [step, runStep, Wf, lag, psi, hc, Int.mul_add, Int.add_mul]
        simp [List.count_cons, hc] at h3
        omega

theorem yield_pot (N P : Nat) (s : St) (hw : Wf N P s) :
    Wf N P (step N P s .yield) ∧
    lag N (step N P s .yield) + psi N P (step N P s .yield) ≤ lag N s + psi N P s := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  simp only at h1 h2 h3 h4 h5
  by_cases hc : cur = 0
  · subst hc
    simp [step, Wf, lag, psi]
    simp at h3 h4
    exact ⟨h1, h2, h3, h4, h5⟩
  · have hq : 0 ∈ q := by
      cases Decidable.em (0 ∈ q) with
      | inl hm => exact hm
      | inr hn =>
        have : List.count 0 q = 0 := List.count_eq_zero.mpr hn
        simp [List.count_cons, hc] at h3
        omega
    have hP0 := nonneg_mul (pos q) P
    cases q with
    | nil => simp at hq
    | cons x r =>
      by_cases hx : x = 0
      · subst hx
        simp [List.count_cons, hc] at h3
        cases ex <;>
        simp [step, Wf, lag, psi, hc, pos_cons_zero, List.count_cons] at * <;> omega
      · have hr := mem_tail hq hx
        simp [List.count_cons, hc, hx] at h3
        cases ex <;>
        simp [step, Wf, lag, psi, hc, hx, pos_cons_ne r hx, pos_append_of_mem cur hr,
          List.count_cons, Int.add_mul] at * <;> omega

theorem block_pot (N P : Nat) (s : St) (hw : Wf N P s) :
    Wf N P (step N P s .block) ∧
    lag N (step N P s .block) + psi N P (step N P s .block) ≤ lag N s + psi N P s := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  simp only at h1 h2 h3 h4 h5
  by_cases hc : cur = 0
  · subst hc
    simp [step, Wf, lag, psi]
    simp at h3 h4
    exact ⟨h1, h2, h3, h4, h5⟩
  · have hq : 0 ∈ q := by
      cases Decidable.em (0 ∈ q) with
      | inl hm => exact hm
      | inr hn =>
        have : List.count 0 q = 0 := List.count_eq_zero.mpr hn
        simp [List.count_cons, hc] at h3
        omega
    have hP0 := nonneg_mul (pos q) P
    cases q with
    | nil => simp at hq
    | cons x r =>
      by_cases hx : x = 0
      · subst hx
        simp [List.count_cons, hc] at h3
        cases ex <;>
        simp [step, Wf, lag, psi, hc, pos_cons_zero, List.count_cons] at * <;> omega
      · have hr := mem_tail hq hx
        simp [List.count_cons, hc, hx] at h3
        cases ex <;>
        simp [step, Wf, lag, psi, hc, hx, pos_cons_ne r hx, pos_append_of_mem cur hr,
          List.count_cons, Int.add_mul] at * <;> omega

theorem wake_pot (N P : Nat) (s : St) (t : Nat) (hw : Wf N P s) :
    Wf N P (step N P s (.wake t)) ∧
    lag N (step N P s (.wake t)) + psi N P (step N P s (.wake t)) ≤ lag N s + psi N P s := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  simp only at h1 h2 h3 h4 h5
  by_cases hg : t ≠ 0 ∧ cnt ⟨cur, ex, q, c, age, time, ran⟩ < N
  · have hlt : q.length + 1 < N := hg.2
    have ht : t ≠ 0 := hg.1
    have e : step N P ⟨cur, ex, q, c, age, time, ran⟩ (.wake t)
        = { cur := cur, ex := ex, q := q ++ [t], c := c, age := age, time := time, ran := ran } := by
      simp only [step]; split
      · rfl
      · contradiction
    rw [e]
    by_cases hc : cur = 0
    · subst hc
      simp [List.count_cons] at h3
      simp [cnt, Wf, lag, psi, List.count_cons, ht]
      simp at h4
      refine ⟨h1, h2, h3, by omega, h5⟩
    · have hq : 0 ∈ q := by
        cases Decidable.em (0 ∈ q) with
        | inl hm => exact hm
        | inr hn =>
          have : List.count 0 q = 0 := List.count_eq_zero.mpr hn
          simp [List.count_cons, hc] at h3
          omega
      simp [List.count_cons, hc] at h3
      simp [cnt, Wf, lag, psi, List.count_cons, ht, hc, pos_append_of_mem t hq]
      simp at h4
      refine ⟨h1, h2, ?_, by omega, h5⟩
      omega
  · simp [step, hg, Int.le_refl]
    exact ⟨h1, h2, h3, h4, h5⟩

theorem step_pot (N P : Nat) (s : St) (hw : Wf N P s) (e : Ev) :
    Wf N P (step N P s e) ∧
    lag N (step N P s e) + psi N P (step N P s e) ≤ lag N s + psi N P s := by
  cases e with
  | run => exact run_pot N P s hw
  | yield => exact yield_pot N P s hw
  | block => exact block_pot N P s hw
  | wake t => exact wake_pot N P s t hw

theorem exec_pot (N P : Nat) : ∀ (es : List Ev) (s : St), Wf N P s →
    Wf N P (exec N P s es) ∧
    lag N (exec N P s es) + psi N P (exec N P s es) ≤ lag N s + psi N P s := by
  intro es
  induction es with
  | nil => intro s hw; exact ⟨hw, Int.le_refl _⟩
  | cons e es ih =>
    intro s hw
    obtain ⟨hw1, hl1⟩ := step_pot N P s hw e
    obtain ⟨hw2, hl2⟩ := ih _ hw1
    exact ⟨hw2, by simp only [exec]; omega⟩

theorem psi_nonneg (N P : Nat) (s : St) (hw : Wf N P s) : 0 ≤ psi N P s := by
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  have hP0 := nonneg_mul (pos s.q) P
  by_cases hc : s.cur = 0
  · cases hex : s.ex with
    | true => simp [psi, hc, hex]
    | false =>
      simp only [psi, hc, hex]
      simp
      exact Int.mul_nonneg (Int.natCast_nonneg _) (by omega)
  · cases hex : s.ex <;> simp [psi, hc, hex] <;> omega

theorem psi_le (N P : Nat) (s : St) (hw : Wf N P s) :
    psi N P s ≤ ((N - 1 : Nat) : Int) * P + s.c := by
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  obtain ⟨M, rfl⟩ : ∃ M, N = M + 1 := ⟨N - 1, by simp at h4; omega⟩
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
  · have hq : 0 ∈ s.q := by
      cases Decidable.em (0 ∈ s.q) with
      | inl hm => exact hm
      | inr hn =>
        have : List.count 0 s.q = 0 := List.count_eq_zero.mpr hn
        simp [List.count_cons, hc] at h3
        omega
    have hp := pos_lt hq
    simp at h4
    have hm : (pos s.q + 1) * P ≤ M * P := Nat.mul_le_mul_right P (by omega)
    rw [Nat.add_mul, Nat.one_mul] at hm
    have hm' : (pos s.q : Int) * P + P ≤ (M : Int) * P := by exact_mod_cast hm
    cases hex : s.ex <;> simp [psi, hc, hex] <;> omega

/-- HEADLINE, difference form. Starting from any well-formed state `s0` and
running any behaviour `es`, the steps that passed minus `N` times the steps in
which the spinner ran grew by at most `(N-1)*P + c0`, where `c0 = s0.c` is the
number of steps until the first tick.

Of `Wf` this uses the countdown in `1..P`, the one spinner and the `N` tasks.
The `age` clause is not needed: no rule reads `age`, and
`Wide.share_bound_diff_base` in `K0ShareWide.lean` is this statement without
it. The statement is kept as it is so that one invariant serves all three
results. -/
theorem share_bound_diff (N P : Nat) (s0 : St) (hw : Wf N P s0) (es : List Ev) :
    (((exec N P s0 es).time : Int) - s0.time) ≤
      (N : Int) * (((exec N P s0 es).ran : Int) - s0.ran) + ((N - 1 : Nat) : Int) * P + s0.c := by
  obtain ⟨hw1, hl⟩ := exec_pot N P es s0 hw
  have h0 := psi_nonneg N P _ hw1
  have h1 := psi_le N P s0 hw
  simp only [lag] at hl
  rw [Int.mul_sub]
  omega

/-- HEADLINE. A system that starts at time 0 (`time = 0`, `ran = 0`) with the
countdown `c0 = P - start` to its first tick, `start < P` the offset of time 0
inside a period. For every `N`, `P`, every adversary behaviour `es`: the number
of steps `t` that have passed is at most `N` times the number of steps in which
the spinner ran, plus `(N-1)*P + (P - start)`. Every prefix of a behaviour is a
behaviour, so this holds at every time `t`. In words: the spinner keeps at
least `1/N` of the CPU up to an additive constant of `N-1` ticks plus the rest
of the first period.

`N` is the cap that `wake` enforces, so the bound is relative to that
assumption. The hypothesis `hr` (`ran = 0` at the start) is not needed: a
larger `ran` at the start only makes the right side larger
(`Wide.share_bound_base` in `K0ShareWide.lean` is this statement without `hr`
and without the `age` clause of `Wf`). It is kept so that the statement reads
as "a system that starts at time 0". -/
theorem share_bound (N P : Nat) (s0 : St) (hw : Wf N P s0)
    (ht : s0.time = 0) (hr : s0.ran = 0) (es : List Ev) :
    (exec N P s0 es).time ≤ N * (exec N P s0 es).ran + (N - 1) * P + s0.c := by
  have := share_bound_diff N P s0 hw es
  rw [ht, hr] at this
  refine Int.ofNat_le.mp ?_
  simp only [Int.natCast_add, Int.natCast_mul]
  simp at this
  omega

/-- A system that starts as `Core::start` does: the first task `cur` entered
between ticks (mark set), `start < P` steps of the period are already gone. It is
well-formed when exactly one task of the system is the spinner and there are at
most `N` tasks. -/
theorem wf_start0 (N P start cur : Nat) (q : List Nat) (hs : start < P)
    (h1 : (cur :: q).count 0 = 1) (h2 : (cur :: q).length ≤ N) :
    Wf N P (start0 cur q P start) := by
  refine ⟨?_, ?_, h1, h2, ?_⟩ <;> simp [start0] <;> omega

/-- The headline at the first `k` events of any behaviour (every time `t` is
reached by a prefix of some behaviour). This is `share_bound` at the list
`es.take k`, stated for `start0`. The prefix length `k` adds nothing: `es`
ranges over all lists, and `es.take k` is one of them, so the statement without
`take` (take `k = es.length`) says the same. -/
theorem share_bound_prefix (N P start cur : Nat) (q : List Nat) (hs : start < P)
    (h1 : (cur :: q).count 0 = 1) (h2 : (cur :: q).length ≤ N) (es : List Ev) (k : Nat) :
    (exec N P (start0 cur q P start) (es.take k)).time ≤
      N * (exec N P (start0 cur q P start) (es.take k)).ran + (N - 1) * P + (P - start) := by
  have := share_bound N P _ (wf_start0 N P start cur q hs h1 h2) rfl rfl (es.take k)
  simpa [start0] using this

/-! ## Lemma 2: a task that got the CPU is not cut before `P` steps -/

/-- A `run` step cuts the running task exactly when the running task's `age`
(steps since it got the CPU) is reset to 0 by it (`step_age`). When that
happens, the task has run at least `P` steps since it got the CPU, the step just
taken included. For one state this is the `age` clause of `Wf` read at the end
of the countdown; the content is that every behaviour keeps `Wf`
(`exec_no_early_cut`) and that `age` counts what its name says (`age_counts`). -/
theorem no_early_cut (N P : Nat) (s : St) (hw : Wf N P s)
    (h : (step N P s .run).age = 0) : P ≤ s.age + 1 := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  simp only at h1 h2 h3 h4 h5
  by_cases hc : c ≤ 1
  · cases ex with
    | true => simp [step, runStep, onTick, hc] at h
    | false =>
      have := h5 rfl
      show P ≤ age + 1
      omega
  · simp [step, runStep, hc] at h

/-- The same along any behaviour: whenever the next `run` step cuts the running
task, that task has run at least `P` steps since it got the CPU. -/
theorem exec_no_early_cut (N P : Nat) (s0 : St) (hw : Wf N P s0) (pre : List Ev)
    (h : (step N P (exec N P s0 pre) .run).age = 0) :
    P ≤ (exec N P s0 pre).age + 1 :=
  no_early_cut N P _ (exec_pot N P pre s0 hw).1 h

/-! ## Lemma 3: a ready task starts within `(k+1)*P` steps

A ready task is a slot `i` of the queue (`i` tasks ahead of it; with the running
task, `k = i + 1` tasks run before it). `adv s i e` follows the slot through the
event `e`: `none` when the task in the slot becomes the running task, `some j`
when it is now in slot `j`. `adv_track` and `adv_none` show that this really
follows the same task. -/

/-- Where the task in queue slot `i` is after event `e`; `none` if it started. -/
def adv (s : St) (i : Nat) : Ev → Option Nat
  | .run =>
    if s.c ≤ 1 ∧ s.ex = false ∧ s.q ≠ [] then
      (match i with | 0 => none | j + 1 => some j)
    else some i
  | .yield =>
    if s.cur ≠ 0 ∧ s.q ≠ [] then (match i with | 0 => none | j + 1 => some j) else some i
  | .block =>
    if s.cur ≠ 0 ∧ s.q ≠ [] then (match i with | 0 => none | j + 1 => some j) else some i
  | .wake _ => some i

/-- The slot `i` of `s` started to run at some event of the behaviour. This is
"became the running task at some event", not "is running at the end": the task
may be cut again later. `started_sound` says what it gives. -/
def started (N P : Nat) : St → Nat → List Ev → Prop
  | _, _, [] => False
  | s, i, e :: es =>
    match adv s i e with
    | none => True
    | some j => started N P (step N P s e) j es

/-- Number of `run` events, that is, steps of time (`exec_time`). -/
def runs : List Ev → Nat
  | [] => 0
  | .run :: es => runs es + 1
  | _ :: es => runs es

/-- Steps still to wait for slot `i`, as a bound. -/
def wt (P : Nat) (s : St) (i : Nat) : Nat := s.c + (if s.ex then P else 0) + i * P

theorem run_q_keep (P : Nat) (s : St) (h : ¬ (s.c ≤ 1 ∧ s.ex = false ∧ s.q ≠ [])) :
    (runStep P s).q = s.q := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  simp only at h
  by_cases hc : c ≤ 1
  · cases ex with
    | true => simp [runStep, onTick, hc]
    | false =>
      cases q with
      | nil => simp [runStep, onTick, hc]
      | cons x r => exact absurd ⟨hc, rfl, by simp⟩ h
  · simp [runStep, hc]

theorem run_q_cut (P : Nat) (s : St) (h : s.c ≤ 1 ∧ s.ex = false ∧ s.q ≠ []) :
    ∃ x r, s.q = x :: r ∧ (runStep P s).q = r ++ [s.cur] := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨hc, hex, hq⟩ := h
  simp only at hc hex hq
  subst hex
  cases q with
  | nil => exact absurd rfl hq
  | cons x r => exact ⟨x, r, rfl, by simp [runStep, onTick, hc]⟩

/-- The slot is followed correctly: the same task, in its new slot. -/
theorem adv_track (N P : Nat) (s : St) (i j : Nat) (e : Ev) (h : adv s i e = some j)
    (hi : i < s.q.length) :
    j < (step N P s e).q.length ∧ (step N P s e).q[j]? = s.q[i]? := by
  cases e with
  | run =>
    simp only [adv] at h
    split at h
    · rename_i hcut
      obtain ⟨x, r, hq, hq'⟩ := run_q_cut P s hcut
      cases i with
      | zero => simp at h
      | succ i =>
        simp at h
        subst h
        simp only [step]
        rw [hq']
        rw [hq] at hi ⊢
        simp at hi ⊢
        refine ⟨by omega, ?_⟩
        rw [List.getElem?_append_left (by omega)]
    · simp at h
      subst h
      simp only [step]
      rw [run_q_keep P s ‹_›]
      exact ⟨hi, rfl⟩
  | yield =>
    simp only [adv] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
      simp only at hc hq hi
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp at h
        | succ i =>
          simp at h
          subst h
          simp [step, hc] at hi ⊢
          refine ⟨by omega, ?_⟩
          rw [List.getElem?_append_left (by omega)]
    · rename_i hn
      simp at h
      subst h
      have : step N P s .yield = s := by
        obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
        simp only at hn
        by_cases hc : cur = 0
        · simp [step, hc]
        · cases q with
          | nil => simp [step]
          | cons x r => exact absurd ⟨hc, by simp⟩ hn
      rw [this]
      exact ⟨hi, rfl⟩
  | block =>
    simp only [adv] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
      simp only at hc hq hi
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp at h
        | succ i =>
          simp at h
          subst h
          simp [step, hc] at hi ⊢
          omega
    · rename_i hn
      simp at h
      subst h
      have : step N P s .block = s := by
        obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
        simp only at hn
        by_cases hc : cur = 0
        · simp [step, hc]
        · cases q with
          | nil => simp [step]
          | cons x r => exact absurd ⟨hc, by simp⟩ hn
      rw [this]
      exact ⟨hi, rfl⟩
  | wake t =>
    simp only [adv] at h
    simp at h
    subst h
    simp only [step]
    split
    · simp
      refine ⟨by omega, ?_⟩
      rw [List.getElem?_append_left hi]
    · exact ⟨hi, rfl⟩

/-- The slot started: it was the head of the queue and it is now the running task. -/
theorem adv_none (N P : Nat) (s : St) (i : Nat) (e : Ev) (h : adv s i e = none) :
    i = 0 ∧ ∃ x r, s.q = x :: r ∧ (step N P s e).cur = x := by
  cases e with
  | run =>
    simp only [adv] at h
    split at h
    · rename_i hcut
      obtain ⟨x, r, hq, _⟩ := run_q_cut P s hcut
      cases i with
      | zero =>
        refine ⟨rfl, x, r, hq, ?_⟩
        obtain ⟨hc, hex, _⟩ := hcut
        obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
        simp only at hq hc hex
        subst hq; subst hex
        simp [step, runStep, onTick, hc]
      | succ i => simp at h
    · simp at h
  | yield =>
    simp only [adv] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
      simp only at hc hq
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => exact ⟨rfl, x, r, rfl, by simp [step, hc]⟩
        | succ i => simp at h
    · simp at h
  | block =>
    simp only [adv] at h
    split at h
    · rename_i hcond
      obtain ⟨hc, hq⟩ := hcond
      obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
      simp only at hc hq
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => exact ⟨rfl, x, r, rfl, by simp [step, hc]⟩
        | succ i => simp at h
    · simp at h
  | wake t => simp [adv] at h

/-- Each event either starts the slot or moves it forward, and the bound `wt`
falls by one for each step of time. -/
theorem wt_step (N P : Nat) (s : St) (hw : Wf N P s) (i j : Nat) (e : Ev)
    (h : adv s i e = some j) (hi : i < s.q.length) :
    wt P (step N P s e) j + (match e with | .run => 1 | _ => 0) ≤ wt P s i := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  obtain ⟨h1, h2, h3, h4, h5⟩ := hw
  simp only at h1 h2 h3 h4 h5 hi
  cases e with
  | run =>
    by_cases hcut : c ≤ 1 ∧ ex = false ∧ q ≠ []
    · obtain ⟨hc, hex, hq⟩ := hcut
      subst hex
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [adv, hc] at h
        | succ i =>
          have hj : j = i := by simp [adv, hc] at h; omega
          subst hj
          have hc1 : c = 1 := by omega
          subst hc1
          simp [step, runStep, onTick, wt, Nat.add_mul]
          omega
    · have hj : j = i := by
        simp [adv, hcut] at h
        omega
      subst hj
      by_cases hc : c ≤ 1
      · cases ex with
        | true =>
          have hc1 : c = 1 := by omega
          subst hc1
          simp [step, runStep, onTick, wt, Nat.add_mul]
          omega
        | false =>
          exfalso
          apply hcut
          refine ⟨hc, rfl, ?_⟩
          intro hq
          subst hq
          simp at hi
      · obtain ⟨c', rfl⟩ : ∃ c', c = c' + 2 := ⟨c - 2, by omega⟩
        cases ex <;> simp [step, runStep, wt] <;> omega
  | yield =>
    by_cases hcond : cur ≠ 0 ∧ q ≠ []
    · obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [adv, hc] at h
        | succ i =>
          have hj : j = i := by simp [adv, hc] at h; omega
          subst hj
          cases ex <;> simp [step, hc, wt, Nat.add_mul] <;> omega
    · have hj : j = i := by
        simp [adv, hcond] at h
        omega
      subst hj
      have : step N P ⟨cur, ex, q, c, age, time, ran⟩ .yield = ⟨cur, ex, q, c, age, time, ran⟩ := by
        by_cases hc : cur = 0
        · simp [step, hc]
        · cases q with
          | nil => simp [step]
          | cons x r => exact absurd ⟨hc, by simp⟩ hcond
      rw [this]
      simp
  | block =>
    by_cases hcond : cur ≠ 0 ∧ q ≠ []
    · obtain ⟨hc, hq⟩ := hcond
      cases q with
      | nil => exact absurd rfl hq
      | cons x r =>
        cases i with
        | zero => simp [adv, hc] at h
        | succ i =>
          have hj : j = i := by simp [adv, hc] at h; omega
          subst hj
          cases ex <;> simp [step, hc, wt, Nat.add_mul] <;> omega
    · have hj : j = i := by
        simp [adv, hcond] at h
        omega
      subst hj
      have : step N P ⟨cur, ex, q, c, age, time, ran⟩ .block = ⟨cur, ex, q, c, age, time, ran⟩ := by
        by_cases hc : cur = 0
        · simp [step, hc]
        · cases q with
          | nil => simp [step]
          | cons x r => exact absurd ⟨hc, by simp⟩ hcond
      rw [this]
      simp
  | wake t =>
    have hj : j = i := by simp [adv] at h; omega
    subst hj
    simp only [step]
    split <;> simp [wt]

/-- The bound `wt` counts steps: if the behaviour has at least `wt P s i` steps
of time then the task in slot `i` has started. -/
theorem wait_wt (N P : Nat) : ∀ (es : List Ev) (s : St) (i : Nat), Wf N P s →
    i < s.q.length → wt P s i ≤ runs es → started N P s i es := by
  intro es
  induction es with
  | nil =>
    intro s i hw _ hb
    obtain ⟨h1, _⟩ := hw
    simp [wt, runs] at hb
    omega
  | cons e es ih =>
    intro s i hw hi hb
    simp only [started]
    cases hadv : adv s i e with
    | none => trivial
    | some j =>
      simp only
      obtain ⟨hj, _⟩ := adv_track N P s i j e hadv hi
      have hwt := wt_step N P s hw i j e hadv hi
      have hw1 := (step_pot N P s hw e).1
      apply ih _ _ hw1 hj
      cases e <;> simp [runs] at hb hwt ⊢ <;> omega

/-- LEMMA 3. A ready task with `k` tasks to run before it (the running one
included), that is in queue slot `k - 1`, starts to run within `(k+1)*P` steps of
time, whatever the other tasks do. -/
theorem wait_bound (N P : Nat) (s : St) (hw : Wf N P s) (k : Nat) (hk : 1 ≤ k)
    (hq : k - 1 < s.q.length) (es : List Ev) (hes : (k + 1) * P ≤ runs es) :
    started N P s (k - 1) es := by
  apply wait_wt N P es s (k - 1) hw hq
  obtain ⟨h1, h2, _⟩ := hw
  have : wt P s (k - 1) ≤ (k + 1) * P := by
    simp only [wt]
    have : (k + 1) * P = (k - 1) * P + 2 * P := by
      obtain ⟨m, rfl⟩ : ∃ m, k = m + 1 := ⟨k - 1, by omega⟩
      simp [Nat.add_mul]
      omega
    rw [this]
    cases s.ex <;> simp <;> omega
  omega

/-! ## What the counters and the helper definitions stand for

`time`, `ran` and `age` are fields that the rules write and never read, and
`runs` and `started` occur only in statements. A change to one of them can
leave every theorem above intact: with `time` frozen the share bound is
trivially true. The statements below tie each of them down from outside its own
definition. -/

/-- `time` grows by one on a `run` and on no other event. -/
theorem step_time (N P : Nat) (s : St) (e : Ev) :
    (step N P s e).time = s.time + (match e with | .run => 1 | _ => 0) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · cases ex <;> cases q <;> simp [step, runStep, onTick, hc]
    · simp [step, runStep, hc]
  | yield =>
    by_cases h0 : cur = 0
    · simp [step, h0]
    · cases q <;> simp [step, h0]
  | block =>
    by_cases h0 : cur = 0
    · simp [step, h0]
    · cases q <;> simp [step, h0]
  | wake t =>
    simp only [step]
    split <;> simp

/-- The steps that pass in a behaviour are its `run` events: `time` at the end
is `time` at the start plus `runs es`. This ties `time` and `runs` to each
other, so `wait_bound` counts the same steps as `share_bound`. -/
theorem exec_time (N P : Nat) : ∀ (es : List Ev) (s : St),
    (exec N P s es).time = s.time + runs es := by
  intro es
  induction es with
  | nil => intro s; rfl
  | cons e es ih =>
    intro s
    have h := step_time N P s e
    simp only [exec, ih]
    cases e <;> simp [runs] at h ⊢ <;> omega

/-- `ran` grows by exactly one on a `run` that is taken while the spinner is the
running task, and on nothing else. The step is credited to the task that ran
it, also when the tick at its end hands the CPU over. -/
theorem step_ran (N P : Nat) (s : St) (e : Ev) :
    (step N P s e).ran =
      s.ran + (match e with | .run => if s.cur = 0 then 1 else 0 | _ => 0) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · cases ex <;> cases q <;> simp [step, runStep, onTick, hc]
    · simp [step, runStep, hc]
  | yield =>
    by_cases h0 : cur = 0
    · simp [step, h0]
    · cases q <;> simp [step, h0]
  | block =>
    by_cases h0 : cur = 0
    · simp [step, h0]
    · cases q <;> simp [step, h0]
  | wake t =>
    simp only [step]
    split <;> simp

/-- Along any behaviour `ran` does not fall and grows by no more than `time`:
the spinner is not credited more steps than pass. -/
theorem exec_ran_le_time (N P : Nat) : ∀ (es : List Ev) (s : St),
    s.ran ≤ (exec N P s es).ran ∧
    (exec N P s es).ran + s.time ≤ (exec N P s es).time + s.ran := by
  intro es
  induction es with
  | nil => intro s; exact ⟨Nat.le_refl _, by simp only [exec]; omega⟩
  | cons e es ih =>
    intro s
    obtain ⟨h1, h2⟩ := ih (step N P s e)
    have ht := step_time N P s e
    have hr := step_ran N P s e
    simp only [exec]
    cases e with
    | run =>
      simp only at ht hr
      by_cases h0 : s.cur = 0
      · rw [ite_eq_left h0] at hr; omega
      · rw [ite_eq_right h0] at hr; omega
    | yield => simp only at ht hr; omega
    | block => simp only at ht hr; omega
    | wake t => simp only at ht hr; omega

/-- With `N = 1` the spinner is alone and gets every step: `time` and `ran` grow
by the same amount along every behaviour. (So for `N = 1` the headline holds
with `c0` to spare.) -/
theorem alone_time_eq_ran (P : Nat) : ∀ (es : List Ev) (s : St), Wf 1 P s →
    (exec 1 P s es).time + s.ran = (exec 1 P s es).ran + s.time := by
  intro es
  induction es with
  | nil => intro s _; simp only [exec]; omega
  | cons e es ih =>
    intro s hw
    have ih' := ih _ (step_pot 1 P s hw e).1
    obtain ⟨_, _, h3, h4, _⟩ := hw
    have hq : s.q = [] := by
      cases hs : s.q with
      | nil => rfl
      | cons x r => simp [hs] at h4
    have hc : s.cur = 0 := by
      cases Decidable.em (s.cur = 0) with
      | inl h => exact h
      | inr h => simp [hq, h] at h3
    have ht := step_time 1 P s e
    have hr := step_ran 1 P s e
    simp only [exec]
    cases e with
    | run => simp only [hc, ite_true] at ht hr; omega
    | yield => simp only at ht hr; omega
    | block => simp only at ht hr; omega
    | wake t => simp only at ht hr; omega

/-- Slot 0 either starts or stays slot 0. -/
theorem adv_head (s : St) (e : Ev) (j : Nat) (h : adv s 0 e = some j) : j = 0 := by
  cases e with
  | run => simp only [adv] at h; split at h <;> simp at h <;> omega
  | yield => simp only [adv] at h; split at h <;> simp at h <;> omega
  | block => simp only [adv] at h; split at h <;> simp at h <;> omega
  | wake t => simp [adv] at h; omega

/-- The events at which the head of the queue starts, spelled out: a `run` at
the end of the countdown with no mark and somebody waiting (a cut), or a
`yield` or a `block` of a task other than the spinner with somebody waiting. -/
theorem adv_head_none (s : St) (e : Ev) :
    adv s 0 e = none ↔
      match e with
      | .run => s.c ≤ 1 ∧ s.ex = false ∧ s.q ≠ []
      | .yield => s.cur ≠ 0 ∧ s.q ≠ []
      | .block => s.cur ≠ 0 ∧ s.q ≠ []
      | .wake _ => False := by
  cases e with
  | run => simp only [adv]; split <;> simp_all
  | yield => simp only [adv]; split <;> simp_all
  | block => simp only [adv]; split <;> simp_all
  | wake t => simp [adv]

/-- The CPU changes hands only at the events at which the head of the queue
starts (`adv s 0 e = none`, spelled out in `adv_head_none`; by `adv_none` the
head is then the running task). At every other event the running task is the
same as before. -/
theorem step_cur_keeps (N P : Nat) (s : St) (e : Ev) (h : adv s 0 e ≠ none) :
    (step N P s e).cur = s.cur := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · cases ex with
      | true => simp [step, runStep, onTick, hc]
      | false =>
        cases q with
        | nil => simp [step, runStep, onTick, hc]
        | cons x r => simp [adv, hc] at h
    · simp [step, runStep, hc]
  | yield =>
    by_cases h0 : cur = 0
    · simp [step, h0]
    · cases q with
      | nil => simp [step, h0]
      | cons x r => simp [adv, h0] at h
  | block =>
    by_cases h0 : cur = 0
    · simp [step, h0]
    · cases q with
      | nil => simp [step, h0]
      | cons x r => simp [adv, h0] at h
  | wake t =>
    simp only [step]
    split <;> rfl

/-- `age` is 0 right after the CPU changed hands (by a `yield`, a `block` or a
cut; `adv_head_none`), it grows by one on every other `run`, and no other event
changes it. -/
theorem step_age (N P : Nat) (s : St) (e : Ev) :
    (step N P s e).age =
      if adv s 0 e = none then 0 else s.age + (match e with | .run => 1 | _ => 0) := by
  obtain ⟨cur, ex, q, c, age, time, ran⟩ := s
  cases e with
  | run =>
    by_cases hc : c ≤ 1
    · cases ex <;> cases q <;> simp [step, runStep, onTick, adv, hc]
    · simp [step, runStep, adv, hc]
  | yield =>
    by_cases h0 : cur = 0
    · simp [step, adv, h0]
    · cases q <;> simp [step, adv, h0]
  | block =>
    by_cases h0 : cur = 0
    · simp [step, adv, h0]
    · cases q <;> simp [step, adv, h0]
  | wake t =>
    simp only [step, adv]
    split <;> simp

/-- `age` counts steps: along a behaviour in which the CPU does not change hands
(the head of the queue never starts), `age` grows by the number of `run`
events. With `step_age` this makes `age` the number of steps since the running
task got the CPU, for a task that got it during the behaviour. For a start
state `age` is whatever `Wf` allows. -/
theorem age_counts (N P : Nat) : ∀ (es : List Ev) (s : St), ¬ started N P s 0 es →
    (exec N P s es).age = s.age + runs es := by
  intro es
  induction es with
  | nil => intro s _; rfl
  | cons e es ih =>
    intro s h
    simp only [started] at h
    have ha := step_age N P s e
    cases hadv : adv s 0 e with
    | none => rw [hadv] at h; exact absurd trivial h
    | some j =>
      rw [hadv] at h
      simp only at h
      have hj := adv_head s e j hadv
      subst hj
      rw [hadv] at ha
      simp only [exec, ih _ h]
      cases e <;> simp [runs] at ha ⊢ <;> omega

/-- `started` means what its name says: if the task in queue slot `i` started
during `es`, then `es` splits as `pre ++ e :: post` such that the task is at the
head of the queue after `pre` and is the running task after `e`. In particular
the empty behaviour starts nothing. (Tasks are anonymous, so "the task" is the
content of the slot.) -/
theorem started_sound (N P : Nat) : ∀ (es : List Ev) (s : St) (i : Nat), i < s.q.length →
    started N P s i es →
    ∃ pre e post, es = pre ++ e :: post ∧ (exec N P s pre).q.head? = s.q[i]? ∧
      some (exec N P s (pre ++ [e])).cur = s.q[i]? := by
  intro es
  induction es with
  | nil => intro s i _ h; simp [started] at h
  | cons e es ih =>
    intro s i hi h
    simp only [started] at h
    cases hadv : adv s i e with
    | none =>
      obtain ⟨hi0, x, r, hq, hcur⟩ := adv_none N P s i e hadv
      subst hi0
      refine ⟨[], e, es, rfl, ?_, ?_⟩
      · simp [exec, hq]
      · simp [exec, hq, hcur]
    | some j =>
      rw [hadv] at h
      simp only at h
      obtain ⟨hj, hq1⟩ := adv_track N P s i j e hadv hi
      obtain ⟨pre, e', post, hes, h1, h2⟩ := ih _ j hj h
      refine ⟨e :: pre, e', post, by rw [hes]; rfl, ?_, ?_⟩
      · rw [← hq1]; exact h1
      · rw [← hq1]; exact h2

/-! ## Tightness: the constant of `share_bound` is reached

In the two instances below the first steps reach `t = N * ran + (N-1)*P + c0`
with `ran = 0`: the first task has the mark, the spinner is at the back, every
other task is busy. `decide` evaluates the model. -/

/-- `N = 3`, `P = 2`, `c0 = 2`: after `(N-1)*P + c0 = 6` steps the spinner has not
run once. -/
example : (exec 3 2 { cur := 1, ex := true, q := [1, 0], c := 2, age := 0, time := 0, ran := 0 }
    [.run, .run, .run, .run, .run, .run]).ran = 0 ∧
  (exec 3 2 { cur := 1, ex := true, q := [1, 0], c := 2, age := 0, time := 0, ran := 0 }
    [.run, .run, .run, .run, .run, .run]).time = 6 := by decide

/-- `N = 4`, `P = 3`, `c0 = 1`: `3*3 + 1 = 10` steps with no spinner step. -/
example : (exec 4 3 { cur := 1, ex := true, q := [1, 1, 0], c := 1, age := 0, time := 0, ran := 0 }
    (List.replicate 10 .run)).ran = 0 ∧
  (exec 4 3 { cur := 1, ex := true, q := [1, 1, 0], c := 1, age := 0, time := 0, ran := 0 }
    (List.replicate 10 .run)).time = 10 := by decide

#print axioms share_bound_diff
#print axioms share_bound
#print axioms share_bound_prefix
#print axioms no_early_cut
#print axioms exec_no_early_cut
#print axioms wait_bound
#print axioms step_time
#print axioms exec_time
#print axioms step_ran
#print axioms exec_ran_le_time
#print axioms alone_time_eq_ran
#print axioms adv_head_none
#print axioms step_cur_keeps
#print axioms step_age
#print axioms age_counts
#print axioms started_sound

end K0Share
