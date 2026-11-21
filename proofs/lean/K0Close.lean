/-
K0Close.lean: statements that pin down definitions the other files leave free.

A sensitivity run changed one definition of the model at a time and built the
four files again.  A change that still builds is behaviour that no theorem
constrains.  Each section below closes one such change: the general theorem is
false in the changed file.  Every `inst_<id>` is one concrete instance of the
theorem above it, proved by `decide`, and every `from_<id>` derives the same
instance from the general theorem, so the instance failing means the theorem
fails.  The ids are those of the run (README.md, "Checks that are not in the
repository").

Nothing here changes `K0Part.lean` or `K0Bridge.lean`.
-/
import K0Part
import K0Bridge
open K0Part

namespace K0Close

/-! ## `view` and `viewTrace` (NV01, NV02, NV03, NV05) -/

/-- Equal views give the relation back: a view holds the queue, the waiting
    list and the running task of `D` with its slice bit, and nothing less. -/
theorem R_of_view {dom : Tid → Dom} {D : Dom} {s s' : St} (hi : Inv dom s) (hi' : Inv dom s')
    (hp : s.part = s'.part) (hr : s.rd = s'.rd) (hv : view dom D s = view dom D s') :
    R D s s' := by
  have hq : s.q D = s'.q D := congrArg View.q hv
  have hw : s.wl D = s'.wl D := congrArg View.wl hv
  refine ⟨⟨hp, hr, hq, hw⟩, ?_⟩
  intro hD
  have hrun := congrArg View.run hv
  simp only [view] at hrun
  rcases hc : s.cur with _ | ⟨t, b⟩ <;> rcases hc' : s'.cur with _ | ⟨t', b'⟩
  · rfl
  · have h2 : dom t' = D := by rw [hi'.cur_dom t' b' hc', ← hr]; exact hD
    simp [hc, hc', h2] at hrun
  · have h1 : dom t = D := by rw [hi.cur_dom t b hc]; exact hD
    simp [hc, hc', h1] at hrun
  · have h1 : dom t = D := by rw [hi.cur_dom t b hc]; exact hD
    have h2 : dom t' = D := by rw [hi'.cur_dom t' b' hc', ← hr]; exact hD
    simp [hc, hc', h1, h2] at hrun
    simp [hrun]

/-- The view trace has one view per step, the first one before any event. -/
theorem viewTrace_get (dom : Tid → Dom) (D : Dom) : ∀ (es : List Ev) (s : St) (k : Nat),
    k ≤ es.length →
    (viewTrace dom D s es)[k]? = some (view dom D (exec dom s (es.take k))) := by
  intro es
  induction es with
  | nil =>
    intro s k hk
    have : k = 0 := by simpa using hk
    subst this
    rfl
  | cons e es ih =>
    intro s k hk
    cases k with
    | zero => rfl
    | succ k =>
      simp only [viewTrace, List.getElem?_cons_succ, List.take_succ_cons, exec]
      exact ih _ k (by simpa using hk)

theorem viewTrace_length (dom : Tid → Dom) (D : Dom) : ∀ (es : List Ev) (s : St),
    (viewTrace dom D s es).length = es.length + 1 := by
  intro es
  induction es with
  | nil => intro s; rfl
  | cons e es ih => intro s; simp [viewTrace, ih]

/-- A state in which domain `d` runs task `c` and nothing else exists. -/
def one (d : Dom) (c : Option (Tid × Bool)) (w : List (Tid × Option Nat)) (seen : Nat) : St :=
  { part := { tbl := [(d, 1)], epoch := 0, pend := none, seen := seen }, rd := d, cur := c,
    q := fun _ => [], wl := fun x => if x = d then w else [] }

/-- Such a state satisfies `Inv` when the running task and the waiters belong to `d`. -/
theorem one_inv (d : Dom) (c : Tid) (b : Bool) (w : List (Tid × Option Nat)) (seen : Nat)
    (hc : dom0 c = d) (hw : ∀ x ∈ w, dom0 x.1 = d) :
    Inv dom0 (one d (some (c, b)) w seen) := by
  refine ⟨⟨?_, ⟨?_, ?_⟩, ?_, ?_⟩, ⟨Nat.zero_le _, ?_⟩⟩
  · simp [one, Part.plan, domAt, domAtPos, cycle, Nat.mod_one]
  · intro x t h; simp [one] at h
  · intro x y h
    simp only [one] at h
    split at h
    · next hx => subst hx; exact hw y h
    · simp at h
  · intro t b' h
    simp [one] at h
    rw [← h.1]; exact hc
  · intro h; simp [one] at h
  · intro k t h; simp [one] at h

theorem inst_NV01 : view dom0 1 (one 1 (some (10, false)) [(11, none)] 0)
    ≠ view dom0 1 (one 1 (some (10, false)) [] 0) := by decide
theorem inst_NV02 : view dom0 1 (one 1 (some (10, false)) [] 0)
    ≠ view dom0 1 (one 1 (some (11, false)) [] 0) := by decide
theorem inst_NV03 : view dom0 1 (one 1 (some (10, true)) [] 0)
    ≠ view dom0 1 (one 1 (some (10, false)) [] 0) := by decide
theorem inst_NV05 : (viewTrace dom0 1 sA esA).length = esA.length + 1 := by decide

/-- The three instances are instances of `R_of_view`: the states satisfy `Inv`,
    have the same clock part and running domain, and are not related by `R`. -/
theorem from_NV01 : view dom0 1 (one 1 (some (10, false)) [(11, none)] 0)
    ≠ view dom0 1 (one 1 (some (10, false)) [] 0) := fun hv =>
  have h : R 1 (one 1 (some (10, false)) [(11, none)] 0) (one 1 (some (10, false)) [] 0) :=
    R_of_view (one_inv 1 10 false [(11, none)] 0 (by decide) (by decide))
      (one_inv 1 10 false [] 0 (by decide) (by decide)) rfl rfl hv
  absurd h.wl (by decide)
theorem from_NV02 : view dom0 1 (one 1 (some (10, false)) [] 0)
    ≠ view dom0 1 (one 1 (some (11, false)) [] 0) := fun hv =>
  have h : R 1 (one 1 (some (10, false)) [] 0) (one 1 (some (11, false)) [] 0) :=
    R_of_view (one_inv 1 10 false [] 0 (by decide) (by decide))
      (one_inv 1 11 false [] 0 (by decide) (by decide)) rfl rfl hv
  absurd (h.cur rfl) (by decide)
theorem from_NV03 : view dom0 1 (one 1 (some (10, true)) [] 0)
    ≠ view dom0 1 (one 1 (some (10, false)) [] 0) := fun hv =>
  have h : R 1 (one 1 (some (10, true)) [] 0) (one 1 (some (10, false)) [] 0) :=
    R_of_view (one_inv 1 10 true [] 0 (by decide) (by decide))
      (one_inv 1 10 false [] 0 (by decide) (by decide)) rfl rfl hv
  absurd (h.cur rfl) (by decide)
theorem from_NV05 : (viewTrace dom0 1 sA esA).length = esA.length + 1 :=
  viewTrace_length dom0 1 esA sA

/-! ## The slice bit on the paths the bridge leaves out (ND16, ND25, NX03) -/

theorem enter_bit (s : St) (d : Dom) (t : Tid) (b : Bool) (h : (enter s d).cur = some (t, b)) :
    b = false := by
  unfold enter at h
  split at h
  · simp at h; exact h.2
  · simp at h

/-- After a tick with a new index the running task has no exemption: a tick
    clears the mark, and a task that a tick starts gets none, also when the
    tick finds the domain idle. -/
theorem tick_clears_mark (dom : Tid → Dom) (s : St) (n : Nat) (hn : s.part.seen < n)
    (t : Tid) (b : Bool) (h : (step dom s (.tick n)).cur = some (t, b)) : b = false := by
  have hn' : ¬ n ≤ s.part.seen := by omega
  simp only [step, tickStep, hn', ite_false] at h
  split at h
  · unfold sameStep at h
    split at h
    · simp at h; exact h.2
    · next c hc =>
      split at h
      · simp at h; exact h.2
      · rw [hc] at h; simp at h; exact h.2
    · exact enter_bit _ _ t b h
  · exact enter_bit _ _ t b h

/-- A poll without a new tick leaves a running task alone, with its bit. -/
theorem poll_keeps_running (dom : Tid → Dom) (s : St) (n : Nat) (hn : n ≤ s.part.seen)
    (c : Tid × Bool) (hc : s.cur = some c) : (step dom s (.poll n)).cur = some c := by
  have hn' : ¬ s.part.seen < n := by omega
  simp only [step, pollStep, hn', ite_false]
  unfold pollRest
  have : (serve s s.rd n).cur = some c := by simpa using hc
  rw [this]
  exact this

/-- A task that the idle loop starts has the exemption. -/
theorem poll_idle_mark (dom : Tid → Dom) (s : St) (n : Nat) (hn : n ≤ s.part.seen)
    (hc : s.cur = none) (t : Tid) (b : Bool) (h : (step dom s (.poll n)).cur = some (t, b)) :
    b = true := by
  have hn' : ¬ s.part.seen < n := by omega
  simp only [step, pollStep, hn', ite_false] at h
  unfold pollRest at h
  have hs : (serve s s.rd n).cur = none := by simpa using hc
  rw [hs] at h
  simp only at h
  unfold popBetween at h
  split at h
  · simp at h; exact h.2
  · simp at h

/-- "The running task, if there is one, has bit `b`", in a form `decide` can read. -/
def bitIs (s : St) (b : Bool) : Prop := s.cur.map Prod.snd ≠ some (!b)

instance (s : St) (b : Bool) : Decidable (bitIs s b) := by unfold bitIs; exact inferInstance

theorem bitIs_of (s : St) (b : Bool) (h : ∀ t b', s.cur = some (t, b') → b' = b) : bitIs s b := by
  unfold bitIs
  rcases hc : s.cur with _ | ⟨t, b'⟩
  · simp
  · have := h t b' hc
    subst this
    cases b' <;> simp

theorem inst_ND25 : bitIs (step dom0 (one 0 none [(1, some 1)] 0) (.tick 1)) false := by decide
theorem inst_ND16 : bitIs (step dom0 (one 0 none [(1, some 3)] 5) (.poll 5)) true := by decide
theorem inst_NX03 : (step dom0 (one 0 (some (0, true)) [] 5) (.poll 5)).cur = some (0, true) := by decide

theorem from_ND25 : bitIs (step dom0 (one 0 none [(1, some 1)] 0) (.tick 1)) false :=
  bitIs_of _ _ (tick_clears_mark dom0 _ 1 (by decide))
theorem from_ND16 : bitIs (step dom0 (one 0 none [(1, some 3)] 5) (.poll 5)) true :=
  bitIs_of _ _ (poll_idle_mark dom0 _ 5 (by decide) rfl)
theorem from_NX03 : (step dom0 (one 0 (some (0, true)) [] 5) (.poll 5)).cur = some (0, true) :=
  poll_keeps_running dom0 _ 5 (by decide) (0, true) rfl

/-! ## The waiting list after a poll, in order (ND31) -/

/-- A poll without a new tick takes the due waiters out of the waiting list of
    the running domain and leaves the others in their order. -/
theorem poll_wl (dom : Tid → Dom) (s : St) (n : Nat) (hn : n ≤ s.part.seen) :
    (step dom s (.poll n)).wl s.rd = (s.wl s.rd).filter (fun w => !expired w n) := by
  have hn' : ¬ s.part.seen < n := by omega
  simp only [step, pollStep, hn', ite_false, pollRest_wl]
  simp [serve, upd]

/-- The same for a tick with a new index, for the domain that runs after it
    (a tick inside a window and a tick that changes the domain). -/
theorem tick_wl (dom : Tid → Dom) (s : St) (n : Nat) (hn : s.part.seen < n) :
    (step dom s (.tick n)).wl (step dom s (.tick n)).rd
      = (s.wl (step dom s (.tick n)).rd).filter (fun w => !expired w n) := by
  have hn' : ¬ n ≤ s.part.seen := by omega
  simp only [step, tickStep, hn', ite_false]
  split
  · simp [serve, upd]
  · simp [serve, upd]

theorem inst_ND31 : (step dom0 (one 0 (some (0, false)) [(1, none), (2, none)] 5) (.poll 5)).wl 0
    = [(1, none), (2, none)] := by decide
theorem from_ND31 : (step dom0 (one 0 (some (0, false)) [(1, none), (2, none)] 5) (.poll 5)).wl 0
    = [(1, none), (2, none)] :=
  poll_wl dom0 (one 0 (some (0, false)) [(1, none), (2, none)] 5) 5 (by decide)

/-! ## A wake while nothing runs (NX01) -/

theorem wakeX_idle_noop (dom : Tid → Dom) (s : St) (t : Tid) (hc : s.cur = none) :
    step dom s (.wakeX t) = s := by
  simp [step, hc]

theorem wake_idle_noop (dom : Tid → Dom) (s : St) (t : Tid) (hc : s.cur = none) :
    step dom s (.wake t) = s := by
  simp [step, hc]

/-- Domain 0 idles, task 10 of domain 1 waits. -/
def idleX : St :=
  { part := { tbl := [(0, 1)], epoch := 0, pend := none, seen := 0 }, rd := 0, cur := none,
    q := fun _ => [], wl := fun x => if x = 1 then [(10, none)] else [] }

theorem inst_NX01 : (step dom0 idleX (.wakeX 10)).q 1 = [] := by decide
theorem from_NX01 : (step dom0 idleX (.wakeX 10)).q 1 = [] := by
  rw [wakeX_idle_noop dom0 idleX 10 rfl]; rfl

/-! ## A table of zero ticks (NT01), the boot epoch (NT09) -/

theorem domAtPos_zero_cycle : ∀ (t : Table) (p : Nat), cycle t = 0 → domAtPos t p = 0 := by
  intro t
  induction t with
  | nil => intro p _; rfl
  | cons e r ih =>
    obtain ⟨d, l⟩ := e
    intro p h
    simp only [cycle] at h
    have hl : l = 0 := by omega
    have hr : cycle r = 0 := by omega
    subst hl
    simp only [domAtPos, Nat.not_lt_zero, ite_false, Nat.sub_zero]
    exact ih p hr

/-- A table of zero ticks (empty, or only windows of 0 ticks) gives the root
    domain at every tick. -/
theorem domAt_zero_cycle (t : Table) (n : Nat) (h : cycle t = 0) : domAt t n = 0 :=
  domAtPos_zero_cycle t _ h

theorem inst_NT01 : domAt [] 5 = 0 := by decide
theorem from_NT01 : domAt [] 5 = 0 := domAt_zero_cycle [] 5 rfl

/-- At boot the default table starts at the boot tick. -/
theorem boot_part (now : Nat) (first : Tid) (q0 : Dom → List Tid)
    (w0 : Dom → List (Tid × Option Nat)) :
    (boot now first q0 w0).part = { tbl := defaultTable, epoch := now, pend := none, seen := now } :=
  rfl

theorem inst_NT09 : (boot 3 0 (fun _ => []) (fun _ => [])).part.epoch = 3 := by decide
theorem from_NT09 : (boot 3 0 (fun _ => []) (fun _ => [])).part.epoch = 3 := by
  rw [boot_part]

/-! ## The clause `epoch <= seen` of `PInv` (NV23: no other theorem uses it) -/

/-- What the clause is for, as a statement: at a new tick `n` the offset
    `n - epoch` that `Core::tick` computes after `advance` does not underflow. -/
theorem advance_epoch_le (p : Part) (n : Nat) (h : PInv p) (hn : p.seen < n) :
    (p.advance n).epoch ≤ n := by
  unfold Part.advance
  rcases hp : p.pend with _ | ⟨s0, t0⟩
  · have := h.epoch_le; simp; omega
  · by_cases hs : s0 ≤ n
    · simp [hs]
    · have := h.epoch_le; simp [hs]; omega

/-! ## Definitions that one statement uses (NV28, NB11) -/

/-- `blockIdleRust` does pop: with a task ready it starts that task, with the
    exemption.  (So `blockIdleRust_eq` is not about a function that does nothing.) -/
theorem blockIdleRust_pops (s : St) (x : Tid) (r : List Tid) (h : s.q s.rd = x :: r) :
    (blockIdleRust s).cur = some (x, true) ∧ (blockIdleRust s).q s.rd = r := by
  simp [blockIdleRust, popBetween, h, upd]

/-- Domain 0 has nothing running and task 7 ready (not an `Inv` state). -/
def idleQ : St :=
  { part := { tbl := [(0, 1)], epoch := 0, pend := none, seen := 0 }, rd := 0, cur := none,
    q := fun x => if x = 0 then [7] else [], wl := fun _ => [] }

theorem inst_NV28 : (blockIdleRust idleQ).cur = some (7, true) := by decide
theorem from_NV28 : (blockIdleRust idleQ).cur = some (7, true) :=
  (blockIdleRust_pops idleQ 7 [] rfl).1

/-- An idle step is not a step of the spinner. -/
theorem spin_idle (lab : Tid → Nat) (s : St) (h : s.cur = none) : K0Bridge.spin lab s = 0 := by
  simp [K0Bridge.spin, h]

theorem inst_NB11 : K0Bridge.spin id idleX = 0 := by decide
theorem from_NB11 : K0Bridge.spin id idleX = 0 := spin_idle id idleX rfl

end K0Close

#print axioms K0Close.R_of_view
#print axioms K0Close.viewTrace_get
#print axioms K0Close.tick_clears_mark
#print axioms K0Close.poll_keeps_running
#print axioms K0Close.poll_idle_mark
#print axioms K0Close.poll_wl
#print axioms K0Close.tick_wl
#print axioms K0Close.wakeX_idle_noop
#print axioms K0Close.domAt_zero_cycle
#print axioms K0Close.blockIdleRust_pops
#print axioms K0Close.spin_idle
#print axioms K0Close.inst_NV01
#print axioms K0Close.inst_NV02
#print axioms K0Close.inst_NV03
#print axioms K0Close.inst_NV05
#print axioms K0Close.from_NV01
#print axioms K0Close.from_NV02
#print axioms K0Close.from_NV03
#print axioms K0Close.from_NV05
#print axioms K0Close.inst_ND25
#print axioms K0Close.inst_ND16
#print axioms K0Close.inst_NX03
#print axioms K0Close.from_ND25
#print axioms K0Close.from_ND16
#print axioms K0Close.from_NX03
#print axioms K0Close.inst_ND31
#print axioms K0Close.from_ND31
#print axioms K0Close.inst_NX01
#print axioms K0Close.from_NX01
#print axioms K0Close.inst_NT01
#print axioms K0Close.from_NT01
#print axioms K0Close.inst_NT09
#print axioms K0Close.from_NT09
#print axioms K0Close.inst_NV28
#print axioms K0Close.from_NV28
#print axioms K0Close.inst_NB11
#print axioms K0Close.from_NB11
#print axioms K0Close.advance_epoch_le
