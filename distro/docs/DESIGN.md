# Omnia OS design

Working document. Records what we decided and why, plus what's still open.

## What we're building

An Ubuntu 24.04 derivative where you never need to know a command, and where the OS
builds capabilities it doesn't have.

Ask for nightly photo backups on a machine with no backup tool. It builds one, tests that
a file actually restores, packages it as a `.deb`, and installs it. The machine now has
backup. So does any other machine you copy that package to.

## The problem this has to solve

The OS needs freedom to invent things it doesn't have. It also needs to remember what it
invented. Those pull against each other.

Without discipline you get twelve half-working backup scripts, three of them running at
once, and a machine nobody can explain a year later.

Without freedom you get something that only does what someone anticipated.

**The fix: building ends in declaring, not in running.** Every generated capability
becomes a `.deb`. That gives us:

| Need | How `.deb` answers it |
|---|---|
| Undo it | `apt remove` |
| Why is this here? | `dpkg -l` plus a provenance file |
| Put it on my other machine | copy the file |
| Did it break? | its test is retained and re-run |

Building a custom registry would mean reimplementing packaging, badly.

## How a capability gets built

Any request goes through the same five steps. A request can come from you typing, or from
an event that implies a need (a device appeared, a disk is filling).

| Step | What happens |
|---|---|
| 1. Look it up | Do we already have this? If yes, use it. This is what stops the junk drawer. |
| 2. Plan | Can we build it from vetted parts? The small local model wires them together. If it needs genuinely new code, escalate to a bigger model. |
| 3. Write the permissions | Before any code: exactly which paths, devices, syscalls and network it needs. Anything not listed is blocked. |
| 4. Prove it | The model writes a test that proves the capability works. A backup must restore a file byte-for-byte. If the test fails, nothing gets installed. |
| 5. Declare it | Build the `.deb`, install it, register it. Keep the test and re-run it on every upgrade. |

Step 4 is the important one. It's the difference between "the OS built itself a backup"
being reassuring or alarming.

### Parts library

Vetted building blocks the small model composes: `snapshot`, `schedule`, `encrypt`,
`sync`, `watch-path`, `notify`, `http-fetch`, `usb-bulk`, `i2c-read`, `spi-xfer`,
`serial`, `framing`, `archive`, `verify-restore`.

This is what makes the 4 GB floor realistic. A 1.5B model can't write a correct backup
daemon. It can reliably pick `snapshot + schedule + encrypt` and point them at
`~/Pictures`. The library grows once, centrally, for everyone.

## Decisions

| # | Decision | Why |
|---|---|---|
| 1 | Kernel notices, userspace reasons | No floating point in ring 0, no forked kernel, keeps stock Ubuntu kernel updates and Secure Boot |
| 2 | 4 GB ARM64 board is the floor | If it doesn't work there it isn't in the design. Forces the parts library instead of wishful thinking |
| 3 | One deb source, two image builders | amd64 ISO for desktops, arm64 `.img` for boards, one rootfs recipe |
| 4 | Full autonomy, but only as far as things can be undone | See decision 15 for where that line falls |
| 5 | Rust userspace, C for module and BPF | Shell integration pays interpreter startup per keypress (~250ms on a Pi vs ~2ms). CPython is 55 MB in the initramfs vs ~3 MB. Python also can't read BPF ringbufs without a C shim |
| 6 | Build from parts *and* escalate to a big model | A 1.5B can wire parts but can't write daemons. Escalation covers the rest where hardware allows |
| 7 | Generated code declares its permissions up front | Renders into systemd hardening plus seccomp. Reuses a mature sandbox instead of writing one |
| 8 | Prefer userspace drivers | You can kill a userspace driver. There's no sandbox for a kernel module |
| 9 | Nothing is kept until its test passes | "The backup ran" and "the backup can be restored" are different claims |
| 10 | Nothing audits itself | A broken component's report on itself is worthless. Each layer is checked by the one below it |
| 11 | Learn per-host baselines, not fixed thresholds | A limit that's right for a desktop is wrong for a fanless board in a hot cabinet. Gradual failures cross no threshold |
| 12 | Hash-chained audit plus a kernel cross-check | Makes tampering detectable with no external infrastructure |
| 13 | Learn in capabilities first, weights last | Capabilities can be inspected, reverted and deleted. Weights can't |
| 14 | Adapters train on a builder box, not on the board | A 4 GB board can't train. Training data is structure, not file contents |
| 15 | Curated driver sources act alone; arbitrary repos don't | A backdoored driver that ran as the kernel isn't undone by uninstalling it |

## The driver ladder

When a device appears with no driver, work down this list. Cheapest and safest first.

