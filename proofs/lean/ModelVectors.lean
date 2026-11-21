/-
ModelVectors.lean: the recorded results of the model `K0Part.step`.

WHAT THIS FILE IS
  The executable `vectors`.  It reads a file of event scripts
  (`crates/k0-part/src/model_vectors.txt`), runs every script through
  `K0Part.step` from `K0Part.boot`, and writes one canonical line for the boot
  state and one after every event.  The hash of those lines is the recorded
  result of the script.  The Rust host test `crates/k0-part/src/model_tests.rs`
  runs the same scripts through the real `Core<u64>` and compares the hashes,
  so CI, which has no Lean, fails when the Rust code and the recorded results
  differ.

  This is testing, not proof.  A passing test says that `Core` and `step` gave
  the same lines on the recorded scripts, with a list in place of the kernel's
  queues.  It says nothing about other inputs.  Nothing here is used by a
  theorem, and no theorem is proved here.

USE (in `proofs/lean`)
  lake exe vectors FILE           print FILE again with freshly computed hashes
  lake exe vectors --check FILE   report every script whose stored hash is not
                                  the model's; exit status 1 if there is one
  lake exe vectors --trace FILE   print the canonical lines of every script
  Exit status 2: the file cannot be read, a line is not a script this file
  accepts, or (with --check) a hash is missing or there is no script at all.

FILE
  A line that is empty or starts with `#` is kept as it is.  Every other line
  is one script, fields separated by `;`:
    id ; boot tick ; domain of task 0, 1, ... ; first task ; ready tasks in
    order ; waiting tasks in order as tid@deadline or tid@- ; events ; hash
  Events: `tN` tick with index N, `pN` poll with index N, `y` yield, `b-` and
  `bN` block without a deadline and with deadline N, `wT` wake of task T,
  `xT` wakeX of task T, `sD:W,D:W,...` set_table.  The hash may be left out
  when the file is printed again.

  `Script.check` refuses what does not mean the same on both sides.  It
  accepts: task domains below 8, the first task in domain 0, known task ids,
  tables as the Rust constructors accept them (1 to 16 entries, domain below
  8, window 1 to 255), tick indexes and deadlines below 2^32, no deadline 0
  (the kernel keeps 0 for "no deadline").  One accepted input is treated
  differently by the two sides, a block while nothing runs (below).

THE TWO SIDES
  Here a script is `boot` followed by `step` for every event (`Script.lines`).
  The Rust test starts `Core` with `Core::start` at the boot tick, with the
  ready tasks in the queues and the waiting tasks in the waiting lists of
  their domains.  `tN` and `pN` set `Env::now` to N and call `Core::tick` and
  `Core::poll`.  `y` calls `yield_now`.  `b` puts the running task, if there
  is one, at the end of its domain's waiting list and calls `block`.  `w` and
  `x` move a waiting task to the back of its queue under the condition `step`
  has (a task runs; own domain for `w`, another domain for `x`); they are not
  calls of `Core`.  `s` calls `set_table`.
  While nothing runs, `y` and `b` still call `yield_now` and `block` in the
  test and `step` does nothing.  For `y` that is the same thing.  For `b` it
  is not: `Core::block` pops the queue of the running domain.  The two agree
  because that queue is empty whenever nothing runs (`blockIdleRust_eq`, under
  `Inv`).  The kernel raises neither; the recorded scripts hold them because
  nothing else reaches the early return of `Core::yield_now`.

CANONICAL LINE (`showSt`; `model_tests.rs` builds the same text from `Core`)
  seen=N tbl=D:W,... ep=N pend=START/D:W,... rd=D cur=T q=[..|..] wl=[..|..] nx=B
  `pend=-` without a pending table.  `cur=T*` when the running task has the
  exemption, `cur=-` when nothing runs.  `q` and `wl` list domains 0 to 7
  separated by `|`, tasks separated by a blank, a waiter as tid@deadline or
  tid@-.  `nx` is 1 when `nextSwitches` holds and 0 otherwise.

HASH (`hashLines`; the same in `model_tests.rs`)
  FNV-1a, 64 bit, over the bytes of the lines of a script in order, every line
  followed by one newline byte: start with 14695981039346656037
  (0xcbf29ce484222325); for every byte, xor the byte into the hash, then
  multiply by 1099511628211 (0x100000001b3) modulo 2^64.  Written as 16 hex
  digits.  It only keeps the file small.  It is not a security measure.
-/
import K0Part
open K0Part

namespace ModelVectors

/-! ## The canonical line and the hash -/

/-- Number of domains printed in a line (`MAX_DOMAINS` in Rust). -/
def ndom : Nat := 8

def showTable (t : Table) : String :=
  ",".intercalate (t.map fun e => s!"{e.1}:{e.2}")

def showWait (w : Tid × Option Nat) : String :=
  match w.2 with
  | some k => s!"{w.1}@{k}"
  | none => s!"{w.1}@-"

