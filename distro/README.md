# Omnia OS

**A self-extending agentic operating system, based on Ubuntu 24.04.**

> The user never needs to know a command. When the OS lacks a capability, it builds one,
> proves it works, and permanently gains it.

Ask for nightly photo backups on a machine with no backup tool, and you don't get advice
or a shell command — you get a real, tested, packaged backup capability that the machine
now *has*, and can copy to your other machines. Plug in a device with no driver, and the
OS walks a ladder from `modprobe` up to writing a sandboxed userspace driver, tests that
the device actually returns sane data, and keeps it only if it does.

This is not Ubuntu with a chatbot in the dock. The model is the interface and the
maintainer, and the OS's vocabulary grows over its lifetime.

---

## The central problem

Total **freedom** to invent what it lacks, total **discipline** to remember what it
invented. These fight each other:

- Freedom alone → a junk drawer of twelve half-working backup scripts, three running at
  once, and a machine nobody can explain six months later.
- Discipline alone → something that can only ever do what was anticipated.

**The resolution: building something ends in *declaring* it, not in running it.** Every
generated capability becomes a versioned, tested, uninstallable `.deb`. Undo is
`apt remove`. Fleet distribution is copying a file. "Why is this on my machine" is
`dpkg -l` plus a provenance record. Thirty years of packaging infrastructure, reused
instead of reinvented.

## The capability lifecycle

Every intent — your words, or an event that implies a need — walks one pipeline:

| Stage | What happens |
|---|---|
| **1. Registry** | Do I already have this? Hit → use it. This is what prevents the junk drawer. |
| **2. Plan** | Composable from vetted parts? → the 1.5B local model wires them (offline, Pi-capable). Genuinely novel code? → escalate to a large local or cloud model. |
| **3. Materialise** | Emit a **permission manifest** first: exactly which paths, devices, syscalls and network. Undeclared access is denied by construction. |
| **4. Prove** | The model must write a test that proves the *capability*, not that the process exited 0. A backup must restore a file byte-for-byte. Fails → discarded, never installed. |
| **5. Declare** | Build a real `.deb`, install, register. The test is retained and re-run on every kernel and package upgrade. |

Stage 4 is load-bearing. It is what makes "the OS built itself a backup" trustworthy
rather than terrifying.

## It diagnoses, heals and learns

Governed by one rule: **nothing audits itself.** Capabilities are checked by the daemon,
the daemon by the kernel, the kernel from off-box.

- **Diagnosis** — because every capability carries a test that proves it *works*, the
  machine accumulates an executable definition of "healthy" that grows as it gains
  capabilities. `omni doctor` runs everything the machine claims it can do. Conventional
  monitoring can report that the backup process exited 0; it can never report that the
  backup can be restored.
- **Healing** — a kernel upgrade breaks a generated USB driver, its retained test fails on
  next boot, and the OS **rebuilds the driver against the new kernel** and re-runs the
  test. Passes, and nobody is paged. Fails, and it rolls back the kernel and *then*
  reports. Verification is always the retained test, never the model's own opinion that
  it fixed things.
- **Learning** — ordered by how inspectable it is. Capabilities first, then per-host
  baselines and a knowledge base, and only last a LoRA adapter trained on a builder box
  from verified outcomes. Weights are the only level that can't be selectively deleted,
  so they're the last resort rather than the headline.

## Architecture

The kernel *notices*; userspace *reasons*. No inference in ring 0, ever.

```
   kernel                                     userspace
 ┌────────────────────────────┐
 │ eBPF probes                │──events──►┌─────────────────────────────┐
 │  exec · oom · block · sig  │           │ omniad                       │
 ├────────────────────────────┤           │  forge · registry · sandbox  │
 │ omnia_kmod → /dev/omnia    │◄──ioctl───┤  autonomy · undo · audit     │
 │  event ring, executor claim│           └──────────────┬──────────────┘
 ├────────────────────────────┤                          │
 │ BPF-LSM guard              │                   ┌──────▼───────┐
 │  SELF-PRESERVATION FLOOR   │                   │ omnia-modeld │
 │  enforced below the daemon │                   │ llama.cpp    │
 └────────────────────────────┘                   └──────────────┘
```

**Why the floor is in the kernel:** the system is fully autonomous — a root daemon acts
unattended. A never-list the daemon enforces on itself is one bad generation away from
not existing. The BPF-LSM programs are pinned before `omniad` starts, in a cgroup it
cannot edit, and `omniad` refuses the executor role unless the guard reports ACTIVE.
No floor, no autonomy.

## Status

**Alpha, in active design.** See [docs/DESIGN.md](docs/DESIGN.md) for the full record
including open questions.

| Component | State |
|---|---|
| `kernel/omnia-kmod/` — char device, event ring, executor claim, watchdog | written, ~470 lines C |
| `kernel/bpf/` — CO-RE probes + BPF-LSM self-preservation floor | written, ~470 lines C |
| `kernel/omnia-kmod/omnia_abi.h` — padding-free ABI, parity-checked | written |
| `runtime/` — Rust workspace, 15 crates (abi, core, kernel, model, parts, forge, registry, sandbox, autonomy, audit, learn, http, CLI, daemons) | skeleton only, builds clean |
| `images/` — amd64 ISO + arm64 flashable builders | not started |

Nothing here boots yet. The kernel layer is the part that is real.

## Targets

Floor is a **4 GB ARM64 SBC** (Pi 5 class) running a 1.5B int4 orchestrator — if it
doesn't work there, it isn't in the design. The same multi-arch source produces an amd64
hybrid ISO for desktops and a flashable arm64 image for boards.

## Licence

GPL-3.0-or-later, matching the Ubuntu base it derives from.
