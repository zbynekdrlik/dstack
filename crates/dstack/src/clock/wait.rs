// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The Windows media threads' wait (dstack #1 problem A, #328), on the
//! clock it is given: the timer for all but the spin tail, then a spin
//! that yields (or, under the bounded escape, a pause). Cross-platform so
//! Linux tests it with a fake OS clock; only Windows waits with it.

#[cfg(test)]
mod tests;
