//! CPU-only tests for the border benchmark harness.
//!
//! The benchmark source is included as a module so its `#[cfg(test)]` tests
//! run under the standard libtest harness without creating an EGL context or
//! executing the benchmark `main()`. Run with:
//!
//! ```bash
//! cargo test --locked -p niri --test border_bench_tests
//! ```

// The module contains the benchmark entry point and GPU-rendering functions
// that are only exercised through `cargo bench`; they are dead code here.
#[allow(dead_code)]
#[path = "../benches/border_bench.rs"]
mod border_bench;