| # | Situation | What happens |
|---|---|---|
| 1 | Driver is in the kernel, just not loaded | `modprobe`, check it binds |
| 2 | Needs a firmware blob | Fetch from `linux-firmware`, reload |
| 3 | Needs a quirk, ID, udev rule, or device-tree overlay | Generate config. No code |
| 4 | Driver source exists somewhere | Fetch, build against this kernel, DKMS, load, verify |
| 5 | USB/I2C/SPI/serial with no driver anywhere | Write a sandboxed userspace driver |
| 6 | Novel device needing kernel code | Scaffold and build, load only with rollback armed |

Rows 1 and 2 are two filesystem lookups and don't wake the model at all. Most devices end
there.

Row 5 is where the OS actually writes a driver, and it's sandboxable because it's
userspace. Good fit for boards, where an undocumented I2C sensor is a real and common
case.

Row 6's real blocker isn't writing code. A driver is essentially the chip's register map,
and nothing can infer that from a USB ID. With a datasheet it's doable. Without one,
nothing can do it.

### Driver source trust (rung 4)

Rung 4 compiles third-party code into your kernel. It's the most useful rung and the only
place the OS extends trust outward.

| Sources | Acts alone? |
|---|---|
| Ubuntu archive, `linux-firmware`, archive DKMS packages | Yes, archive-signed |
| A DKMS index you vet and sign | Yes, your key |
| Arbitrary vendor or community repos | No |

For the third case the OS does everything except decide. It finds the candidate, reads
the source, builds it in a sandbox, generates the test, and writes up what the code does
and what privileges it wants. Then it waits. You answer yes or no to a finished analysis,
not a research project.

Two things make this cheaper than it sounds:

- When rung 4 is blocked, rung 5 usually still works, and a sandboxed userspace driver is
  the better outcome anyway.
- You approve a driver once, not once per machine. It joins your signed index and the
  fleet picks it up automatically.

## Keeping itself working

The rule: **nothing audits itself.** Capabilities are checked by the daemon. The daemon is
checked by the kernel. The kernel is checked from off-box.

### Diagnosing

| Level | Question | How |
|---|---|---|
| Liveness | Is it running? | systemd, plus the kernel watchdog on the executor |
| Correctness | Is it doing the right thing? | The retained capability tests |
| Baseline | Is this normal for this machine? | Learned per-host profile |

The middle one comes free. Every capability keeps the test that proved it, so the machine
ends up with a working definition of "healthy" that grows as it gains capabilities.
`omni doctor` runs everything the machine claims it can do.

Normal monitoring can tell you the backup process exited 0. It can't tell you the backup
can be restored.

Baselines tracked: temperature curve, disk growth rate, boot time, memory ceilings,
restart frequency. Takes about a week to learn, and says so rather than guessing before
then.

### Healing

Verification at every tier is the capability's own retained test, not the model's opinion
that it fixed things.

| Tier | Action | How to undo |
|---|---|---|
| 0 | Restart or reload the unit | trivial |
| 1 | Reconfigure | undo journal |
| 2 | Repair or reinstall the package | `apt` |
| 3 | Rebuild the capability | previous `.deb` |
| 4 | Roll back to last known-good | boot sentinel |
| 5 | Stop, degrade safely, report loudly | — |

Tier 3 is the payoff. A kernel upgrade breaks a generated USB driver. Its test fails on
next boot. The OS rebuilds the driver against the new kernel and re-runs the test. If it
passes, nobody gets paged. If it fails, tier 4 rolls the kernel back and then reports.

This only works because the machine knows what its capabilities are supposed to do.

### Auditing

A root daemon writing its own log can lie or leave things out. If `omniad` is broken or
compromised, its account of itself is worth nothing. So:

1. **Hash-chained records.** Each entry commits to the previous one, so edits and
   deletions show up. Cheap, always on.
2. **Kernel cross-check.** The module counts executor actions, and the guard counts
   denials, in maps the daemon can't write. A doctored log shows up as a mismatch.

Records include the reasoning, not just the action: inputs, the plan, what it rejected,
and the test result. "Why did it restart postgres at 3am" needs a better answer than "it
restarted postgres at 3am."

**Limit worth stating plainly:** a fully compromised machine with nothing outside it can't
audit itself. Off-box anchoring and fleet cross-checking are the only real answers, and
both need something beyond the one box. Deferred, not rejected.

## Learning

Ordered by how inspectable each option is. Weights come last.

| # | Mechanism | Can you read it? | Can you undo it? | Can you copy it? |
|---|---|---|---|---|
| 1 | Capabilities | Yes, it's a `.deb` | `apt remove` | Copy the file |
| 2 | Baselines | Yes, numbers | Delete the row | Per-host anyway |
| 3 | Knowledge base (incidents, what worked) | Yes, editable text | Delete the entry | Selectively |
| 4 | Learned routing and examples | Yes, text | Revert | Yes |
| 5 | LoRA adapter | No | Swap the file | Yes |
| 6 | Full fine-tune | No | No | No |

Levels 1 to 4 give most of what "it's learning" feels like, with nothing hidden.

