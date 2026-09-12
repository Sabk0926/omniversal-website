//! Baselines, the host knowledge base, and adapter training.
//!
//! Learning is ordered cheapest and safest first -- capabilities, baselines,
//! knowledge base, learned routing -- with LoRA adapters last, because weights
//! are the only level that is neither inspectable nor selectively deletable.
//!
//! Adapters are capabilities: trained on the builder box (never on the floor
//! target, which has nowhere near the memory), proven against a frozen eval
//! suite that includes a general-
//! capability regression set, shadow-run against the incumbent, then shipped as
//! a signed package. Only verified examples are eligible for training, which is
//! what keeps the system from collapsing onto its own output.

#![forbid(unsafe_op_in_unsafe_fn)]