/-- One list per domain, domains separated by `|`. -/
def showDoms (f : Dom → List String) : String :=
  "|".intercalate ((List.range ndom).map fun d => " ".intercalate (f d))

instance (s : St) : Decidable (nextSwitches s) := by
  unfold nextSwitches
  exact inferInstance

/-- The canonical line of a state. -/
def showSt (s : St) : String :=
  let p := s.part
  let pend := match p.pend with
    | some (start, t) => s!"{start}/{showTable t}"
    | none => "-"
  let cur := match s.cur with
    | some (t, true) => s!"{t}*"
    | some (t, false) => s!"{t}"
    | none => "-"
  let q := showDoms fun d => (s.q d).map toString
  let wl := showDoms fun d => (s.wl d).map showWait
  let nx := if nextSwitches s then "1" else "0"
  s!"seen={p.seen} tbl={showTable p.tbl} ep={p.epoch} pend={pend} rd={s.rd} cur={cur} q=[{q}] wl=[{wl}] nx={nx}"

/-- One round of FNV-1a over the bytes of `s`. -/
def fnv (h : UInt64) (s : String) : UInt64 :=
  s.toUTF8.foldl (fun h b => (h ^^^ b.toUInt64) * 1099511628211) h

/-- The hash of the lines of a script. -/
def hashLines (ls : List String) : UInt64 :=
  ls.foldl (fun h l => fnv h (l ++ "\n")) 14695981039346656037

def hex (h : UInt64) : String :=
  let d := Nat.toDigits 16 h.toNat
  String.ofList (List.replicate (16 - d.length) '0' ++ d)

-- The published FNV-1a values of the empty input, "a" and "foobar".
#guard hex (fnv 14695981039346656037 "") == "cbf29ce484222325"
#guard hex (fnv 14695981039346656037 "a") == "af63dc4c8601ec8c"
#guard hex (fnv 14695981039346656037 "foobar") == "85944171f73967e8"

/-! ## Scripts -/

/-- A script.  An event is kept with its text for `--trace`. -/
structure Script where
  now : Nat
  dom : Array Dom
  first : Tid
  ready : List Tid
  waits : List (Tid × Option Nat)
  evs : List (String × Ev)

def words (s : String) : List String :=
  (s.splitOn " ").filter (· != "")

def parseNat (s : String) : Except String Nat :=
  match s.toNat? with
  | some n => pure n
  | none => throw s!"bad number '{s}'"

def parseEntry (s : String) : Except String (Dom × Nat) :=
  match s.splitOn ":" with
  | [d, w] => do pure (← parseNat d, ← parseNat w)
  | _ => throw s!"bad table entry '{s}'"

def parseWait (s : String) : Except String (Tid × Option Nat) :=
  match s.splitOn "@" with
  | [t, "-"] => do pure (← parseNat t, none)
  | [t, d] => do pure (← parseNat t, some (← parseNat d))
  | _ => throw s!"bad waiter '{s}'"

def parseEv (tok : String) : Except String Ev :=
  let r := (tok.drop 1).toString
  match tok.front with
  | 't' => Ev.tick <$> parseNat r
  | 'p' => Ev.poll <$> parseNat r
  | 'y' => if r == "" then pure Ev.yield else throw s!"bad event '{tok}'"
  | 'b' => if r == "-" then pure (Ev.block none) else (Ev.block ∘ some) <$> parseNat r
  | 'w' => Ev.wake <$> parseNat r
  | 'x' => Ev.wakeX <$> parseNat r
  | 's' => Ev.setTable <$> (r.splitOn ",").mapM parseEntry
  | _ => throw s!"bad event '{tok}'"

/-- Tick indexes and deadlines stay below this, far from the end of `u64`. -/
def limit : Nat := 2 ^ 32

/-- A table the Rust constructors accept. -/
def okTable (t : Table) : Bool :=
  0 < t.length && t.length ≤ 16 && t.all fun e => e.1 < ndom && 0 < e.2 && e.2 ≤ 255

def okDeadline : Option Nat → Bool
  | some k => 0 < k && k < limit
  | none => true

/-- The event is one that means the same in the Rust test (`n` tasks). -/
def okEv (n : Nat) : Ev → Bool
  | .tick k | .poll k => k < limit
  | .yield => true
  | .block dl => okDeadline dl
  | .wake t | .wakeX t => t < n
  | .setTable t => okTable t

/-- The script is one that means the same on both sides. -/
def Script.check (sc : Script) : Except String Unit := do
  let n := sc.dom.size
  unless sc.dom.all (· < ndom) do throw "task domain out of range"
  unless sc.first < n && sc.dom.getD sc.first 0 == 0 do throw "the first task must be a task of domain 0"
  unless sc.now < limit do throw "boot tick too large"
  unless sc.ready.all (· < n) && sc.waits.all (·.1 < n) do throw "unknown task"
  unless sc.waits.all (okDeadline ·.2) do throw "deadline 0 or too large"
  for (tok, e) in sc.evs do
    unless okEv n e do throw s!"event '{tok}' is outside the common input"