### Why adapters are still worth it

The retained tests produce labelled training data as a side effect. Every build attempt is
`(request → plan chosen → test passed or failed)`, with a real label. Most on-device
learning has no ground truth and ends up training on "the user didn't complain," which is
noise.

Scope stays narrow: picking which parts to compose, tool-call formatting, and local
vocabulary like your device names. Not general reasoning.

### The three risks

| Risk | What we do about it |
|---|---|
| Forgetting. A 1.5B model has little headroom and is more fragile to fine-tuning than a big one | The eval suite includes a frozen general-capability set. An adapter that regresses it is thrown away |
| Collapse from training on its own output | Only examples whose test actually passed are eligible |
| Can't train on a 4 GB board | LoRA on a 1.5B needs 6–12 GB. Training happens on the builder box |

### How an adapter ships

An adapter is a capability, so it goes through the same pipeline.

```
collect verified examples (plus redacted user corrections)
  → train on the builder box
  → must beat the current adapter on a frozen eval suite
  → shadow run: compare decisions against the current one
  → ship as a signed .deb; apt remove reverts it
```

Same builder box that handles novel code generation. One capable machine serves the
fleet.

### What can become training data

Allowed: the request, the plan, which parts were used, the test result, and your explicit
corrections after redaction.

Not allowed: file contents, command output, anything the context providers read.

So it learns that requests shaped a certain way are served by `snapshot + schedule +
encrypt`. It never learns what's in your photos.

The reason for the line: levels 1–4 let you delete one specific thing. A trained adapter
doesn't. There's no "forget that one file."

## Safety

Three layers, none of which ask you anything:

1. **Kernel floor.** BPF-LSM programs pinned before `omniad` starts, in a cgroup it can't
   edit. Blocks: writing the boot device, deleting the undo journal, modifying the guard,
   unmounting root, and touching the paths that keep you able to log in. `omniad` refuses
   to run autonomously unless the guard reports active.
2. **Capability sandbox.** The permission manifest becomes systemd hardening plus seccomp.
3. **Reversibility.** Every action records its undo first. Generated capabilities are
   packages. If a boot after an autonomous change fails twice, the initramfs replays the
   undo journal.

Plus an action budget with a circuit breaker. If it keeps failing to fix the same thing,
it drops to watching instead of looping all night.

## Where the code lives

```
distro/
├── kernel/          C. Written.
│   ├── omnia-kmod/  char device, event ring, executor claim, watchdog
│   └── bpf/         probes + the BPF-LSM floor + loader
├── runtime/         Rust workspace, 15 crates. Stubs.
│   └── crates/      abi, core, http, kernel, model, parts, forge, registry,
│                    sandbox, autonomy, audit, learn, omni, omniad, omnia-modeld
├── images/          Not started. amd64 ISO + arm64 img.
└── docs/            This file.
```

No libbpf anywhere. The ringbuf is read by mmapping the map fd directly, which is stable
kernel ABI. So the image ships no libbpf and the build needs no libelf.

## Build order

The original plan was seven horizontal phases: foundation, then model layer, then the
forge, and so on. That's probably wrong. It means building three layers before finding
out whether the core idea works.

Better: a thin vertical slice. The minimum of every layer needed to make one capability
work end to end.

- enough `omnia-core` to load config
- enough `omnia-model` to talk to llama.cpp
- three parts: `snapshot`, `schedule`, `verify-restore`
- the full five-step pipeline
- the cold-capability test

That's the riskiest part of the system. If it works, everything else is a variation on a
pipeline that already runs. If it doesn't, we find out in a week.

## Notes on the kernel code

- `omnia_abi.h` has no padding by design, so Rust can assert struct sizes at compile time
  and re-parse the header in a test. A DKMS module built from one version talking to a
  package from another otherwise shows up as garbled events, not an error.
- The event ring drops the **oldest** record when full. During an incident the newest
  events are the ones that explain it. Sequence numbers skip so the gap is visible.
- One process holds the executor claim, and the kernel raises an event if it stops
  heartbeating. A stuck daemon otherwise looks exactly like a healthy idle one.
- `OMNIA_IOC_EMIT` refuses `GUARD_DENY`. That's the event proving the floor works, so
  userspace must not be able to fake it.

## Still open

1. Which 1.5B and 0.5B GGUF builds to pin. Also whether their licences allow shipping them
   in an image and redistributing a LoRA adapter trained on them.
2. Whether escalation defaults to a big local model, a cloud API, or a builder box on the
   LAN when more than one is available.
3. How much of the parts library ships in v1. This decides whether a board can build
   anything useful offline.
4. What's in the frozen general-capability eval set, and who owns it. It's the only thing
   between incremental learning and a model that quietly forgot its job.
5. When off-box audit anchoring becomes necessary. It does the moment this runs on
   someone else's hardware.
