//! Configuration, paths, errors and logging shared by every binary.
//!
//! Config layers lowest-to-highest: shipped -> machine -> drop-ins -> user ->
//! environment -> argv, with admin-lockable keys.

#![forbid(unsafe_op_in_unsafe_fn)]