def parseScript (now doms first ready waits evs : String) : Except String Script := do
  let sc : Script := {
    now := ← parseNat now
    dom := (← (words doms).mapM parseNat).toArray
    first := ← parseNat first
    ready := ← (words ready).mapM parseNat
    waits := ← (words waits).mapM parseWait
    evs := ← (words evs).mapM fun tok => do pure (tok, ← parseEv tok) }
  sc.check
  pure sc

/-! ## Running a script through the model -/

/-- The state before the first event and after every event. -/
def states (dom : Tid → Dom) (s : St) : List Ev → List St
  | [] => [s]
  | e :: es => s :: states dom (step dom s e) es

/-- The canonical lines of a script: `boot`, then `step` for every event. -/
def Script.lines (sc : Script) : List String :=
  let dom : Tid → Dom := fun t => sc.dom.getD t 0
  let s0 := boot sc.now sc.first (fun d => sc.ready.filter (dom · == d))
    (fun d => sc.waits.filter (dom ·.1 == d))
  (states dom s0 (sc.evs.map (·.2))).map showSt

/-! ## The file -/

/-- A line of the file. -/
inductive Line where
  /-- A comment or an empty line. -/
  | text (s : String)
  /-- A script: its text without the hash, its id, the stored hash if any. -/
  | script (body id : String) (stored : Option String) (sc : Script)

def parseLine (l : String) : Except String Line :=
  if l == "" || l.front == '#' then pure (.text l) else
  match l.splitOn ";" with
  | [id, now, doms, first, ready, waits, evs] =>
    .script l id none <$> parseScript now doms first ready waits evs
  | [id, now, doms, first, ready, waits, evs, h] =>
    .script (";".intercalate [id, now, doms, first, ready, waits, evs]) id (some h)
      <$> parseScript now doms first ready waits evs
  | _ => throw "not a script (7 or 8 fields separated by ';')"

/-- The lines of the file, or the first line that is not accepted. -/
def parseFile (text : String) : Except String (List Line) := do
  let ls := text.splitOn "\n"
  let ls := if ls.getLast? == some "" then ls.dropLast else ls
  let mut out := []
  let mut i := 1
  for l in ls do
    match parseLine l with
    | .ok x => out := x :: out
    | .error e => throw s!"line {i}: {e}"
    i := i + 1
  pure out.reverse

/-- The file again, every script with the hash the model gives. -/
def regen (ls : List Line) : String :=
  String.join (ls.map fun
    | .text s => s ++ "\n"
    | .script body _ _ sc => s!"{body};{hex (hashLines sc.lines)}\n")

/-- The scripts whose stored hash is not the model's, or the reason the file cannot be checked. -/
def check (ls : List Line) : Except String (Nat × List String) := do
  let mut n := 0
  let mut bad := []
  for l in ls do
    if let .script _ id stored sc := l then
      let h := hex (hashLines sc.lines)
      match stored with
      | none => throw s!"script {id} has no hash"
      | some st => if st != h then bad := s!"script {id}: stored {st}, model {h}" :: bad
      n := n + 1
  if n == 0 then throw "no script"
  pure (n, bad.reverse)

/-- The canonical lines of every script, each after the event that gave it. -/
def trace (ls : List Line) : String :=
  String.join (ls.map fun
    | .text _ => ""
    | .script _ id _ sc =>
      let toks := "boot" :: sc.evs.map (·.1)
      s!"script {id}\n" ++ String.join ((toks.zip sc.lines).map fun (t, l) => s!"  {t} -> {l}\n"))

end ModelVectors

open ModelVectors in
def main (args : List String) : IO UInt32 := do
  let (mode, path) := match args with
    | [path] => ("", path)
    | [mode, path] => (mode, path)
    | _ => ("?", "")
  unless mode == "" || mode == "--check" || mode == "--trace" do
    IO.eprintln "usage: vectors [--check | --trace] FILE"
    return 2
  let fail (e : String) : IO UInt32 := do
    IO.eprintln s!"{path}: {e}"
    return 2
  match ← (IO.FS.readFile path).toBaseIO with
  | .error e => fail (toString e)
  | .ok text =>
    match parseFile text with
    | .error e => fail e
    | .ok ls =>
      if mode == "" then
        IO.print (regen ls)
      else if mode == "--trace" then
        IO.print (trace ls)
      else
        match check ls with
        | .error e => return ← fail e
        | .ok (n, bad) =>
          for b in bad do IO.println b
          IO.println s!"scripts: {n}, differing from the model: {bad.length}"
          if !bad.isEmpty then return 1
      return 0
